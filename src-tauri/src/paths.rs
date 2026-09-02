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

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use directories::ProjectDirs;

/// Qualifier/organization/application triple for [`ProjectDirs`]. One spelling,
/// referenced everywhere — a second literal anywhere in the tree is the bug this
/// module exists to prevent, and `app_dir_is_single_sourced` fails if one appears.
const APP_DIRS: (&str, &str, &str) = ("io.github", "tiredithumans", "NinjaOnePatchToolkit");

/// The directory holding `settings.json`, `action-audit.jsonl` and `logs/`.
pub fn app_dir() -> Result<PathBuf> {
    let (qualifier, organization, application) = APP_DIRS;
    ProjectDirs::from(qualifier, organization, application)
        .map(|d| d.config_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not determine the application config directory"))
}

/// `logs/`, where the rolling `tracing` file appender writes.
///
/// A subdirectory rather than loose files beside `settings.json`, so "reveal the
/// diagnostics folder" can open one place that holds only diagnostics and nothing an
/// operator might edit by hand.
pub fn log_dir() -> Result<PathBuf> {
    Ok(app_dir()?.join("logs"))
}

/// The audit trail's current location.
pub fn audit_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("action-audit.jsonl"))
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

    #[test]
    fn log_dir_and_audit_sit_inside_the_one_app_dir() {
        let app = app_dir().expect("app dir resolves on a test host");
        assert_eq!(log_dir().unwrap().parent(), Some(app.as_path()));
        assert_eq!(audit_path().unwrap().parent(), Some(app.as_path()));
        assert_eq!(
            audit_path().unwrap().file_name().unwrap(),
            "action-audit.jsonl"
        );
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
