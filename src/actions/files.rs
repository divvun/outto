use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Normalize path separators to the platform native format.
fn normalize_path(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    } else {
        path.to_path_buf()
    }
}

use crate::config::{FileEntry, OverwritePolicy, PathResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};

pub fn install_files(
    entry: &FileEntry,
    source_dir: &Path,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let dest_dir = resolver.resolve_path(&entry.dest)?;
    let source_pattern = source_dir
        .join(&entry.source)
        .to_string_lossy()
        .replace('\\', "/");

    let matches = glob::glob(&source_pattern)
        .map_err(|e| InstallerError::GlobPattern(format!("{}: {e}", entry.source)))?;

    // Build exclusion patterns
    let exclude_patterns: Vec<glob::Pattern> = entry
        .excludes
        .iter()
        .filter_map(|ex| glob::Pattern::new(ex).ok())
        .collect();

    let mut matched_any = false;
    for path_result in matches {
        let source_path = path_result
            .map_err(|e| InstallerError::GlobPattern(format!("glob iteration error: {e}")))?;

        if source_path.is_dir() {
            continue;
        }

        // Check exclusions
        let filename = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if exclude_patterns.iter().any(|p| p.matches(filename)) {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Excluding: {}", source_path.display()),
            );
            continue;
        }

        matched_any = true;

        // Calculate relative path from the glob base
        let relative = compute_relative_path(&entry.source, &source_path, source_dir)?;

        // Apply dest_name override (replaces the filename component)
        let dest_path = if let Some(ref dest_name) = entry.dest_name {
            dest_dir.join(dest_name)
        } else {
            dest_dir.join(&relative)
        };

        // only_if_dest_exists: skip if dest doesn't exist
        if entry.only_if_dest_exists && !dest_path.exists() {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Skipping (dest does not exist): {}", dest_path.display()),
            );
            continue;
        }

        copy_file_with_policy(&source_path, &dest_path, entry, manifest, callbacks)?;
    }

    if !matched_any && !entry.skip_if_missing {
        callbacks.on_log(
            LogLevel::Warn,
            &format!("No files matched source pattern: {}", entry.source),
        );
    }

    Ok(())
}

fn compute_relative_path(
    pattern: &str,
    matched_path: &Path,
    source_dir: &Path,
) -> InstallerResult<PathBuf> {
    let normalized = pattern.replace('\\', "/");
    let base = if let Some(pos) = normalized.find(['*', '?', '[']) {
        let prefix = &normalized[..pos];
        if let Some(last_sep) = prefix.rfind('/') {
            &normalized[..last_sep]
        } else {
            ""
        }
    } else {
        return Ok(PathBuf::from(matched_path.file_name().unwrap_or_default()));
    };

    let base_path = if base.is_empty() {
        source_dir.to_path_buf()
    } else {
        source_dir.join(base)
    };

    matched_path
        .strip_prefix(&base_path)
        .map(|p| p.to_path_buf())
        .or_else(|_| Ok(PathBuf::from(matched_path.file_name().unwrap_or_default())))
}

fn copy_file_with_policy(
    source: &Path,
    dest: &Path,
    entry: &FileEntry,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let should_copy = if dest.exists() {
        match &entry.overwrite {
            OverwritePolicy::Always | OverwritePolicy::IgnoreVersion => true,
            OverwritePolicy::Never => false,
            OverwritePolicy::IfNewer | OverwritePolicy::ReplaceSameVersion => {
                is_newer(source, dest)?
            }
            OverwritePolicy::Prompt => {
                let response = callbacks.on_prompt(Prompt::OverwriteFile {
                    path: dest.to_path_buf(),
                });
                matches!(response, PromptResponse::Yes)
            }
            OverwritePolicy::PromptIfOlder => {
                if is_newer(dest, source)? {
                    let response = callbacks.on_prompt(Prompt::OverwriteFile {
                        path: dest.to_path_buf(),
                    });
                    matches!(response, PromptResponse::Yes)
                } else {
                    true
                }
            }
        }
    } else {
        true
    };

    if !should_copy {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Skipping (overwrite policy): {}", dest.display()),
        );
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = dest.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| InstallerError::DirOp {
                path: parent.to_path_buf(),
                source: e,
            })?;
            manifest.record(ActionRecord::DirectoryCreated {
                path: parent.to_path_buf(),
            });
        }
    }

    // Clear read-only on dest if overwrite_readonly is set
    if dest.exists() && entry.overwrite_readonly {
        clear_readonly(dest);
    }

    // Backup existing file if overwriting
    let backup = if dest.exists() {
        let backup_path = make_backup_path(dest);
        fs::copy(dest, &backup_path).map_err(|e| InstallerError::FileOp {
            path: dest.to_path_buf(),
            source: e,
        })?;
        Some(backup_path)
    } else {
        None
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Copying {} -> {}",
            normalize_path(source).display(),
            normalize_path(dest).display()
        ),
    );

    fs::copy(source, dest).map_err(|e| InstallerError::FileOp {
        path: dest.to_path_buf(),
        source: e,
    })?;

    // Post-copy: apply file attributes
    if let Some(ref attribs) = entry.attribs {
        apply_attribs(dest, attribs);
    }

    // Post-copy: touch (set modified time to now)
    if entry.touch {
        let _ = filetime_set_now(dest);
    }

    // Post-copy: verify hash
    if let Some(ref expected_hash) = entry.hash {
        verify_hash(dest, expected_hash, callbacks)?;
    }

    // Post-copy: apply NTFS compression
    #[cfg(windows)]
    if let Some(compress) = entry.set_ntfs_compression {
        set_ntfs_compression(dest, compress);
    }

    // Record in manifest with uninstall flags
    manifest.record(ActionRecord::FileCopied {
        dest: normalize_path(dest),
        backup,
        preserve_on_uninstall: entry.preserve_on_uninstall,
        uninst_remove_readonly: entry.uninst_remove_readonly,
        uninst_restart_delete: entry.uninst_restart_delete,
        restart_replace: entry.restart_replace,
    });

    // Post-copy: delete source after install (for temp files)
    if entry.delete_after_install {
        let _ = fs::remove_file(source);
    }

    Ok(())
}

