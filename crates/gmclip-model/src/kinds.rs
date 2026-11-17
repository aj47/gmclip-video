//! The closed list of entity kinds and per-kind dispatch.
//!
//! `kinds!` generates, from one list: the `Tables` struct (one `imbl::OrdMap` per kind,
//! so `Doc::clone` is O(1) and iteration is deterministic), the [`Stored`] trait that
//! maps a type to its table, and [`KINDS`], the type-erased per-kind operations used
//! wherever only an [`EntityKind`] is known (op apply, storage rows, undo diffs).

use std::marker::PhantomData;
use std::sync::Arc;

use imbl::OrdMap;

use crate::doc::{Doc, Slot};
use crate::entities::*;
use crate::entity::{Entity, FieldChange, FieldDesc, FieldPath, FieldValue, ModelError};
use crate::ids::{EntityId, EntityKind, Id, Rev, Ulid};
use crate::op::{Author, Op};

/// An entity kind with a table in the [`Doc`].
pub trait Stored: Entity {
    #[doc(hidden)]
    fn table(t: &Tables) -> &OrdMap<Ulid, Slot<Self>>;
    #[doc(hidden)]
    fn table_mut(t: &mut Tables) -> &mut OrdMap<Ulid, Slot<Self>>;
}

macro_rules! kinds {
    ($($field:ident : $ty:ty),* $(,)?) => {
        /// One persistent map per entity kind.
        #[doc(hidden)]
        #[derive(Clone, Default, PartialEq, Debug)]
        pub struct Tables {
            $(pub(crate) $field: OrdMap<Ulid, Slot<$ty>>,)*
        }

        $(impl Stored for $ty {
            fn table(t: &Tables) -> &OrdMap<Ulid, Slot<Self>> {
                &t.$field
            }
            fn table_mut(t: &mut Tables) -> &mut OrdMap<Ulid, Slot<Self>> {
                &mut t.$field
            }
        })*

        /// Every kind, in declaration order.
        pub static KINDS: &[&dyn ErasedKind] = &[$(&KindOps::<$ty>(PhantomData),)*];
    };
}

kinds! {
    project: Project,
    caption_corrections: CaptionCorrections,
    terms: Terms,
    keep_pairs: KeepPairs,
    styles: Styles,
    caption_presets: CaptionPresets,
    style_library: StyleLibrary,
    brand_kit: BrandKit,
    asset: Asset,
    font: Font,
    source: Source,
    region: Region,
    sync_group: SyncGroup,
    sync_member: SyncMember,
    sync_segment: SyncSegment,
    stream_layout: StreamLayout,
    moment: Moment,
    cut: Cut,
    story_suggestion: StorySuggestion,
    review: Review,
    part: Part,
    visual_split: VisualSplit,
    content_hold: ContentHold,
    hide_roles: HideRoles,
    framing: Framing,
    layout: Layout,
    layer: Layer,
    overlay: Overlay,
    keyframe: Keyframe,
    audio_overlay: AudioOverlay,
    gain_segment: GainSegment,
    caption_doc: CaptionDoc,
    cue: Cue,
    clip: Clip,
    measurement: Measurement,
    topic: Topic,
    fluff_proposal: FluffProposal,
    comment: Comment,
    edit_proposal: EditProposal,
    thumbnail: Thumbnail,
    opening: Opening,
    copy_draft: CopyDraft,
    section: Section,
}

/// The kind with this name, if any.
pub fn kind_by_name(name: &str) -> Option<EntityKind> {
    KINDS.iter().map(|k| k.kind()).find(|k| k.0 == name)
}

/// Type-erased operations for the kind `kind`.
pub fn kind_ops(kind: EntityKind) -> Option<&'static dyn ErasedKind> {
    KINDS.iter().copied().find(|k| k.kind().0 == kind.0)
}

/// Per-kind operations behind an [`EntityKind`] (object-safe).
pub trait ErasedKind: Sync {
    fn kind(&self) -> EntityKind;
    fn fields(&self) -> &'static [FieldDesc];
    /// Applies one op of this kind at `rev` by `by`.
    fn apply_op(&self, doc: &mut Doc, op: &Op, rev: Rev, by: Author) -> Result<(), ModelError>;
    /// The entity's canonical CBOR.
    fn encode(&self, doc: &Doc, id: Ulid) -> Option<FieldValue>;
    fn field(&self, doc: &Doc, id: Ulid, path: &FieldPath) -> Result<FieldValue, ModelError>;
    /// `v` decoded as entity `id`'s field at `path` and encoded again: the canonical
    /// bytes a `Set` op needs, or why `v` is not a value of that field.
    fn canonical_field(
        &self,
        doc: &Doc,
        id: Ulid,
        path: &FieldPath,
        v: &FieldValue,
    ) -> Result<FieldValue, ModelError>;
    fn meta(&self, doc: &Doc, id: Ulid) -> Option<(Rev, Author)>;
    fn parent(&self, doc: &Doc, id: Ulid) -> Option<EntityId>;
    fn ids(&self, doc: &Doc) -> Vec<Ulid>;
    fn len(&self, doc: &Doc) -> usize;
    /// Inserts an entity from storage (entity snapshot rows), bypassing the op path.
    fn load(
        &self,
        doc: &mut Doc,
        id: Ulid,
        rev: Rev,
        by: Author,
        bytes: &[u8],
    ) -> Result<(), ModelError>;
    /// The net ops taking entity `id` from its state in `from` to its state in `to`.
    fn diff(&self, from: &Doc, to: &Doc, id: Ulid, out: &mut Vec<Op>);
}

