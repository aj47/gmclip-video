//! The M1 commands on the model: the 15 hot commands of R-CMD-8 (strike, restore,
//! trim, split, merge, reorder, set gain, caption edit, layout set, overlay move, crop
//! set, place set, word-edge nudge, keyword toggle, visual-split add), the creates they
//! need, and the history commands (undo/redo, runs, restore points), which the actor
//! runs itself because they read history rather than the document.
//!
//! Times on these commands are source times in flicks (`at_src` values, SPEC §4.3); the
//! anchor-based agent vocabulary (`cut_edit`, A24) maps onto them.

pub mod analysis;
pub mod brand;
pub mod caption;
pub mod comment;
pub mod compose;
pub mod cursor;
pub mod cut;
pub mod edit;
pub mod handoff;
pub mod history;
pub mod hold;
pub mod library;
pub mod longform;
pub mod look;
pub mod marker;
pub mod matte;
pub mod moment;
pub mod opening;
pub mod overlay;
pub mod packaging;
pub mod part;
pub mod polish;
pub mod project;
pub mod review;
pub mod screenlook;
pub mod section;
pub mod sound;
pub mod source;
pub mod story;
pub mod stylelib;
pub mod text;
pub mod topic;
pub mod vocab;
pub mod words;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An empty result.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct Done {}

/// The id of something a command created.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Created {
    pub id: String,
}

/// Bounds a caller string (R-CMD-6: the error never quotes it).
pub(crate) fn bounded(s: &str, path: &str, max_chars: usize) -> Result<(), gmclip_proto::CmdError> {
    if s.chars().count() > max_chars {
        return Err(gmclip_proto::CmdError::invalid(path, "too long"));
    }
    Ok(())
}

/// Records that a structural edit on `part` reads its own layers and visual splits
/// (the model's edit helpers read them through `Staging`, unrecorded).
pub(crate) fn record_part_children(
    cx: &mut crate::command::Cx<'_>,
    part: gmclip_model::Id<gmclip_model::entities::Part>,
) {
    let _ = cx
        .tx
        .children::<gmclip_model::entities::Layer>(part.entity());
    let _ = cx
        .tx
        .children::<gmclip_model::entities::VisualSplit>(part.entity());
}

pub(crate) fn created<E>(id: gmclip_model::Id<E>) -> Created {
    Created {
        id: id.0.to_string(),
    }
}
