//! The exit codes of the installer and uninstaller binaries, which callers
//! running them silently rely on.

use std::fmt;

use crate::error::InstallerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success,
    /// The operation failed and left the machine as it was.
    Failed,
    /// An `[[uninstall.refuse_while]]` condition holds and nothing changed.
    /// The condition may pass, so retrying later can succeed.
    Refused,
    /// The operation failed and could not undo everything it had done.
    Incomplete,
    /// The command line, configuration or payload is unusable and nothing
    /// changed. Retrying the same invocation cannot succeed.
    InvalidInvocation,
    /// The operation succeeded, but the machine must restart before it takes
    /// full effect, and outto did not restart it.
    RebootRequired,
}

impl ExitCode {
    pub const fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Failed => 1,
            Self::Refused => 2,
            Self::Incomplete => 3,
            Self::InvalidInvocation => 4,
            // Windows Installer's ERROR_SUCCESS_REBOOT_REQUIRED, which
            // deployment tools and outto's own `[[run]]` handler already read
            // as a successful install that needs a restart.
            Self::RebootRequired => 3010,
        }
    }

    pub const fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Success,
            1 => Self::Failed,
            2 => Self::Refused,
            3 => Self::Incomplete,
            4 => Self::InvalidInvocation,
            3010 => Self::RebootRequired,
            _ => return None,
        })
    }
}

/// A failed install or uninstall as a binary reports it: the message to show
/// and the code to exit with. It crosses the GUI bridge and the elevated
/// child's progress file, so the code chosen where the [`InstallerError`]
/// arose is the one the process exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub exit: ExitCode,
    pub message: String,
}

impl Failure {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            exit: ExitCode::Failed,
            message: message.into(),
        }
    }
}

impl From<InstallerError> for Failure {
    fn from(error: InstallerError) -> Self {
        let exit = match &error {
            InstallerError::Elevated(failure) => return failure.clone(),
            InstallerError::RollbackFailed { .. } | InstallerError::PartialRollback(_) => {
                ExitCode::Incomplete
            }
            InstallerError::UninstallRefused(_) => ExitCode::Refused,
            _ => ExitCode::Failed,
        };
        Self {
            exit,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for exit in [
            ExitCode::Success,
            ExitCode::Failed,
            ExitCode::Refused,
            ExitCode::Incomplete,
            ExitCode::InvalidInvocation,
            ExitCode::RebootRequired,
        ] {
            assert_eq!(ExitCode::from_code(exit.code()), Some(exit));
        }
        assert_eq!(ExitCode::from_code(5), None);
    }

    #[test]
    fn classifies_installer_errors() {
        let incomplete = InstallerError::RollbackFailed {
            original_error: "copy failed".into(),
            rollback_error: "registry".into(),
        };
        assert_eq!(Failure::from(incomplete).exit, ExitCode::Incomplete);
        let partial = InstallerError::PartialRollback("2 errors".into());
        assert_eq!(Failure::from(partial).exit, ExitCode::Incomplete);
        let refused = InstallerError::UninstallRefused("Word is running".into());
        assert_eq!(Failure::from(refused).exit, ExitCode::Refused);
        let failed = InstallerError::Prerequisite {
            name: "dotnet".into(),
        };
        assert_eq!(Failure::from(failed).exit, ExitCode::Failed);
    }

    #[test]
    fn elevated_failures_keep_their_code() {
        let child = Failure {
            exit: ExitCode::Incomplete,
            message: "rollback failed".into(),
        };
        let parent = Failure::from(InstallerError::Elevated(child.clone()));
        assert_eq!(parent, child);
    }
}
