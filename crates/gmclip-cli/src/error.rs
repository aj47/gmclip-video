//! The CLI's one error shape (A-01; SPEC R-CMD-6) and its exit codes.
//!
//! Every failure is one JSON object on stderr:
//! `{code, step?, frame?, source?, os_status?, path?, reason?, more?, trace_id}`.
//!
//! | exit | code | meaning |
//! |---|---|---|
//! | 0 | — | success (a JSON object on stdout) |
//! | 1 | `Invalid` | the input was refused (plan, media) |
//! | 2 | `Usage` | bad arguments |
//! | 3 | `Unsupported` | well formed, but not built or not supported |
//! | 4 | `NotFound` | an input file does not exist |
//! | 5 | `Io` | reading or writing a file failed otherwise |
//! | 6 | `Media` | a media step failed (`step`, `frame?`, `source?`, `os_status?`) |
//! | 7 | `Render` | a render step failed (`step`, `frame?`) |
//! | 8 | `Conflict` | a socket command's `base` moved (M1 socket subcommands) |
//! | 9 | `Leased` | another process holds the project |
//! | 77 | `Unauthorized` | refused: scope, file roots, Owner code or TTY (`reason` says which) |
//! | 70 | `Internal` | a gmclip bug (a panic); the log has the details under `trace_id` |
//!
//! `reason` is written by gmclip, never copied from the input (a usage error names the
//! offending argument cut to 16 bytes); `path` is a RenderPlan JSON Pointer that echoes
//! at most 16 caller bytes.
//!
//! Relation to R-CMD-6's closed set (for A-15's `CmdError`): `Invalid`, `Unsupported` and
//! `Internal{trace_id}` are R-CMD-6 codes; `Media` and `Render` carry R-CMD-6's media
//! error shape `{step, frame?, source?, os_status?}` under a code; `Usage`, `NotFound` and
//! `Io` exist only at the M0 CLI edge (no project, no socket). `Unsupported` has no
//! `platform` yet because no M0 refusal is platform-specific.

use std::io;

use gmclip_media::MediaError;
use gmclip_media::ops::OpError;
use gmclip_plan::PlanError;
use gmclip_render::ops::RenderError;
use serde_json::{Map, Value};

/// The stable error codes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Code {
    Invalid,
    Usage,
    Unsupported,
    NotFound,
    Io,
    Media,
    Render,
    Conflict,
    Leased,
    Unauthorized,
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Invalid => "Invalid",
            Code::Usage => "Usage",
            Code::Unsupported => "Unsupported",
            Code::NotFound => "NotFound",
            Code::Io => "Io",
            Code::Media => "Media",
            Code::Render => "Render",
            Code::Conflict => "Conflict",
            Code::Leased => "Leased",
            Code::Unauthorized => "Unauthorized",
            Code::Internal => "Internal",
        }
    }

    /// The process exit code (table above).
    pub fn exit(self) -> u8 {
        match self {
            Code::Invalid => 1,
            Code::Usage => 2,
            Code::Unsupported => 3,
            Code::NotFound => 4,
            Code::Io => 5,
            Code::Media => 6,
            Code::Render => 7,
            Code::Conflict => 8,
            Code::Leased => 9,
            Code::Unauthorized => 77,
            Code::Internal => 70,
        }
    }
}

/// One CLI failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliError {
    pub code: Code,
    pub step: Option<String>,
    pub frame: Option<u64>,
    pub source: Option<String>,
    pub os_status: Option<i64>,
    pub path: Option<String>,
    pub reason: Option<String>,
    /// Further errors of the same run not shown (plan validation).
    pub more: Option<usize>,
}

impl CliError {
    pub fn new(code: Code) -> Self {
        Self {
            code,
            step: None,
            frame: None,
            source: None,
            os_status: None,
            path: None,
            reason: None,
            more: None,
        }
    }

    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// A file could not be read or written at `step`. The path is not echoed (the
    /// caller knows it); the OS error kind is.
    pub fn io(step: &str, e: &io::Error) -> Self {
        let code = if e.kind() == io::ErrorKind::NotFound {
            Code::NotFound
        } else {
            Code::Io
        };
        Self::new(code).step(step).reason(e.kind().to_string())
    }

    /// The JSON object written to stderr.
    pub fn to_json(&self, trace_id: &str) -> Value {
        let mut m = Map::new();
        m.insert("code".into(), self.code.as_str().into());
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.into(), v);
            }
        };
        put("step", self.step.clone().map(Value::from));
        put("frame", self.frame.map(Value::from));
        put("source", self.source.clone().map(Value::from));
        put("os_status", self.os_status.map(Value::from));
        put("path", self.path.clone().map(Value::from));
        put("reason", self.reason.clone().map(Value::from));
        put("more", self.more.map(Value::from));
        m.insert("trace_id".into(), trace_id.into());
        Value::Object(m)
    }
}

