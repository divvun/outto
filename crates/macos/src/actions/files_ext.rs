//! Bundle-aware copies via `ditto`.
//!
//! For entries with `bundle = true` in the TOML, we shell out to `ditto`
//! instead of using `std::fs::copy` — `.app` bundles carry resource forks,
//! extended attributes, symlinks within the bundle, and embedded code
//! signatures that `std::fs::copy` would lose.

use std::path::Path;
use std::process::Command;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{CoreAction, InstallManifest};

use crate::config::FileEntry;
use crate::manifest::Action;

/// Copy a whole bundle (or any directory tree) from `source` to `dest` using
/// `ditto`, and record a FileCopied action for the bundle root.
pub fn install_bundle(
    entry: &FileEntry,
    source_dir: &Path,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let source = source_dir.join(&entry.source);
    if !source.exists() {
        if entry.skip_if_missing {
            return Ok(());
        }
        return Err(InstallerError::Other(format!(
            "Files: bundle source {} not found",
            source.display()
        )));
    }

    let dest_dir = resolver.resolve_path(&entry.dest)?;
    std::fs::create_dir_all(&dest_dir).map_err(|e| InstallerError::DirOp {
        path: dest_dir.clone(),
        source: e,
    })?;

    let file_name = entry
        .dest_name
        .as_deref()
        .map(std::ffi::OsStr::new)
        .or_else(|| source.file_name())
        .ok_or_else(|| InstallerError::Other("Files: can't derive bundle name".into()))?;

    let dest = dest_dir.join(file_name);

    // If dest already exists, remove it first — `ditto --rsrc` with an existing
    // target merges rather than overwrites, which isn't what most installers want.
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| InstallerError::DirOp {
            path: dest.clone(),
            source: e,
        })?;
    }

    callbacks.on_log(
        LogLevel::Info,
        &format!("Files: ditto {} -> {}", source.display(), dest.display()),
    );

    let status = Command::new("ditto")
        .arg(&source)
        .arg(&dest)
        .status()
        .map_err(|e| InstallerError::Other(format!("Files: failed to run ditto: {e}")))?;

    if !status.success() {
        return Err(InstallerError::Other(format!(
            "Files: ditto {} -> {} failed (exit {status})",
            source.display(),
            dest.display()
        )));
    }

    manifest.record(CoreAction::FileCopied {
        dest,
        backup: None,
        preserve_on_uninstall: entry.preserve_on_uninstall,
        uninst_remove_readonly: false,
        uninst_restart_delete: false,
        restart_replace: false,
    });

    Ok(())
}
