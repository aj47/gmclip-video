//! `cargo xt deps`: the crate dependency direction, checked from `cargo metadata`.
//!
//! The rule (SPEC §3.1, implementer Part C as amended by SPEC v5):
//!
//! ```text
//! model ← store ← core ← {mcp, cli, app}
//! plan  ← {render, media, core, cli}
//! gpu   ← {media, render}
//! proto ← {core, mcp, cli, app}
//! analysis ← {core, ml, cli, app}   (pure data: model, serde, ciborium; decision 0056)
//! core never depends on render, media or gpu, directly or transitively (R-ARCH-1):
//! it reaches them only through the `Renderer` / `MediaHost` traits it defines.
//! ```
//!
//! It is enforced as an allowlist ([`ALLOWED`]): a workspace crate may depend (normal,
//! build or dev) only on the internal crates listed for it, and an internal crate not in
//! the table is an error, so a new crate cannot land without a rule. The `←` arrows above
//! are re-checked against the table in the unit tests. Changing the table is a layering
//! decision: record it in `decisions/` (SPEC §22).
//!
//! "Internal" means every workspace member and every path dependency, whatever its name,
//! plus anything named `gmclip-*`; each must have a row in [`ALLOWED`].
//!
//! The same pass checks the `unsafe` policy (SPEC §3.1). The compiler enforces it: the
//! workspace lints set `unsafe_code = "forbid"`, and this check requires every member
//! except gmclip-gpu to inherit them (`[lints] workspace = true`). As a visible
//! backstop, every lib / proc-macro / bin root except gmclip-gpu's also carries
//! `#![forbid(unsafe_code)]`.
//!
//! One scoped exception (decisions/0019, proposed): gmclip-platform holds the Windows
//! FFI (ACLs, named pipes, process identity). Its manifest sets `unsafe_code = "deny"`
//! itself (a scoped `allow` cannot lower a `forbid`), and its root must say
//! `#![cfg_attr(not(windows), forbid(unsafe_code))]`, so off Windows it is exactly as
//! forbidden as any other crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// The only crate allowed to contain `unsafe` (SPEC §3.1).
pub const UNSAFE_ALLOWED: &str = "gmclip-gpu";

/// The crate allowed `unsafe` on Windows only (decisions/0019), and the root attribute
/// that keeps it forbidden everywhere else.
pub const UNSAFE_ON_WINDOWS: &str = "gmclip-platform";
const WINDOWS_ONLY_FORBID: &str = "#![cfg_attr(not(windows), forbid(unsafe_code))]";

/// `(crate, internal crates it may depend on)`. `gmclip-app` is `app/src-tauri` (M2).
pub const ALLOWED: &[(&str, &[&str])] = &[
    ("gmclip-macros", &[]),
    ("gmclip-platform", &[]),
    ("gmclip-plan", &[]),
    ("gmclip-model", &["gmclip-macros"]),
    ("gmclip-store", &["gmclip-model", "gmclip-macros"]),
    ("gmclip-proto", &["gmclip-model"]),
    (
        "gmclip-core",
        &[
            "gmclip-model",
            "gmclip-macros",
            "gmclip-store",
            "gmclip-proto",
            "gmclip-plan",
            "gmclip-analysis",
        ],
    ),
    ("gmclip-gpu", &["gmclip-platform"]),
    (
        "gmclip-media",
        &[
            "gmclip-model",
            "gmclip-plan",
            "gmclip-gpu",
            "gmclip-platform",
        ],
    ),
    (
        "gmclip-render",
        &[
            "gmclip-model",
            "gmclip-plan",
            "gmclip-gpu",
            "gmclip-media",
            "gmclip-platform",
        ],
    ),
    ("gmclip-analysis", &["gmclip-model"]),
    (
        "gmclip-ml",
        &["gmclip-model", "gmclip-analysis", "gmclip-platform"],
    ),
    // gmclip-platform: the Windows run-dir ACLs and owner checks (decisions/0019).
    ("gmclip-ai", &["gmclip-model", "gmclip-platform"]),
    // Dev-only eval tooling (SPEC §17.1): drives the launcher; parses RenderPlans as plain
    // JSON so the grader stays independent of the code it grades (decisions/0006).
    ("gmclip-eval", &["gmclip-ai"]),
    (
        "gmclip-mcp",
        // gmclip-platform: the Windows named pipe and its peer checks (decisions/0019).
        &[
            "gmclip-model",
            "gmclip-proto",
            "gmclip-core",
            "gmclip-platform",
        ],
    ),
    (
        "gmclip-cli",
        &[
            "gmclip-model",
            "gmclip-proto",
            "gmclip-core",
            "gmclip-plan",
            "gmclip-mcp",
            "gmclip-media",
            "gmclip-render",
            "gmclip-analysis",
            "gmclip-ml",
            "gmclip-ai",
            "gmclip-platform",
        ],
    ),
    (
        "gmclip-app",
        &[
            "gmclip-model",
            "gmclip-proto",
            "gmclip-core",
            "gmclip-mcp",
            "gmclip-media",
            "gmclip-render",
            "gmclip-analysis",
            "gmclip-ml",
            "gmclip-ai",
            "gmclip-platform",
        ],
    ),
    // Repo tooling: depends on no gmclip crate, and nothing depends on it.
    ("xtask", &[]),
];

