//! Privilege detection and `osascript`-based self-elevation.
//!
//! macOS doesn't have an in-process "please elevate me" API (`AuthorizationExecuteWithPrivileges`
//! is deprecated; `SMJobBless` requires a pre-shipped helper tool). The pragmatic
//! option is `osascript -e 'do shell script ... with administrator privileges'`,
//! which shows the standard macOS password prompt and relaunches the installer
//! as root.

use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

use outto_core::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use outto_core::error::{ErrorAction, InstallerError, InstallerResult};

use crate::config::RequiredPrivileges;

/// True if the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Decide whether an install needs elevation given the TOML `required` setting
/// and the resolved install directory. `system_roots` is a list of paths that
/// require root to write into; any install path under one of them forces
/// elevation.
pub fn needs_elevation(
    required: &RequiredPrivileges,
    install_dir: &Path,
    system_roots: &[&str],
) -> bool {
    if is_root() {
        return false;
    }
    match required {
        RequiredPrivileges::Admin => true,
        RequiredPrivileges::User => false,
        RequiredPrivileges::Auto => system_roots
            .iter()
            .any(|root| install_dir.starts_with(root)),
    }
}

/// Default set of paths that need root to modify.
pub const DEFAULT_SYSTEM_ROOTS: &[&str] = &[
    "/Library",
    "/usr/local",
    "/Library/LaunchDaemons",
    "/Library/LaunchAgents",
    "/Applications", // technically admin on single-user macs, but writable; treat as user by default
    "/System",
    "/private",
];

/// Build the `osascript` invocation that runs `exe argv...` with admin rights.
///
/// Composes a POSIX shell command `'<exe>' '<arg1>' ...` (single-quoted, with
/// embedded single quotes escaped as `'\''`), then wraps it in
/// `do shell script "..." with administrator privileges` with AppleScript's
/// double-quote/backslash escaping.
fn build_elevated_command(exe: &Path, argv: &[OsString]) -> std::process::Command {
    let mut shell_cmd = quote_posix(&exe.to_string_lossy());
    for a in argv {
        shell_cmd.push(' ');
        shell_cmd.push_str(&quote_posix(&a.to_string_lossy()));
    }

    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        escape_applescript(&shell_cmd)
    );

    let mut cmd = std::process::Command::new("osascript");
    cmd.arg("-e").arg(script);
    cmd
}

/// Spawn `exe argv...` with admin rights via `osascript` and return the child
/// without waiting. stdout is discarded (`do shell script` buffers the inner
/// command's output as its result anyway); stderr is piped so the caller can
/// distinguish an auth-prompt cancel (AppleScript error -128) from a real
/// failure after the child exits.
pub fn spawn_elevated(exe: &Path, argv: &[OsString]) -> std::io::Result<std::process::Child> {
    build_elevated_command(exe, argv)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Relaunch the current process with admin rights, passing the same argv.
/// Returns `Err(ElevationRequired)` on failure; on success, the current process
/// is replaced so this call typically doesn't return.
pub fn elevate_self(extra_args: &[String]) -> InstallerResult<()> {
    let exe = std::env::current_exe()
        .map_err(|e| InstallerError::Other(format!("can't locate current exe: {e}")))?;

    let mut argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    for a in extra_args {
        argv.push(OsString::from(a));
    }

    let status = build_elevated_command(&exe, &argv)
        .status()
        .map_err(|e| InstallerError::Other(format!("osascript failed to launch: {e}")))?;

    if !status.success() {
        return Err(InstallerError::ElevationRequired(format!(
            "osascript exited with {status} (user may have cancelled the password prompt)"
        )));
    }

    // The elevated child has run to completion; exit so we don't double-install.
    std::process::exit(0);
}

// --- Progress streaming between an unprivileged GUI and an elevated child ---
//
// The GUI process stays unprivileged (it owns the window); the elevated child
// runs the actual install/uninstall headlessly and reports progress by
// appending JSON lines to a file the parent tails. A plain file rather than a
// FIFO: opening a FIFO blocks until the peer appears, which would wedge the
// parent if the user cancels the password prompt.

/// A progress/log event streamed from the elevated child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Progress {
        phase: String,
        current: u64,
        total: u64,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

/// How an elevated child run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevatedOutcome {
    /// The child ran; the result is the install/uninstall result.
    Completed(Result<(), String>),
    /// The user dismissed the macOS password prompt; nothing ran.
    AuthCancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StreamLine {
    Event(StreamEvent),
    Finished(Result<(), String>),
}

fn level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

fn parse_level(s: &str) -> LogLevel {
    match s {
        "debug" => LogLevel::Debug,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn parse_stream_line(line: &str) -> Option<StreamLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v["type"].as_str()? {
        "progress" => Some(StreamLine::Event(StreamEvent::Progress {
            phase: v["phase"].as_str().unwrap_or_default().to_string(),
            current: v["current"].as_u64().unwrap_or(0),
            total: v["total"].as_u64().unwrap_or(0),
        })),
        "log" => Some(StreamLine::Event(StreamEvent::Log {
            level: parse_level(v["level"].as_str().unwrap_or("info")),
            message: v["message"].as_str().unwrap_or_default().to_string(),
        })),
        "finished" => Some(StreamLine::Finished(if v["ok"].as_bool().unwrap_or(false) {
            Ok(())
        } else {
            Err(v["error"].as_str().unwrap_or("operation failed").to_string())
        })),
        _ => None,
    }
}

/// Child-side `InstallerCallbacks` that appends JSON-line events to the
/// progress file, one flushed line per event. Prompts are auto-accepted and
/// errors abort, matching `/VERYSILENT /SUPPRESSMSGBOXES` semantics — the
/// parent GUI can't answer prompts across the privilege boundary.
pub struct FileProgressCallbacks {
    file: Mutex<std::fs::File>,
}

impl FileProgressCallbacks {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    fn write_line(&self, value: serde_json::Value) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{value}");
            let _ = f.flush();
        }
    }

    /// Write the terminal `finished` event. Call exactly once, right before
    /// the child exits.
    pub fn write_finished(&self, result: Result<(), &str>) {
        self.write_line(match result {
            Ok(()) => serde_json::json!({"type": "finished", "ok": true}),
            Err(e) => serde_json::json!({"type": "finished", "ok": false, "error": e}),
        });
    }
}

