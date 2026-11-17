//! gmclip data model (SPEC §4).
//!
//! No I/O and no async: everything here is plain data plus pure functions, so the
//! model, store and core build and test on every OS with no GPU (R-ARCH-1).
//!
//! - [`entities`]: every SPEC §4.3 kind, `#[derive(Entity)]`, serde + schemars.
//! - [`entity`]: the `Entity` trait, field paths (`#[entity(map)]` keys), field values
//!   as canonical CBOR ([`cbor`], RFC 8949 §4.2.1).
//! - [`op`]: field triples, create/delete, transactions; [`doc`]: the `imbl` document
//!   with O(1) clone and the children index; [`staging`]: a transaction being built.
//! - [`effective`]: the §4.3.1 fold; [`edit`]: split/merge/reorder/delete/trim/link.
//! - [`anchor`], [`clock`] (skeleton), [`time`] (Lane A), [`geometry`], [`order`].
#![forbid(unsafe_code)]

// The derive emits `::gmclip_model::…` paths; this makes them resolve inside the crate.
extern crate self as gmclip_model;

pub mod anchor;
pub mod cbor;
pub mod clock;
pub mod doc;
pub mod edit;
pub mod effective;
pub mod entities;
pub mod entity;
pub mod fixtures;
pub mod geometry;
pub mod ids;
pub mod kinds;
pub mod lab;
pub mod op;
pub mod order;
pub mod rec;
mod schema_ext;
pub mod staging;
pub mod time;

/// The model's schema epoch (R-MODEL-3). Bumped by every model change a store must
/// record as a history epoch; the store writes "epoch k start" when it first opens a
/// project older than this. 1 = decision 0011 (`Cut.kind`, `Topic`, `FluffProposal`,
/// `lineage.topics`), all additive: older rows and ops keep their exact bytes.
/// 2 = decision 0015 (`Comment`, `EditProposal`, `Thumbnail`), additive the same way.
/// 3 = decisions 0023 and 0024, one epoch: `Opening` and `CopyDraft` added (0024);
/// `Moment.delivery`, the `mined` and `import` moment origins and the `import` payoff
/// source removed, the `rt_story_v5` caption grouping renamed `story_v5` (0023, the
/// greenfield decision). Not additive; nothing older than it was ever deployed.
/// 4 = decision 0034 (`Window`, `Cut.kind: assembly`), additive.
/// 5 = decision 0036: `Window` and `Cut.kind: assembly` removed (the stream edit is the
/// main cut, holding every video). Not additive; a project with a project timeline does
/// not open (none was ever deployed).
/// 6 = decision 0046: `Section` (zoom and layout sections of a cut), additive.
pub const MODEL_EPOCH: u32 = 6;

pub use doc::Doc;
pub use effective::{ResolvedStack, effective};
pub use entity::{Entity, FieldDesc, FieldId, FieldPath, FieldValue, ModelError};
pub use ids::{EntityId, EntityKind, EventId, Id, RequestId, Rev, RunId, TxnId, Ulid};
pub use kinds::{KINDS, Stored};
pub use op::{Author, CommandKey, Op, ReadRef, Txn};
pub use order::OrderKey;
pub use staging::{MintId, Staging};
pub use time::{Flicks, Rational, Rounding, rescale};
