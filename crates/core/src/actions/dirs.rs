use std::fs;

use crate::callbacks::{InstallerCallbacks, LogLevel};
use crate::config::{DirEntry, VariableResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{CoreAction, InstallManifest};

/// Create a directory from a `DirEntry`. On Windows, applies file attributes
/// and (via `permissions`) icacls ACL entries after creation; these are handled
/// by the windows crate's action pipeline, not here.
pub fn create_directory<A>(
    entry: &DirEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<A>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()>
where
    A: From<CoreAction>,
{
    let path = resolver.resolve_path(&entry.path)?;

    if !path.exists() {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Dirs: creating {}", path.display()),
        );
        fs::create_dir_all(&path).map_err(|e| InstallerError::DirOp {
            path: path.clone(),
            source: e,
        })?;
        manifest.record(CoreAction::DirectoryCreated { path: path.clone() });
    }

    #[cfg(windows)]
    if let Some(ref attribs) = entry.attribs {
        super::files::apply_attribs(&path, attribs);
    }

    Ok(())
}