struct KindOps<E>(PhantomData<fn() -> E>);

impl<E: Stored> ErasedKind for KindOps<E> {
    fn kind(&self) -> EntityKind {
        E::KIND
    }

    fn fields(&self) -> &'static [FieldDesc] {
        E::fields()
    }

    fn apply_op(&self, doc: &mut Doc, op: &Op, rev: Rev, by: Author) -> Result<(), ModelError> {
        let id = Id::<E>::new(op.entity().id);
        match op {
            Op::Create { value, .. } => {
                if doc.get(id).is_some() {
                    return Err(ModelError::Exists);
                }
                let e: E = crate::cbor::from_slice(&value.0)?;
                // Only canonical bytes: otherwise the inverse Delete (which compares
                // bytes) could not apply. Catches unknown keys and non-preferred floats.
                if e.id() != id || crate::cbor::to_vec(&e) != value.0 {
                    return Err(ModelError::Mismatch);
                }
                doc.put_entity(Arc::new(e), rev, by);
                Ok(())
            }
            Op::Delete { value, .. } => {
                let cur = doc.get(id).ok_or(ModelError::NotFound)?;
                if crate::cbor::to_vec(cur) != value.0 {
                    return Err(ModelError::Mismatch);
                }
                doc.remove_entity(id, rev);
                Ok(())
            }
            Op::Set {
                f, before, after, ..
            } => {
                let cur = doc.get(id).ok_or(ModelError::NotFound)?;
                if cur.get_field(f)? != *before {
                    return Err(ModelError::Mismatch);
                }
                let mut next = cur.clone();
                next.set_field(f, after)?;
                // `after` must be the canonical form, or the inverse Set would not match.
                if next.get_field(f)? != *after {
                    return Err(ModelError::Mismatch);
                }
                doc.put_entity(Arc::new(next), rev, by);
                Ok(())
            }
        }
    }

    fn encode(&self, doc: &Doc, id: Ulid) -> Option<FieldValue> {
        doc.get(Id::<E>::new(id))
            .map(|e| FieldValue(crate::cbor::to_vec(e)))
    }

    fn field(&self, doc: &Doc, id: Ulid, path: &FieldPath) -> Result<FieldValue, ModelError> {
        doc.get(Id::<E>::new(id))
            .ok_or(ModelError::NotFound)?
            .get_field(path)
    }

    fn canonical_field(
        &self,
        doc: &Doc,
        id: Ulid,
        path: &FieldPath,
        v: &FieldValue,
    ) -> Result<FieldValue, ModelError> {
        let mut e = doc
            .get(Id::<E>::new(id))
            .ok_or(ModelError::NotFound)?
            .clone();
        e.set_field(path, v)?;
        e.get_field(path)
    }

    fn meta(&self, doc: &Doc, id: Ulid) -> Option<(Rev, Author)> {
        doc.slot(Id::<E>::new(id)).map(|s| (s.rev, s.by))
    }

    fn parent(&self, doc: &Doc, id: Ulid) -> Option<EntityId> {
        doc.get(Id::<E>::new(id)).and_then(Entity::parent)
    }

    fn ids(&self, doc: &Doc) -> Vec<Ulid> {
        E::table(&doc.tables).keys().copied().collect()
    }

    fn len(&self, doc: &Doc) -> usize {
        E::table(&doc.tables).len()
    }

    fn load(
        &self,
        doc: &mut Doc,
        id: Ulid,
        rev: Rev,
        by: Author,
        bytes: &[u8],
    ) -> Result<(), ModelError> {
        let e: E = crate::cbor::from_slice(bytes)?;
        if e.id().0 != id || crate::cbor::to_vec(&e) != bytes {
            return Err(ModelError::Mismatch);
        }
        doc.put_entity(Arc::new(e), rev, by);
        Ok(())
    }

    fn diff(&self, from: &Doc, to: &Doc, id: Ulid, out: &mut Vec<Op>) {
        let tid = Id::<E>::new(id);
        let eid = tid.entity();
        match (from.get(tid), to.get(tid)) {
            (None, None) => {}
            (None, Some(b)) => out.push(Op::Create {
                e: eid,
                value: FieldValue(crate::cbor::to_vec(b)),
            }),
            (Some(a), None) => out.push(Op::Delete {
                e: eid,
                value: FieldValue(crate::cbor::to_vec(a)),
            }),
            (Some(a), Some(b)) => {
                if a == b {
                    return;
                }
                let mut changes: Vec<FieldChange> = Vec::new();
                a.diff(b, &mut changes);
                out.extend(changes.into_iter().map(|c| Op::Set {
                    e: eid,
                    f: c.path,
                    before: c.before,
                    after: c.after,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_are_unique_and_resolve() {
        let mut names: Vec<&str> = KINDS.iter().map(|k| k.kind().0).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate kind names");
        assert!(n >= 20, "A02: derive used by ≥ 20 kinds, got {n}");
        for k in KINDS {
            assert_eq!(kind_by_name(k.kind().0), Some(k.kind()));
        }
        assert_eq!(kind_by_name("nope"), None);
    }

    #[test]
    fn every_kind_has_a_schema_and_unique_field_names() {
        for k in KINDS {
            let mut names: Vec<&str> = k.fields().iter().map(|f| f.name).collect();
            let n = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), n, "{}", k.kind().0);
            for (i, f) in k.fields().iter().enumerate() {
                assert_eq!(usize::from(f.id.0), i);
            }
        }
    }
}