impl InstallerCallbacks for FileProgressCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        self.write_line(serde_json::json!({
            "type": "progress", "phase": phase, "current": current, "total": total,
        }));
    }

    fn on_log(&self, level: LogLevel, message: &str) {
        self.write_line(serde_json::json!({
            "type": "log", "level": level_str(level), "message": message,
        }));
    }

    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        self.write_line(serde_json::json!({
            "type": "log", "level": "error", "message": error.to_string(),
        }));
        ErrorAction::Abort
    }
}

/// Byte-level tail state for the progress file: tracks the read offset and a
/// partial trailing line (writes aren't atomic, and a flush can land mid-way
/// through a multi-byte character).
struct Tail {
    pos: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn new() -> Self {
        Self {
            pos: 0,
            partial: Vec::new(),
        }
    }

    fn drain(&mut self, path: &Path, mut handle: impl FnMut(StreamLine)) {
        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.pos)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.pos += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        while let Some(nl) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            if let Ok(text) = std::str::from_utf8(&line) {
                if let Some(parsed) = parse_stream_line(text) {
                    handle(parsed);
                }
            }
        }
    }
}

/// Run `exe argv...` elevated, tailing `progress_path` and forwarding each
/// streamed event to `on_event` until the child exits. Blocks; call from a
/// worker thread.
///
/// The result reconciliation order: a `finished` event from the child wins;
/// else a zero exit status means success; else AppleScript error -128 (or no
/// other evidence the child ever ran) means the user cancelled the password
/// prompt; anything else is a failure.
pub fn run_elevated_with_progress(
    exe: &Path,
    argv: &[OsString],
    progress_path: &Path,
    mut on_event: impl FnMut(StreamEvent),
) -> InstallerResult<ElevatedOutcome> {
    std::fs::write(progress_path, b"").map_err(|e| {
        InstallerError::Other(format!(
            "can't create progress file {}: {e}",
            progress_path.display()
        ))
    })?;

    let mut child = spawn_elevated(exe, argv)
        .map_err(|e| InstallerError::Other(format!("osascript failed to launch: {e}")))?;

    let mut tail = Tail::new();
    let mut finished: Option<Result<(), String>> = None;

    let pump = |tail: &mut Tail,
                finished: &mut Option<Result<(), String>>,
                on_event: &mut dyn FnMut(StreamEvent)| {
        tail.drain(progress_path, |line| match line {
            StreamLine::Event(ev) => on_event(ev),
            StreamLine::Finished(result) => *finished = Some(result),
        });
    };

    let status = loop {
        pump(&mut tail, &mut finished, &mut on_event);
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => {
                return Err(InstallerError::Other(format!(
                    "waiting for elevated process failed: {e}"
                )));
            }
        }
    };
    // Catch anything written between the last poll and exit.
    pump(&mut tail, &mut finished, &mut on_event);

    let mut stderr_text = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr_text);
    }

    if let Some(result) = finished {
        return Ok(ElevatedOutcome::Completed(result));
    }
    if status.success() {
        return Ok(ElevatedOutcome::Completed(Ok(())));
    }
    // `do shell script` reports an auth-prompt dismissal as AppleScript error
    // -128 ("User canceled."); the error number survives localization.
    if stderr_text.contains("(-128)") || stderr_text.contains("User canceled") {
        return Ok(ElevatedOutcome::AuthCancelled);
    }
    Ok(ElevatedOutcome::Completed(Err(format!(
        "elevated process exited with {status}: {}",
        stderr_text.trim()
    ))))
}