impl From<PlanError> for CliError {
    fn from(e: PlanError) -> Self {
        let code = match e {
            PlanError::Invalid { .. } => Code::Invalid,
            PlanError::Unsupported { .. } => Code::Unsupported,
        };
        let mut c = Self::new(code).step("plan").reason(e.reason());
        c.path = Some(e.path().to_owned());
        c
    }
}

impl From<MediaError> for CliError {
    fn from(e: MediaError) -> Self {
        let mut c = Self::new(Code::Media).step(e.step);
        c.frame = e.frame;
        c.source = e.source_id;
        c.os_status = e.os_status;
        c
    }
}

impl From<OpError> for CliError {
    fn from(e: OpError) -> Self {
        match e {
            OpError::Media(m) => m.into(),
            OpError::Invalid { step, reason } => Self::new(Code::Invalid).step(step).reason(reason),
            OpError::Unsupported { step, reason } => {
                Self::new(Code::Unsupported).step(step).reason(reason)
            }
        }
    }
}

impl From<RenderError> for CliError {
    fn from(e: RenderError) -> Self {
        match e {
            RenderError::Plan(p) => p.into(),
            RenderError::Media(m) => m.into(),
            RenderError::Failed {
                step,
                frame,
                reason,
            } => {
                let mut c = Self::new(Code::Render).step(step).reason(reason);
                c.frame = frame;
                c
            }
            RenderError::Unsupported { step, reason } => {
                Self::new(Code::Unsupported).step(step).reason(reason)
            }
            RenderError::Invalid { step, reason } => {
                Self::new(Code::Invalid).step(step).reason(reason)
            }
            RenderError::Io { step, what, kind } => {
                let code = if kind == std::io::ErrorKind::NotFound {
                    Code::NotFound
                } else {
                    Code::Io
                };
                Self::new(code).step(step).reason(format!("{what}: {kind}"))
            }
        }
    }
}

impl From<gmclip_core::CmdError> for CliError {
    fn from(e: gmclip_core::CmdError) -> Self {
        use gmclip_core::CmdError as E;
        let reason = e.to_string();
        match e {
            E::Invalid { path, reason } => {
                let mut c = CliError::new(Code::Invalid).reason(reason);
                c.path = Some(path);
                c
            }
            E::NotFound => CliError::new(Code::NotFound).reason(reason),
            E::Unauthorized { scope } => {
                CliError::new(Code::Unauthorized).reason(format!("refused: {scope}"))
            }
            E::Conflict(_) => CliError::new(Code::Conflict).reason(reason),
            E::Leased => CliError::new(Code::Leased).reason(reason),
            E::Unsupported { .. } => CliError::new(Code::Unsupported).reason(reason),
            _ => CliError::new(Code::Internal).reason(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct_and_nonzero() {
        let all = [
            Code::Invalid,
            Code::Usage,
            Code::Unsupported,
            Code::NotFound,
            Code::Io,
            Code::Media,
            Code::Render,
            Code::Conflict,
            Code::Leased,
            Code::Unauthorized,
            Code::Internal,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in all {
            assert_ne!(c.exit(), 0);
            assert!(seen.insert(c.exit()), "{c:?}");
        }
    }

    #[test]
    fn media_errors_carry_the_r_cmd_6_fields() {
        let e: CliError = MediaError {
            step: "decode",
            frame: Some(42),
            source_id: Some("mkv1440".into()),
            os_status: Some(-12909),
        }
        .into();
        assert_eq!(
            e.to_json("01J00000000000000000000000"),
            serde_json::json!({
                "code": "Media", "step": "decode", "frame": 42, "source": "mkv1440",
                "os_status": -12909, "trace_id": "01J00000000000000000000000"
            })
        );
        assert_eq!(e.code.exit(), 6);
    }

    #[test]
    fn plan_errors_keep_their_pointer() {
        let e: CliError = PlanError::invalid("/parts/1/out", "range start must be < end").into();
        let j = e.to_json("t");
        assert_eq!(j["code"], "Invalid");
        assert_eq!(j["step"], "plan");
        assert_eq!(j["path"], "/parts/1/out");
        let u: CliError = RenderError::Unsupported {
            step: "render",
            reason: "not built yet (H-10)".into(),
        }
        .into();
        assert_eq!(u.code.exit(), 3);
    }
}
