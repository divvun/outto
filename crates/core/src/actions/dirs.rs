use std::fs;
use std::path::Path;

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
        create_dir_all_recorded(&path, manifest)?;
    }

    #[cfg(windows)]
    if let Some(ref attribs) = entry.attribs {
        super::files::apply_attribs(&path, attribs);
    }

    Ok(())
}

/// `create_dir_all`, recording a `DirectoryCreated` for every directory that
/// did not exist before, outermost first, so uninstall (which undoes actions
/// in reverse) removes the innermost first and can remove each once it is
/// empty. Directories that already existed are never recorded. Returns
/// whether `path` itself was created.
pub fn create_dir_all_recorded<A>(
    path: &Path,
    manifest: &mut InstallManifest<A>,
) -> InstallerResult<bool>
where
    A: From<CoreAction>,
{
    let missing: Vec<&Path> = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .collect();
    for dir in missing.iter().rev() {
        match fs::create_dir(dir) {
            Ok(()) => manifest.record(CoreAction::DirectoryCreated {
                path: dir.to_path_buf(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(InstallerError::DirOp {
                    path: dir.to_path_buf(),
                    source: e,
                });
            }
        }
    }
    Ok(!missing.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn records_only_missing_dirs_outermost_first() {
        let tmp = tempfile::tempdir().unwrap();
        let existing = tmp.path().join("existing");
        fs::create_dir(&existing).unwrap();
        let target = existing.join("a").join("b").join("c");

        let mut manifest = InstallManifest::<CoreAction>::new("t", "T", "1", tmp.path(), vec![]);
        assert!(create_dir_all_recorded(&target, &mut manifest).unwrap());
        assert!(target.is_dir());

        let recorded: Vec<PathBuf> = manifest
            .actions
            .iter()
            .map(|a| match a {
                CoreAction::DirectoryCreated { path } => path.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            recorded,
            vec![
                existing.join("a"),
                existing.join("a").join("b"),
                target.clone()
            ]
        );

        assert!(!create_dir_all_recorded(&target, &mut manifest).unwrap());
        assert_eq!(manifest.actions.len(), 3);
    }
}
