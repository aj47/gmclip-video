//! Recording markers (decision 0052): the times ⌃⌥M was pressed while recording, kept on
//! each recorded file's source (`Source.markers`, on that file's clock) and shown where
//! a cut plays them.
//!
//! - `source_markers_set`: the import stores a session's markers on its files.
//! - `marker_list`: a cut's markers on its clock, one per part that plays it (a part of
//!   a recording's sync group reads them through its members' offsets). `cut_get` and the
//!   timeline's ruler show the same list.

use gmclip_macros::command;
use gmclip_model::clock::{self, FLICKS_PER_MS};
use gmclip_model::entities::*;
use gmclip_model::{Doc, Flicks, Id};
use gmclip_proto::CmdError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::command::{Command, Cx};

/// Markers one source may hold.
pub const MAX_MARKERS: usize = 10_000;
/// The latest marker time, ms (24 h).
const MAX_MS: i64 = 24 * 3600 * 1000;

/// Rounds flicks to the nearest ms.
fn to_ms(f: i64) -> i64 {
    (f + FLICKS_PER_MS / 2).div_euclid(FLICKS_PER_MS)
}

/// One marker where a cut plays it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CutMarker {
    /// Which marker of the recording (1 = the first pressed).
    pub n: u32,
    /// Where the cut plays it, ms on the cut's clock.
    pub cut_ms: i64,
    /// The part that plays it.
    pub part: String,
    /// The recorded file it is on, and its time there (ms).
    pub source: String,
    pub src_ms: i64,
}

/// The markers `cut` plays, by cut time: each once per part that plays it.
pub fn cut_markers(doc: &Doc, cut: Id<Cut>) -> Vec<CutMarker> {
    let mut out = Vec::new();
    for span in clock::cut_layout(doc, cut) {
        let Some(p) = doc.get(span.part) else {
            continue;
        };
        // the part's sources, with each one's time → the part's media clock
        let sources: Vec<(Id<Source>, Option<Id<SyncGroup>>)> = match p.media {
            MediaRef::Source(s) => vec![(s, None)],
            MediaRef::Group(g) => doc
                .children::<SyncMember>(g.entity())
                .filter_map(|m| doc.get(m))
                .map(|m| (m.source, Some(g)))
                .collect(),
        };
        let first = out.len();
        for (s, g) in sources {
            let Some(src) = doc.get(s) else { continue };
            for (i, t) in src.markers.iter().enumerate() {
                let n = i as u32 + 1;
                // a recording's files share its markers: one per part
                if out[first..].iter().any(|m: &CutMarker| m.n == n) {
                    continue;
                }
                let tm = match g {
                    None => *t,
                    Some(g) => match crate::anchors::member_to_group(doc, g, s, *t) {
                        Some(tm) => tm,
                        None => continue,
                    },
                };
                if tm < p.src_in || tm >= p.src_out {
                    continue;
                }
                let local = clock::src_to_local(p, Flicks(tm.0 - p.src_in.0));
                out.push(CutMarker {
                    n,
                    cut_ms: to_ms(span.start.0 + local.0),
                    part: p.id.0.to_string(),
                    source: s.0.to_string(),
                    src_ms: to_ms(t.0),
                });
            }
        }
    }
    out.sort_by_key(|m| (m.cut_ms, m.n));
    out
}

// ---------------------------------------------------------------------------------------
// source_markers_set
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceMarkersSetArgs {
    pub source: Id<Source>,
    /// The markers' times on the file's clock, ms, in the order they were pressed
    /// (empty: none).
    pub markers_ms: Vec<i64>,
}

#[derive(Debug)]
pub struct SourceMarkersSet;

#[command(
    name = "source_markers_set",
    scope = "edit",
    undo = "undoable",
    doc = "Set the markers dropped while recording (⌃⌥M, decision 0052) on a recorded video, as ms on its own clock in the order they were pressed; opening a recording sets them. marker_list and cut_get show where a cut plays them.",
    example = r#"{"source": "01J00000000000000000000002", "markers_ms": [4200, 61000]}"#
)]
impl Command for SourceMarkersSet {
    type Args = SourceMarkersSetArgs;
    type Output = super::Done;
    fn run(a: SourceMarkersSetArgs, cx: &mut Cx<'_>) -> Result<super::Done, CmdError> {
        cx.tx.get(a.source)?;
        if a.markers_ms.len() > MAX_MARKERS {
            return Err(CmdError::invalid("/markers_ms", "at most 10000 markers"));
        }
        for (i, ms) in a.markers_ms.iter().enumerate() {
            if !(0..=MAX_MS).contains(ms) || (i > 0 && *ms < a.markers_ms[i - 1]) {
                return Err(CmdError::invalid(
                    &format!("/markers_ms/{i}"),
                    "ms from 0 to 24 h, in order",
                ));
            }
        }
        let markers: Vec<Flicks> = a
            .markers_ms
            .iter()
            .map(|m| clock::ms_to_flicks(*m))
            .collect();
        cx.tx.update(a.source, |s| s.markers = markers)?;
        Ok(super::Done {})
    }
}

// ---------------------------------------------------------------------------------------
// marker_list
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkerListArgs {
    pub cut: Id<Cut>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MarkersListed {
    /// By cut time.
    pub markers: Vec<CutMarker>,
}

#[derive(Debug)]
pub struct MarkerList;

#[command(
    name = "marker_list",
    scope = "read",
    undo = "read_only",
    doc = "List the recording markers (⌃⌥M while recording, decision 0052) a cut plays, by cut time: n (the n-th marker of the recording), cut_ms, the part that plays it, and the recorded video and its time there (src_ms). A marker the cut does not play is not listed; one played by two parts is listed twice.",
    example = r#"{"cut": "01J00000000000000000000001"}"#
)]
impl Command for MarkerList {
    type Args = MarkerListArgs;
    type Output = MarkersListed;
    fn run(a: MarkerListArgs, cx: &mut Cx<'_>) -> Result<MarkersListed, CmdError> {
        cx.tx.get(a.cut)?;
        let _ = cx.tx.children::<Part>(a.cut.entity());
        Ok(MarkersListed {
            markers: cut_markers(cx.tx.peek(), a.cut),
        })
    }
}
