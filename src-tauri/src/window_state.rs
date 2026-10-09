//! Remembers the main window's size, position and maximized state across launches.
//!
//! A small file of its own (`window-state.json` beside `settings.json`), not a
//! `Settings` field: settings writes go through `AppState::write_settings`, which
//! serializes writers across a disk write before publishing a new snapshot, and a
//! window being dragged emits dozens of move events a second. Geometry is a per-machine convenience with no
//! bearing on any query, so it never touches that path.
//!
//! Written on move/resize after the events settle (debounced, then on a blocking
//! thread) and once more, synchronously, on close — the process may exit before a
//! spawned task would run. Restored before the window is first shown (the window
//! starts hidden in `tauri.conf.json`), clamped by [`placement`] so a position
//! saved on a monitor that is gone never puts the window off screen.
//!
//! A hand-rolled ~150 lines rather than `tauri-plugin-window-state`: the plugin
//! is a new dependency tree plus a capability grant for its JS API, and all this
//! app needs is one window's rectangle.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, Runtime, WebviewWindow, Window, WindowEvent};

/// The window whose geometry is remembered — the one `tauri.conf.json` declares.
pub const MAIN_WINDOW: &str = "main";

/// How long move/resize events must be quiet before the geometry is written. A
/// drag is a stream of `Moved` events; one write at the end is the point.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// How much of the window's top edge must land on a monitor for a saved position
/// to be kept: enough title bar to grab and drag it back.
const MIN_VISIBLE_WIDTH: i64 = 100;
const MIN_VISIBLE_HEIGHT: i64 = 40;

/// The saved rectangle, in physical pixels: outer position, inner size. When
/// `maximized`, the rectangle is the last *normal* one, so un-maximizing after a
/// relaunch returns to it rather than to the full screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// A monitor's work area (the screen minus taskbar/dock), in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// What to apply at startup: always a size, a position only when the saved one is
/// still reachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub width: u32,
    pub height: u32,
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
}

/// Fits a saved geometry to the monitors present now. `areas` lists the work
/// areas with the primary monitor first.
///
/// - A position is kept only when the window's top edge overlaps some work area
///   by a grab-able strip (`MIN_VISIBLE_*`); the window is then shrunk to that
///   area and nudged fully inside it.
/// - Otherwise the position is dropped (the OS places the window) and the size is
///   fitted to the primary monitor.
/// - A degenerate saved size, or no monitors reported at all, yields `None`: the
///   configured default is safer than a guess.
pub fn placement(saved: WindowGeometry, areas: &[WorkArea]) -> Option<Placement> {
    if saved.width == 0 || saved.height == 0 {
        return None;
    }
    let primary = areas.first()?;
    let home = areas.iter().find(|a| title_bar_visible(&saved, a));
    let area = home.unwrap_or(primary);
    let width = saved.width.min(area.width);
    let height = saved.height.min(area.height);
    let position = home.map(|a| {
        let right = i64::from(a.x) + i64::from(a.width) - i64::from(width);
        let bottom = i64::from(a.y) + i64::from(a.height) - i64::from(height);
        let x = i64::from(saved.x).min(right).max(i64::from(a.x));
        let y = i64::from(saved.y).min(bottom).max(i64::from(a.y));
        (x as i32, y as i32)
    });
    Some(Placement {
        width,
        height,
        position,
        maximized: saved.maximized,
    })
}

fn title_bar_visible(w: &WindowGeometry, a: &WorkArea) -> bool {
    let (wx, wy, ww) = (i64::from(w.x), i64::from(w.y), i64::from(w.width));
    let (ax, ay) = (i64::from(a.x), i64::from(a.y));
    let (ar, ab) = (ax + i64::from(a.width), ay + i64::from(a.height));
    let overlap = (wx + ww).min(ar) - wx.max(ax);
    overlap >= MIN_VISIBLE_WIDTH.min(ww) && wy >= ay && wy <= ab - MIN_VISIBLE_HEIGHT
}

