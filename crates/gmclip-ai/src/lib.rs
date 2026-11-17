//! AI providers and passes (SPEC §10), and the agent launcher (SPEC §10.7).
#![forbid(unsafe_code)]

pub mod acp;
pub mod launcher;

/// An AI provider (SPEC §10.1). Methods arrive with M1.
pub trait Provider: Send + Sync {
    /// Stable provider id, e.g. `anthropic`.
    fn id(&self) -> &str;
}