fn quote_posix(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_root_returns_bool() {
        let _b: bool = is_root();
    }

    #[test]
    fn test_needs_elevation_user_scope() {
        let p = Path::new("/Users/test/Applications/MyApp.app");
        assert!(!needs_elevation(
            &RequiredPrivileges::User,
            p,
            DEFAULT_SYSTEM_ROOTS
        ));
    }

    #[test]
    fn test_needs_elevation_auto_system_scope() {
        let p = Path::new("/Library/LaunchDaemons");
        let expected = !is_root(); // true unless already root
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Auto, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_needs_elevation_admin_always_unless_root() {
        let p = Path::new("/tmp");
        let expected = !is_root();
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Admin, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_quote_posix_escapes_single_quotes() {
        assert_eq!(quote_posix("foo"), "'foo'");
        assert_eq!(quote_posix("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_escape_applescript() {
        assert_eq!(escape_applescript("hello"), "hello");
        assert_eq!(escape_applescript("he \"said\""), "he \\\"said\\\"");
        assert_eq!(escape_applescript("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_build_elevated_command_quotes_spaces_and_quotes() {
        let cmd = build_elevated_command(
            Path::new("/Applications/My App.app/Contents/MacOS/installer"),
            &[
                OsString::from("/DIR=/Library/It's Here"),
                OsString::from("--progress-file"),
                OsString::from("/tmp/p f.jsonl"),
            ],
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "-e");
        let script = &args[1];
        assert!(script.starts_with("do shell script \""));
        assert!(script.ends_with("\" with administrator privileges"));
        assert!(script.contains("'/Applications/My App.app/Contents/MacOS/installer'"));
        // Single quote inside an arg → '\'' POSIX escape, then AppleScript
        // doubles the backslash.
        assert!(script.contains("'/DIR=/Library/It'\\\\''s Here'"));
        assert!(script.contains("'/tmp/p f.jsonl'"));
    }

    #[test]
    fn test_stream_event_round_trip() {
        let path = std::env::temp_dir().join(format!("outto-stream-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cb = FileProgressCallbacks::create(&path).unwrap();
        cb.on_progress("copy files", 3, 10);
        cb.on_log(LogLevel::Warn, "skipped \"weird\" påth\nwith newline");
        cb.on_error(&InstallerError::Other("boom".into()));
        cb.write_finished(Err("it broke"));

        let mut lines: Vec<StreamLine> = Vec::new();
        let mut tail = Tail::new();
        tail.drain(&path, |l| lines.push(l));
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            lines,
            vec![
                StreamLine::Event(StreamEvent::Progress {
                    phase: "copy files".into(),
                    current: 3,
                    total: 10,
                }),
                StreamLine::Event(StreamEvent::Log {
                    level: LogLevel::Warn,
                    message: "skipped \"weird\" påth\nwith newline".into(),
                }),
                StreamLine::Event(StreamEvent::Log {
                    level: LogLevel::Error,
                    message: "boom".into(),
                }),
                StreamLine::Finished(Err("it broke".into())),
            ]
        );
    }

    #[test]
    fn test_tail_buffers_partial_lines() {
        let path = std::env::temp_dir().join(format!("outto-tail-test-{}", std::process::id()));
        let full = "{\"type\":\"log\",\"level\":\"info\",\"message\":\"hø\"}\n";
        let bytes = full.as_bytes();
        // Split mid-way through the multi-byte 'ø'.
        let split = full.find('ø').unwrap() + 1;

        std::fs::write(&path, &bytes[..split]).unwrap();
        let mut tail = Tail::new();
        let mut lines: Vec<StreamLine> = Vec::new();
        tail.drain(&path, |l| lines.push(l));
        assert!(lines.is_empty());

        std::fs::write(&path, bytes).unwrap();
        tail.drain(&path, |l| lines.push(l));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            lines,
            vec![StreamLine::Event(StreamEvent::Log {
                level: LogLevel::Info,
                message: "hø".into(),
            })]
        );
    }

    #[test]
    fn test_parse_stream_line_garbage() {
        assert_eq!(parse_stream_line(""), None);
        assert_eq!(parse_stream_line("not json"), None);
        assert_eq!(parse_stream_line("{\"type\":\"unknown\"}"), None);
        assert_eq!(
            parse_stream_line("{\"type\":\"finished\",\"ok\":true}"),
            Some(StreamLine::Finished(Ok(())))
        );
        assert_eq!(
            parse_stream_line("{\"type\":\"finished\",\"ok\":false}"),
            Some(StreamLine::Finished(Err("operation failed".into())))
        );
    }
}
