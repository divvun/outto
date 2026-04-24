//! Build and read `.box` archives that carry the staged source tree,
//! the `outto.toml` config, and an optional uninstaller.
//!
//! The Windows CLI embeds the output of [`pack_payload`] as a PE section;
//! the macOS chain does the same with a Mach-O section inside its `.app`
//! bundle. Both sides share this packer.
//!
//! The uninstaller can be either a single file (Windows: `uninstall.exe`) or
//! a directory tree (macOS: `uninstall.app`). The packer figures out which
//! from the filesystem and writes it under a top-level prefix of the same
//! name — i.e. `uninstall.exe` becomes `uninstall.exe` at the archive root,
//! `uninstall.app` becomes `uninstall.app/**` rooted there.

use std::io;
use std::path::Path;

use box_format::sync::BoxWriter;
use box_format::{BoxPath, Compression, CompressionConfig, HashMap};

/// Pack the config + staged source dir + optional uninstaller into a
/// zstd-compressed `.box` archive at `output_box`.
///
/// Archive layout:
/// - `outto.toml` at the root
/// - `uninstall.exe` at the root, OR `uninstall.app/**` subtree (if provided)
/// - `source/**` — the entire staged source tree
pub fn pack_payload(
    config_path: &Path,
    source_dir: &Path,
    output_box: &Path,
    uninstall_path: Option<&Path>,
) -> io::Result<()> {
    let compression = CompressionConfig::new(Compression::Zstd);
    let mut writer = BoxWriter::create(output_box)?;

    let config_box_path =
        BoxPath::new("outto.toml").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    writer.insert_file(&compression, config_path, config_box_path, HashMap::new())?;

    if let Some(p) = uninstall_path {
        // Prefix the archive entry with the on-disk filename so Windows gets
        // `uninstall.exe` and macOS gets `uninstall.app/` — consumers key off
        // that name to decide how to extract.
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "uninstaller path has no filename"))?;

        if p.is_dir() {
            // Recursively add the bundle's contents.
            pack_directory_tree(&mut writer, &compression, p, name)?;
        } else {
            let box_path = BoxPath::new(name)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            writer.insert_file(&compression, p, box_path, HashMap::new())?;
        }
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
        let rel_path = abs_path
            .strip_prefix(dir)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        if rel_path.as_os_str().is_empty() {
            continue;
        }

        let box_path_str = format!("{prefix}/{}", rel_path.to_string_lossy().replace('\\', "/"));
        let box_path = BoxPath::new(&*box_path_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if entry.file_type().is_dir() {
            writer.mkdir_all(box_path, HashMap::new())?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = BoxPath::new(&*box_path_str)
                .ok()
                .and_then(|p| p.parent().map(|p| p.into_owned()))
            {
                writer.mkdir_all(parent, HashMap::new())?;
            }
            writer.insert_file(compression, abs_path, box_path, HashMap::new())?;
        }
    }
    Ok(())
}
