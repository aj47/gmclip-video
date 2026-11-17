//! Agent pass definitions (SPEC §10.3, §10.6; R-AI-5): each pass's prompt is a
//! versioned asset (`assets/passes/<name>.v<version>.md`), its hash is recorded on what
//! the pass produces (`pass_ref` = `<name>@<version>:<sha256 prefix>`), and it names
//! the commands a launched run of it may call.
//!
//! The pass runs through the user's own agent CLI and the ordinary MCP tools; gmclip
//! never calls a model here.

use sha2::{Digest, Sha256};

/// One pass: its prompt asset and the commands a run of it may call.
#[derive(Debug)]
pub struct PassDef {
    pub name: &'static str,
    pub version: u32,
    /// The prompt, with `{PASS_REF}` where the run's pass reference goes.
    pub prompt: &'static str,
    /// Mutating commands a launched run may call (reads are always allowed).
    pub commands: &'static [&'static str],
}

impl PassDef {
    /// SHA-256 of the prompt asset, hex.
    pub fn hash(&self) -> String {
        let d = Sha256::digest(self.prompt.as_bytes());
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// `<name>@<version>:<first 12 hex of the hash>`, recorded on the pass's outputs.
    pub fn pass_ref(&self) -> String {
        format!("{}@{}:{}", self.name, self.version, &self.hash()[..12])
    }

    /// The prompt a run is given (the asset up to its `<!-- steps -->` marker, if any).
    pub fn brief(&self) -> String {
        let text = self
            .prompt
            .split_once(STEPS_MARKER)
            .map_or(self.prompt, |(b, _)| b.trim_end());
        text.replace("{PASS_REF}", &self.pass_ref())
    }

    /// A step's instructions: the asset's `## <name>` section after the steps marker
    /// (gmclip serves them one step at a time, SPEC §10.6).
    pub fn step(&self, name: &str) -> Option<&'static str> {
        let (_, steps) = self.prompt.split_once(STEPS_MARKER)?;
        let head = format!("## {name}\n");
        let i = steps.find(&head)? + head.len();
        let rest = &steps[i..];
        Some(rest[..rest.find("\n## ").unwrap_or(rest.len())].trim())
    }
}

/// Where a pass asset's per-step instructions start.
const STEPS_MARKER: &str = "\n<!-- steps -->";

/// SPEC §10.3.1, §10.6 (A.01): find moments, agent-executed. The run speaks the pass
/// contract (`ai_pass` / `pass_step`, whose tools are not commands) and may run no
/// command but `run_end`: every moment goes through gmclip's checks.
pub static FIND_MOMENTS: PassDef = PassDef {
    name: "find_moments",
    version: 1,
    prompt: include_str!("../assets/passes/find_moments.v1.md"),
    commands: &["run_end"],
};

/// SPEC §10.3.2 (A.01): the full sweep, agent-executed, the same way.
pub static FULL_SWEEP: PassDef = PassDef {
    name: "full_sweep",
    version: 1,
    prompt: include_str!("../assets/passes/full_sweep.v1.md"),
    commands: &["run_end"],
};

/// Decision 0011: topics and fluff on the stream edit, proposed only.
pub static TOPICS_FLUFF: PassDef = PassDef {
    name: "topics_fluff",
    version: 1,
    prompt: include_str!("../assets/passes/topics_fluff.v1.md"),
    commands: &[
        "topic_set",
        "topic_split",
        "topic_merge",
        "topic_state",
        "topic_delete",
        "fluff_propose",
        "fluff_state",
        "run_end",
    ],
};

/// Decision 0015: open comments on a cut as tasks for the user's agent. It edits this
/// cut like an edit pass (no command limit beyond its scopes and the server's rules: it
/// cannot open or resolve threads, decide suggestions or pick a thumbnail) and replies in
/// each task's thread; `commands` names what it is expected to call.
pub static FIX_COMMENTS: PassDef = PassDef {
    name: "fix_comments",
    version: 1,
    prompt: include_str!("../assets/passes/fix_comments.v1.md"),
    commands: &["cut_edit", "layout_edit", "comment_add", "run_end"],
};

/// Decision 0015: a thumbnail version with three text-hook options, proposed only (a
/// launched run of it may write `thumbnail_add` and nothing else).
pub static THUMBNAIL: PassDef = PassDef {
    name: "thumbnail",
    version: 1,
    prompt: include_str!("../assets/passes/thumbnail.v1.md"),
    commands: &["thumbnail_add", "run_end"],
};

