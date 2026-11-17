//! `cargo xt codeowners [--check] [paths…]`: which owners GitHub would require for a path,
//! from `.github/CODEOWNERS` (A-04, SR R-SUP-2).
//!
//! GitHub applies the **last** matching line, and any one listed owner can approve.
//! Patterns follow GitHub's documented gitignore subset: a leading `/` anchors at the repo
//! root, a trailing `/` matches everything below a directory, `*` stays within one path
//! segment (`docs/*` does not match `docs/a/b.md`), `**` spans segments, and a pattern
//! with no `/` except a trailing one matches at any depth.
//!
//! `--check` (and the tests) require, for the committed file:
//! - every path in [`MUST_BE_AJ`] — and every tracked path under a `codec` directory or
//!   named `codec.rs`, wherever it lives — is owned by **exactly** `@aj47` (a co-owner
//!   could approve alone);
//! - no rule after an AJ-only rule narrows into what that rule protects with other (or
//!   no) owners, and no non-AJ rule names `codec` at all;
//! - nothing under a `codec/` directory pulls in code from outside it (`#[path]`,
//!   `include!`), which a CODEOWNERS path rule could not see.

use std::path::Path;
use std::process::Command;

/// Paths whose changes need AJ alone (SPEC D3, §15.0, §15.4, §19.2; SR R-SUP-2).
pub const MUST_BE_AJ: &[&str] = &[
    ".github/CODEOWNERS",
    ".github/workflows/ci.yml",
    ".github/pull_request_template.md",
    "Cargo.toml",
    "deny.toml",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "supply-chain/config.toml",
    "supply-chain/audits.toml",
    "supply-chain/build-and-proc-macro.txt",
    "xtask/src/deps.rs",
    "xtask/src/supply.rs",
    "xtask/src/fixtures.rs",
    "xtask/src/codeowners.rs",
    "xtask/fixtures/ci-fmt/src/lib.rs",
    // codec/, wherever a crate puts it (A-04).
    "crates/gmclip-media/src/codec/ALLOWED_FIELDS.md",
    "crates/gmclip-media/src/codec/mod.rs",
    "crates/gmclip-media/src/codec.rs",
    "crates/gmclip-media/src/codec/h264/sps.rs",
    "crates/gmclip-media/src/codec/vp9.rs",
    "crates/gmclip-media/codec/x.rs",
    "crates/gmclip-platform/src/codec/sps.rs",
    "crates/gmclip-codec/src/codec/obu.rs",
    "codec/x.rs",
    "app/src-tauri/capabilities/default.json",
    "app/src-tauri/tauri.conf.json",
    "app/src-tauri/gmclip.entitlements",
    "app/src-tauri/signing/notary.json",
];

/// One CODEOWNERS rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub pattern: String,
    pub owners: Vec<String>,
}

impl Rule {
    fn aj_only(&self) -> bool {
        self.owners == ["@aj47"]
    }
}

pub fn parse(text: &str) -> Vec<Rule> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pattern = it.next()?.to_owned();
            // An owner list ends at a comment.
            let owners = it
                .take_while(|t| !t.starts_with('#'))
                .map(str::to_owned)
                .collect();
            Some(Rule { pattern, owners })
        })
        .collect()
}

/// Glob match of `pat` segments against `path` segments (`**` = any number of segments).
fn segs_match(pat: &[&str], path: &[&str]) -> bool {
    match (pat.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => (0..=path.len()).any(|k| segs_match(&pat[1..], &path[k..])),
        (Some(p), Some(s)) => {
            seg_match(p.as_bytes(), s.as_bytes()) && segs_match(&pat[1..], &path[1..])
        }
        _ => false,
    }
}

/// `*` and `?` within one segment.
fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => (0..=s.len()).any(|k| seg_match(&p[1..], &s[k..])),
        (Some(b'?'), Some(_)) => seg_match(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => seg_match(&p[1..], &s[1..]),
        _ => false,
    }
}

