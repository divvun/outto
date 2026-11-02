use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod pkg;

#[cfg(target_os = "macos")]
use outto_macos::Config;
#[cfg(windows)]
use outto_windows::Config;

#[cfg(windows)]
use outto_core::archive::pack_payload;
#[cfg(windows)]
use std::fs;

/// macOS-only: wrap the installer `.app` in a bootstrap `.pkg`.
#[derive(Default)]
struct PkgOptions {
    /// Produce a `.pkg` at `--output` instead of an `.app`/SFX.
    enabled: bool,
    /// pkg identifier; defaults to `config.package.id`.
    identifier: Option<String>,
    /// pkg version; defaults to `config.package.version`.
    version: Option<String>,
    /// "Developer ID Installer" identity passed to `productbuild --sign`.
    sign: Option<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).is_some_and(|arg| arg == "--version") {
        println!("outto {}", outto_core::VERSION);
        return;
    }

    if args.len() < 2 || args[1] != "build" {
        eprintln!(
            "Usage: outto build --config <file> --source <dir> --output <exe> [--compress] [--compression-level <0-22>] [-S|--sign <command>]\n\
             macOS .pkg: [--pkg] [--pkg-identifier <id>] [--pkg-version <v>] [--pkg-sign <Developer ID Installer identity>]\n\
             A .pkg is for one architecture, that of the outto-gui beside outto: build outto with --target <triple> for each.\n\
             outto --version"
        );
        std::process::exit(2);
    }

    let mut config_path: Option<PathBuf> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut compress = false;
    let mut compression_level: i32 = 3;
    let mut sign_command: Option<String> = None;
    let mut pkg = PkgOptions::default();

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
            "--pkg" => {
                pkg.enabled = true;
            }
            "--pkg-identifier" => {
                i += 1;
                pkg.identifier = args.get(i).cloned();
            }
            "--pkg-version" => {
                i += 1;
                pkg.version = args.get(i).cloned();
            }
            "--pkg-sign" => {
                i += 1;
                pkg.sign = args.get(i).cloned();
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

    if pkg.enabled && !cfg!(target_os = "macos") {
        eprintln!("--pkg is only supported on macOS");
        std::process::exit(2);
    }

    match build_installer(
        &config_path,
        &source_dir,
        &output,
        compress,
        compression_level,
        sign_command.as_deref(),
        &pkg,
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
    _pkg: &PkgOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    use outto_core::NoOpCallbacks;
    use outto_core::actions::signing;
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

    let mut uninstall_exe = find_binary(&[&libexec_dir, cli_dir], "outto-uninstall.exe")
        .ok_or_else(|| {
            format!(
                "Uninstaller outto-uninstall.exe not found.\nLooked in: {}, {}",
                libexec_dir.display(),
                cli_dir.display()
            )
        })?;
    eprintln!("Uninstaller: {}", uninstall_exe.display());

    let temp_dir = tempfile::tempdir()?;

    if let Some(cmd) = sign_command {
        let callbacks = NoOpCallbacks;

        let temp_uninstall = temp_dir.path().join("outto-uninstall.exe");
        fs::copy(&uninstall_exe, &temp_uninstall)?;

        eprintln!("Signing uninstaller...");
        signing::sign_file(cmd, &temp_uninstall, &callbacks)
            .map_err(|e| format!("Failed to sign uninstaller: {e}"))?;

        uninstall_exe = temp_uninstall;
    }

    let temp_box_path = temp_dir.path().join("payload.box");

    eprintln!("Packing payload...");
    pack_payload(config_path, source_dir, &temp_box_path, &uninstall_exe)?;

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
    pkg: &PkgOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    use outto_core::NoOpCallbacks;
    use outto_core::actions::signing;
    use outto_core::archive::pack_payload;

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
    let uninstall_bin = find_binary(&[&libexec, cli_dir], "outto-uninstall")
        .ok_or("outto-uninstall binary not found (built by build-release.sh)")?;
    let sfx_bin = find_binary(&[&libexec, cli_dir], "outto-sfx-macos");

    eprintln!("GUI template: {}", gui_bin.display());
    let pkg_slice = if pkg.enabled {
        Some(pkg::pkg_slice(&gui_bin, &uninstall_bin)?)
    } else {
        None
    };
    let version = &config.package.version;

    // 1. Build uninstall.app.
    let scratch = tempfile::tempdir()?;
    eprintln!("Uninstaller: {}", uninstall_bin.display());
    let uninstall_app = scratch.path().join("uninstall.app");
    build_app_bundle(
        &uninstall_app,
        &uninstall_bin,
        &config.package.name,
        &format!("{}.uninstall", config.package.id),
        "Uninstall",
        version,
    )?;
    if let Some(cmd) = sign_command {
        eprintln!("Signing uninstall.app...");
        signing::sign_file(cmd, &uninstall_app, &callbacks)
            .map_err(|e| format!("sign uninstall.app: {e}"))?;
    }

    // 2. Pack payload: config + source/** + uninstall.app/**.
    let box_path = scratch.path().join("payload.box");
    eprintln!("Packing payload...");
    pack_payload(config_path, source_dir, &box_path, &uninstall_app)?;
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
        "installer",
        version,
    )?;

    // Copy the payload into Contents/Resources/ instead of a Mach-O segment —
    // segment-append after __LINKEDIT breaks codesign's strict layout validation,
    // and Resources/ is what codesign --deep naturally covers.
    let inner_payload = inner_app.join("Contents/Resources/payload.box");
    eprintln!("Staging payload at {}...", inner_payload.display());
    std::fs::copy(&box_path, &inner_payload)?;

    if let Some(cmd) = sign_command {
        eprintln!("Signing inner installer.app...");
        signing::sign_file(cmd, &inner_app, &callbacks)
            .map_err(|e| format!("sign inner app: {e}"))?;
    }

    // 4. Emit the standalone .app at --output: the inner installer.app directly
    // when uncompressed, or an SFX wrapper when --compress.
    if !compress {
        eprintln!("Copying to {}...", output.display());
        if output.exists() {
            std::fs::remove_dir_all(output).ok();
        }
        ditto(&inner_app, output)?;
    } else {
        // Tar+zstd the inner .app, then build the outer SFX .app.
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

        if output.exists() {
            std::fs::remove_dir_all(output).ok();
        }
        build_app_bundle(
            output,
            &sfx_bin,
            &config.package.name,
            &format!("{}.sfx", config.package.id),
            "sfx",
            version,
        )?;

        // Stash the compressed tarball in the SFX's Resources/ — see the
        // inner installer payload for the rationale (codesign strict layout).
        let outer_payload = output.join("Contents/Resources/payload.tar.zst");
        eprintln!(
            "Staging compressed payload at {}...",
            outer_payload.display()
        );
        std::fs::write(&outer_payload, &compressed)?;

        if let Some(cmd) = sign_command {
            eprintln!("Signing SFX .app...");
            signing::sign_file(cmd, output, &callbacks)
                .map_err(|e| format!("sign SFX app: {e}"))?;
        }
    }

    let app_size = walk_dir_size(output)?;
    eprintln!(
        "Done! {} ({:.1} MB)",
        output.display(),
        app_size as f64 / 1_048_576.0
    );

    // 5. Additionally wrap the embedded inner installer.app in a bootstrap
    // .pkg, written alongside the .app (same stem, .pkg extension).
    if let Some(slice) = pkg_slice {
        let spec = pkg::PkgSpec {
            identifier: pkg.identifier.as_deref().unwrap_or(&config.package.id),
            version: pkg.version.as_deref().unwrap_or(version),
            title: &config.package.name,
            cpu: slice.cpu,
        };
        pkg::build_pkg(
            &inner_app,
            &output.with_extension("pkg"),
            &spec,
            slice.min_macos,
            pkg.sign.as_deref(),
        )?;
    }

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
/// `Contents/MacOS/<exe_name>` and writes an Info.plist that gives the
/// package's `version` and, as `LSMinimumSystemVersion`, the oldest macOS
/// every architecture in `mach_o` loads on.
#[cfg(target_os = "macos")]
fn build_app_bundle(
    app_path: &Path,
    mach_o: &Path,
    display_name: &str,
    bundle_id: &str,
    exe_name: &str,
    version: &str,
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

    let min_macos = outto_macos::macho::slices(mach_o)
        .map_err(|e| format!("{}: {e}", mach_o.display()))?
        .iter()
        .filter_map(|s| s.min_macos)
        .max();
    std::fs::write(
        app_path.join("Contents/Info.plist"),
        info_plist(display_name, bundle_id, exe_name, version, min_macos),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn info_plist(
    display_name: &str,
    bundle_id: &str,
    exe_name: &str,
    version: &str,
    min_macos: Option<outto_macos::macho::MacosVersion>,
) -> String {
    use pkg::xml_escape;
    let min_macos = min_macos.map_or_else(String::new, |v| {
        format!("    <key>LSMinimumSystemVersion</key>\n    <string>{v}</string>\n")
    });
    format!(
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
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
{min_macos}    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
"#,
        display_name = xml_escape(display_name),
        exe_name = xml_escape(exe_name),
        bundle_id = xml_escape(bundle_id),
        version = xml_escape(version),
    )
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

/// Tar up a directory tree with a flat archive layout — `src`'s *contents*
/// become the tarball's root, not `src` itself. The SFX stub unpacks into
/// a fresh `$TMPDIR/outto-installer-<pid>.app/` and expects to find
/// `Contents/MacOS/...` directly, no extra nesting.
#[cfg(target_os = "macos")]
fn tar_directory(src: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let tar_file = std::fs::File::create(output)?;
    let mut builder = tar::Builder::new(tar_file);
    builder.follow_symlinks(false);
    // append_dir_all("", src) uses empty prefix so archive entries are
    // "Contents/Info.plist", "Contents/MacOS/installer", etc.
    builder.append_dir_all("", src)?;
    builder.finish()?;
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
    _pkg: &PkgOptions,
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use outto_macos::macho::MacosVersion;

    #[test]
    fn info_plist_carries_the_version_and_minimum_macos() {
        let xml = info_plist(
            "Pahkat <&>",
            "no.divvun.pahkat.installer",
            "installer",
            "3.0.0",
            Some(MacosVersion::new(10, 12, 0)),
        );
        for expected in [
            "<key>CFBundleShortVersionString</key>\n    <string>3.0.0</string>",
            "<key>CFBundleVersion</key>\n    <string>3.0.0</string>",
            "<key>LSMinimumSystemVersion</key>\n    <string>10.12</string>",
            "<string>Pahkat &lt;&amp;&gt;</string>",
        ] {
            assert!(xml.contains(expected), "{expected} not in {xml}");
        }
        let without = info_plist("P", "p.p", "p", "1.0.0", None);
        assert!(!without.contains("LSMinimumSystemVersion"), "{without}");
    }

    /// The bundle states the minimum macOS of the binary it holds, here this
    /// test binary, built like outto's own.
    #[test]
    fn app_bundle_takes_its_minimum_from_the_binary() {
        let exe = std::env::current_exe().unwrap();
        let min = outto_macos::macho::slices(&exe).unwrap()[0]
            .min_macos
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("x.app");
        build_app_bundle(&app, &exe, "X", "no.divvun.x", "x", "3.0.0").unwrap();
        let xml = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        assert!(
            xml.contains(&format!(
                "<key>LSMinimumSystemVersion</key>\n    <string>{min}</string>"
            )),
            "{xml}"
        );
        assert!(app.join("Contents/MacOS/x").is_file());
    }
}
