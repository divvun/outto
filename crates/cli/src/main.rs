use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use outto_macos::Config;
#[cfg(windows)]
use outto_windows::Config;

#[cfg(windows)]
use outto_core::archive::pack_payload;
#[cfg(windows)]
use std::fs;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] != "build" {
        eprintln!("Usage: outto build --config <file> --source <dir> --output <exe> [--compress] [--compression-level <0-22>] [-S|--sign <command>]");
        std::process::exit(2);
    }

    let mut config_path: Option<PathBuf> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut compress = false;
    let mut compression_level: i32 = 3;
    let mut sign_command: Option<String> = None;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).map(PathBuf::from);
            }
            "--source" => {
                i += 1;
                source_dir = args.get(i).map(PathBuf::from);
            }
            "--output" => {
                i += 1;
                output = args.get(i).map(PathBuf::from);
            }
            "--compress" => {
                compress = true;
            }
            "--compression-level" => {
                i += 1;
                compression_level = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(3);
                if !(0..=22).contains(&compression_level) {
                    eprintln!("Compression level must be 0-22");
                    std::process::exit(2);
                }
            }
            "--sign" | "-S" => {
                i += 1;
                sign_command = args.get(i).cloned();
            }
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let config_path = config_path.unwrap_or_else(|| {
        eprintln!("Missing --config <file>");
        std::process::exit(2);
    });
    let source_dir = source_dir.unwrap_or_else(|| {
        eprintln!("Missing --source <dir>");
        std::process::exit(2);
    });
    let output = output.unwrap_or_else(|| {
        eprintln!("Missing --output <exe>");
        std::process::exit(2);
    });

    match build_installer(
        &config_path,
        &source_dir,
        &output,
        compress,
        compression_level,
        sign_command.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Build failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn build_installer(
    config_path: &Path,
    source_dir: &Path,
    output: &Path,
    compress: bool,
    compression_level: i32,
    sign_command: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use outto_core::actions::signing;
    use outto_core::NoOpCallbacks;
    use outto_windows::pe;

    if !config_path.exists() {
        return Err(format!("Config file not found: {}", config_path.display()).into());
    }
    if !source_dir.exists() || !source_dir.is_dir() {
        return Err(format!("Source directory not found: {}", source_dir.display()).into());
    }

    let _config = Config::from_file(config_path).map_err(|e| format!("Invalid config: {e}"))?;

    let cli_exe = std::env::current_exe()?;
    let cli_dir = cli_exe
        .parent()
        .ok_or("Cannot determine CLI exe directory")?;

    let libexec_dir = cli_dir
        .join("../libexec")
        .canonicalize()
        .unwrap_or_else(|_| cli_dir.to_path_buf());

    let template_exe = find_binary(&[&libexec_dir, cli_dir], "outto-gui.exe").ok_or_else(|| {
        format!(
            "Installer template not found.\nLooked in: {}, {}",
            libexec_dir.display(),
            cli_dir.display()
        )
    })?;

    let mut uninstall_exe = find_binary(&[&libexec_dir, cli_dir], "outto-uninstall.exe");
    if let Some(ref p) = uninstall_exe {
        eprintln!("Uninstaller: {}", p.display());
    } else {
        eprintln!(
            "Warning: outto-uninstall binary not found. Installer will not include an uninstaller."
        );
    }

    if let (Some(cmd), Some(ref uninstall_path)) = (sign_command, &uninstall_exe) {
        let callbacks = NoOpCallbacks;

        let temp_uninstall = tempfile::tempdir()?.into_path().join("outto-uninstall.exe");
        fs::copy(uninstall_path, &temp_uninstall)?;

        eprintln!("Signing uninstaller...");
        signing::sign_file(cmd, &temp_uninstall, &callbacks)
            .map_err(|e| format!("Failed to sign uninstaller: {e}"))?;

        uninstall_exe = Some(temp_uninstall);
    }

    let temp_dir = tempfile::tempdir()?;
    let temp_box_path = temp_dir.path().join("payload.box");

    eprintln!("Packing payload...");
    pack_payload(
        config_path,
        source_dir,
        &temp_box_path,
        uninstall_exe.as_deref(),
    )?;

    let box_size = fs::metadata(&temp_box_path)?.len();
    eprintln!(
        "Payload size: {} bytes ({:.1} MB)",
        box_size,
        box_size as f64 / 1_048_576.0
    );

    let box_data = fs::read(&temp_box_path)?;

    eprintln!("Template: {}", template_exe.display());
    fs::copy(&template_exe, output)?;

    eprintln!("Embedding payload into {}...", output.display());
    pe::embed_section(output, ".outto", &box_data)?;

    if compress {
        if let Some(cmd) = sign_command {
            let callbacks = NoOpCallbacks;
            eprintln!("Signing installer...");
            signing::sign_file(cmd, output, &callbacks)
                .map_err(|e| format!("Failed to sign installer: {e}"))?;
        }

        let sfx_stub = find_binary(&[&libexec_dir, cli_dir], "outto-sfx.exe").ok_or_else(|| {
            format!(
                "SFX stub not found.\nLooked in: {}, {}",
                libexec_dir.display(),
                cli_dir.display()
            )
        })?;

        eprintln!("Compressing installer...");
        let raw_installer = fs::read(output)?;
        let compressed = zstd::encode_all(&raw_installer[..], compression_level)?;

        eprintln!(
            "Compressed: {:.1} MB -> {:.1} MB ({:.0}% reduction)",
            raw_installer.len() as f64 / 1_048_576.0,
            compressed.len() as f64 / 1_048_576.0,
            (1.0 - compressed.len() as f64 / raw_installer.len() as f64) * 100.0,
        );

        fs::copy(&sfx_stub, output)?;
        pe::embed_section(output, ".outto", &compressed)?;
    }

    if let Some(cmd) = sign_command {
        let callbacks = NoOpCallbacks;
        eprintln!("Signing output...");
        signing::sign_file(cmd, output, &callbacks)
            .map_err(|e| format!("Failed to sign {}: {e}", output.display()))?;
    }

    let final_size = fs::metadata(output)?.len();
    eprintln!(
        "Done! {} ({:.1} MB)",
        output.display(),
        final_size as f64 / 1_048_576.0
    );

    Ok(())
}

#[cfg(target_os = "macos")]
fn build_installer(
    config_path: &Path,
    source_dir: &Path,
    output: &Path,
    compress: bool,
    compression_level: i32,
    sign_command: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use outto_core::actions::signing;
    use outto_core::archive::pack_payload;
    use outto_core::NoOpCallbacks;
    use outto_macos::macho;

    if !config_path.exists() {
        return Err(format!("Config file not found: {}", config_path.display()).into());
    }
    if !source_dir.exists() || !source_dir.is_dir() {
        return Err(format!("Source directory not found: {}", source_dir.display()).into());
    }

    let config = Config::from_file(config_path).map_err(|e| format!("Invalid config: {e}"))?;

    let callbacks = NoOpCallbacks;

    // Locate sibling binaries (bin/outto, libexec/outto-gui, libexec/outto-uninstall, libexec/outto-sfx-macos).
    let cli_exe = std::env::current_exe()?;
    let cli_dir = cli_exe
        .parent()
        .ok_or("Cannot determine CLI exe directory")?;
    let libexec = cli_dir
        .join("../libexec")
        .canonicalize()
        .unwrap_or_else(|_| cli_dir.to_path_buf());

    let gui_bin = find_binary(&[&libexec, cli_dir], "outto-gui")
        .ok_or("outto-gui binary not found (built by build-release.sh)")?;
    let uninstall_bin = find_binary(&[&libexec, cli_dir], "outto-uninstall");
    let sfx_bin = find_binary(&[&libexec, cli_dir], "outto-sfx-macos");

    eprintln!("GUI template: {}", gui_bin.display());

    // 1. Build uninstall.app (if uninstaller binary available).
    let scratch = tempfile::tempdir()?;
    let uninstall_app = if let Some(ref bin) = uninstall_bin {
        eprintln!("Uninstaller: {}", bin.display());
        let app = scratch.path().join("uninstall.app");
        build_app_bundle(
            &app,
            bin,
            &config.package.name,
            &format!("{}.uninstall", config.package.id),
            &format!("Uninstall {}", config.package.name),
            "Uninstall",
        )?;
        if let Some(cmd) = sign_command {
            eprintln!("Signing uninstall.app...");
            signing::sign_file(cmd, &app, &callbacks)
                .map_err(|e| format!("sign uninstall.app: {e}"))?;
        }
        Some(app)
    } else {
        eprintln!(
            "Warning: outto-uninstall binary not found; installer will not include an uninstaller."
        );
        None
    };

    // 2. Pack payload: config + source/** + uninstall.app/**.
    let box_path = scratch.path().join("payload.box");
    eprintln!("Packing payload...");
    pack_payload(config_path, source_dir, &box_path, uninstall_app.as_deref())?;
    let box_size = std::fs::metadata(&box_path)?.len();
    eprintln!(
        "Payload size: {} bytes ({:.1} MB)",
        box_size,
        box_size as f64 / 1_048_576.0
    );

    // 3. Build inner installer.app, embed payload in its Mach-O, codesign.
    let inner_app = scratch.path().join("installer-inner.app");
    build_app_bundle(
        &inner_app,
        &gui_bin,
        &config.package.name,
        &format!("{}.installer", config.package.id),
        &format!("{} Installer", config.package.name),
        "installer",
    )?;

    // Embed payload into the installer's Mach-O.
    let inner_mach_o = inner_app.join("Contents/MacOS/installer");
    eprintln!("Embedding payload into {}...", inner_mach_o.display());
    let box_data = std::fs::read(&box_path)?;
    macho::embed_segment(&inner_mach_o, "__OUTTO", &box_data)?;

    if let Some(cmd) = sign_command {
        eprintln!("Signing inner installer.app...");
        signing::sign_file(cmd, &inner_app, &callbacks)
            .map_err(|e| format!("sign inner app: {e}"))?;
    }

    if !compress {
        // Emit the inner installer.app directly.
        eprintln!("Copying to {}...", output.display());
        if output.exists() {
            std::fs::remove_dir_all(output).ok();
        }
        ditto(&inner_app, output)?;
        return Ok(());
    }

    // 4. Tar+zstd the inner .app.
    let sfx_bin = sfx_bin.ok_or("outto-sfx-macos not found (needed when --compress is set)")?;
    eprintln!("Tarring inner installer.app...");
    let tarball_path = scratch.path().join("inner.tar");
    tar_directory(&inner_app, &tarball_path)?;
    eprintln!("Compressing tarball with zstd level {compression_level}...");
    let tarball = std::fs::read(&tarball_path)?;
    let compressed = zstd::encode_all(&tarball[..], compression_level)?;
    eprintln!(
        "Compressed: {:.1} MB -> {:.1} MB ({:.0}% reduction)",
        tarball.len() as f64 / 1_048_576.0,
        compressed.len() as f64 / 1_048_576.0,
        (1.0 - compressed.len() as f64 / tarball.len() as f64) * 100.0,
    );

    // 5. Build outer SFX .app.
    if output.exists() {
        std::fs::remove_dir_all(output).ok();
    }
    build_app_bundle(
        output,
        &sfx_bin,
        &config.package.name,
        &format!("{}.sfx", config.package.id),
        &format!("Install {}", config.package.name),
        "sfx",
    )?;

    let outer_mach_o = output.join("Contents/MacOS/sfx");
    eprintln!("Embedding compressed payload into SFX...");
    macho::embed_segment(&outer_mach_o, "__OUTTO", &compressed)?;

    if let Some(cmd) = sign_command {
        eprintln!("Signing SFX .app...");
        signing::sign_file(cmd, output, &callbacks).map_err(|e| format!("sign SFX app: {e}"))?;
    }

    let final_size = walk_dir_size(output)?;
    eprintln!(
        "Done! {} ({:.1} MB)",
        output.display(),
        final_size as f64 / 1_048_576.0
    );

    Ok(())
}

#[cfg(target_os = "macos")]
fn find_binary(dirs: &[&Path], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Build a minimal `.app` bundle at `app_path`. Copies `mach_o` into
/// `Contents/MacOS/<exe_name>` and writes an Info.plist.
#[cfg(target_os = "macos")]
fn build_app_bundle(
    app_path: &Path,
    mach_o: &Path,
    display_name: &str,
    bundle_id: &str,
    _description: &str,
    exe_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let macos_dir = app_path.join("Contents/MacOS");
    let resources_dir = app_path.join("Contents/Resources");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(&resources_dir)?;

    // Copy the Mach-O binary into Contents/MacOS/<exe_name>.
    let dest_exe = macos_dir.join(exe_name);
    std::fs::copy(mach_o, &dest_exe)?;

    // Ensure the copied binary is executable.
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&dest_exe)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(&dest_exe, perms)?;

    // Write Info.plist.
    let info_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>{display_name}</string>
    <key>CFBundleExecutable</key>
    <string>{exe_name}</string>
    <key>CFBundleIdentifier</key>
    <string>{bundle_id}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>{display_name}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
"#
    );
    std::fs::write(app_path.join("Contents/Info.plist"), info_plist)?;
    Ok(())
}

