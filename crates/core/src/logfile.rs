//! Inno-style `/LOG` support: a plain-text log file of everything the install
//! or uninstall reports through [`InstallerCallbacks`], plus whatever the
//! front-end writes directly (start-up, the final result, fatal errors).
//!
//! Besides that log, a front-end can open the package's own log with
//! [`start_package_log`], which every run of the package appends to and
//! which is kept to about [`PACKAGE_LOG_LIMIT`]. [`write`] writes each line
//! to both.
//!
//! The logs are process-global so every front-end and code path can reach
//! them without threading a handle through; until one is opened, [`write`]
//! is a no-op.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use crate::error::{ErrorAction, InstallerError};

static LOG: OnceLock<(PathBuf, Mutex<File>)> = OnceLock::new();
static PACKAGE_LOG: OnceLock<(PathBuf, Mutex<File>)> = OnceLock::new();

/// A package log larger than this when a run starts is cut back to its
/// newest [`PACKAGE_LOG_KEEP`] bytes, from the start of a line.
pub const PACKAGE_LOG_LIMIT: u64 = 1024 * 1024;
/// How much of a package log a cut keeps.
pub const PACKAGE_LOG_KEEP: u64 = PACKAGE_LOG_LIMIT / 2;

/// Parse a log switch: `/LOG` (any case) or `--log` give `Some(None)`,
/// `/LOG=path` or `--log=path` give `Some(Some(path))` with surrounding quotes
/// removed. Anything else gives `None`.
pub fn parse_log_switch(arg: &str) -> Option<Option<String>> {
    if arg.eq_ignore_ascii_case("/LOG") || arg == "--log" {
        return Some(None);
    }
    let value = if arg
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("/LOG="))
    {
        &arg[5..]
    } else {
        arg.strip_prefix("--log=")?
    };
    let value = value.trim_matches('"');
    Some((!value.is_empty()).then(|| value.to_string()))
}

/// Where a bare `/LOG` writes: `<dir>/<kind> Log YYYY-MM-DD #NNN.txt`, Inno
/// Setup's naming, with the first `NNN` (from 001) that isn't already taken.
pub fn default_log_path(dir: &Path, kind: &str, now: SystemTime) -> PathBuf {
    let (y, m, d, ..) = civil_utc(now);
    let mut n = 1u32;
    loop {
        let candidate = dir.join(format!("{kind} Log {y:04}-{m:02}-{d:02} #{n:03}.txt"));
        if !candidate.exists() || n == 999 {
            return candidate;
        }
        n += 1;
    }
}

/// Resolve a parsed `/LOG` value to a path: an explicit path as given, a bare
/// `/LOG` to [`default_log_path`] in the temp directory.
pub fn resolve_log_path(value: Option<&str>, kind: &str) -> PathBuf {
    match value {
        Some(path) => PathBuf::from(path),
        None => default_log_path(&std::env::temp_dir(), kind, SystemTime::now()),
    }
}

/// Start logging to `path`, truncating it, or appending when `append` — an
/// elevated child continuing the log its parent started. Only the first call
/// in a process takes effect.
pub fn init(path: &Path, append: bool) -> io::Result<()> {
    // Absolute, so that what is passed on in /LOG still names it after a
    // change of directory, and in a child running elsewhere.
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&path)?;
    let _ = LOG.set((path, Mutex::new(file)));
    Ok(())
}

/// Start the log a front-end was asked for with `/LOG` (`value` is the parsed
/// switch value, `kind` is `"Setup"` or `"Uninstall"`), saying on stderr and
/// in the log where it goes. A log that can't be opened is a warning, not a
/// reason to abort the install.
pub fn start(value: Option<&str>, kind: &str, append: bool) {
    let path = resolve_log_path(value, kind);
    match init(&path, append) {
        Ok(()) => {
            let path = self::path().unwrap_or(&path);
            eprintln!("Logging to {}", path.display());
            write(LogLevel::Info, &format!("Log file: {}", path.display()));
        }
        Err(e) => eprintln!("Warning: cannot open log file {}: {e}", path.display()),
    }
}

/// The file being logged to, if [`init`] has been called.
pub fn path() -> Option<&'static Path> {
    LOG.get().map(|(path, _)| path.as_path())
}

/// The log of another installer that this one runs, `name` added to the name
/// of `log` before its extension: `C:\logs\install.log` gives
/// `C:\logs\install.<name>.log`.
pub fn child_log_path(log: &Path, name: &str) -> PathBuf {
    let stem = log.file_stem().unwrap_or_default().to_string_lossy();
    let file_name = match log.extension() {
        Some(ext) => format!("{stem}.{name}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{name}"),
    };
    log.with_file_name(file_name)
}