pub fn load_from(path: &Path) -> Option<WindowGeometry> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Atomic, like `settings.json`: a temporary file renamed over the real one, so a
/// crash mid-write leaves the previous geometry rather than half a file.
/// **Blocking** file I/O.
pub fn save_to(path: &Path, geometry: &WindowGeometry) -> Result<()> {
    let dir = path.parent().context("window-state path has no parent")?;
    fs::create_dir_all(dir).context("create app dir")?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let written = serde_json::to_vec(geometry)
        .context("serialize window state")
        .and_then(|bytes| fs::write(&tmp, bytes).context("write window state"))
        .and_then(|()| fs::rename(&tmp, path).context("replace window state"));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Applies the saved geometry to the (still hidden) main window. Every failure is
/// a quiet fall back to the configured default size — never a reason not to start.
pub fn restore<R: Runtime>(window: &WebviewWindow<R>) {
    let Ok(path) = crate::paths::window_state_path() else {
        return;
    };
    let Some(saved) = load_from(&path) else {
        return;
    };
    let mut areas: Vec<WorkArea> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let r = m.work_area();
            WorkArea {
                x: r.position.x,
                y: r.position.y,
                width: r.size.width,
                height: r.size.height,
            }
        })
        .collect();
    if let Ok(Some(primary)) = window.primary_monitor() {
        let p = primary.work_area();
        if let Some(i) = areas
            .iter()
            .position(|a| a.x == p.position.x && a.y == p.position.y)
        {
            areas.swap(0, i);
        }
    }
    let Some(place) = placement(saved, &areas) else {
        return;
    };
    let _ = window.set_size(PhysicalSize::new(place.width, place.height));
    if let Some((x, y)) = place.position {
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    if place.maximized {
        let _ = window.maximize();
    }
}

/// Tracks the main window's geometry and writes it when it settles.
#[derive(Default)]
pub struct WindowStateSaver {
    /// Bumped per move/resize; a debounced write goes ahead only if no newer
    /// event arrived while it slept.
    generation: AtomicU64,
    /// The last un-maximized rectangle, which is what a maximized window saves.
    last_normal: Mutex<Option<WindowGeometry>>,
}

impl WindowStateSaver {
    /// The `on_window_event` hook for the main window.
    pub fn on_event<R: Runtime>(self: &Arc<Self>, window: &Window<R>, event: &WindowEvent) {
        if window.label() != MAIN_WINDOW {
            return;
        }
        match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                let Some(geometry) = self.observe(window) else {
                    return;
                };
                let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                let this = Arc::clone(self);
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(SAVE_DEBOUNCE).await;
                    if this.generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let _ = tauri::async_runtime::spawn_blocking(move || persist(&geometry)).await;
                });
            }
            // Synchronous on purpose: the app is closing, and a task spawned now
            // may never run. A few dozen bytes to a local file.
            WindowEvent::CloseRequested { .. } => {
                // Invalidate any debounced write still sleeping, so it cannot land
                // an older rectangle after this one.
                self.generation.fetch_add(1, Ordering::SeqCst);
                if let Some(geometry) = self.observe(window) {
                    persist(&geometry);
                }
            }
            _ => {}
        }
    }

    /// Reads the window's current geometry. `None` while minimized, where
    /// platforms report placeholder coordinates (Windows: -32000) that must never
    /// be saved as a position.
    fn observe<R: Runtime>(&self, window: &Window<R>) -> Option<WindowGeometry> {
        if window.is_minimized().unwrap_or(false) {
            return None;
        }
        let maximized = window.is_maximized().unwrap_or(false);
        let mut last = self.last_normal.lock().unwrap_or_else(|e| e.into_inner());
        if !maximized {
            let pos = window.outer_position().ok()?;
            let size = window.inner_size().ok()?;
            *last = Some(WindowGeometry {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
                maximized: false,
            });
        }
        // Maximized before any normal rectangle was seen (it launched maximized):
        // keep the saved normal one on disk rather than record the full screen.
        let normal = match *last {
            Some(g) => g,
            None => crate::paths::window_state_path()
                .ok()
                .and_then(|p| load_from(&p))?,
        };
        Some(WindowGeometry {
            maximized,
            ..normal
        })
    }
}

