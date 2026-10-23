//! The progress file: JSON lines an install or uninstall appends while it
//! runs, written when the binaries get `--progress-file <path>`.
//!
//! Each line is one object with a `type`:
//!
//! - `progress`: `phase`, `current`, `total`
//! - `log`: `level` (`debug`, `info`, `warn`, `error`) and `message`
//! - `reboot_required`: the reboot policy decided a restart is wanted
//! - `finished`: `ok`; when false, also `error` and `exit_code`. Written last.
//!
//! A plain file rather than a pipe: opening a FIFO blocks until the peer
//! appears, which would wedge a reader whose writer never starts.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};
use crate::error::{ErrorAction, InstallerError};
use crate::exit::{ExitCode, Failure};

/// An event read back from a progress file.
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
    /// The install's reboot policy decided a restart is wanted.
    RebootRequired,
}

/// One line of a progress file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamLine {
    Event(StreamEvent),
    Finished(Result<(), Failure>),
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

/// Parse one line of a progress file. Unknown event types and malformed
/// lines yield `None`, so readers tolerate events added later.
pub fn parse_stream_line(line: &str) -> Option<StreamLine> {
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
        "reboot_required" => Some(StreamLine::Event(StreamEvent::RebootRequired)),
        "finished" => Some(StreamLine::Finished(
            if v["ok"].as_bool().unwrap_or(false) {
                Ok(())
            } else {
                Err(Failure {
                    exit: v["exit_code"]
                        .as_i64()
                        .and_then(|code| i32::try_from(code).ok())
                        .and_then(ExitCode::from_code)
                        .unwrap_or(ExitCode::Failed),
                    message: v["error"]
                        .as_str()
                        .unwrap_or("operation failed")
                        .to_string(),
                })
            },
        )),
        _ => None,
    }
}

/// `InstallerCallbacks` that append JSON-line events to the progress file,
/// one flushed line per event. Prompts are auto-accepted and errors abort,
/// matching `/VERYSILENT /SUPPRESSMSGBOXES` semantics: whoever reads the file
/// can't answer prompts.
pub struct FileProgressCallbacks {
    file: Mutex<std::fs::File>,
    reboot_required: AtomicBool,
}

impl FileProgressCallbacks {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
            reboot_required: AtomicBool::new(false),
        })
    }

    pub fn reboot_required(&self) -> bool {
        self.reboot_required.load(Ordering::SeqCst)
    }

    fn write_line(&self, value: serde_json::Value) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{value}");
            let _ = f.flush();
        }
    }

    /// Write the terminal `finished` event. Call exactly once, right before
    /// the process exits.
    pub fn write_finished(&self, result: Result<(), &Failure>) {
        self.write_line(match result {
            Ok(()) => serde_json::json!({"type": "finished", "ok": true}),
            Err(failure) => serde_json::json!({
                "type": "finished",
                "ok": false,
                "error": failure.message,
                "exit_code": failure.exit.code(),
            }),
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

    fn on_reboot_required(&self) {
        self.reboot_required.store(true, Ordering::SeqCst);
        self.write_line(serde_json::json!({"type": "reboot_required"}));
    }
}

/// Byte-level tail state for the progress file: tracks the read offset and a
/// partial trailing line (writes aren't atomic, and a flush can land mid-way
/// through a multi-byte character).
pub struct Tail {
    pos: u64,
    partial: Vec<u8>,
}

impl Tail {
    pub fn new() -> Self {
        Self {
            pos: 0,
            partial: Vec::new(),
        }
    }

    pub fn drain(&mut self, path: &Path, mut handle: impl FnMut(StreamLine)) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_event_round_trip() {
        let path = std::env::temp_dir().join(format!("outto-stream-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let cb = FileProgressCallbacks::create(&path).unwrap();
        cb.on_progress("copy files", 3, 10);
        cb.on_log(LogLevel::Warn, "skipped \"weird\" påth\nwith newline");
        cb.on_error(&InstallerError::Other("boom".into()));
        cb.on_reboot_required();
        let failure = Failure {
            exit: ExitCode::Incomplete,
            message: "it broke".into(),
        };
        cb.write_finished(Err(&failure));

        let mut lines: Vec<StreamLine> = Vec::new();
        let mut tail = Tail::new();
        tail.drain(&path, |l| lines.push(l));
        std::fs::remove_file(&path).unwrap();

        assert!(cb.reboot_required());
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
                StreamLine::Event(StreamEvent::RebootRequired),
                StreamLine::Finished(Err(failure)),
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
            Some(StreamLine::Finished(Err(Failure::failed(
                "operation failed"
            ))))
        );
        assert_eq!(
            parse_stream_line(
                "{\"type\":\"finished\",\"ok\":false,\"error\":\"x\",\"exit_code\":2}"
            ),
            Some(StreamLine::Finished(Err(Failure {
                exit: ExitCode::Refused,
                message: "x".into(),
            })))
        );
    }
}
