//! Build and read `.box` archives that carry the staged source tree,
//! the `outto.toml` config, and an optional uninstaller binary.
//!
//! The Windows CLI embeds the output of [`pack_payload`] as a PE section;
//! the eventual macOS chain will do the same with a Mach-O section inside
//! its `.app` bundle. Both sides share this packer.

use std::io;
use std::path::Path;

use box_format::sync::BoxWriter;
use box_format::{BoxPath, Compression, CompressionConfig, HashMap};

/// Pack the config + staged source dir + optional uninstaller into a zstd-compressed
/// `.box` archive at `output_box`.
///
/// Archive layout:
/// - `outto.toml` at the root
/// - `uninstall.exe` at the root (if provided)
/// - `source/**` — the entire staged source tree
pub fn pack_payload(
    config_path: &Path,
    source_dir: &Path,
    output_box: &Path,
    uninstall_exe: Option<&Path>,
) -> io::Result<()> {
    let compression = CompressionConfig::new(Compression::Zstd);
    let mut writer = BoxWriter::create(output_box)?;

    let config_box_path =
        BoxPath::new("outto.toml").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    writer.insert_file(&compression, config_path, config_box_path, HashMap::new())?;

    if let Some(uninstall_path) = uninstall_exe {
        let box_path = BoxPath::new("uninstall.exe")
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        writer.insert_file(&compression, uninstall_path, box_path, HashMap::new())?;
    }

    for entry in walkdir::WalkDir::new(source_dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let abs_path = entry.path();
        let rel_path = abs_path
            .strip_prefix(source_dir)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        if rel_path.as_os_str().is_empty() {
            continue;
        }

        let box_path_str = format!("source/{}", rel_path.to_string_lossy().replace('\\', "/"));
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
            writer.insert_file(&compression, abs_path, box_path, HashMap::new())?;
        }
    }

    writer.finish()?;
    Ok(())
}