/// Decision 0024: a title and description per platform, proposed only. The run speaks
/// the pass contract (`ai_pass{pass: publish_copy}` / `pass_step`), which checks every
/// draft and writes it as a proposal; it may run no command but `run_end`. It never
/// publishes or approves anything.
pub static PUBLISH_COPY: PassDef = PassDef {
    name: "publish_copy",
    version: 1,
    prompt: include_str!("../assets/passes/publish_copy.v1.md"),
    commands: &["run_end"],
};

/// Every pass defined here.
pub static PASSES: &[&PassDef] = &[
    &TOPICS_FLUFF,
    &PUBLISH_COPY,
    &FIX_COMMENTS,
    &THUMBNAIL,
    &FIND_MOMENTS,
    &FULL_SWEEP,
];

/// The pass named `name`.
pub fn find(name: &str) -> Option<&'static PassDef> {
    PASSES.iter().copied().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_topics_pass_is_a_versioned_hashed_asset() {
        let p = find("topics_fluff").unwrap();
        assert_eq!(p.hash().len(), 64);
        let r = p.pass_ref();
        assert!(r.starts_with("topics_fluff@1:"), "{r}");
        let b = p.brief();
        assert!(!b.contains("{PASS_REF}"));
        assert!(b.contains(&r));
        // it only proposes: every command it may run exists, and none of them strikes
        for c in p.commands {
            assert!(crate::registry::find(c).is_some(), "{c}");
        }
        assert!(!p.commands.contains(&"words_strike"));
        assert!(!p.commands.contains(&"sweep_apply"));
    }

    #[test]
    fn the_collab_passes_are_versioned_and_name_real_commands() {
        for name in ["fix_comments", "thumbnail"] {
            let p = find(name).unwrap();
            assert!(p.pass_ref().starts_with(&format!("{name}@1:")));
            assert!(!p.brief().contains("{PASS_REF}"));
            for c in p.commands {
                assert!(crate::registry::find(c).is_some(), "{c}");
            }
            // neither may resolve, decide suggestions or pick
            for c in ["comment_resolve", "edit_proposal_state", "thumbnail_set"] {
                assert!(!p.commands.contains(&c));
            }
        }
        assert_eq!(THUMBNAIL.commands, &["thumbnail_add", "run_end"]);
    }

    #[test]
    fn the_copy_pass_proposes_through_the_contract_only() {
        let p = find("publish_copy").unwrap();
        assert!(p.pass_ref().starts_with("publish_copy@1:"));
        let b = p.brief();
        assert!(b.contains("ai_pass") && b.contains("pass_step") && b.contains("publish_copy"));
        assert!(!b.contains("<!-- steps -->"));
        let step = p.step("copy").unwrap();
        assert!(step.len() > 100 && !b.contains(step));
        assert_eq!(p.commands, &["run_end"]);
        for c in ["copy_drafts_add", "copy_draft_state", "send_to_clippy"] {
            assert!(!p.commands.contains(&c));
        }
    }

    #[test]
    fn the_moment_passes_serve_their_steps_apart_from_the_brief() {
        for (p, steps) in [
            (&FIND_MOMENTS, &["first", "sweep", "repair", "gap"][..]),
            (&FULL_SWEEP, &["full_sweep"][..]),
        ] {
            let b = p.brief();
            assert!(
                b.contains(&p.pass_ref()) && !b.contains("<!-- steps -->"),
                "{b}"
            );
            assert!(b.contains("ai_pass") && b.contains("pass_step"));
            for s in steps {
                let t = p.step(s).unwrap_or_else(|| panic!("{} {s}", p.name));
                assert!(t.len() > 100 && !t.contains("## "), "{s}: {t}");
                assert!(!b.contains(t), "a step is not in the brief");
            }
            assert!(p.step("nope").is_none());
            // the contract is the only way in: no moment command at all
            assert_eq!(p.commands, &["run_end"]);
        }
        assert!(TOPICS_FLUFF.step("first").is_none());
        assert_eq!(
            TOPICS_FLUFF.brief(),
            TOPICS_FLUFF
                .prompt
                .replace("{PASS_REF}", &TOPICS_FLUFF.pass_ref())
        );
    }
}
