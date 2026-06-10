use std::path::PathBuf;

/// Inno Setup-compatible CLI flags.
#[derive(Debug, Clone, Default)]
pub struct CliFlags {
    /// /SILENT — skip wizard, show only progress
    pub silent: bool,
    /// /VERYSILENT — no GUI at all
    pub very_silent: bool,
    /// /SUPPRESSMSGBOXES — auto-accept prompts
    pub suppress_msgboxes: bool,
    /// /SP- — skip the welcome/startup prompt
    pub sp_minus: bool,
    /// /DIR="path" — override install directory
    pub dir: Option<String>,
    /// /COMPONENTS="a,b,c" — override component selection
    pub components: Option<Vec<String>>,
    /// /LOG or /LOG="path" — enable logging
    pub log: Option<Option<String>>,
    /// /NORESTART — suppress reboot
    pub no_restart: bool,
    /// /NOCANCEL — disable cancel button
    pub no_cancel: bool,
    /// --progress-file <path> — internal: stream JSON-line progress events to
    /// this file (set by the parent GUI when spawning an elevated child)
    pub progress_file: Option<PathBuf>,
    /// --uninstall-app <path> — internal: pre-extracted uninstaller to copy
    /// into the receipt (set by the parent GUI when spawning an elevated child)
    pub uninstall_app: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Mode {
    Install {
        config_path: PathBuf,
        source_dir: PathBuf,
    },
    /// Embedded mode: config + source files are packed inside the exe
    InstallEmbedded,
    Uninstall {
        dir: PathBuf,
    },
}

#[derive(Debug)]
pub struct Args {
    pub mode: Mode,
    pub flags: CliFlags,
}

pub fn parse_args() -> Result<Args, String> {
    let args: Vec<String> = std::env::args().collect();

    // No subcommand at all — try embedded mode (double-clicked installer)
    if args.len() < 2 {
        return Ok(Args {
            mode: Mode::InstallEmbedded,
            flags: CliFlags::default(),
        });
    }

    let subcommand = args[1].to_lowercase();

    // If the first arg starts with / or -- it's a flag, not a subcommand — embedded mode with flags
    if subcommand.starts_with('/') || subcommand.starts_with("--") {
        let mut flags = CliFlags::default();
        parse_flags(
            &args[1..],
            &mut flags,
            &mut None,
            &mut None,
            &mut None,
            &mut None,
        )?;
        return Ok(Args {
            mode: Mode::InstallEmbedded,
            flags,
        });
    }

    let rest = &args[2..];

    let mut flags = CliFlags::default();
    let mut config_path: Option<PathBuf> = None;
    let mut source_dir: Option<PathBuf> = None;
    let mut uninstall_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    parse_flags(
        rest,
        &mut flags,
        &mut config_path,
        &mut source_dir,
        &mut uninstall_dir,
        &mut output,
    )?;

    let mode = match subcommand.as_str() {
        "install" => match (config_path, source_dir) {
            (Some(config_path), Some(source_dir)) => Mode::Install {
                config_path,
                source_dir,
            },
            (None, None) => Mode::InstallEmbedded,
            _ => {
                return Err(
                    "Either provide both --config and --source, or neither (embedded mode)".into(),
                );
            }
        },
        "uninstall" => {
            let dir = uninstall_dir
                .or_else(|| flags.dir.as_ref().map(PathBuf::from))
                .ok_or("Missing --dir <path> for uninstall")?;
            Mode::Uninstall { dir }
        }
        _ => return Err(format!("Unknown subcommand: {subcommand}\n{}", usage())),
    };

    Ok(Args { mode, flags })
}

fn parse_flags(
    rest: &[String],
    flags: &mut CliFlags,
    config_path: &mut Option<PathBuf>,
    source_dir: &mut Option<PathBuf>,
    uninstall_dir: &mut Option<PathBuf>,
    output: &mut Option<PathBuf>,
) -> Result<(), String> {
    let mut i = 0;
    while i < rest.len() {
        let arg = &rest[i];
        let upper = arg.to_uppercase();

        // Inno-compatible /FLAGS (case-insensitive) and --flags
        if upper == "/SILENT" || arg == "--silent" {
            flags.silent = true;
        } else if upper == "/VERYSILENT" || arg == "--very-silent" {
            flags.very_silent = true;
            flags.silent = true;
        } else if upper == "/SUPPRESSMSGBOXES" || arg == "--suppress-msgboxes" {
            flags.suppress_msgboxes = true;
        } else if upper == "/SP-" {
            flags.sp_minus = true;
        } else if upper == "/NORESTART" || arg == "--no-restart" {
            flags.no_restart = true;
        } else if upper == "/NOCANCEL" || arg == "--no-cancel" {
            flags.no_cancel = true;
        } else if upper.starts_with("/DIR=") {
            flags.dir = Some(strip_value(arg, "/DIR="));
        } else if upper.starts_with("/COMPONENTS=") {
            let val = strip_value(arg, "/COMPONENTS=");
            flags.components = Some(
                val.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        } else if upper == "/LOG" || arg == "--log" {
            flags.log = Some(None);
        } else if upper.starts_with("/LOG=") {
            flags.log = Some(Some(strip_value(arg, "/LOG=")));
        }
        // Standard --flags
        else if arg == "--config" {
            i += 1;
            *config_path = rest.get(i).map(PathBuf::from);
        } else if arg == "--source" {
            i += 1;
            *source_dir = rest.get(i).map(PathBuf::from);
        } else if arg == "--dir" {
            i += 1;
            *uninstall_dir = rest.get(i).map(PathBuf::from);
        } else if arg == "--output" {
            i += 1;
            *output = rest.get(i).map(PathBuf::from);
        } else if arg == "--progress-file" {
            i += 1;
            flags.progress_file = rest.get(i).map(PathBuf::from);
        } else if arg == "--uninstall-app" {
            i += 1;
            flags.uninstall_app = rest.get(i).map(PathBuf::from);
        } else {
            return Err(format!("Unknown argument: {arg}\n{}", usage()));
        }

        i += 1;
    }
    Ok(())
}

fn strip_value(arg: &str, prefix: &str) -> String {
    let val = &arg[prefix.len()..];
    val.trim_matches('"').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> CliFlags {
        let rest: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut flags = CliFlags::default();
        parse_flags(
            &rest,
            &mut flags,
            &mut None,
            &mut None,
            &mut None,
            &mut None,
        )
        .unwrap();
        flags
    }

    #[test]
    fn test_components_empty_value_is_empty_vec() {
        assert_eq!(parse(&["/COMPONENTS="]).components, Some(vec![]));
        assert_eq!(
            parse(&["/COMPONENTS=a, b,"]).components,
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn test_internal_elevation_flags() {
        let flags = parse(&[
            "--progress-file",
            "/tmp/p.jsonl",
            "--uninstall-app",
            "/tmp/uninstall.app",
        ]);
        assert_eq!(flags.progress_file, Some(PathBuf::from("/tmp/p.jsonl")));
        assert_eq!(
            flags.uninstall_app,
            Some(PathBuf::from("/tmp/uninstall.app"))
        );
    }
}

fn usage() -> String {
    "Usage:\n  \
     outto-gui install [--config <file> --source <dir>] [/SILENT] [/VERYSILENT] ...\n  \
     outto-gui uninstall --dir <path> [/SILENT] [/VERYSILENT] ...\n  \
     outto-gui [/FLAGS]  (embedded installer mode)\n\n\
     Inno Setup-compatible flags:\n  \
     /SILENT /VERYSILENT /SUPPRESSMSGBOXES /SP- /DIR=\"...\" /COMPONENTS=\"a,b\"\n  \
     /LOG /LOG=\"path\" /NORESTART /NOCANCEL"
        .to_string()
}
