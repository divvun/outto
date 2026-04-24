//! Extract embedded `.box` payloads from the installer's own executable
//! (PE `.outto` section on Windows, Mach-O `__OUTTO` segment on macOS).

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
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

#[cfg(windows)]
const SECTION_NAME: &str = ".outto";
#[cfg(target_os = "macos")]
const SECTION_NAME: &str = "__OUTTO";

#[cfg(windows)]
fn find_payload_section(exe_path: &std::path::Path) -> io::Result<Option<(u64, u64)>> {
    crate::pe::find_section(exe_path, SECTION_NAME)
}

#[cfg(target_os = "macos")]
fn find_payload_section(exe_path: &std::path::Path) -> io::Result<Option<(u64, u64)>> {
    outto_macos::macho::find_segment(exe_path, SECTION_NAME)
}

/// Extract the embedded payload from the current executable, if any.
///
/// Windows: reads the PE `.outto` section from the installer .exe.
/// macOS: reads the Mach-O `__OUTTO` segment from the installer binary
/// (typically at `*.app/Contents/MacOS/installer`).
pub fn extract_embedded_payload() -> Result<Option<ExtractedPayload>, Box<dyn std::error::Error>> {
    let exe_path = std::env::current_exe()?;

    let Some((offset, size)) = find_payload_section(&exe_path)? else {
        return Ok(None);
    };

    let temp_dir = tempfile::tempdir()?;

    let temp_box_path = temp_dir.path().join("payload.box");
    {
        let mut exe_file = fs::File::open(&exe_path)?;
        exe_file.seek(SeekFrom::Start(offset))?;
        let mut box_data = vec![0u8; size as usize];
        exe_file.read_exact(&mut box_data)?;
        fs::write(&temp_box_path, &box_data)?;
    }

    let reader = BoxReader::open(&temp_box_path).map_err(|e| {
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
            if p.exists() {
                Some(p)
            } else {
                None
            }
        }
        #[cfg(target_os = "macos")]
        {
            let p = extract_dir.join("uninstall.app");
            if p.exists() {
                Some(p)
            } else {
                None
            }
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