/// Append one timestamped line to the `/LOG` log and the package log,
/// whichever are open. Each line is written straight through, so the logs
/// survive `process::exit` and crashes up to the last line.
pub fn write(level: LogLevel, message: &str) {
    if LOG.get().is_none() && PACKAGE_LOG.get().is_none() {
        return;
    }
    let line = format_line(SystemTime::now(), level, message);
    for (_, file) in [LOG.get(), PACKAGE_LOG.get()].into_iter().flatten() {
        if let Ok(mut f) = file.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// The file name of `package_id`'s log: the id, with `_` for each character
/// a file name can't hold, and `.log`.
pub fn package_log_file_name(package_id: &str) -> String {
    let name: String = package_id
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim_matches(['.', ' ']);
    if name.is_empty() {
        "package.log".to_string()
    } else {
        format!("{name}.log")
    }
}

/// The first line a run writes to the package log.
pub fn run_header(operation: &str, command_line: &str) -> String {
    format!("outto {} {operation}: {command_line}", crate::VERSION)
}

/// Start appending to `path`, the package's own log, beginning with a line
/// of `header` (see [`run_header`]). A log past [`PACKAGE_LOG_LIMIT`] is cut
/// back first. Only the first call in a process takes effect.
pub fn start_package_log(path: &Path, header: &str) -> io::Result<()> {
    if PACKAGE_LOG.get().is_some() {
        return Ok(());
    }
    trim_log(path, PACKAGE_LOG_LIMIT, PACKAGE_LOG_KEEP)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(format_header(SystemTime::now(), header).as_bytes())?;
    let _ = PACKAGE_LOG.set((path.to_path_buf(), Mutex::new(file)));
    Ok(())
}

/// The package log, if [`start_package_log`] has opened it.
pub fn package_log_path() -> Option<&'static Path> {
    PACKAGE_LOG.get().map(|(path, _)| path.as_path())
}

/// If the file at `path` is larger than `limit`, keep only its last `keep`
/// bytes, less the part of a line they start with. Returns whether it cut.
fn trim_log(path: &Path, limit: u64, keep: u64) -> io::Result<bool> {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= limit {
        return Ok(false);
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.seek(SeekFrom::Start(len - keep.min(len)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let start = tail
        .iter()
        .position(|&b| b == b'\n')
        .map_or(tail.len(), |i| i + 1);
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tail[start..])?;
    file.set_len((tail.len() - start) as u64)?;
    Ok(true)
}

fn format_header(now: SystemTime, text: &str) -> String {
    let (y, mo, d, h, mi, s, ms) = civil_utc(now);
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    let text = text.replace(['\r', '\n'], " ");
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}Z ===== {text}{eol}")
}

/// Log the outcome of a whole operation (`"Installation"`, `"Uninstallation"`).
pub fn write_result<E: std::fmt::Display>(operation: &str, result: &Result<(), E>) {
    match result {
        Ok(()) => write(
            LogLevel::Info,
            &format!("{operation} finished successfully"),
        ),
        Err(e) => write(LogLevel::Error, &format!("{operation} failed: {e}")),
    }
}

fn format_line(now: SystemTime, level: LogLevel, message: &str) -> String {
    let (y, mo, d, h, mi, s, ms) = civil_utc(now);
    let level = match level {
        LogLevel::Debug => "DEBUG",
        LogLevel::Info => "INFO",
        LogLevel::Warn => "WARN",
        LogLevel::Error => "ERROR",
    };
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut out = String::new();
    for (i, text) in message.lines().enumerate() {
        let text = if i == 0 {
            text.to_string()
        } else {
            format!("    {text}")
        };
        out.push_str(&format!(
            "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}Z {level:<5} {text}{eol}"
        ));
    }
    if out.is_empty() {
        out = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{ms:03}Z {level:<5} {eol}");
    }
    out
}

/// UTC calendar time: (year, month, day, hour, minute, second, millisecond).
fn civil_utc(t: SystemTime) -> (i64, u32, u32, u32, u32, u32, u32) {
    let dur = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;

    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);

    (
        y,
        m,
        d,
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
        dur.subsec_millis(),
    )
}

/// Wraps a front-end's callbacks so everything they are told also goes to the
/// log file: log lines, errors, prompts and their answers, reboot requests.
pub struct LoggingCallbacks<'a> {
    inner: &'a dyn InstallerCallbacks,
}

impl<'a> LoggingCallbacks<'a> {
    pub fn new(inner: &'a dyn InstallerCallbacks) -> Self {
        Self { inner }
    }
}

