//! Where this app keeps its files on disk — one definition, used by everything.
//!
//! There used to be two. `settings.rs` resolved
//! `ProjectDirs::from("io.github", "tiredithumans", "NinjaOnePatchToolkit")` and
//! `actions/audit.rs` resolved `ProjectDirs::from("", "", "ninjaone-patch-toolkit")`,
//! which are different directories on every platform — on macOS,
//! `~/Library/Application Support/io.github.tiredithumans.NinjaOnePatchToolkit` and
//! `~/Library/Application Support/ninjaone-patch-toolkit`. So the README's promise
//! that "every dispatched action is appended to `action-audit.jsonl` beside
//! `settings.json`" was not true, `docs/TROUBLESHOOTING.md` sent operators to the
//! wrong folder when they went looking for the audit trail, and a diagnostics bundle
//! would have had to know both.
//!
//! [`app_dir`] is now the only place the qualifier is written down. It keeps the
//! `settings.json` spelling, because that is the directory installs already have
//! their data in — moving settings would orphan every existing configuration.
//! [`legacy_audit_path`] covers the other direction: audit records written before
//! this change are still read, so no dispatch history is lost.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use tracing::warn;

/// Qualifier/organization/application triple for [`ProjectDirs`]. One spelling,
/// referenced everywhere — a second literal anywhere in the tree is the bug this
/// module exists to prevent, and `app_dir_is_single_sourced` fails if one appears.
const APP_DIRS: (&str, &str, &str) = ("io.github", "tiredithumans", "NinjaOnePatchToolkit");

fn project_dirs() -> Result<ProjectDirs> {
    let (qualifier, organization, application) = APP_DIRS;
    ProjectDirs::from(qualifier, organization, application)
        .ok_or_else(|| anyhow!("could not determine the application directories"))
}

/// The directory holding `settings.json`, `action-audit.jsonl`, `run-history.jsonl`
/// and `window-state.json` — what the operator configured and the durable record.
pub fn app_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// The machine-local directory for state that is bulky or rebuildable: `logs/`, the
/// first-seen ledger, the run snapshots and the exploit data.
///
/// On Windows [`app_dir`] is *Roaming* AppData, which a domain's roaming profiles
/// copy to the profile server at every logon and logoff, and which folder
/// redirection turns into an SMB write on every query — for files of up to 128 MB
/// holding every device's unpatched backlog. This is Local AppData instead. On macOS
/// the two are the same directory; on Linux this is `~/.local/share/…`.
pub fn local_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_local_dir().to_path_buf())
}

/// `name` under [`local_dir`], moved there from [`app_dir`] the first time an
/// upgraded install asks for it (see [`settle`]).
pub fn local_subdir(name: &str) -> Result<PathBuf> {
    Ok(settle(&app_dir()?.join(name), &local_dir()?.join(name)))
}

/// Serializes [`settle`]: the ledger, the snapshots and the exploit store can each
/// ask for their directory first, from different threads, on an upgrade's first run.
static MIGRATION: Mutex<()> = Mutex::new(());

/// Where a directory that moved from `old` to `new` now lives, moving it if only the
/// old one exists. A move that fails (another volume, a locked file) keeps the old
/// location in use rather than starting an empty new one: the first-seen ledger in
/// particular is the only record of when this app first saw each pending patch, and
/// losing it would restart every age.
fn settle(old: &Path, new: &Path) -> PathBuf {
    let _guard = MIGRATION.lock().unwrap_or_else(|e| e.into_inner());
    if old == new || new.exists() || !old.exists() {
        return new.to_path_buf();
    }
    if let Some(parent) = new.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        warn!(
            ?err,
            "could not create the local data directory; keeping the old one"
        );
        return old.to_path_buf();
    }
    match std::fs::rename(old, new) {
        Ok(()) => new.to_path_buf(),
        Err(err) => {
            warn!(
                ?err,
                "could not move a data directory to local storage; keeping it where it is"
            );
            old.to_path_buf()
        }
    }
}

