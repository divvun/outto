use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("config error: {0}")]
    Config(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("file operation failed: {path}: {source}")]
    FileOp {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("directory operation failed: {path}: {source}")]
    DirOp {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("registry error: {key}: {message}")]
    Registry { key: String, message: String },

    #[error("shortcut error: {name}: {message}")]
    Shortcut { name: String, message: String },

    #[error("environment variable error: {name}: {message}")]
    Environment { name: String, message: String },

    #[error("service error: {name}: {message}")]
    Service { name: String, message: String },

    #[error("file association error: {extension}: {message}")]
    Association { extension: String, message: String },

    #[error("COM registration error: {file}: {message}")]
    ComRegistration { file: String, message: String },

    #[error("prerequisite not met: {name}")]
    Prerequisite { name: String },

    #[error("command execution failed: {command}: {message}")]
    CommandExec { command: String, message: String },

    #[error("font installation error: {file}: {message}")]
    Font { file: String, message: String },

    #[error("elevation required: {0}")]
    ElevationRequired(String),

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("rollback failed: {original_error} (rollback error: {rollback_error})")]
    RollbackFailed {
        original_error: String,
        rollback_error: String,
    },

    #[error("upgrade conflict: {0}")]
    UpgradeConflict(String),

    #[error("glob pattern error: {0}")]
    GlobPattern(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    Abort,
    Retry,
    Ignore,
}

pub type InstallerResult<T> = Result<T, InstallerError>;
