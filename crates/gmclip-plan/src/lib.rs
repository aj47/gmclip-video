//! RenderPlan (SPEC §3.1, §6.5; `spec/renderplan-v0.md`): the integer-only, output-bound
//! description of one export.
//!
//! - [`types`]: one serde + schemars type per schema object (`deny_unknown_fields`).
//! - [`schema`]: the committed `schemas/renderplan@0.json` (equal to RP §2.2) and an
//!   evaluator for it, which gives every structural error a JSON Pointer.
//! - [`validate`]: RP §2.1's rules the schema cannot say (tiling, sample telescoping,
//!   aspect, fades at continuous joins, referential integrity, unique ids …).
//!
//! [`parse`] runs all three: strict JSON (document order, duplicate keys refused) →
//! schema → typed plan → rules. Every refusal is a [`PlanError`] with a path.
//!
//! No I/O: callers hand this crate bytes. Dependency direction:
//! `plan ← {render, media, core, cli}` (`cargo xt deps`).
#![forbid(unsafe_code)]

pub mod anim;
pub mod consts;
pub mod error;
pub mod json;
pub mod pattern;
pub mod retime;
pub mod scalars;
pub mod schema;
pub mod seam;
pub mod story_cues;
pub mod subtitles;
pub mod types;
pub mod validate;

pub use error::PlanError;
pub use types::Plan;

/// The `schema` value of every v0 plan.
pub const SCHEMA_V0: &str = "gmclip.renderplan/0";

/// The `schema` value of a v1 plan (decision 0014).
pub const SCHEMA_V1: &str = "gmclip.renderplan/1";

/// [`check`] reports at most this many errors (the rest are counted in `more`).
pub const MAX_ERRORS: usize = 100;

/// What [`check`] found.
#[derive(Debug)]
pub struct Report {
    /// The typed plan, when it got past the schema pass.
    pub plan: Option<Plan>,
    /// The violations: schema errors in document order, or (if there were none) the
    /// rule violations, `Invalid` before `Unsupported`. At most [`MAX_ERRORS`]. Empty
    /// means valid.
    pub errors: Vec<PlanError>,
    /// How many more violations there were past [`MAX_ERRORS`].
    pub more: usize,
}

impl Report {
    fn new(plan: Option<Plan>, mut errors: Vec<PlanError>) -> Self {
        let more = errors.len().saturating_sub(MAX_ERRORS);
        errors.truncate(MAX_ERRORS);
        Self { plan, errors, more }
    }
}

/// Parses and validates `bytes`, reporting every violation found.
///
/// Schema errors stop the run before typing (a plan that breaks the schema has no
/// meaningful rule checks); otherwise the typed plan is returned with the rule errors.
pub fn check(bytes: &[u8]) -> Report {
    let doc = match json::parse(bytes) {
        Ok(doc) => doc,
        Err(e) => return Report::new(None, vec![e]),
    };
    let errors = schema::check(&doc);
    if !errors.is_empty() {
        return Report::new(None, errors);
    }
    let plan: Plan = match serde_json::from_slice(bytes) {
        Ok(p) => p,
        // The schema pass accepted it, so the types and the committed schema disagree:
        // a bug here, reported without echoing serde's message (which quotes input).
        Err(e) => {
            return Report::new(
                None,
                vec![PlanError::invalid(
                    "",
                    format!(
                        "conforms to the schema but not to the plan types (line {}, column {}); \
                         this is a gmclip-plan bug",
                        e.line(),
                        e.column()
                    ),
                )],
            );
        }
    };
    let errors = validate::rules(&plan);
    Report::new(Some(plan), errors)
}

/// Validates a `Plan` built in code (a converter, the M1 planner) exactly as a file is
/// validated: it is serialised and run through [`check`], so the schema's bounds apply
/// too. Empty means valid.
pub fn check_plan(plan: &Plan) -> Vec<PlanError> {
    match serde_json::to_vec(plan) {
        Ok(bytes) => check(&bytes).errors,
        Err(_) => vec![PlanError::invalid(
            "",
            "does not serialise (a non-finite number?)",
        )],
    }
}

/// Parses and validates `bytes`: the plan, or the first violation in document order.
pub fn parse(bytes: &[u8]) -> Result<Plan, PlanError> {
    let mut r = check(bytes);
    match (r.plan, r.errors.is_empty()) {
        (Some(p), true) => Ok(p),
        _ => Err(r.errors.swap_remove(0)),
    }
}