/// Replaces `path` with `bytes` atomically. See [`write_atomic_with`].
pub fn write_atomic(path: &Path, bytes: &[u8], owner_only: bool) -> std::io::Result<()> {
    write_atomic_with(path, owner_only, |file| file.write_all(bytes))
}

/// Distinguishes two writes of one file from one process (the pid covers two
/// processes), so neither can rename the other's half-written temp into place.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Replaces `path` with whatever `write` puts into the handle it is given, atomically:
/// the bytes go to a temporary file beside `path`, are synced to disk, and the temp is
/// renamed over `path`. A plain write truncates first, so a crash, a full disk or a
/// power cut mid-write left half a file — unparseable on the next launch, or, for an
/// export, a shorter file that looked whole. On any failure the temp is removed and
/// `path` is untouched. The caller creates the directory.
///
/// The temp is hidden (`.name.tmp-<pid>-<n>`): every app instance and every writer
/// gets its own (a fixed `.tmp` let two instances interleave into one), and a
/// directory scan by extension never mistakes one for data. `owner_only` makes it
/// 0600 on unix before the first byte (other targets inherit the per-user profile
/// ACL), so the file is never readable by others, not even mid-write.
pub fn write_atomic_with<E: From<std::io::Error>>(
    path: &Path,
    owner_only: bool,
    write: impl FnOnce(&mut File) -> Result<(), E>,
) -> Result<(), E> {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = path.with_file_name(name);
    let written = (|| {
        let mut file = open_temp(&tmp, owner_only)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

fn open_temp(tmp: &Path, owner_only: bool) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if owner_only {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options.mode(0o600);
        let file = options.open(tmp)?;
        // `mode` covers only a file the open creates; a stale temp a crashed process
        // with a recycled pid left behind keeps its own mode unless narrowed here.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        return Ok(file);
    }
    #[cfg(not(unix))]
    let _ = owner_only;
    options.open(tmp)
}

/// `logs/`, where the rolling `tracing` file appender writes.
///
/// A subdirectory rather than loose files, so "reveal the diagnostics folder" can
/// open one place that holds only diagnostics and nothing an operator might edit by
/// hand. Machine-local: a week of logs has no business roaming.
pub fn log_dir() -> Result<PathBuf> {
    local_subdir("logs")
}

/// The audit trail's current location.
pub fn audit_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("action-audit.jsonl"))
}

/// The main window's remembered size and position (`window_state`).
pub fn window_state_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("window-state.json"))
}

