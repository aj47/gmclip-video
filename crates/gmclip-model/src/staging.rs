//! A transaction being built: a working copy of the document plus the set of entities
//! touched, in first-touch order. The ops are the **net** diff from the base document
//! to the working copy for those entities, so a create-then-update is one `Create`, an
//! update back to the original value is nothing, and the ops always apply to the base.
//!
//! gmclip-core's `Tx` wraps this and adds read-set tracking; the model's edit helpers
//! ([`crate::edit`]) take a `Staging` so they are testable without the actor.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::doc::Doc;
use crate::entity::ModelError;
use crate::ids::{EntityId, Id, Rev, Ulid};
use crate::kinds::Stored;
use crate::op::{Author, Op};

/// Mints ULIDs for new entities (R-MODEL-1: from the actor's deterministic services;
/// the minted id is stored in the forward op, so replay never mints).
pub trait MintId {
    fn mint(&mut self) -> Ulid;
}

impl<F: FnMut() -> Ulid> MintId for F {
    fn mint(&mut self) -> Ulid {
        self()
    }
}

/// See the module docs.
pub struct Staging<'m> {
    base: Doc,
    work: Doc,
    touched: Vec<EntityId>,
    touched_set: BTreeSet<EntityId>,
    rev: Rev,
    by: Author,
    mint: &'m mut dyn MintId,
}

impl core::fmt::Debug for Staging<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Staging")
            .field("base_rev", &self.base.rev())
            .field("touched", &self.touched.len())
            .finish()
    }
}

impl<'m> Staging<'m> {
    pub fn new(base: Doc, by: Author, mint: &'m mut dyn MintId) -> Self {
        let rev = base.rev() + 1;
        Self {
            work: base.clone(),
            base,
            touched: Vec::new(),
            touched_set: BTreeSet::new(),
            rev,
            by,
            mint,
        }
    }

    /// The document the transaction starts from.
    pub fn base(&self) -> &Doc {
        &self.base
    }

    /// The working copy (reads see earlier writes of the same transaction).
    pub fn doc(&self) -> &Doc {
        &self.work
    }

    pub fn author(&self) -> Author {
        self.by
    }

    /// A fresh id (R-MODEL-1).
    pub fn mint<E>(&mut self) -> Id<E> {
        Id::new(self.mint.mint())
    }

    pub fn get<E: Stored>(&self, id: Id<E>) -> Option<&E> {
        self.work.get(id)
    }

    fn touch(&mut self, e: EntityId) {
        if self.touched_set.insert(e) {
            self.touched.push(e);
        }
    }

    /// Creates an entity; its id must be new.
    pub fn create<E: Stored>(&mut self, e: E) -> Result<Id<E>, ModelError> {
        let id = e.id();
        if self.work.get(id).is_some() {
            return Err(ModelError::Exists);
        }
        self.touch(id.entity());
        self.work.put_entity(Arc::new(e), self.rev, self.by);
        Ok(id)
    }

    /// Replaces an existing entity wholesale (same id).
    pub fn replace<E: Stored>(&mut self, e: E) -> Result<(), ModelError> {
        let id = e.id();
        if self.work.get(id).is_none() {
            return Err(ModelError::NotFound);
        }
        self.touch(id.entity());
        self.work.put_entity(Arc::new(e), self.rev, self.by);
        Ok(())
    }

    /// Clones the entity, runs `f`, and stores the result. The id cannot change.
    pub fn update<E: Stored, R>(
        &mut self,
        id: Id<E>,
        f: impl FnOnce(&mut E) -> R,
    ) -> Result<R, ModelError> {
        let mut e = self.work.get(id).ok_or(ModelError::NotFound)?.clone();
        let r = f(&mut e);
        if e.id() != id {
            return Err(ModelError::Invalid("an update cannot change the id"));
        }
        self.touch(id.entity());
        self.work.put_entity(Arc::new(e), self.rev, self.by);
        Ok(r)
    }

    /// Deletes an entity, returning its last value.
    pub fn delete<E: Stored>(&mut self, id: Id<E>) -> Result<E, ModelError> {
        let e = self.work.get(id).ok_or(ModelError::NotFound)?.clone();
        self.touch(id.entity());
        self.work.remove_entity(id, self.rev);
        Ok(e)
    }

    /// Applies raw ops (undo inverses, restores) to the working copy.
    pub fn apply_ops(&mut self, ops: &[Op]) -> Result<(), ModelError> {
        let mut next = self.work.clone();
        next.apply_ops(ops, self.rev, self.by)?;
        for op in ops {
            self.touch(op.entity());
        }
        self.work = next;
        Ok(())
    }

    /// Entities touched so far, in first-touch order.
    pub fn touched(&self) -> &[EntityId] {
        &self.touched
    }

    /// The net ops from the base to the working copy.
    pub fn ops(&self) -> Vec<Op> {
        self.base.diff_to(&self.work, &self.touched)
    }

    pub fn into_work(self) -> Doc {
        self.work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Part;
    use crate::fixtures::*;
    use crate::op::{CommandKey, Txn};
    use crate::order::OrderKey;

    #[test]
    fn net_ops_apply_to_the_base_and_reach_the_working_copy() {
        let mut d = Doc::new();
        let c = cut(1);
        d.insert_for_test(c.clone());
        let p = part(2, c.id, OrderKey::first(), 0, 100);
        d.insert_for_test(p.clone());
        let mut n = 100u128;
        let mut mint = || {
            n += 1;
            Ulid(n)
        };
        let mut s = Staging::new(d.clone(), Author::Human, &mut mint);
        // create then update: one Create
        let new_id = s.mint::<Part>();
        let mut q = part(
            0,
            c.id,
            OrderKey::between(Some(&p.order), None).unwrap(),
            100,
            200,
        );
        q.id = new_id;
        s.create(q).unwrap();
        s.update(new_id, |q| q.gain_db = 3.0).unwrap();
        // update and back: nothing
        s.update(p.id, |p| p.gain_db = 1.0).unwrap();
        s.update(p.id, |p| p.gain_db = 0.0).unwrap();
        // a real change
        s.update(p.id, |p| p.src_out = crate::time::Flicks(90))
            .unwrap();
        let ops = s.ops();
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(matches!(ops[0], Op::Create { .. }));
        assert!(matches!(ops[1], Op::Set { .. }));
        let work = s.into_work();
        let mut applied = d.clone();
        applied
            .apply(&Txn {
                rev: 1,
                id: crate::TxnId(Ulid(9)),
                author: Author::Human,
                run: None,
                command: CommandKey {
                    name: "t".into(),
                    version: 1,
                },
                request_id: None,
                reverts: None,
                reads: vec![],
                ops: ops.clone(),
                ts: 0,
            })
            .unwrap();
        assert_eq!(applied.get(new_id), work.get(new_id));
        assert_eq!(applied.get(p.id), work.get(p.id));
        // and the inverse takes it back
        let mut back = applied.clone();
        back.apply_ops(&crate::op::inverse_ops(&ops), 2, Author::Human)
            .unwrap();
        assert_eq!(back.get(p.id), d.get(p.id));
        assert!(back.get(new_id).is_none());
    }
}
