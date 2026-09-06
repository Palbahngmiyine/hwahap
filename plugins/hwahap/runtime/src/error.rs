//! The one error type.
//!
//! Hwahap fails closed. Each variant answers "what should the host do about it", not "where did it
//! come from", because the only consumers are the MCP layer (which turns an error into a tool
//! error) and the engine (which turns some of them into a `blocked` run).

use std::path::Path;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The caller asked for something the current state does not allow.
    #[error("{0}")]
    Rejected(String),

    /// Durable state on disk is unreadable or inconsistent with itself.
    #[error("hwahap state is corrupt: {0}")]
    Corrupt(String),

    /// A configured or requested model/effort profile cannot be honoured exactly.
    #[error("unsupported_profile: {0}")]
    UnsupportedProfile(String),

    #[error("execution boundary violated: {0}")]
    BoundaryViolation(String),

    #[error("execution limit reached: {0}")]
    ExecutionLimit(String),

    /// An external command (git or gh) failed.
    #[error("{command} failed: {detail}")]
    Command { command: String, detail: String },

    /// Filesystem failure, with the path that caused it.
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A bug in Hwahap itself.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Wraps an IO error with the path it applies to.
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    /// Wraps an external command failure.
    pub fn command(command: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Command {
            command: command.into(),
            detail: detail.into(),
        }
    }

    /// True when the error means "this run cannot continue", as opposed to "this call was wrong".
    ///
    /// The engine turns these into a `blocked` run instead of a transient tool error, so the user
    /// is told once and is not invited to retry something that cannot succeed.
    pub fn is_terminal_for_run(&self) -> bool {
        matches!(
            self,
            Error::Corrupt(_)
                | Error::UnsupportedProfile(_)
                | Error::BoundaryViolation(_)
                | Error::ExecutionLimit(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_name_the_path() {
        let err = Error::io(
            "/tmp/plan.json",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(err.to_string().starts_with("/tmp/plan.json: "));
    }

    #[test]
    fn unsupported_profile_uses_the_documented_blocked_token() {
        let err = Error::UnsupportedProfile("effort xhigh is not advertised".into());
        assert!(
            err.to_string().starts_with("unsupported_profile: "),
            "the effort policy requires this exact token, got {err}"
        );
    }

    #[test]
    fn unrecoverable_execution_errors_stop_the_run() {
        assert!(Error::Corrupt("x".into()).is_terminal_for_run());
        assert!(Error::BoundaryViolation("x".into()).is_terminal_for_run());
        assert!(Error::ExecutionLimit("x".into()).is_terminal_for_run());
        assert!(Error::UnsupportedProfile("x".into()).is_terminal_for_run());
        assert!(!Error::Rejected("x".into()).is_terminal_for_run());
        assert!(!Error::command("git", "x").is_terminal_for_run());
        assert!(!Error::Internal("x".into()).is_terminal_for_run());
        assert!(
            !Error::io("/p", std::io::Error::from(std::io::ErrorKind::NotFound))
                .is_terminal_for_run()
        );
    }

    #[test]
    fn command_errors_name_the_command() {
        assert_eq!(
            Error::command("gh", "exit 1").to_string(),
            "gh failed: exit 1"
        );
    }
}