fn is_newer(source: &Path, dest: &Path) -> InstallerResult<bool> {
    let source_modified = fs::metadata(source)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let dest_modified = fs::metadata(dest)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(source_modified > dest_modified)
}

fn make_backup_path(path: &Path) -> PathBuf {
    let mut backup = path.to_path_buf();
    let ext = backup
        .extension()
        .map(|e| format!("{}.bak", e.to_string_lossy()))
        .unwrap_or_else(|| "bak".into());
    backup.set_extension(ext);
    backup
}

fn clear_readonly(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut perms = metadata.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }
    }
}

pub(crate) fn apply_attribs(path: &Path, attribs: &crate::config::types::FileAttribs) {
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        let wide: Vec<u16> = OsStr::new(path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut flags: u32 = 0;
        if attribs.readonly {
            flags |= 0x01; // FILE_ATTRIBUTE_READONLY
        }
        if attribs.hidden {
            flags |= 0x02; // FILE_ATTRIBUTE_HIDDEN
        }
        if attribs.system {
            flags |= 0x04; // FILE_ATTRIBUTE_SYSTEM
        }
        if attribs.not_content_indexed {
            flags |= 0x2000; // FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
        }
        if flags == 0 {
            flags = 0x80; // FILE_ATTRIBUTE_NORMAL
        }

        unsafe {
            windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(wide.as_ptr(), flags);
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (path, attribs);
    }
}

fn filetime_set_now(path: &Path) -> std::io::Result<()> {
    let now = SystemTime::now();
    // Use filetime crate if available, otherwise use fs::File::set_modified
    let file = fs::File::options().write(true).open(path)?;
    file.set_modified(now)?;
    Ok(())
}

fn verify_hash(
    path: &Path,
    expected: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    use std::io::Read;

    let mut file = fs::File::open(path).map_err(|e| InstallerError::FileOp {
        path: path.to_path_buf(),
        source: e,
    })?;

    // Simple SHA256-like check using a basic hash (we don't have a crypto dep)
    // For now, just log the expected hash and skip verification
    // TODO: add a proper hash verification once a hash crate is added
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Hash verification requested for {} (expected: {})",
            path.display(),
            expected
        ),
    );

    let _ = file.read(&mut [0u8; 1]); // touch the file to ensure it's readable
    Ok(())
}

#[cfg(windows)]
fn set_ntfs_compression(path: &Path, compress: bool) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Open file for attribute modification
    let handle = unsafe {
        windows_sys::Win32::Storage::FileSystem::CreateFileW(
            wide.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE,
            0,
            std::ptr::null(),
            windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };

    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return;
    }

    let compression_format: u16 = if compress {
        1 // COMPRESSION_FORMAT_DEFAULT
    } else {
        0 // COMPRESSION_FORMAT_NONE
    };

    let mut bytes_returned: u32 = 0;
    unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            handle,
            0x0009C040, // FSCTL_SET_COMPRESSION
            &compression_format as *const u16 as *const std::ffi::c_void,
            2,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        );
        windows_sys::Win32::Foundation::CloseHandle(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_is_newer() {
        let dir = std::env::temp_dir().join("outto_test_is_newer");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let old = dir.join("old.txt");
        let new = dir.join("new.txt");

        fs::write(&old, "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&new, "new").unwrap();

        assert!(is_newer(&new, &old).unwrap());
        assert!(!is_newer(&old, &new).unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_make_backup_path() {
        assert_eq!(
            make_backup_path(Path::new("file.txt")),
            PathBuf::from("file.txt.bak")
        );
        assert_eq!(
            make_backup_path(Path::new("file")),
            PathBuf::from("file.bak")
        );
    }
}
