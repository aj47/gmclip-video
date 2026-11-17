//! Where the lab tests find their media and fixtures (tests and benches only).
//!
//! Lab media is AJ's own footage, never committed: the folder `GMCLIP_LAB_MEDIA` names
//! (default `~/Development/gmclip-lab-media`) holds symlinks to the files the lab tests
//! read, made by `tools/lab_media.sh`. Every lab media path in a test goes through
//! [`media`]. Fixtures written by gmclip itself (RenderPlans, reference numbers) are
//! committed under `lab/fixtures/`. The frozen stream projects carry whole transcripts,
//! so they live with the media they describe ([`project`]), not in the repository.
#![doc(hidden)]

use std::path::{Path, PathBuf};

/// The lab media folder: `GMCLIP_LAB_MEDIA`, else `~/Development/gmclip-lab-media`.
pub fn root() -> PathBuf {
    if let Some(p) = std::env::var_os("GMCLIP_LAB_MEDIA").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Development/gmclip-lab-media")
}

/// A file in the lab media folder (`rel` is relative to [`root`]).
pub fn media(rel: impl AsRef<Path>) -> PathBuf {
    root().join(rel)
}

/// [`media`] as a string, for tests that pass paths as JSON.
pub fn media_str(rel: impl AsRef<Path>) -> String {
    media(rel).to_string_lossy().into_owned()
}

/// Whether every named lab file is on this machine.
pub fn has(rels: &[&str]) -> bool {
    rels.iter().all(|r| media(r).exists())
}

/// How a lab path is named in a test's output: relative to the lab media folder when it
/// is inside it, else as given.
pub fn name(path: impl AsRef<Path>) -> String {
    let p = path.as_ref();
    p.strip_prefix(root())
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

/// The first of `paths` that is not on this machine. Each is relative to [`root`] or
/// absolute; a link whose target is gone counts as missing.
pub fn first_missing<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> Option<PathBuf> {
    paths.into_iter().map(media).find(|p| !p.exists())
}

/// The one line a lab test prints when it skips for a missing source.
pub fn skip_line(path: impl AsRef<Path>) -> String {
    format!("skipped: lab source {} not on this machine", name(path))
}

/// When one of `paths` is not on this machine, prints [`skip_line`] for it and returns
/// true: the test returns there. Only a missing file skips; everything else still fails.
pub fn skip_without<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> bool {
    match first_missing(paths) {
        Some(p) => {
            eprintln!("{}", skip_line(p));
            true
        }
        None => false,
    }
}

/// The part of a lab set whose sources are on this machine ([`present`]).
#[derive(Debug)]
pub struct Present<T> {
    /// The items whose every source is here, in the set's order.
    pub items: Vec<T>,
    /// How many the full set has.
    pub total: usize,
    /// Each left-out item's name and the first of its sources that is missing.
    pub missing: Vec<(String, String)>,
}

impl<T> Present<T> {
    /// Whether the whole set is here.
    pub fn complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// "all 11 golden cuts", or "7 of 11 golden cuts (not on this machine: streams/…,
    /// for loc_bd89f38b_h41, loc_bd89f38b_h43, …)": the subset a measurement covered, for
    /// a lab whose acceptance number was defined on the full set.
    pub fn note(&self, what: &str) -> String {
        if self.complete() {
            return format!("all {} {what}", self.total);
        }
        // one entry per missing source, naming the items that need it
        let mut by_src: Vec<(&str, Vec<&str>)> = Vec::new();
        for (n, src) in &self.missing {
            match by_src.iter_mut().find(|(s, _)| *s == src.as_str()) {
                Some((_, names)) => names.push(n),
                None => by_src.push((src, vec![n])),
            }
        }
        let gone: Vec<String> = by_src
            .iter()
            .map(|(src, names)| format!("{src}, for {}", names.join(", ")))
            .collect();
        format!(
            "{} of {} {what} (not on this machine: {})",
            self.items.len(),
            self.total,
            gone.join("; ")
        )
    }

    /// When none of the set is here, prints [`skip_line`] for the first missing source
    /// and returns true.
    pub fn skip_if_empty(&self) -> bool {
        if !self.items.is_empty() {
            return false;
        }
        let src = self
            .missing
            .first()
            .map_or("(none listed)", |m| m.1.as_str());
        eprintln!("{}", skip_line(src));
        true
    }
}

/// Splits a lab set into the items whose `sources` are all on this machine and the ones
/// left out (named by `name`, with the source that is missing). A lab that can measure a
/// subset runs on [`Present::items`] and reports [`Present::note`].
pub fn present<T, P: AsRef<Path>>(
    items: impl IntoIterator<Item = T>,
    name_of: impl Fn(&T) -> String,
    sources: impl Fn(&T) -> Vec<P>,
) -> Present<T> {
    let mut out = Present {
        items: Vec::new(),
        total: 0,
        missing: Vec::new(),
    };
    for it in items {
        out.total += 1;
        match first_missing(sources(&it)) {
            Some(p) => out.missing.push((name_of(&it), name(p))),
            None => out.items.push(it),
        }
    }
    out
}

/// A committed fixture under the repo's `lab/fixtures/`.
pub fn fixture(rel: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lab/fixtures")
        .join(rel)
}

/// A frozen stream project, `fixtures/projects/<name>.json` in the lab media folder.
pub fn project(name: &str) -> PathBuf {
    root()
        .join("fixtures/projects")
        .join(format!("{name}.json"))
}

/// The repo's `schemas/examples/<name>`.
pub fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/examples")
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_splits_into_what_is_here_and_what_is_not() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let gone = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-lab-source.mp4");
        assert_eq!(first_missing([&here]), None);
        assert_eq!(first_missing([&here, &gone]), Some(gone.clone()));
        let set = present(
            [
                ("a", here.clone()),
                ("b", gone.clone()),
                ("c", here.clone()),
            ],
            |x| x.0.to_owned(),
            |x| vec![x.1.clone()],
        );
        assert_eq!(set.items.len(), 2);
        assert!(!set.complete() && !set.skip_if_empty());
        let note = set.note("cuts");
        assert!(
            note.starts_with("2 of 3 cuts (not on this machine: "),
            "{note}"
        );
        assert!(note.ends_with("no-such-lab-source.mp4, for b)"), "{note}");
        let two = present(
            [("a", gone.clone()), ("b", gone.clone())],
            |x| x.0.to_owned(),
            |x| vec![x.1.clone()],
        );
        assert!(
            two.note("cuts")
                .ends_with("no-such-lab-source.mp4, for a, b)")
        );
        let none = present([gone.clone()], |_| "x".into(), |p| vec![p.clone()]);
        assert!(none.skip_if_empty());
        assert_eq!(
            present([&here], |_| "x".into(), |p| vec![*p]).note("cuts"),
            "all 1 cuts"
        );
        assert!(skip_line(&gone).starts_with("skipped: lab source "));
        assert!(skip_line(&gone).ends_with("no-such-lab-source.mp4 not on this machine"));
    }
}
