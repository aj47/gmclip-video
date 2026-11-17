//! `cargo xt check-registry` (SPEC §3.2): the command registry is one explicit list.
//!
//! It parses (with syn) every Rust file of the gmclip crates and the `registry! { … }`
//! invocation in `crates/gmclip-core/src/registry.rs`, and fails when:
//! - a `#[command(...)]` impl is not listed, or a listed type has no `#[command]`;
//! - a command name does not match `^[a-z0-9_]{1,64}$`, or two commands share a name
//!   or a type name (types are matched by name, so names must be unambiguous);
//! - the list is not sorted by command name (the runtime looks names up by binary
//!   search);
//! - `#[command]` sits on anything but `impl Command for X` / `impl JobCommand for X`,
//!   or has no `name`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::{Expr, ExprLit, Item, Lit, Meta, Token};

/// Crates that never define gmclip commands. gmclip-cli is excluded because clap's
/// derive also uses a `#[command(...)]` attribute; gmclip-macros defines the macro.
const SKIP_CRATES: &[&str] = &["gmclip-cli", "gmclip-macros"];

#[derive(Debug, Clone)]
struct Annotated {
    ty: String,
    name: String,
    file: String,
}

fn valid_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn check(root: &Path) -> Result<String, String> {
    let reg_file = root.join("crates/gmclip-core/src/registry.rs");
    let src = std::fs::read_to_string(&reg_file)
        .map_err(|e| format!("cannot read {}: {e}", reg_file.display()))?;
    let listed = parse_registry(&src)?;
    let mut annotated = Vec::new();
    let mut errors = Vec::new();
    for file in rust_files(&root.join("crates"))? {
        if SKIP_CRATES
            .iter()
            .any(|c| file.components().any(|p| p.as_os_str() == *c))
        {
            continue;
        }
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .display()
            .to_string();
        match command_impls(&text, &rel) {
            Ok(mut v) => annotated.append(&mut v),
            Err(e) => errors.push(e),
        }
    }
    errors.extend(verify(&listed, &annotated));
    if errors.is_empty() {
        Ok(format!(
            "registry ok: {} command(s) listed, {} #[command] impl(s) found, sorted by name",
            listed.len(),
            annotated.len()
        ))
    } else {
        Err(format!(
            "cargo xt check-registry:\n  {}",
            errors.join("\n  ")
        ))
    }
}

fn verify(listed: &[String], annotated: &[Annotated]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut by_type: BTreeMap<&str, &Annotated> = BTreeMap::new();
    let mut names: BTreeMap<&str, &Annotated> = BTreeMap::new();
    for a in annotated {
        if !valid_name(&a.name) {
            errors.push(format!(
                "{}: command name {:?} does not match ^[a-z0-9_]{{1,64}}$",
                a.file, a.name
            ));
        }
        if let Some(prev) = names.insert(&a.name, a) {
            errors.push(format!(
                "command name {:?} used by {} and {}",
                a.name, prev.ty, a.ty
            ));
        }
        if let Some(prev) = by_type.insert(&a.ty, a) {
            errors.push(format!(
                "two #[command] types named {} ({} and {})",
                a.ty, prev.file, a.file
            ));
        }
    }
    let listed_types: Vec<&str> = listed.iter().map(|p| last_segment(p)).collect();
    let mut seen = BTreeSet::new();
    for t in &listed_types {
        if !seen.insert(*t) {
            errors.push(format!("registry lists {t} twice"));
        }
        if !by_type.contains_key(t) {
            errors.push(format!("registry lists {t}, which has no #[command] impl"));
        }
    }
    for a in annotated {
        if !seen.contains(a.ty.as_str()) {
            errors.push(format!(
                "#[command] {} ({:?}, {}) is not listed in registry! {{}}",
                a.ty, a.name, a.file
            ));
        }
    }
    let order: Vec<&str> = listed_types
        .iter()
        .filter_map(|t| by_type.get(t).map(|a| a.name.as_str()))
        .collect();
    if let Some(w) = order.windows(2).find(|w| w[0] >= w[1]) {
        errors.push(format!(
            "registry! is not sorted by command name: {:?} before {:?}",
            w[0], w[1]
        ));
    }
    errors
}

/// The type paths inside the top-level `registry! { … }` invocation (not the
/// `macro_rules!` definition).
fn parse_registry(src: &str) -> Result<Vec<String>, String> {
    let file = syn::parse_file(src).map_err(|e| format!("registry.rs does not parse: {e}"))?;
    let mut found = None;
    for item in &file.items {
        if let Item::Macro(m) = item
            && m.ident.is_none()
            && m.mac.path.is_ident("registry")
        {
            if found.is_some() {
                return Err("two registry! { … } invocations".into());
            }
            let paths = m
                .mac
                .parse_body_with(Punctuated::<syn::Path, Token![,]>::parse_terminated)
                .map_err(|e| format!("registry! body: {e}"))?;
            found = Some(
                paths
                    .iter()
                    .map(|p| {
                        p.segments
                            .iter()
                            .map(|s| s.ident.to_string())
                            .collect::<Vec<_>>()
                            .join("::")
                    })
                    .collect(),
            );
        }
    }
    found.ok_or_else(|| {
        "no top-level registry! { … } invocation in gmclip-core/src/registry.rs".into()
    })
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path).trim()
}

