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
        };
        ToolError {
            code: code.into(),
            message,
            retryable,
        }
    }
}
