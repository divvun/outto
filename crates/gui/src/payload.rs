//! Locate and extract the installer's embedded `.box` payload.
//!
//! - **Windows**: the `.outto` PE section of the installer `.exe`.
//! - **macOS**: `Contents/Resources/payload.box` next to the installer's
//!   Mach-O binary. Stored as a plain file rather than a Mach-O segment so
//!   codesign can cover it via `--deep` without any Mach-O surgery.

use std::fs;
use std::io;
use std::path::PathBuf;

use box_format::sync::BoxReader;

use crate::bridge::Config;

pub struct ExtractedPayload {
    pub config: Config,
    pub source_dir: PathBuf,
    pub license_text: Option<String>,
    pub uninstall_exe: Option<PathBuf>,
    _temp_dir: tempfile::TempDir,
}

/// Obtain a `.box` file on disk for the current process to read, or `None`
/// if there's no embedded payload (running as a standalone CLI, for example).
///
/// Windows: extracts the `.outto` PE section into a temp `.box`.
/// macOS: returns the path of `Contents/Resources/payload.box` directly.
#[cfg(windows)]
fn locate_box(_scratch: &std::path::Path) -> io::Result<Option<PathBuf>> {
    use std::io::{Read, Seek, SeekFrom};
    let exe_path = std::env::current_exe()?;
    let Some((offset, size)) = crate::pe::find_section(&exe_path, ".outto")? else {
        return Ok(None);
    };
    let tmp = _scratch.join("payload.box");
    let mut f = fs::File::open(&exe_path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; size as usize];
    f.read_exact(&mut buf)?;
    fs::write(&tmp, &buf)?;
    Ok(Some(tmp))
}

#[cfg(target_os = "macos")]
fn locate_box(_scratch: &std::path::Path) -> io::Result<Option<PathBuf>> {
    let exe_path = std::env::current_exe()?;
    // Contents/MacOS/<exe> → Contents/Resources/payload.box
    let Some(contents) = exe_path.parent().and_then(|p| p.parent()) else {
        return Ok(None);
    };
    let candidate = contents.join("Resources/payload.box");
    if candidate.exists() {
        Ok(Some(candidate))
    } else {
        Ok(None)
    }
}

/// Extract the embedded payload from the current executable, if any.
pub fn extract_embedded_payload() -> Result<Option<ExtractedPayload>, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;

    let Some(box_path) = locate_box(temp_dir.path())? else {
        return Ok(None);
    };

    let reader = BoxReader::open(&box_path).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to open embedded payload: {e}"),
        )
    })?;

    let extract_dir = temp_dir.path().join("contents");
    fs::create_dir_all(&extract_dir)?;

    reader.extract_all(&extract_dir).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to extract payload: {e}"),
        )
    })?;

    let config_path = extract_dir.join("outto.toml");
    if !config_path.exists() {
        return Err("Embedded payload does not contain outto.toml".into());
    }
    let config = Config::from_file(&config_path)
        .map_err(|e| format!("Failed to parse embedded config: {e}"))?;

    let source_dir = extract_dir.join("source");

    let license_text = config.package.license_file.as_ref().and_then(|lf| {
        let license_path = source_dir.join(lf);
        fs::read_to_string(&license_path).ok()
    });

    // The CLI packs the uninstaller as `uninstall.exe` on Windows; on macOS the
    // uninstaller is stored as a directory tree representing `uninstall.app`.
    let uninstall_exe = {
        #[cfg(windows)]
        {
            let p = extract_dir.join("uninstall.exe");
            if p.exists() { Some(p) } else { None }
        }
        #[cfg(target_os = "macos")]
        {
            let p = extract_dir.join("uninstall.app");
            if p.exists() { Some(p) } else { None }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            None
        }
    };

    Ok(Some(ExtractedPayload {
        config,
        source_dir,
        license_text,
        uninstall_exe,
        _temp_dir: temp_dir,
    }))
}
