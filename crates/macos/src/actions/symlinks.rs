//! Create and roll back symlinks declared in `[[symlinks]]`.

use std::path::Path;

use outto_core::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{OverwritePolicy, SymlinkEntry};
use crate::manifest::Action;

pub fn create_symlink(
    entry: &SymlinkEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let target = resolver.resolve_path(&entry.target)?;
    let link = resolver.resolve_path(&entry.link)?;

    // Ensure parent directory exists.
    if let Some(parent) = link.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| InstallerError::DirOp {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    // Determine whether to replace an existing link/file.
    let (should_create, previous_target) = if link.exists() || link.is_symlink() {
        let existing_target = std::fs::read_link(&link).ok();
        match entry.overwrite {
            OverwritePolicy::Always | OverwritePolicy::IgnoreVersion => (true, existing_target),
            OverwritePolicy::Never => (false, None),
            OverwritePolicy::IfNewer | OverwritePolicy::ReplaceSameVersion => {
                // Filesystem mtime doesn't make sense for symlinks; fall back to "replace".
                (true, existing_target)
            }
            OverwritePolicy::Prompt | OverwritePolicy::PromptIfOlder => {
                let response = callbacks.on_prompt(Prompt::OverwriteFile { path: link.clone() });
                (matches!(response, PromptResponse::Yes), existing_target)
            }
        }
    } else {
        (true, None)
    };

    if !should_create {
        callbacks.on_log(
            LogLevel::Debug,
            &format!("Symlinks: skipping {} (overwrite policy)", link.display()),
        );
        return Ok(());
    }

    if link.exists() || link.is_symlink() {
        let _ = std::fs::remove_file(&link);
    }

    callbacks.on_log(
        LogLevel::Info,
        &format!("Symlinks: {} -> {}", link.display(), target.display()),
    );

    std::os::unix::fs::symlink(&target, &link).map_err(|e| InstallerError::FileOp {
        path: link.clone(),
        source: e,
    })?;

    manifest.record(Action::SymlinkCreated {
        link,
        target,
        previous_target,
    });

    Ok(())
}

/// Remove the symlink we created, optionally restoring a previous symlink target.
pub fn rollback_symlink(link: &Path, previous_target: Option<&Path>) -> InstallerResult<()> {
    if link.exists() || link.is_symlink() {
        std::fs::remove_file(link).map_err(|e| InstallerError::FileOp {
            path: link.to_path_buf(),
            source: e,
        })?;
    }
    if let Some(prev) = previous_target {
        std::os::unix::fs::symlink(prev, link).map_err(|e| InstallerError::FileOp {
            path: link.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::NoOpCallbacks;

    #[test]
    fn test_create_symlink_basic() {
        let dir = std::env::temp_dir().join(format!("outto-sl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let target = dir.join("target.txt");
        std::fs::write(&target, "content").unwrap();
        let link = dir.join("link.txt");

        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", dir.to_string_lossy());

        let entry = SymlinkEntry {
            target: "#{base}/target.txt".to_string(),
            link: "#{base}/link.txt".to_string(),
            overwrite: OverwritePolicy::Always,
            component: None,
            arch: None,
        };

        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        create_symlink(&entry, &resolver, &mut manifest, &NoOpCallbacks).unwrap();

        assert!(link.is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(manifest.actions.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_symlink_overwrite_always_captures_previous() {
        let dir = std::env::temp_dir().join(format!("outto-sl-ow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let orig_target = dir.join("orig.txt");
        std::fs::write(&orig_target, "orig").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&orig_target, &link).unwrap();

        let new_target = dir.join("new.txt");
        std::fs::write(&new_target, "new").unwrap();

        let mut resolver = VariableResolver::new().with_windows_paths(false);
        resolver.set_variable("base", dir.to_string_lossy());

        let entry = SymlinkEntry {
            target: "#{base}/new.txt".to_string(),
            link: "#{base}/link".to_string(),
            overwrite: OverwritePolicy::Always,
            component: None,
            arch: None,
        };

        let mut manifest = InstallManifest::<Action>::new("t", "T", "1.0.0", &dir, vec![]);
        create_symlink(&entry, &resolver, &mut manifest, &NoOpCallbacks).unwrap();

        // Replaced; points to new target.
        assert_eq!(std::fs::read_link(&link).unwrap(), new_target);

        // Previous target was captured.
        if let Action::SymlinkCreated {
            previous_target, ..
        } = &manifest.actions[0]
        {
            assert_eq!(previous_target.as_deref(), Some(orig_target.as_path()));
        } else {
            panic!("expected SymlinkCreated");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_restores_previous_symlink() {
        let dir = std::env::temp_dir().join(format!("outto-sl-rb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let old_target = dir.join("old.txt");
        std::fs::write(&old_target, "old").unwrap();
        let new_target = dir.join("new.txt");
        std::fs::write(&new_target, "new").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&new_target, &link).unwrap();

        rollback_symlink(&link, Some(&old_target)).unwrap();

        assert!(link.is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), old_target);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_deletes_when_no_previous() {
        let dir = std::env::temp_dir().join(format!("outto-sl-rb2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let target = dir.join("target.txt");
        std::fs::write(&target, "x").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        rollback_symlink(&link, None).unwrap();
        assert!(!link.exists() && !link.is_symlink());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
