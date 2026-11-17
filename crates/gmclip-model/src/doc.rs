//! The document (SPEC §4.4; implementer Part C): one `imbl::OrdMap` per entity kind plus
//! derived indexes, so `Doc::clone` is O(1) (a doc version for jobs, drafts and
//! restores) and iteration order is deterministic.
//!
//! Every entity carries the `rev` of the transaction that last wrote it and that
//! transaction's author (R-CC-1, R-CMD-6 `Conflict.by`). The children index keeps, per
//! `(owner, child kind)`, the children in `OrderKey` order (then id), with its own rev,
//! bumped whenever membership or order changes: an index read records that rev.

use std::sync::Arc;

use imbl::{OrdMap, OrdSet};

use crate::entity::{FieldPath, FieldValue, ModelError};
use crate::ids::{EntityId, EntityKind, Id, Rev, Ulid};
use crate::kinds::{KINDS, Stored, Tables, kind_ops};
use crate::op::{Author, Op, Txn};
use crate::order::OrderKey;

/// One stored entity with its last-write rev and author.
#[derive(Debug)]
pub struct Slot<E> {
    pub rev: Rev,
    pub by: Author,
    pub e: Arc<E>,
}

impl<E> Clone for Slot<E> {
    fn clone(&self) -> Self {
        Self {
            rev: self.rev,
            by: self.by,
            e: Arc::clone(&self.e),
        }
    }
}

impl<E: PartialEq> PartialEq for Slot<E> {
    fn eq(&self, o: &Self) -> bool {
        self.rev == o.rev && self.by == o.by && (Arc::ptr_eq(&self.e, &o.e) || self.e == o.e)
    }
}

type ChildKey = (EntityId, EntityKind);
type ChildEntry = (Option<OrderKey>, Ulid);

/// A project document at one revision.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Doc {
    rev: Rev,
    pub(crate) tables: Tables,
    children: OrdMap<ChildKey, OrdSet<ChildEntry>>,
    index_revs: OrdMap<ChildKey, Rev>,
}

impl Doc {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rev of the last transaction applied (0 = empty document).
    pub fn rev(&self) -> Rev {
        self.rev
    }

    pub fn get<E: Stored>(&self, id: Id<E>) -> Option<&E> {
        E::table(&self.tables).get(&id.0).map(|s| &*s.e)
    }

    pub fn get_arc<E: Stored>(&self, id: Id<E>) -> Option<Arc<E>> {
        E::table(&self.tables).get(&id.0).map(|s| Arc::clone(&s.e))
    }

    pub fn slot<E: Stored>(&self, id: Id<E>) -> Option<&Slot<E>> {
        E::table(&self.tables).get(&id.0)
    }