/// Copy a directory tree with `ditto` to preserve xattrs/symlinks/signatures.
#[cfg(target_os = "macos")]
fn ditto(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new("ditto")
        .arg(src)
        .arg(dst)
        .status()?;
    if !status.success() {
        return Err(format!(
            "ditto {} -> {} failed ({status})",
            src.display(),
            dst.display()
        )
        .into());
    }
    Ok(())
}

/// Tar up a directory tree (non-compressed) into `output`.
#[cfg(target_os = "macos")]
fn tar_directory(src: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let tar_file = std::fs::File::create(output)?;
    let mut builder = tar::Builder::new(tar_file);
    let parent = src.parent().unwrap_or(std::path::Path::new("."));
    let name = src.file_name().unwrap_or(std::ffi::OsStr::new("."));
    builder.follow_symlinks(false);
    builder.append_dir_all(name, src)?;
    builder.finish()?;
    let _ = parent;
    Ok(())
}

#[cfg(target_os = "macos")]
fn walk_dir_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn build_installer(
    _config_path: &Path,
    _source_dir: &Path,
    _output: &Path,
    _compress: bool,
    _compression_level: i32,
    _sign_command: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("outto only supports Windows and macOS".into())
}

#[cfg(windows)]
fn find_binary(dirs: &[&Path], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}
