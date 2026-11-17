//! Anchors (SPEC §4.1.3): how agent surfaces and stored time points name a time.
//! Types only; resolution against analysis data is gmclip-core's (R-ANC-1/2, A15).
//! [`crate::effective`] resolves the subset that needs no analysis (word fingerprints,
//! part/cut edges, raw times) through [`crate::clock`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::clock::ModelTime;
use crate::entities::{Cut, Measurement, Part, Source};
use crate::entity::MapKey;
use crate::ids::{EventId, Id};
use crate::time::Flicks;

/// A word of one ASR revision (R-MODEL-4): `(asr_revision, ordinal)`.
#[derive(
    Clone, Eq, Ord, PartialEq, PartialOrd, Hash, Debug, Serialize, Deserialize, JsonSchema,
)]
pub struct WordId {
    pub asr_revision: String,
    pub ordinal: u32,
}

impl MapKey for WordId {
    /// `<asr_revision>#<ordinal>`; the revision may itself contain `#`, so the ordinal
    /// is taken after the last one.
    fn to_key(&self) -> String {
        format!("{}#{}", self.asr_revision, self.ordinal)
    }
    fn from_key(s: &str) -> Option<Self> {
        let (rev, ord) = s.rsplit_once('#')?;
        let ordinal: u32 = ord.parse().ok()?;
        // canonical spelling only (no "+1", no leading zeros)
        (ordinal.to_string() == ord).then(|| WordId {
            asr_revision: rev.to_owned(),
            ordinal,
        })
    }
}

/// A word reference with its fingerprint (R-MODEL-4), so it can be re-anchored when
/// `active_asr` changes and resolved to source times without the transcript.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WordRef {
    pub word_id: WordId,
    pub t_src_start: Flicks,
    pub t_src_end: Flicks,
    pub normalised_text: String,
    #[serde(default)]
    pub prev2: Vec<String>,
    #[serde(default)]
    pub next2: Vec<String>,
}

/// A transcript line of one ASR revision.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LineRef {
    pub asr_revision: String,
    pub line: u32,
}

/// A source frame by presentation index (SPEC §4.1.1).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FrameRef {
    pub source: Id<Source>,
    pub frame_index: i64,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CutSide {
    Before,
    After,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CutRule {
    EnergyMin,
    Arbiter,
}

/// A part or cut edge plus an optional offset in integer ms.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PartEdge {
    pub part: Id<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CutEdge {
    pub cut: Id<Cut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CutPoint {
    pub word: WordRef,
    pub side: CutSide,
    pub rule: CutRule,
}

/// A free time with the reason it is not anchored (linted, R-ANC-1).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RawTime {
    pub time: ModelTime,
    pub reason: String,
}

/// A time point on an agent surface or in the model (SPEC §4.1.3). Durations are
/// integer ms; nothing stores free seconds.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    WordStart(WordRef),
    WordEnd(WordRef),
    AudibleStart(WordRef),
    AudibleEnd(WordRef),
    LineStart(LineRef),
    LineEnd(LineRef),
    CutPoint(CutPoint),
    PartStart(PartEdge),
    PartEnd(PartEdge),
    CutStart(CutEdge),
    CutEnd(CutEdge),
    Measured(Id<Measurement>),
    VisualEvent(EventId),
    Frame(FrameRef),
    Raw(RawTime),
}

impl Anchor {
    /// The part this anchor names, if it is a part edge.
    pub fn part(&self) -> Option<Id<Part>> {
        match self {
            Anchor::PartStart(e) | Anchor::PartEnd(e) => Some(e.part),
            _ => None,
        }
    }
}

/// How a stored time came to be (R-ANC-1): `raw` times and agent-typed offsets are
/// linted; `import` (VES) is not.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeProvenance {
    Anchored,
    Measured,
    Grounded,
    Raw,
    Import,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_ids_are_map_keys() {
        let w = WordId {
            asr_revision: "import:a#b".into(),
            ordinal: 42,
        };
        assert_eq!(w.to_key(), "import:a#b#42");
        assert_eq!(WordId::from_key(&w.to_key()), Some(w));
        assert_eq!(WordId::from_key("x#042"), None);
        assert_eq!(WordId::from_key("x#+1"), None);
        assert_eq!(WordId::from_key("x"), None);
    }

    #[test]
    fn anchors_serialise_tagged() {
        let a = Anchor::PartEnd(PartEdge {
            part: Id::new(crate::Ulid(7)),
            offset_ms: Some(-40),
        });
        let j = serde_json::to_value(&a).unwrap();
        assert_eq!(j["part_end"]["offset_ms"], -40);
        let back: Anchor = serde_json::from_value(j).unwrap();
        assert_eq!(back, a);
    }
}