fn is_command_attr(a: &syn::Attribute) -> bool {
    a.path()
        .segments
        .last()
        .is_some_and(|s| s.ident == "command")
}

/// Every `#[command(...)]` impl in a file, with its name; nested inline modules too.
fn command_impls(src: &str, file: &str) -> Result<Vec<Annotated>, String> {
    let parsed = syn::parse_file(src).map_err(|e| format!("{file}: does not parse: {e}"))?;
    let mut out = Vec::new();
    let mut errors = Vec::new();
    walk(&parsed.items, file, &mut out, &mut errors);
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors.join("\n  "))
    }
}

fn walk(items: &[Item], file: &str, out: &mut Vec<Annotated>, errors: &mut Vec<String>) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    walk(inner, file, out, errors);
                }
            }
            Item::Impl(imp) => {
                let Some(attr) = imp.attrs.iter().find(|a| is_command_attr(a)) else {
                    continue;
                };
                let is_command = imp
                    .trait_
                    .as_ref()
                    .and_then(|(p, _)| p.segments.last())
                    .is_some_and(|s| s.ident == "Command" || s.ident == "JobCommand");
                let ty = match &*imp.self_ty {
                    syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
                    _ => None,
                };
                let (Some(ty), true) = (ty, is_command) else {
                    errors.push(format!(
                        "{file}: #[command] must be on `impl Command for <Type>`"
                    ));
                    continue;
                };
                match command_name(attr) {
                    Some(name) => out.push(Annotated {
                        ty,
                        name,
                        file: file.to_owned(),
                    }),
                    None => errors.push(format!("{file}: #[command] on {ty} has no name = \"…\"")),
                }
            }
            other => {
                if item_attrs(other).iter().any(is_command_attr) {
                    errors.push(format!(
                        "{file}: #[command] must be on `impl Command for <Type>`"
                    ));
                }
            }
        }
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Struct(s) => &s.attrs,
        Item::Enum(e) => &e.attrs,
        Item::Fn(f) => &f.attrs,
        _ => &[],
    }
}

fn command_name(attr: &syn::Attribute) -> Option<String> {
    let metas = attr
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()?;
    metas.iter().find_map(|m| match m {
        Meta::NameValue(nv) if nv.path.is_ident("name") => match &nv.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn rust_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_passes() {
        let r = check(&crate::repo_root());
        assert!(r.is_ok(), "{}", r.unwrap_err());
        assert!(r.unwrap().contains("command(s) listed"));
    }

    const CMD: &str = r##"
        #[command(name = "part_trim", scope = "edit", undo = "undoable", doc = "d", example = r#"{}"#, ui(label = "Trim", menu = "Edit"))]
        impl Command for PartTrim { }
        mod inner {
            #[gmclip_macros::command(
                name = "part_split",
                scope = "edit",
            )]
            impl gmclip_core::Command for PartSplit { }
        }
    "##;

    fn annotated() -> Vec<Annotated> {
        command_impls(CMD, "x.rs").unwrap()
    }

    #[test]
    fn parses_registry_and_impls() {
        let src = "macro_rules! registry { () => {} }\nregistry! {\n    crate::cmds::PartSplit,\n    crate::cmds::PartTrim,\n}\n";
        assert_eq!(
            parse_registry(src).unwrap(),
            ["crate::cmds::PartSplit", "crate::cmds::PartTrim"]
        );
        let a = annotated();
        assert_eq!(
            a.iter()
                .map(|a| (a.ty.as_str(), a.name.as_str()))
                .collect::<Vec<_>>(),
            [("PartTrim", "part_trim"), ("PartSplit", "part_split")]
        );
    }

    #[test]
    fn every_failure_mode_is_caught() {
        let a = annotated();
        let ok = vec!["c::PartSplit".to_owned(), "c::PartTrim".to_owned()];
        assert!(verify(&ok, &a).is_empty(), "{:?}", verify(&ok, &a));
        // not listed
        let e = verify(&ok[..1], &a);
        assert!(
            e.iter()
                .any(|e| e.contains("PartTrim") && e.contains("not listed")),
            "{e:?}"
        );
        // listed without #[command]
        let mut extra = ok.clone();
        extra.push("c::Ghost".into());
        assert!(verify(&extra, &a).iter().any(|e| e.contains("Ghost")));
        // unsorted
        let rev = vec![ok[1].clone(), ok[0].clone()];
        assert!(verify(&rev, &a).iter().any(|e| e.contains("not sorted")));
        // listed twice
        let twice = vec![ok[0].clone(), ok[0].clone(), ok[1].clone()];
        assert!(verify(&twice, &a).iter().any(|e| e.contains("twice")));
        // bad and duplicate names
        let mut bad = a.clone();
        bad[0].name = "Part-Trim".into();
        assert!(
            verify(&ok, &bad)
                .iter()
                .any(|e| e.contains("does not match"))
        );
        let mut dup = a.clone();
        dup[1].name = "part_trim".into();
        assert!(verify(&ok, &dup).iter().any(|e| e.contains("used by")));
        // #[command] on a struct, or with no name
        assert!(command_impls("#[command(name = \"x\")]\nstruct X;", "f.rs").is_err());
        assert!(
            command_impls(
                "#[command(scope = \"edit\")]\nimpl Command for X {}",
                "f.rs"
            )
            .is_err()
        );
    }
}