fn persist(geometry: &WindowGeometry) {
    let result = crate::paths::window_state_path().and_then(|path| save_to(&path, geometry));
    if let Err(e) = result {
        tracing::warn!(error = %e, "could not save the window position");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: WorkArea = WorkArea {
        x: 0,
        y: 0,
        width: 2560,
        height: 1560,
    };
    /// An external display to the laptop's right.
    const EXTERNAL: WorkArea = WorkArea {
        x: 2560,
        y: 0,
        width: 3840,
        height: 2120,
    };

    fn saved(x: i32, y: i32, width: u32, height: u32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
            maximized: false,
        }
    }

    #[test]
    fn an_on_screen_window_is_restored_as_saved() {
        let p = placement(saved(3000, 200, 2000, 1400), &[LAPTOP, EXTERNAL]).unwrap();
        assert_eq!(p.position, Some((3000, 200)));
        assert_eq!((p.width, p.height), (2000, 1400));
    }

    #[test]
    fn a_window_from_an_unplugged_monitor_drops_its_position() {
        // Saved on the external display, relaunched on the laptop alone: the
        // position would be off screen, so the OS places it and it fits the laptop.
        let p = placement(saved(3000, 200, 3000, 1800), &[LAPTOP]).unwrap();
        assert_eq!(p.position, None);
        assert_eq!((p.width, p.height), (2560, 1560));
    }

    #[test]
    fn a_title_bar_above_the_screen_is_not_kept() {
        let p = placement(saved(100, -500, 1360, 1000), &[LAPTOP]).unwrap();
        assert_eq!(p.position, None);
    }

    #[test]
    fn a_sliver_on_screen_is_not_enough_to_keep() {
        // 50 px of the title bar on screen: not something to grab.
        let p = placement(saved(-1310, 100, 1360, 1000), &[LAPTOP]).unwrap();
        assert_eq!(p.position, None);
        // 300 px is — and the window is nudged fully back on screen.
        let p = placement(saved(-1060, 100, 1360, 1000), &[LAPTOP]).unwrap();
        assert_eq!(p.position, Some((0, 100)));
    }

    #[test]
    fn a_window_hanging_off_the_bottom_right_is_pulled_inside() {
        let p = placement(saved(2000, 1200, 1360, 1000), &[LAPTOP]).unwrap();
        assert_eq!(p.position, Some((2560 - 1360, 1560 - 1000)));
    }

    #[test]
    fn a_window_larger_than_its_monitor_is_shrunk_to_it() {
        let p = placement(saved(0, 0, 5000, 4000), &[LAPTOP]).unwrap();
        assert_eq!((p.width, p.height), (2560, 1560));
        assert_eq!(p.position, Some((0, 0)));
    }

    #[test]
    fn negative_coordinates_on_a_left_hand_monitor_are_fine() {
        let left = WorkArea {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let p = placement(saved(-1800, 50, 1360, 900), &[LAPTOP, left]).unwrap();
        assert_eq!(p.position, Some((-1800, 50)));
    }

    #[test]
    fn maximized_is_carried_through() {
        let g = WindowGeometry {
            maximized: true,
            ..saved(10, 10, 1360, 1000)
        };
        assert!(placement(g, &[LAPTOP]).unwrap().maximized);
    }

    #[test]
    fn degenerate_input_falls_back_to_the_default() {
        assert_eq!(placement(saved(0, 0, 0, 1000), &[LAPTOP]), None);
        assert_eq!(placement(saved(0, 0, 1360, 1000), &[]), None);
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        let p = placement(saved(i32::MAX, i32::MIN, u32::MAX, u32::MAX), &[LAPTOP]).unwrap();
        assert_eq!(p.position, None);
        assert_eq!((p.width, p.height), (2560, 1560));
    }

    #[test]
    fn saves_atomically_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("npt-window-state-{}", std::process::id()));
        let path = dir.join("window-state.json");
        let g = WindowGeometry {
            maximized: true,
            ..saved(-40, 25, 1400, 900)
        };
        save_to(&path, &g).expect("save");
        assert_eq!(load_from(&path), Some(g));
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name() != "window-state.json")
            .collect();
        assert!(leftovers.is_empty(), "temporary file left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_file_is_no_saved_state() {
        let dir = std::env::temp_dir().join(format!("npt-window-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("window-state.json");
        fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_from(&path), None);
        assert_eq!(load_from(&dir.join("missing.json")), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