impl InstallerCallbacks for LoggingCallbacks<'_> {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        self.inner.on_progress(phase, current, total);
    }

    fn on_prompt(&self, prompt: Prompt) -> PromptResponse {
        let description = format!("{prompt:?}");
        let response = self.inner.on_prompt(prompt);
        write(
            LogLevel::Info,
            &format!("Prompt: {description} -> {response:?}"),
        );
        response
    }

    fn on_log(&self, level: LogLevel, message: &str) {
        write(level, message);
        self.inner.on_log(level, message);
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        let action = self.inner.on_error(error);
        write(LogLevel::Error, &format!("{error} -> {action:?}"));
        action
    }

    fn on_reboot_required(&self) {
        write(LogLevel::Info, "A system restart is required");
        self.inner.on_reboot_required();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_log_switches() {
        assert_eq!(parse_log_switch("/LOG"), Some(None));
        assert_eq!(parse_log_switch("/log"), Some(None));
        assert_eq!(parse_log_switch("--log"), Some(None));
        assert_eq!(
            parse_log_switch(r#"/LOG="C:\Temp\my install.log""#),
            Some(Some(r"C:\Temp\my install.log".to_string()))
        );
        // What the Windows argv parser leaves of /LOG="C:\Temp\x.log".
        assert_eq!(
            parse_log_switch(r"/Log=C:\Temp\x.log"),
            Some(Some(r"C:\Temp\x.log".to_string()))
        );
        assert_eq!(
            parse_log_switch("--log=/tmp/x.log"),
            Some(Some("/tmp/x.log".to_string()))
        );
        assert_eq!(parse_log_switch(r#"/LOG="""#), Some(None));
        assert_eq!(parse_log_switch("/LOGX"), None);
        assert_eq!(parse_log_switch("/LO"), None);
        assert_eq!(parse_log_switch("/LOõ"), None);
        assert_eq!(parse_log_switch("/SILENT"), None);
        assert_eq!(parse_log_switch("--logfile"), None);
    }

    #[test]
    fn utc_calendar() {
        assert_eq!(civil_utc(UNIX_EPOCH), (1970, 1, 1, 0, 0, 0, 0));
        // 2026-10-07T20:24:32.250Z
        let t = UNIX_EPOCH + Duration::from_millis(1_791_404_672_250);
        assert_eq!(civil_utc(t), (2026, 10, 7, 20, 24, 32, 250));
        // 2024-02-29T23:59:59Z
        let t = UNIX_EPOCH + Duration::from_secs(1_709_251_199);
        assert_eq!(civil_utc(t), (2024, 2, 29, 23, 59, 59, 0));
    }

    #[test]
    fn line_format() {
        let t = UNIX_EPOCH + Duration::from_millis(1_791_404_672_250);
        let eol = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(
            format_line(t, LogLevel::Warn, "Run: exited with 1\nstderr"),
            format!(
                "2026-10-07 20:24:32.250Z WARN  Run: exited with 1{eol}\
                 2026-10-07 20:24:32.250Z WARN      stderr{eol}"
            )
        );
    }

    #[test]
    fn package_log_names() {
        assert_eq!(
            package_log_file_name("no.divvun.pahkat"),
            "no.divvun.pahkat.log"
        );
        assert_eq!(package_log_file_name(r"a\b/c:d*e?"), "a_b_c_d_e_.log");
        assert_eq!(package_log_file_name(".."), "package.log");
        assert_eq!(package_log_file_name(""), "package.log");
        assert_eq!(package_log_file_name("x\ty"), "x_y.log");
    }

    #[test]
    fn header_line() {
        let t = UNIX_EPOCH + Duration::from_millis(1_791_404_672_250);
        let eol = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(
            format_header(
                t,
                &run_header("install (very silent)", "setup.exe /VERYSILENT")
            ),
            format!(
                "2026-10-07 20:24:32.250Z ===== outto {} install (very silent): \
                 setup.exe /VERYSILENT{eol}",
                crate::VERSION
            )
        );
        assert!(!format_header(t, "a\nb").trim_end().contains('\n'));
    }

    #[test]
    fn a_log_past_the_limit_keeps_its_newest_whole_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("p.log");
        assert!(!trim_log(&path, 100, 50).unwrap());

        let lines: Vec<String> = (0..20).map(|i| format!("line {i:02} ......\n")).collect();
        std::fs::write(&path, lines.concat()).unwrap();
        assert!(!trim_log(&path, 1000, 50).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), lines.concat());

        // 15 bytes a line; the last 50 bytes start inside line 16.
        assert!(trim_log(&path, 100, 50).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            lines[17..].concat()
        );

        std::fs::write(&path, "x".repeat(200)).unwrap();
        assert!(trim_log(&path, 100, 50).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn child_logs_sit_next_to_the_log() {
        assert_eq!(
            child_log_path(Path::new("/var/log/install.log"), "text-service"),
            Path::new("/var/log/install.text-service.log")
        );
        assert_eq!(
            child_log_path(Path::new("/tmp/Setup Log 2026-10-07 #001.txt"), "tip"),
            Path::new("/tmp/Setup Log 2026-10-07 #001.tip.txt")
        );
        assert_eq!(
            child_log_path(Path::new("/tmp/a.b.log"), "c"),
            Path::new("/tmp/a.b.c.log")
        );
        assert_eq!(
            child_log_path(Path::new("/tmp/install"), "tip"),
            Path::new("/tmp/install.tip")
        );
    }

    #[test]
    fn default_path_picks_first_free_number() {
        let tmp = tempfile::tempdir().unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(1_791_404_672);
        let first = default_log_path(tmp.path(), "Setup", t);
        assert_eq!(first, tmp.path().join("Setup Log 2026-10-07 #001.txt"));
        std::fs::write(&first, "").unwrap();
        assert_eq!(
            default_log_path(tmp.path(), "Setup", t),
            tmp.path().join("Setup Log 2026-10-07 #002.txt")
        );
        assert_eq!(
            default_log_path(tmp.path(), "Uninstall", t),
            tmp.path().join("Uninstall Log 2026-10-07 #001.txt")
        );
    }
}