    /// Every entity of kind `E`, in id order.
    pub fn iter<E: Stored>(&self) -> impl Iterator<Item = &E> + '_ {
        E::table(&self.tables).values().map(|s| &*s.e)
    }

    pub fn count<E: Stored>(&self) -> usize {
        E::table(&self.tables).len()
    }

    /// Total entities of every kind.
    pub fn len(&self) -> usize {
        KINDS.iter().map(|k| k.len(self)).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, e: EntityId) -> bool {
        kind_ops(e.kind).is_some_and(|k| k.meta(self, e.id).is_some())
    }

    pub fn entity_rev(&self, e: EntityId) -> Option<Rev> {
        kind_ops(e.kind)?.meta(self, e.id).map(|(r, _)| r)
    }

    pub fn last_author(&self, e: EntityId) -> Option<Author> {
        kind_ops(e.kind)?.meta(self, e.id).map(|(_, a)| a)
    }

    pub fn parent_of(&self, e: EntityId) -> Option<EntityId> {
        kind_ops(e.kind)?.parent(self, e.id)
    }

    /// The entity's canonical CBOR.
    pub fn encode(&self, e: EntityId) -> Option<FieldValue> {
        kind_ops(e.kind)?.encode(self, e.id)
    }

    /// One field (or map key) of an entity, as canonical CBOR.
    pub fn field(&self, e: EntityId, path: &FieldPath) -> Result<FieldValue, ModelError> {
        kind_ops(e.kind)
            .ok_or(ModelError::UnknownKind)?
            .field(self, e.id, path)
    }

    /// Children of `E` under `owner`, in `OrderKey` order (then id).
    pub fn children<E: Stored>(&self, owner: EntityId) -> impl Iterator<Item = Id<E>> + '_ {
        self.children
            .get(&(owner, E::KIND))
            .into_iter()
            .flat_map(|s| s.iter().map(|(_, id)| Id::new(*id)))
    }

    /// Children of `E` under `owner`, with their order keys.
    pub fn children_keyed<E: Stored>(
        &self,
        owner: EntityId,
    ) -> impl Iterator<Item = (Option<&OrderKey>, Id<E>)> + '_ {
        self.children
            .get(&(owner, E::KIND))
            .into_iter()
            .flat_map(|s| s.iter().map(|(k, id)| (k.as_ref(), Id::new(*id))))
    }

    /// The rev of the last membership or order change of `kind` children under `owner`
    /// (0 if never).
    pub fn index_rev(&self, owner: EntityId, kind: EntityKind) -> Rev {
        self.index_revs.get(&(owner, kind)).copied().unwrap_or(0)
    }

    /// Every entity id in the document, kind by kind (deterministic order).
    pub fn entity_ids(&self) -> Vec<EntityId> {
        KINDS
            .iter()
            .flat_map(|k| {
                let kind = k.kind();
                k.ids(self).into_iter().map(move |id| EntityId { kind, id })
            })
            .collect()
    }

    /// Applies a committed transaction (actor and replay only; never the UI). Atomic:
    /// on error the document is unchanged. Every `Set` checks its `before` value and
    /// every `Delete` the entity's bytes, so replay detects divergence.
    pub fn apply(&mut self, t: &Txn) -> Result<(), ModelError> {
        if t.rev != self.rev + 1 {
            return Err(ModelError::RevOrder {
                doc: self.rev,
                got: t.rev,
            });
        }
        let mut next = self.clone();
        next.apply_ops(&t.ops, t.rev, t.author)?;
        // no transaction may leave an entity whose parent is gone
        if let Some(e) = next.orphaning_deletes(&t.ops).first() {
            let _ = e;
            return Err(ModelError::Invalid("a delete leaves children behind"));
        }
        next.rev = t.rev;
        *self = next;
        Ok(())
    }

    /// Whether any entity is indexed under `e` as its parent.
    pub fn has_children(&self, e: EntityId) -> bool {
        self.children
            .range((e, EntityKind(""))..)
            .take_while(|((p, _), _)| *p == e)
            .any(|(_, set)| !set.is_empty())
    }

    /// Every entity indexed under `e` as its parent, of any kind.
    pub fn child_ids(&self, e: EntityId) -> Vec<EntityId> {
        self.children
            .range((e, EntityKind(""))..)
            .take_while(|((p, _), _)| *p == e)
            .flat_map(|((_, kind), set)| {
                set.iter().map(move |(_, id)| EntityId {
                    kind: *kind,
                    id: *id,
                })
            })
            .collect()
    }

    /// `e` and everything below it (children, their children…), deepest first: the
    /// order a subtree is deleted in.
    pub fn subtree(&self, e: EntityId) -> Vec<EntityId> {
        let mut out = Vec::new();
        let mut stack = vec![(e, false)];
        while let Some((x, expanded)) = stack.pop() {
            if expanded {
                out.push(x);
                continue;
            }
            stack.push((x, true));
            for c in self.child_ids(x) {
                stack.push((c, false));
            }
        }
        out
    }

    /// The entities `ops` delete that still have children in this document (after
    /// the ops were applied): applying such ops would orphan those children.
    pub fn orphaning_deletes(&self, ops: &[Op]) -> Vec<EntityId> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Delete { e, .. } if self.has_children(*e) => Some(*e),
                _ => None,
            })
            .collect()
    }

    /// Applies ops at `rev` without moving the document rev (staging and history
    /// reconstruction). Not atomic on its own; callers work on a clone.
    pub fn apply_ops(&mut self, ops: &[Op], rev: Rev, by: Author) -> Result<(), ModelError> {
        for op in ops {
            kind_ops(op.entity().kind)
                .ok_or(ModelError::UnknownKind)?
                .apply_op(self, op, rev, by)?;
        }
        Ok(())
    }

    /// The net ops taking `self` to `to` for the entities in `ids`.
    pub fn diff_to(&self, to: &Doc, ids: &[EntityId]) -> Vec<Op> {
        let mut out = Vec::new();
        for e in ids {
            if let Some(k) = kind_ops(e.kind) {
                k.diff(self, to, e.id, &mut out);
            }
        }
        out
    }

    /// Storage: sets the rev after loading entity snapshot rows.
    pub fn set_rev(&mut self, rev: Rev) {
        self.rev = rev;
    }

    /// Storage: inserts an entity from an entity snapshot row.
    pub fn load_row(
        &mut self,
        kind: EntityKind,
        id: Ulid,
        rev: Rev,
        by: Author,
        bytes: &[u8],
    ) -> Result<(), ModelError> {
        kind_ops(kind)
            .ok_or(ModelError::UnknownKind)?
            .load(self, id, rev, by, bytes)
    }

    /// Storage: the children-index revs, for the entity snapshot rows (so a reopened
    /// document detects the same index conflicts as the one that wrote them).
    pub fn index_revs(&self) -> Vec<(EntityId, EntityKind, Rev)> {
        self.index_revs
            .iter()
            .map(|((e, k), r)| (*e, *k, *r))
            .collect()
    }

    /// Storage: replaces the children-index revs after loading entity snapshot rows.
    pub fn restore_index_revs(
        &mut self,
        revs: impl IntoIterator<Item = (EntityId, EntityKind, Rev)>,
    ) {
        self.index_revs = revs.into_iter().map(|(e, k, r)| ((e, k), r)).collect();
    }

    pub(crate) fn put_entity<E: Stored>(&mut self, e: Arc<E>, rev: Rev, by: Author) {
        let id = e.id().0;
        let new_parent = e.parent();
        let new_order = e.order_key().cloned();
        let old = E::table(&self.tables).get(&id).map(|s| Arc::clone(&s.e));
        let mut reindex = true;
        if let Some(old) = &old {
            let (op, oo) = (old.parent(), old.order_key().cloned());
            if op == new_parent && oo == new_order {
                reindex = false;
            } else if let Some(p) = op {
                self.unindex(p, E::KIND, oo, id, rev);
            }
        }
        if reindex && let Some(p) = new_parent {
            let key = (p, E::KIND);
            let mut set = self.children.get(&key).cloned().unwrap_or_default();
            set.insert((new_order, id));
            self.children.insert(key, set);
            self.index_revs.insert(key, rev);
        }
        E::table_mut(&mut self.tables).insert(id, Slot { rev, by, e });
    }

    pub(crate) fn remove_entity<E: Stored>(&mut self, id: Id<E>, rev: Rev) {
        if let Some(old) = E::table_mut(&mut self.tables).remove(&id.0)
            && let Some(p) = old.e.parent()
        {
            let oo = old.e.order_key().cloned();
            self.unindex(p, E::KIND, oo, id.0, rev);
        }
    }

    fn unindex(
        &mut self,
        parent: EntityId,
        kind: EntityKind,
        order: Option<OrderKey>,
        id: Ulid,
        rev: Rev,
    ) {
        let key = (parent, kind);
        if let Some(set) = self.children.get(&key) {
            let mut set = set.clone();
            set.remove(&(order, id));
            if set.is_empty() {
                self.children.remove(&key);
            } else {
                self.children.insert(key, set);
            }
        }
        self.index_revs.insert(key, rev);
    }

    /// Inserts or replaces an entity directly (fixtures and tests; commands go through
    /// [`crate::staging::Staging`] so the change is an op).
    pub fn insert_for_test<E: Stored>(&mut self, e: E) {
        let rev = self.rev;
        self.put_entity(Arc::new(e), rev, Author::System);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;

    #[test]
    fn clone_is_structural_and_indexes_follow_order() {
        let mut d = Doc::new();
        let cut = cut(1);
        d.insert_for_test(cut.clone());
        let keys = OrderKey::sequence(None, 3).unwrap();
        for (i, k) in keys.iter().enumerate().rev() {
            d.insert_for_test(part(10 + i as u128, cut.id, k.clone(), 0, 10));
        }
        let ids: Vec<_> = d
            .children::<crate::entities::Part>(cut.id.entity())
            .collect();
        assert_eq!(
            ids,
            vec![Id::new(Ulid(10)), Id::new(Ulid(11)), Id::new(Ulid(12))]
        );
        let snapshot = d.clone();
        d.insert_for_test(part(
            13,
            cut.id,
            OrderKey::between(None, Some(&keys[0])).unwrap(),
            0,
            1,
        ));
        assert_eq!(
            snapshot
                .children::<crate::entities::Part>(cut.id.entity())
                .count(),
            3
        );
        assert_eq!(
            d.children::<crate::entities::Part>(cut.id.entity()).next(),
            Some(Id::new(Ulid(13)))
        );
    }
}
