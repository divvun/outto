mod pe;

use box_format::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use box_format::sync::BoxWriter;
use box_format::{BoxPath, Compression, CompressionConfig};
use outto::Config;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] != "build" {
        eprintln!("Usage: outto-cli build --config <file> --source <dir> --output <exe>");
        std::process::exit(2);
    }

    let mut config_path: Option<PathBuf> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut compress = false;

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

    match build_installer(&config_path, &source_dir, &output, compress) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Build failed: {e}");
            std::process::exit(1);
        }
    }
}

fn build_installer(
    config_path: &Path,
    source_dir: &Path,
    output: &Path,
    compress: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !config_path.exists() {
        return Err(format!("Config file not found: {}", config_path.display()).into());
    }
    if !source_dir.exists() || !source_dir.is_dir() {
        return Err(format!("Source directory not found: {}", source_dir.display()).into());
    }

    // Validate the config parses
    let _config = Config::from_file(config_path).map_err(|e| format!("Invalid config: {e}"))?;

    // Find the installer template (outto-gui.exe) and uninstaller next to us
    let cli_exe = std::env::current_exe()?;
    let cli_dir = cli_exe
        .parent()
        .ok_or("Cannot determine CLI exe directory")?;

    let template_exe = cli_dir.join("outto-gui.exe");
    if !template_exe.exists() {
        return Err(format!(
            "Installer template not found: {}\nBuild outto-gui first and place it next to outto-cli.",
            template_exe.display()
        ).into());
    }

    let uninstall_exe = find_binary(cli_dir, &["outto-uninstall.exe", "uninstall.exe"]);
    if let Some(ref p) = uninstall_exe {
        eprintln!("Uninstaller: {}", p.display());
    } else {
        eprintln!(
            "Warning: outto-uninstall binary not found. Installer will not include an uninstaller."
        );
    }

    // Create the .box archive
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

    // Copy the installer template
    eprintln!("Template: {}", template_exe.display());
    fs::copy(&template_exe, output)?;

    // Embed the .box data as a PE section
    eprintln!("Embedding payload into {}...", output.display());
    pe::embed_section(output, ".outto", &box_data)?;

    if compress {
        // Compress the raw installer into an SFX wrapper
        let sfx_stub = cli_dir.join("outto-sfx.exe");
        if !sfx_stub.exists() {
            return Err(format!(
                "SFX stub not found: {}\nBuild outto-sfx first and place it next to outto-cli.",
                sfx_stub.display()
            )
            .into());
        }

        eprintln!("Compressing installer...");
        let raw_installer = fs::read(output)?;
        let compressed = zstd::encode_all(&raw_installer[..], 19)?; // level 19 for max compression

        eprintln!(
            "Compressed: {:.1} MB -> {:.1} MB ({:.0}% reduction)",
            raw_installer.len() as f64 / 1_048_576.0,
            compressed.len() as f64 / 1_048_576.0,
            (1.0 - compressed.len() as f64 / raw_installer.len() as f64) * 100.0,
        );

        // Replace output with SFX stub + compressed payload
        fs::copy(&sfx_stub, output)?;
        pe::embed_section(output, ".outto", &compressed)?;
    }

    let final_size = fs::metadata(output)?.len();
    eprintln!(
        "Done! {} ({:.1} MB)",
        output.display(),
        final_size as f64 / 1_048_576.0
    );

    Ok(())
}

fn pack_payload(
    config_path: &Path,
    source_dir: &Path,
    output_box: &Path,
    uninstall_exe: Option<&Path>,
) -> io::Result<()> {
    let compression = CompressionConfig::new(Compression::Zstd);
    let mut writer = BoxWriter::create(output_box)?;

    // Config at archive root
    let config_box_path =
        BoxPath::new("outto.toml").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    writer.insert_file(&compression, config_path, config_box_path, HashMap::new())?;

    // Uninstaller at archive root
    if let Some(uninstall_path) = uninstall_exe {
        let box_path = BoxPath::new("uninstall.exe")
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        writer.insert_file(&compression, uninstall_path, box_path, HashMap::new())?;
    }

    // Source files under source/ prefix
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

fn find_binary(dir: &Path, candidates: &[&str]) -> Option<PathBuf> {
    for name in candidates {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}
