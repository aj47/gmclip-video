//! Client versions and the tested range (R-AGT-1: "a client version outside the tested
//! range is refused"). The ranges mirror `evals/h79/clients.toml`; a test in
//! `gmclip-eval` asserts the two agree.
//!
//! A range is closed on the tested version and open below the next minor: an unknown
//! major (or minor) is never supported by default, because a new major is exactly when
//! flags get renamed and isolation silently weakens (S7).

use serde::Serialize;

use super::Harness;

/// A parsed `major.minor.patch`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `[min, max)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Range {
    pub min: Version,
    pub max_exclusive: Version,
}

const fn v(major: u32, minor: u32, patch: u32) -> Version {
    Version {
        major,
        minor,
        patch,
    }
}

/// The tested versions (S7, `spikes/S7/README.md`): exactly the patch the probe ran on.
/// Both clients ship behaviour changes in patch releases (2.1.300's `--verbose`
/// requirement is one), so a range wider than one patch would admit builds nobody
/// probed. Adding a version is a re-run of the S7 probe on it, never an edit alone.
///
/// `None`: no version of the client has been through S7 yet (pi, opencode: their flag
/// sets are written from their `--help` and docs, `decisions/0012`). Every build is then
/// "outside the tested range"; the app runs one only under
/// [`super::VersionPolicy::Verified`] for that exact binary.
pub fn supported(h: Harness) -> Option<Range> {
    match h {
        // checked 2026-10-14 (spikes/S7)
        Harness::ClaudeCode => Some(Range {
            min: v(2, 1, 300),
            max_exclusive: v(2, 1, 301),
        }),
        // checked 2026-10-14 (spikes/S7). It fails S7-X1 and S7-X5; AJ accepted that on
        // 2026-10-16 ("dont worry about codex safety", decision 0012), so it runs like
        // the others
        Harness::Codex => Some(Range {
            min: v(0, 160, 0),
            max_exclusive: v(0, 160, 1),
        }),
        Harness::Pi | Harness::OpenCode => None,
    }
}

/// Parses the first `N.N.N` in a client's `--version` output
/// (`2.1.300 (Claude Code)`, `codex-cli 0.160.0`).
pub fn parse(output: &str) -> Option<Version> {
    for token in output.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',') {
        let token = token.strip_prefix('v').unwrap_or(token);
        let mut it = token.splitn(3, '.');
        let (Some(a), Some(b), Some(c)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        // a pre-release / build suffix ("0.160.0-alpha.1") is not a tested version
        let parse = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        if let (Some(major), Some(minor), Some(patch)) = (parse(a), parse(b), parse(c)) {
            return Some(v(major, minor, patch));
        }
        return None;
    }
    None
}

/// The verdict on one client version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Unsupported { reason: String },
}

pub fn check(h: Harness, version: Option<Version>) -> Verdict {
    let Some(ver) = version else {
        return Verdict::Unsupported {
            reason: "the client did not report a plain major.minor.patch version".into(),
        };
    };
    let Some(r) = supported(h) else {
        return Verdict::Unsupported {
            reason: format!(
                "no {} version has been through gmclip's isolation check (S7) yet; {ver} runs only once you allow this exact build",
                h.label()
            ),
        };
    };
    if ver.major != r.min.major {
        return Verdict::Unsupported {
            reason: format!(
                "{} major {} is unknown (tested: {}..{})",
                h.as_str(),
                ver.major,
                r.min,
                r.max_exclusive
            ),
        };
    }
    if ver < r.min || ver >= r.max_exclusive {
        return Verdict::Unsupported {
            reason: format!(
                "{} {ver} is outside the tested range {}..{}",
                h.as_str(),
                r.min,
                r.max_exclusive
            ),
        };
    }
    Verdict::Supported
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version-range fixture: real `--version` strings of the installed clients, and
    /// the shapes a future release could take. Unknown majors must fail.
    #[test]
    fn version_range_fixture() {
        let cases: &[(Harness, &str, bool)] = &[
            (Harness::ClaudeCode, "2.1.300 (Claude Code)", true),
            (Harness::ClaudeCode, "2.1.317 (Claude Code)", false),
            (Harness::ClaudeCode, "2.1.301 (Claude Code)", false),
            (Harness::ClaudeCode, "2.1.299 (Claude Code)", false),
            (Harness::ClaudeCode, "2.2.0 (Claude Code)", false),
            (Harness::ClaudeCode, "3.0.0 (Claude Code)", false),
            (Harness::ClaudeCode, "1.9.999 (Claude Code)", false),
            (Harness::ClaudeCode, "2.1.300-beta.1 (Claude Code)", false),
            (Harness::ClaudeCode, "Claude Code", false),
            (Harness::ClaudeCode, "", false),
            // the tested Codex (its S7 leaks accepted, decision 0012 2026-10-16)
            (Harness::Codex, "codex-cli 0.160.0", true),
            (Harness::Codex, "codex-cli 0.160.4", false),
            (Harness::Codex, "codex-cli 0.161.0", false),
            (Harness::Codex, "codex-cli 0.159.9", false),
            (Harness::Codex, "codex-cli 1.0.0", false),
            (Harness::Codex, "codex-cli 0.160.0-alpha.2", false),
            // no tested version yet: every build needs the per-binary allowance
            (Harness::Pi, "0.85.1", false),
            (Harness::OpenCode, "1.18.0", false),
        ];
        for (h, text, ok) in cases {
            let verdict = check(*h, parse(text));
            assert_eq!(
                verdict == Verdict::Supported,
                *ok,
                "{h:?} {text:?} → {verdict:?}"
            );
        }
    }

    /// Codex is gated like Claude Code (AJ, 2026-10-16: "dont worry about codex
    /// safety"): its tested build runs, a newer one is outside the range and takes the
    /// same one-time check / your allowance as a new Claude Code.
    #[test]
    fn codex_is_gated_like_claude_code() {
        assert_eq!(
            check(Harness::Codex, parse("codex-cli 0.160.0")),
            Verdict::Supported
        );
        let Verdict::Unsupported { reason } = check(Harness::Codex, parse("codex-cli 0.161.0"))
        else {
            panic!()
        };
        assert!(reason.contains("outside the tested range"), "{reason}");
    }

    #[test]
    fn an_untested_client_says_why() {
        let Verdict::Unsupported { reason } = check(Harness::OpenCode, parse("1.18.11")) else {
            panic!()
        };
        assert!(
            reason.contains("opencode") && reason.contains("1.18.11"),
            "{reason}"
        );
        assert_eq!(parse("0.85.1"), Some(v(0, 85, 1)));
    }

    #[test]
    fn unknown_major_says_so() {
        let Verdict::Unsupported { reason } =
            check(Harness::ClaudeCode, parse("3.0.1 (Claude Code)"))
        else {
            panic!()
        };
        assert!(reason.contains("major 3 is unknown"), "{reason}");
    }
}
