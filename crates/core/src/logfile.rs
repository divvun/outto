//! Inno-style `/LOG` support: a plain-text log file of everything the install
//! or uninstall reports through [`InstallerCallbacks`], plus whatever the
//! front-end writes directly (start-up, the final result, fatal errors).
//!
//! The log is process-global so every front-end and code path can reach it
//! without threading a handle through; until [`init`] is called, [`write`] is
//! a no-op.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use crate::error::{ErrorAction, InstallerError};

static LOG: OnceLock<(PathBuf, Mutex<File>)> = OnceLock::new();

/// Parse a log switch: `/LOG` (any case) or `--log` give `Some(None)`,
/// `/LOG=path` or `--log=path` give `Some(Some(path))` with surrounding quotes
/// removed. Anything else gives `None`.
pub fn parse_log_switch(arg: &str) -> Option<Option<String>> {
    if arg.eq_ignore_ascii_case("/LOG") || arg == "--log" {
        return Some(None);
    }
    let value = if arg.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("/LOG=")) {
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)?;
    let _ = LOG.set((path.to_path_buf(), Mutex::new(file)));
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

/// Append one timestamped line to the log, if logging is enabled. Each line
/// is written straight through, so the log survives `process::exit` and
/// crashes up to the last line.
pub fn write(level: LogLevel, message: &str) {
    let Some((_, file)) = LOG.get() else {
        return;
    };
    let line = format_line(SystemTime::now(), level, message);
    if let Ok(mut f) = file.lock() {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Log the outcome of a whole operation (`"Installation"`, `"Uninstallation"`).
pub fn write_result<E: std::fmt::Display>(operation: &str, result: &Result<(), E>) {
    match result {
        Ok(()) => write(LogLevel::Info, &format!("{operation} finished successfully")),
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
