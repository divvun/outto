//! Build and read `.box` archives that carry the staged source tree,
//! the `outto.toml` config, and an optional uninstaller.
//!
//! The Windows CLI embeds the output of [`pack_payload`] as a PE section;
//! the macOS chain does the same with a Mach-O section inside its `.app`
//! bundle. Both sides share this packer.
//!
//! The uninstaller is either a single file (Windows) or a directory tree
//! (macOS `.app`). Whatever it is called on disk, the packer stores it under a
//! fixed name at the archive root — [`UNINSTALL_EXE`] for a file,
//! [`UNINSTALL_APP`] for a directory — and [`find_extracted_uninstaller`] is
//! the one place that looks it up again, so the two sides can't drift apart.

use std::io;
use std::path::{Path, PathBuf};

use box_format::sync::BoxWriter;
use box_format::{BoxPath, Compression, CompressionConfig, HashMap};

/// Attributes to record for a packed file or directory.
///
/// Without this every entry was inserted with an empty attribute map, so no
/// `unix.mode` was stored and everything extracted as 0644 — silently stripping
/// the executable bit off anything the payload ships. That broke two things at
/// once on macOS: a payload binary invoked by a `[[run]]` hook (the hook fails,
/// usually invisibly, because it runs in the elevated child) and the extracted
/// uninstaller, which could never be launched.
///
/// Ownership is deliberately not recorded: the build agent's uid/gid mean
/// nothing on the target machine. box only stores a mode when it differs from
/// the default, so this adds bytes only for entries that actually need it.
fn attrs_for(path: &Path) -> HashMap<String, Vec<u8>> {
    match std::fs::metadata(path) {
        Ok(meta) => box_format::fs::metadata_to_attrs(&meta, true, false),
        Err(_) => HashMap::new(),
    }
}

/// Archive name of a single-file (Windows) uninstaller.
pub const UNINSTALL_EXE: &str = "uninstall.exe";
/// Archive name of a bundle (macOS) uninstaller.
pub const UNINSTALL_APP: &str = "uninstall.app";
/// Names older packers used for a single-file uninstaller: they kept the
/// source file name, so payloads built from `outto-uninstall.exe` carry it
/// under that name. Still accepted when extracting.
pub const LEGACY_UNINSTALL_EXE_NAMES: &[&str] = &["outto-uninstall.exe"];

/// The fixed archive name an uninstaller at `path` is stored under.
pub fn uninstaller_archive_name(path: &Path) -> &'static str {
    if path.is_dir() {
        UNINSTALL_APP
    } else {
        UNINSTALL_EXE
    }
}

/// Find the uninstaller in an extracted payload rooted at `extract_dir`:
/// [`UNINSTALL_EXE`], then any of [`LEGACY_UNINSTALL_EXE_NAMES`], then
/// [`UNINSTALL_APP`].
pub fn find_extracted_uninstaller(extract_dir: &Path) -> Option<PathBuf> {
    std::iter::once(UNINSTALL_EXE)
        .chain(LEGACY_UNINSTALL_EXE_NAMES.iter().copied())
        .map(|name| extract_dir.join(name))
        .find(|p| p.is_file())
        .or_else(|| {
            let app = extract_dir.join(UNINSTALL_APP);
            app.is_dir().then_some(app)
        })
}

/// Pack the config + staged source dir + uninstaller into a zstd-compressed
/// `.box` archive at `output_box`.
///
/// Archive layout:
/// - `outto.toml` at the root
/// - `uninstall.exe` at the root, OR `uninstall.app/**` subtree
/// - `source/**` — the entire staged source tree
///
/// An installer without an uninstaller registers nothing that can remove it,
/// so a missing `uninstall_path` is an error rather than a silent omission.
pub fn pack_payload(
    config_path: &Path,
    source_dir: &Path,
    output_box: &Path,
    uninstall_path: &Path,
) -> io::Result<()> {
    if !uninstall_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("uninstaller not found: {}", uninstall_path.display()),
        ));
    }

    let compression = CompressionConfig::new(Compression::Zstd);
    let mut writer = BoxWriter::create(output_box)?;

    let config_box_path =
        BoxPath::new("outto.toml").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    writer.insert_file(
        &compression,
        config_path,
        config_box_path,
        attrs_for(config_path),
    )?;

    let name = uninstaller_archive_name(uninstall_path);
    if uninstall_path.is_dir() {
        pack_directory_tree(&mut writer, &compression, uninstall_path, name)?;
    } else {
        let box_path =
            BoxPath::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        writer.insert_file(
            &compression,
            uninstall_path,
            box_path,
            attrs_for(uninstall_path),
        )?;
    }

    pack_directory_tree(&mut writer, &compression, source_dir, "source")?;

    writer.finish()?;
    Ok(())
}

