//! Mach-O self-extractor for `outto` macOS installers.
//!
//! Parallel to `outto-sfx` (Windows PE): find the `__OUTTO` segment in our own
//! Mach-O, zstd-decompress the tarball it contains to `$TMPDIR/outto-installer-<pid>.app/`,
//! exec the inner installer's `Contents/MacOS/<name>` binary, wait, clean up.

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
    use std::io::{Read, Seek, SeekFrom};

    let exe = std::env::current_exe()?;
    let (offset, size) = outto_macos::macho::find_segment(&exe, "__OUTTO")?
        .ok_or("No embedded installer found in __OUTTO segment")?;

    // Read compressed tarball.
    let mut f = fs::File::open(&exe)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut compressed = vec![0u8; size as usize];
    f.read_exact(&mut compressed)?;
    drop(f);

    // Decompress + extract tarball to $TMPDIR/outto-installer-<pid>.app
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("outto-installer-{pid}.app"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;

    eprintln!("Decompressing installer...");
    let mut decoder = zstd::Decoder::new(&compressed[..])?;
    let mut archive = tar::Archive::new(&mut decoder);
    archive.unpack(&tmp)?;

    // Locate the inner binary: Contents/MacOS/<first file in MacOS dir>.
    let macos_dir = tmp.join("Contents/MacOS");
    let inner_exe = fs::read_dir(&macos_dir)?
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .ok_or("No executable found in extracted Contents/MacOS")?;

    // Make sure it's executable.
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&inner_exe)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(&inner_exe, perms)?;

    // Forward argv.
    let args: Vec<String> = std::env::args().skip(1).collect();
    eprintln!("Launching installer at {}", inner_exe.display());

    let status = std::process::Command::new(&inner_exe)
        .args(&args)
        .status()?;

    // Clean up the extracted bundle best-effort.
    let _ = fs::remove_dir_all(&tmp);

    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(target_os = "macos")]
fn fatal(msg: &str) -> ! {
    eprintln!("outto-sfx-macos: {msg}");
    std::process::exit(1);
}