/// Crates whose whole internal dependency closure must stay free of rendering, media
/// and GPU, so they build and test on every OS with no GPU (R-ARCH-1).
const HEADLESS: &[&str] = &[
    "gmclip-model",
    "gmclip-store",
    "gmclip-proto",
    "gmclip-plan",
    "gmclip-core",
];
const NOT_IN_HEADLESS_CLOSURE: &[&str] = &["gmclip-render", "gmclip-media", "gmclip-gpu"];

/// External crates the headless crates may not depend on directly (name prefixes).
const NOT_IN_HEADLESS_EXTERNAL: &[&str] = &["wgpu", "tauri", "wry", "metal", "objc2"];

#[derive(Debug)]
struct Pkg {
    name: String,
    manifest_path: String,
    /// `(dependency name, kind, is a path dependency)`
    deps: Vec<(String, String, bool)>,
    /// Source files of lib / proc-macro / bin targets.
    roots: Vec<String>,
}

fn load(manifest: &Path) -> Result<Vec<Pkg>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed for {}:\n{}",
            manifest.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))?;
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut pkgs = Vec::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        if !members.contains(p["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let str_of = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        let deps = p["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                let kind = d["kind"].as_str().unwrap_or("normal").to_owned();
                (str_of(&d["name"]), kind, d["path"].is_string())
            })
            .collect();
        let roots = p["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["kind"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| matches!(k.as_str(), Some("lib" | "rlib" | "proc-macro" | "bin")))
            })
            .map(|t| str_of(&t["src_path"]))
            .collect();
        pkgs.push(Pkg {
            name: str_of(&p["name"]),
            manifest_path: str_of(&p["manifest_path"]),
            deps,
            roots,
        });
    }
    Ok(pkgs)
}

fn allowed_for(name: &str) -> Option<&'static [&'static str]> {
    ALLOWED.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
}

/// Does this manifest inherit the workspace lints (`[lints]` then `workspace = true`)?
fn inherits_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_lints = line == "[lints]";
        } else if in_lints && line.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    false
}

/// Does this manifest set its own `[lints.rust]` with `unsafe_code = "deny"` (and not
/// inherit the workspace's `forbid`)?
fn denies_unsafe_itself(manifest: &str) -> bool {
    let mut in_rust = false;
    let mut deny = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_rust = line == "[lints.rust]";
        } else if in_rust && line.replace(' ', "") == "unsafe_code=\"deny\"" {
            deny = true;
        }
    }
    deny && !inherits_workspace_lints(manifest)
}

