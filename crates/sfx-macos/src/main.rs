//! Mach-O self-extractor for `outto` macOS installers.
//!
//! Reads `Contents/Resources/payload.tar.zst` relative to our own Mach-O
//! (`Contents/MacOS/sfx`), zstd-decompresses the tarball to
//! `$TMPDIR/outto-installer-<pid>.app/`, execs the inner installer's
//! `Contents/MacOS/<name>` binary, waits, and cleans up.
//!
//! The payload lives in Resources rather than a custom Mach-O segment so
//! that `codesign --deep` can cover it without any Mach-O surgery. See the
//! CLI (`crates/cli/src/main.rs` macOS branch) for the packing side.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("outto-sfx-macos: only supported on macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    if let Err(e) = run() {
        fatal(&format!("{e}"));
    }
}

#[cfg(target_os = "macos")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    let exe = std::env::current_exe()?;
    // Contents/MacOS/sfx → Contents/Resources/payload.tar.zst
    let payload_path = exe
        .parent()
        .and_then(|p| p.parent())
        .map(|contents| contents.join("Resources/payload.tar.zst"))
        .ok_or("Could not derive Resources path from current exe")?;

    if !payload_path.exists() {
        return Err(format!("No embedded payload: expected {}", payload_path.display()).into());
    }

    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("outto-installer-{pid}.app"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;

    eprintln!("Decompressing installer...");
    let compressed = fs::File::open(&payload_path)?;
    let mut decoder = zstd::Decoder::new(compressed)?;
    let mut archive = tar::Archive::new(&mut decoder);
    archive.unpack(&tmp)?;

    let macos_dir = tmp.join("Contents/MacOS");
    let inner_exe = fs::read_dir(&macos_dir)?
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .ok_or("No executable found in extracted Contents/MacOS")?;

    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&inner_exe)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(&inner_exe, perms)?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    eprintln!("Launching installer at {}", inner_exe.display());

    let status = std::process::Command::new(&inner_exe)
        .args(&args)
        .status()?;

    let _ = fs::remove_dir_all(&tmp);
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(target_os = "macos")]
fn fatal(msg: &str) -> ! {
    eprintln!("outto-sfx-macos: {msg}");
    std::process::exit(1);
}
