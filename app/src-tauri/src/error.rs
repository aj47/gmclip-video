//! Errors on the UI wire. Registry commands return their `CmdError` as is (R-CMD-6's
//! closed set, serialised with its `code` tag); app-shell commands use the same shape
//! with a plain-words `message` for the person reading it. Messages never quote file
//! content, transcript text or more than a file's name.

use gmclip_proto::CmdError;
use serde::Serialize;
use serde_json::{Value, json};

/// One error as the UI receives it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("Invalid", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("NotFound", message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("Internal", message)
    }
}

impl From<CmdError> for AppError {
    fn from(e: CmdError) -> Self {
        let detail = serde_json::to_value(&e).ok();
        let code = detail
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("Internal")
            .to_owned();
        Self {
            code,
            message: e.to_string(),
            detail,
        }
    }
}

impl From<gmclip_render::ops::RenderError> for AppError {
    fn from(e: gmclip_render::ops::RenderError) -> Self {
        Self::new("Render", e.to_string())
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

/// The `{ok, …}` reply body for an error.
pub fn reply_err(e: &AppError) -> Value {
    json!({ "ok": false, "error": e })
}

pub type AppResult<T> = Result<T, AppError>;