/// Checks the workspace at `manifest`. `Ok` carries a one-line summary; `Err` lists
/// every violation, one per line.
pub fn check(manifest: &Path) -> Result<String, String> {
    let pkgs = load(manifest)?;
    let mut errors = Vec::new();
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut n_edges = 0;
    // Every member and every path dependency is internal, whatever its name.
    let internal: BTreeSet<&str> = pkgs
        .iter()
        .map(|p| p.name.as_str())
        .chain(
            pkgs.iter()
                .flat_map(|p| p.deps.iter())
                .filter(|(_, _, is_path)| *is_path)
                .map(|(n, _, _)| n.as_str()),
        )
        .collect();
    let is_internal =
        |name: &str| internal.contains(name) || name.starts_with("gmclip-") || name == "xtask";

    for p in &pkgs {
        if p.name == UNSAFE_ON_WINDOWS {
            match std::fs::read_to_string(&p.manifest_path) {
                Ok(m) if denies_unsafe_itself(&m) => {}
                Ok(_) => errors.push(format!(
                    "{}: Cargo.toml must set [lints.rust] unsafe_code = \"deny\" (decisions/0019)",
                    p.name
                )),
                Err(e) => errors.push(format!("{}: cannot read {}: {e}", p.name, p.manifest_path)),
            }
        } else if p.name != UNSAFE_ALLOWED {
            match std::fs::read_to_string(&p.manifest_path) {
                Ok(m) if inherits_workspace_lints(&m) => {}
                Ok(_) => errors.push(format!(
                    "{}: Cargo.toml must inherit the workspace lints ([lints] workspace = true), which forbid unsafe_code",
                    p.name
                )),
                Err(e) => errors.push(format!("{}: cannot read {}: {e}", p.name, p.manifest_path)),
            }
        }
        let allowed = allowed_for(&p.name);
        if allowed.is_none() {
            errors.push(format!(
                "unknown crate {}: add it to xtask/src/deps.rs ALLOWED (a layering decision)",
                p.name
            ));
        }
        for (dep, kind, is_path) in &p.deps {
            if dep == "xtask" {
                errors.push(format!("forbidden edge: {} -> xtask ({kind})", p.name));
                continue;
            }
            if !is_internal(dep) {
                if HEADLESS.contains(&p.name.as_str())
                    && NOT_IN_HEADLESS_EXTERNAL.iter().any(|x| dep.starts_with(x))
                {
                    errors.push(format!(
                        "forbidden dependency: {} -> {dep} ({kind}): headless crates build with no GPU or webview (R-ARCH-1)",
                        p.name
                    ));
                }
                continue;
            }
            if !is_path {
                // a registry/git crate impersonating an internal name
                errors.push(format!(
                    "{} -> {dep}: internal crates must be path dependencies",
                    p.name
                ));
            }
            n_edges += 1;
            edges
                .entry(p.name.as_str())
                .or_default()
                .insert(dep.as_str());
            if !allowed.is_some_and(|a| a.contains(&dep.as_str())) {
                errors.push(format!("forbidden edge: {} -> {dep} ({kind})", p.name));
            }
        }
        if p.name != UNSAFE_ALLOWED {
            let want = if p.name == UNSAFE_ON_WINDOWS {
                WINDOWS_ONLY_FORBID
            } else {
                "#![forbid(unsafe_code)]"
            };
            for root in &p.roots {
                match std::fs::read_to_string(root) {
                    Ok(src) if src.lines().any(|l| l.trim() == want) => {}
                    Ok(_) if p.name == UNSAFE_ON_WINDOWS => errors.push(format!(
                        "{}: crate root {root} lacks {WINDOWS_ONLY_FORBID} (decisions/0019)",
                        p.name
                    )),
                    Ok(_) => errors.push(format!(
                        "{}: crate root {root} lacks #![forbid(unsafe_code)] (only {UNSAFE_ALLOWED} may use unsafe)",
                        p.name
                    )),
                    Err(e) => errors.push(format!("{}: cannot read {root}: {e}", p.name)),
                }
            }
        }
    }

    // Transitive: the headless crates never reach render/media/gpu, by any path.
    for &h in HEADLESS {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<(&str, String)> = vec![(h, h.to_owned())];
        while let Some((at, path)) = stack.pop() {
            for &next in edges.get(at).into_iter().flatten() {
                if !seen.insert(next) {
                    continue;
                }
                let path = format!("{path} -> {next}");
                if NOT_IN_HEADLESS_CLOSURE.contains(&next) {
                    errors.push(format!("{h} reaches {next}: {path} (R-ARCH-1)"));
                }
                stack.push((next, path));
            }
        }
    }

    if errors.is_empty() {
        Ok(format!(
            "deps ok: {} crates, {n_edges} internal edges, unsafe only in {UNSAFE_ALLOWED} (and {UNSAFE_ON_WINDOWS} on Windows)",
            pkgs.len()
        ))
    } else {
        errors.sort();
        errors.dedup();
        let mut msg = format!("cargo xt deps: {} violation(s)\n", errors.len());
        for e in &errors {
            let _ = writeln!(msg, "  {e}");
        }
        Err(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_root;

    /// Crates that may depend on `target`, per the allowlist.
    fn dependents(target: &str) -> BTreeSet<&'static str> {
        ALLOWED
            .iter()
            .filter(|(_, a)| a.contains(&target))
            .map(|(n, _)| *n)
            .collect()
    }

    fn set<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
        names.iter().copied().collect()
    }

    #[test]
    fn allowlist_matches_the_spec_arrows() {
        let sub = |target: &str, allowed: &[&str]| {
            let d = dependents(target);
            assert!(
                d.is_subset(&set(allowed)),
                "{target} ← {d:?} exceeds {allowed:?}"
            );
        };
        sub("gmclip-store", &["gmclip-core"]);
        sub("gmclip-core", &["gmclip-mcp", "gmclip-cli", "gmclip-app"]);
        sub(
            "gmclip-plan",
            &["gmclip-render", "gmclip-media", "gmclip-core", "gmclip-cli"],
        );
        sub("gmclip-gpu", &["gmclip-media", "gmclip-render"]);
        sub(
            "gmclip-proto",
            &["gmclip-core", "gmclip-mcp", "gmclip-cli", "gmclip-app"],
        );
        sub("xtask", &[]);
        let core = allowed_for("gmclip-core").unwrap();
        for x in NOT_IN_HEADLESS_CLOSURE {
            assert!(!core.contains(x), "core may not depend on {x}");
        }
    }

    #[test]
    fn allowlist_is_acyclic() {
        fn visit(n: &str, stack: &mut Vec<String>) {
            assert!(!stack.iter().any(|s| s == n), "cycle: {stack:?} -> {n}");
            stack.push(n.to_owned());
            for d in allowed_for(n).unwrap_or(&[]) {
                visit(d, stack);
            }
            stack.pop();
        }
        for (n, _) in ALLOWED {
            visit(n, &mut Vec::new());
        }
    }

    #[test]
    fn the_windows_ffi_crate_must_deny_unsafe_itself() {
        let own = "[lints.rust]\nunsafe_code = \"deny\"\n[lints.clippy]\n";
        assert!(denies_unsafe_itself(own));
        assert!(!denies_unsafe_itself("[lints]\nworkspace = true\n"));
        assert!(!denies_unsafe_itself(
            "[lints.rust]\nunsafe_code = \"allow\"\n"
        ));
        assert!(!denies_unsafe_itself(&format!(
            "{own}[lints]\nworkspace = true\n"
        )));
    }

    #[test]
    fn the_workspace_passes() {
        let r = check(&repo_root().join("Cargo.toml"));
        assert!(r.is_ok(), "{}", r.unwrap_err());
    }

    #[test]
    fn a_crate_without_the_prefix_cannot_launder_an_edge() {
        let fixture = repo_root().join("xtask/fixtures/deps-laundered/Cargo.toml");
        let err = check(&fixture).expect_err("the fixture launders core -> render via `shim`");
        assert!(err.contains("unknown crate shim"), "{err}");
        assert!(err.contains("forbidden edge: gmclip-core -> shim"), "{err}");
        assert!(
            err.contains("gmclip-core reaches gmclip-render: gmclip-core -> shim -> gmclip-render"),
            "{err}"
        );
        assert!(
            err.contains("shim: Cargo.toml must inherit the workspace lints"),
            "{err}"
        );
    }

    #[test]
    fn lints_inheritance_is_parsed() {
        assert!(inherits_workspace_lints(
            "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n"
        ));
        assert!(!inherits_workspace_lints(
            "[lints]\n\n[x]\nworkspace = true\n"
        ));
        assert!(!inherits_workspace_lints(
            "[lints.rust]\nunsafe_code = \"allow\"\n"
        ));
    }

    #[test]
    fn a_core_to_render_edge_is_rejected() {
        let fixture = repo_root().join("xtask/fixtures/deps-core-render/Cargo.toml");
        let err = check(&fixture).expect_err("the fixture has a forbidden edge");
        assert!(
            err.contains("forbidden edge: gmclip-core -> gmclip-render (normal)"),
            "{err}"
        );
        assert!(err.contains("gmclip-core reaches gmclip-render"), "{err}");
    }
}