/// Whether a CODEOWNERS pattern matches a repo-relative file path.
pub fn matches(pattern: &str, path: &str) -> bool {
    let dir = pattern.ends_with('/');
    let trimmed = pattern.trim_end_matches('/');
    let anchored = trimmed.starts_with('/') || trimmed.contains('/');
    let mut pat: Vec<&str> = trimmed.trim_start_matches('/').split('/').collect();
    if !anchored {
        pat.insert(0, "**");
    }
    let path: Vec<&str> = path.split('/').collect();
    let last_is_glob = pat
        .last()
        .is_some_and(|l| l.contains(['*', '?']) && *l != "**");
    if dir {
        // `dir/` matches the directory's contents at any depth.
        (1..path.len()).any(|k| segs_match(&pat, &path[..k]))
    } else if last_is_glob {
        // `docs/*` matches files directly in docs/ only (GitHub's documented rule).
        segs_match(&pat, &path)
    } else {
        // A plain name matches that file, or a directory of that name and its contents.
        segs_match(&pat, &path) || (1..path.len()).any(|k| segs_match(&pat, &path[..k]))
    }
}

/// The owners of `path` (the last matching rule's), empty if none.
pub fn owners<'r>(rules: &'r [Rule], path: &str) -> &'r [String] {
    rules
        .iter()
        .rev()
        .find(|r| matches(&r.pattern, path))
        .map(|r| r.owners.as_slice())
        .unwrap_or_default()
}

/// Structural problems: a later rule narrowing into an AJ-only rule's scope with other
/// owners, or a non-AJ rule about `codec`.
pub fn overrides(rules: &[Rule]) -> Vec<String> {
    let mut out = Vec::new();
    for (j, r) in rules.iter().enumerate() {
        if r.aj_only() {
            continue;
        }
        let norm = r.pattern.trim_start_matches('/');
        if norm
            .split('/')
            .any(|s| s == "codec" || s.starts_with("codec.") || s == "codec*")
        {
            out.push(format!("`{}` names codec but is not AJ-only", r.pattern));
            continue;
        }
        for p in rules[..j]
            .iter()
            .filter(|p| p.aj_only() && p.pattern.ends_with('/'))
        {
            let scope = p.pattern.trim_start_matches('/');
            if norm.starts_with(scope) || norm.starts_with("**") {
                out.push(format!(
                    "`{}` (after `{}`) takes paths away from AJ",
                    r.pattern, p.pattern
                ));
            }
        }
    }
    out
}

fn tracked_codec_paths(root: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|p| p.split('/').any(|s| s == "codec" || s == "codec.rs"))
        .map(str::to_owned)
        .collect()
}

/// `#[path]` / `include!` inside a codec/ directory (code a path rule cannot see).
fn codec_escapes(root: &Path, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| p.ends_with(".rs"))
        .filter(|p| {
            std::fs::read_to_string(root.join(p))
                .is_ok_and(|t| t.contains("#[path") || t.contains("include!("))
        })
        .cloned()
        .collect()
}

/// Everything `--check` verifies; empty means pass.
pub fn problems(root: &Path, text: &str) -> Vec<String> {
    let rules = parse(text);
    let tracked = tracked_codec_paths(root);
    let mut out = Vec::new();
    for p in MUST_BE_AJ
        .iter()
        .copied()
        .chain(tracked.iter().map(String::as_str))
    {
        let o = owners(&rules, p);
        if o != ["@aj47"] {
            out.push(format!("{p} is owned by {o:?}, not exactly @aj47"));
        }
    }
    out.extend(overrides(&rules));
    for p in codec_escapes(root, &tracked) {
        out.push(format!(
            "{p} uses #[path] or include!, which CODEOWNERS cannot follow"
        ));
    }
    out
}

