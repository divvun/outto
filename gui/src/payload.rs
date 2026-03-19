//! Extract embedded .box payloads from the installer's own PE sections.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use box_format::sync::BoxReader;
use outto::Config;

use crate::pe;

/// An extracted payload ready to be used for installation.
pub struct ExtractedPayload {
    pub config: Config,
    pub source_dir: PathBuf,
    pub license_text: Option<String>,
    pub uninstall_exe: Option<PathBuf>,
    _temp_dir: tempfile::TempDir,
}

const SECTION_NAME: &str = ".outto";

/// Try to extract an embedded payload from the current executable's PE sections.
///
/// Returns `None` if no `.outto` section is found (traditional CLI mode).
/// Returns `Some(payload)` if the section exists and extraction succeeds.
pub fn extract_embedded_payload() -> Result<Option<ExtractedPayload>, Box<dyn std::error::Error>> {
    let exe_path = std::env::current_exe()?;

    let Some((offset, size)) = pe::find_section(&exe_path, SECTION_NAME)? else {
        return Ok(None);
    };

    let temp_dir = tempfile::tempdir()?;

    // Extract the .box section bytes to a temp file
    let temp_box_path = temp_dir.path().join("payload.box");
    {
        let mut exe_file = fs::File::open(&exe_path)?;
        exe_file.seek(SeekFrom::Start(offset))?;

        let mut box_data = vec![0u8; size as usize];
        exe_file.read_exact(&mut box_data)?;

        fs::write(&temp_box_path, &box_data)?;
    }

    // Open and extract
    let reader = BoxReader::open(&temp_box_path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Failed to open embedded payload: {e}")))?;

    let extract_dir = temp_dir.path().join("contents");
    fs::create_dir_all(&extract_dir)?;

    reader.extract_all(&extract_dir)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Failed to extract payload: {e}")))?;

    // Load config
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

    let uninstall_exe_path = extract_dir.join("uninstall.exe");
    let uninstall_exe = if uninstall_exe_path.exists() {
        Some(uninstall_exe_path)
    } else {
        None
    };

    Ok(Some(ExtractedPayload {
        config,
        source_dir,
        license_text,
        uninstall_exe,
        _temp_dir: temp_dir,
    }))
}
