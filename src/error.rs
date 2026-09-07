//! Tool-level errors. Wire shape matches the post-control-plane contract:
//! every failure is data (`ToolError`), never a protocol error, so the model
//! can self-correct inline.

use crate::types::ToolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("backend unavailable ({backend}): {detail}")]
    Unavailable { backend: &'static str, detail: String },
    #[error("stale handle: {0}")]
    StaleHandle(String),
    #[error("action failed: {0}")]
    Failed(String),
    #[error("bus disconnected: {detail}")]
    BusDisconnected { detail: String },
    #[error("i/o error on {path}: {error}")]
    Io { path: String, error: String },
    #[error("external command failed: {stderr}")]
    ExternalCommandFailed { stderr: String },
    #[error("input dispatch failed: {detail}")]
    InputDispatchFailed { detail: String },
    #[error("timeout: {detail}")]
    Timeout { detail: String },
    #[error("unsupported: {reason}")]
    Unsupported { reason: String },
}

impl BackendError {
    pub fn tool(&self, retryable: bool) -> ToolError {
        let (code, message) = match self {
            Self::NotImplemented(what) => ("not_implemented", format!("not implemented: {what}")),
            Self::Unavailable { backend, detail } => {
                ("backend_unavailable", format!("{backend}: {detail}"))
            }
            Self::StaleHandle(msg) => ("stale_handle", msg.clone()),
            Self::Failed(msg) => ("action_failed", msg.clone()),
            Self::BusDisconnected { detail } => ("bus_disconnected", detail.clone()),
            Self::Io { path, error } => ("io", format!("{path}: {error}")),
            Self::ExternalCommandFailed { stderr } => ("external_command_failed", stderr.clone()),
            Self::InputDispatchFailed { detail } => ("input_dispatch_failed", detail.clone()),
            Self::Timeout { detail } => ("timeout", detail.clone()),
            Self::Unsupported { reason } => ("unsupported", reason.clone()),
        };
        ToolError {
            code: code.into(),
            message,
            retryable,
        }
    }
}