/// Where the audit trail was written before [`app_dir`] single-sourced the
/// qualifier. Read-only: nothing appends here any more, but an install that
/// dispatched actions on an older build still has its history in this file and it
/// stays readable rather than silently disappearing on upgrade.
pub fn legacy_audit_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "ninjaone-patch-toolkit")
        .map(|d| d.config_dir().join("action-audit.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The temps left in `dir` (the hidden `.name.tmp-…` files).
    fn temps_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect()
    }

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp() {
        let root = scratch("atomic");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.json");
        std::fs::write(&path, b"old and much longer contents").unwrap();
        write_atomic(&path, b"new", false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(temps_in(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A failed write keeps the previous file whole and removes its temp.
    #[test]
    fn a_failed_atomic_write_keeps_the_old_file() {
        let root = scratch("atomic-fail");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.json");
        std::fs::write(&path, b"old").unwrap();
        let err = write_atomic_with(&path, true, |file| {
            file.write_all(b"half")?;
            Err::<(), _>(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert!(temps_in(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Owner-only before the first byte, and the replaced file ends up 0600 even if
    /// the old one was world-readable; without the flag the umask decides.
    #[cfg(unix)]
    #[test]
    fn an_owner_only_atomic_write_is_0600_from_its_first_byte() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let root = scratch("atomic-mode");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic_with(&path, true, |file| {
            assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
            file.write_all(b"{\"a\":1}")
        })
        .unwrap();
        assert_eq!(mode(&path), 0o600);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two writers of one file in one process get different temps, so the file
    /// ends up as one writer's bytes, whole.
    #[test]
    fn concurrent_atomic_writes_never_share_a_temp() {
        let root = scratch("atomic-race");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.json");
        std::thread::scope(|s| {
            for i in 0..8u8 {
                let path = &path;
                s.spawn(move || write_atomic(path, &[b'0' + i; 4096], true).unwrap());
            }
        });
        let body = std::fs::read(&path).unwrap();
        assert_eq!(body.len(), 4096);
        assert!(body.iter().all(|b| *b == body[0]));
        assert!(temps_in(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The audit trail stays beside `settings.json`: it is the durable record, and
    /// the README sends operators there. (`log_dir` is not called here — on a machine
    /// that has run the app, it would move the real `logs/`.)
    #[test]
    fn the_audit_trail_sits_in_the_app_dir() {
        let app = app_dir().expect("app dir resolves on a test host");
        assert_eq!(audit_path().unwrap().parent(), Some(app.as_path()));
        assert_eq!(
            audit_path().unwrap().file_name().unwrap(),
            "action-audit.jsonl"
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("npt-paths-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// An upgrade moves the directory, files and all, and later calls find it there.
    #[test]
    fn a_directory_only_the_old_location_has_is_moved() {
        let root = scratch("move");
        let (old, new) = (
            root.join("roaming/first-seen"),
            root.join("local/first-seen"),
        );
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("ledger.json"), b"{}").unwrap();
        assert_eq!(settle(&old, &new), new);
        assert!(new.join("ledger.json").exists() && !old.exists());
        assert_eq!(
            settle(&old, &new),
            new,
            "settled: the new place, nothing moved"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing to move, or already moved (both exist: the new one wins, the old is
    /// left alone), or the same directory on this platform: the new location.
    #[test]
    fn nothing_moves_unless_only_the_old_location_exists() {
        let root = scratch("stay");
        let (old, new) = (root.join("a/x"), root.join("b/x"));
        assert_eq!(settle(&old, &new), new, "a fresh install");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(settle(&old, &new), new);
        assert!(old.exists(), "an existing old directory is never deleted");
        assert_eq!(settle(&old, &old), old);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A move that cannot happen keeps the old directory in use, never an empty new one.
    #[cfg(unix)]
    #[test]
    fn a_move_that_fails_keeps_the_old_location() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = scratch("fail");
        let old = root.join("a/x");
        std::fs::create_dir_all(&old).unwrap();
        let locked = root.join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let new = locked.join("sub/x");
        let kept = settle(&old, &new);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Running as root ignores the mode, and the move succeeds; either way nothing
        // is lost.
        assert!(kept == old && old.exists() || kept == new && new.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_legacy_audit_path_is_a_different_directory() {
        // The whole reason this module exists: these two resolved independently and
        // disagreed. If a `directories` upgrade ever made them equal, the legacy
        // fallback would be reading the live file twice and this should be revisited.
        let legacy = legacy_audit_path().expect("legacy dirs resolve on a test host");
        assert_ne!(
            legacy.parent().unwrap(),
            app_dir().unwrap().as_path(),
            "legacy audit records live in a different directory than current ones"
        );
    }

    /// The qualifier triple must appear exactly once in the source tree — here.
    /// A second `ProjectDirs::from` is how the two directories diverged originally,
    /// and prose in this module's doc comment did not stop it happening.
    #[test]
    fn app_dir_is_single_sourced() {
        let roots = ["src"];
        let mut offenders = Vec::new();
        for root in roots {
            visit(std::path::Path::new(root), &mut |path, body| {
                // This file legitimately holds the one definition plus the
                // read-only legacy path.
                if path.ends_with("paths.rs") {
                    return;
                }
                if body.contains("ProjectDirs::from") {
                    offenders.push(path.display().to_string());
                }
            });
        }
        assert!(
            offenders.is_empty(),
            "ProjectDirs::from outside paths.rs — call paths::app_dir() instead: {offenders:?}"
        );
    }

    fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, f);
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(body) = std::fs::read_to_string(&path)
            {
                f(&path, &body);
            }
        }
    }
}