/// `cargo xt codeowners [--check] [paths…]`.
pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let text = std::fs::read_to_string(root.join(".github/CODEOWNERS"))
        .map_err(|e| format!(".github/CODEOWNERS: {e}"))?;
    let rules = parse(&text);
    let mut check = false;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            f if f.starts_with('-') => return Err(format!("unknown flag {f}")),
            p => paths.push(p),
        }
    }
    for p in &paths {
        println!("{p}: {}", owners(&rules, p).join(" "));
    }
    if check {
        let bad = problems(root, &text);
        if !bad.is_empty() {
            return Err(format!("CODEOWNERS check failed:\n  {}", bad.join("\n  ")));
        }
        println!(
            "codeowners ok: {} protected paths need exactly @aj47; no later rule overrides them",
            MUST_BE_AJ.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_style_matching() {
        assert!(matches("/.github/", ".github/workflows/ci.yml"));
        assert!(!matches("/.github/", "x/.github/a"));
        assert!(matches(
            "**/codec/ALLOWED_FIELDS.md",
            "crates/gmclip-media/src/codec/ALLOWED_FIELDS.md"
        ));
        assert!(matches("codec/", "crates/a/src/codec/x.rs"));
        assert!(matches("codec/", "codec/x.rs"));
        assert!(!matches("codec/", "crates/a/src/codecs/x.rs"));
        assert!(matches("codec.rs", "crates/gmclip-media/src/codec.rs"));
        assert!(matches(
            "*.entitlements",
            "app/src-tauri/gmclip.entitlements"
        ));
        assert!(matches("/Cargo.toml", "Cargo.toml"));
        assert!(!matches("/Cargo.toml", "crates/x/Cargo.toml"));
        // GitHub: `docs/*` does not reach nested files; a plain directory name does.
        assert!(matches("docs/*", "docs/a.md"));
        assert!(!matches("docs/*", "docs/a/b.md"));
        assert!(matches("/xtask/fixtures", "xtask/fixtures/a/b"));
    }

    #[test]
    fn the_last_matching_rule_wins() {
        let rules = parse("/a/ @x\n/a/b/ @y # comment\n/c/ #@z\n");
        assert_eq!(owners(&rules, "a/b/c"), ["@y"]);
        assert_eq!(owners(&rules, "a/c"), ["@x"]);
        assert!(owners(&rules, "c/x").is_empty(), "an owner list ends at #");
        assert!(owners(&rules, "d").is_empty());
    }

    fn committed() -> String {
        std::fs::read_to_string(crate::repo_root().join(".github/CODEOWNERS")).unwrap()
    }

    /// A-04 acceptance, statically: a PR touching anything under codec/ (any crate, any
    /// layout) needs AJ alone, and so does every other protected path.
    #[test]
    fn the_committed_codeowners_passes_the_check() {
        let bad = problems(&crate::repo_root(), &committed());
        assert!(bad.is_empty(), "{bad:#?}");
        let rules = parse(&committed());
        for p in [
            "crates/gmclip-media/src/decode.rs",
            "crates/gmclip-plan/src/lib.rs",
            "README.md",
        ] {
            assert!(owners(&rules, p).is_empty(), "{p}");
        }
    }

    /// Edits that would quietly remove AJ's gate are caught.
    #[test]
    fn the_check_catches_bypasses() {
        let root = crate::repo_root();
        for appended in [
            "/crates/gmclip-media/src/codec/hevc/ @gmclip-bot",
            "/crates/gmclip-media/src/codec/av1.rs",
            "/crates/ @gmclip-bot",
            "codec/ @aj47 @gmclip-bot",
            "**/vp9.rs @gmclip-bot",
        ] {
            let text = format!("{}\n{appended}\n", committed());
            assert!(!problems(&root, &text).is_empty(), "not caught: {appended}");
        }
        let star = committed().replace("codec/ ", "codec/* ");
        assert!(
            !problems(&root, &star).is_empty(),
            "`codec/*` leaves subdirectories open"
        );
    }

    #[test]
    fn the_pr_template_carries_the_d3_checklist() {
        let t =
            std::fs::read_to_string(crate::repo_root().join(".github/pull_request_template.md"))
                .unwrap();
        for must in [
            "decoded or encoded",
            "AAC feature off",
            "No ffmpeg",
            "ALLOWED_FIELDS.md",
            "cargo vet",
            "Needs AJ",
            "codeowners --check",
        ] {
            assert!(t.contains(must), "{must}");
        }
    }
}