/// Walk `dir` and insert every file/dir under `prefix/` in the archive.
fn pack_directory_tree(
    writer: &mut BoxWriter,
    compression: &CompressionConfig,
    dir: &Path,
    prefix: &str,
) -> io::Result<()> {
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let abs_path = entry.path();
        let rel_path = abs_path.strip_prefix(dir).map_err(io::Error::other)?;

        if rel_path.as_os_str().is_empty() {
            continue;
        }

        let box_path_str = format!("{prefix}/{}", rel_path.to_string_lossy().replace('\\', "/"));
        let box_path = BoxPath::new(&*box_path_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if entry.file_type().is_dir() {
            writer.mkdir_all(box_path, attrs_for(abs_path))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = BoxPath::new(&*box_path_str)
                .ok()
                .and_then(|p| p.parent().map(|p| p.into_owned()))
            {
                writer.mkdir_all(parent, HashMap::new())?;
            }
            writer.insert_file(compression, abs_path, box_path, attrs_for(abs_path))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use box_format::sync::BoxReader;
    use std::fs;

    fn stage(root: &Path) -> (PathBuf, PathBuf) {
        let config = root.join("outto.toml");
        fs::write(&config, "[package]\n").unwrap();
        let source = root.join("src");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("a.txt"), "a").unwrap();
        (config, source)
    }

    fn pack_and_extract(root: &Path, uninstaller: &Path) -> PathBuf {
        let (config, source) = stage(root);
        let out = root.join("payload.box");
        pack_payload(&config, &source, &out, uninstaller).unwrap();
        let extract = root.join("extract");
        fs::create_dir_all(&extract).unwrap();
        BoxReader::open(&out)
            .unwrap()
            .extract_all(&extract)
            .unwrap();
        extract
    }

    #[test]
    fn exe_uninstaller_is_stored_under_fixed_name() {
        let tmp = tempfile::tempdir().unwrap();
        let uninst = tmp.path().join("outto-uninstall.exe");
        fs::write(&uninst, "MZ").unwrap();

        let extract = pack_and_extract(tmp.path(), &uninst);

        assert!(extract.join(UNINSTALL_EXE).is_file());
        assert!(!extract.join("outto-uninstall.exe").exists());
        assert_eq!(
            find_extracted_uninstaller(&extract),
            Some(extract.join(UNINSTALL_EXE))
        );
    }

    #[test]
    fn app_uninstaller_is_stored_under_fixed_name() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Something.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/MacOS/Uninstall"), "bin").unwrap();

        let extract = pack_and_extract(tmp.path(), &app);

        assert!(
            extract
                .join("uninstall.app/Contents/MacOS/Uninstall")
                .is_file()
        );
        assert_eq!(
            find_extracted_uninstaller(&extract),
            Some(extract.join(UNINSTALL_APP))
        );
    }

    #[test]
    fn missing_uninstaller_fails_to_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let (config, source) = stage(tmp.path());
        let err = pack_payload(
            &config,
            &source,
            &tmp.path().join("payload.box"),
            &tmp.path().join("outto-uninstall.exe"),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn legacy_payload_name_is_still_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("outto-uninstall.exe"), "MZ").unwrap();
        assert_eq!(
            find_extracted_uninstaller(tmp.path()),
            Some(tmp.path().join("outto-uninstall.exe"))
        );
    }

    #[test]
    fn fixed_name_wins_over_legacy_name() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("outto-uninstall.exe"), "MZ").unwrap();
        fs::write(tmp.path().join(UNINSTALL_EXE), "MZ").unwrap();
        assert_eq!(
            find_extracted_uninstaller(tmp.path()),
            Some(tmp.path().join(UNINSTALL_EXE))
        );
    }

    #[test]
    fn no_uninstaller_in_payload() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("source")).unwrap();
        assert_eq!(find_extracted_uninstaller(tmp.path()), None);
    }
}
