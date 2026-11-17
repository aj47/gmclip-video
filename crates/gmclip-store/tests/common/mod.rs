//! Shared by the store's tests: a temp dir, and a deterministic session generator — the
//! op-replay oracle. `next_txn(doc, seed)` depends only on the document and the seed, so
//! a session interrupted by a kill and continued after reopening produces the same
//! transactions as one that never stopped.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use gmclip_model::edit;
use gmclip_model::entities::*;
use gmclip_model::fixtures;
use gmclip_model::{Author, CommandKey, Doc, Flicks, Id, OrderKey, Staging, Txn, TxnId, Ulid};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // unique per call: the clock alone repeats across parallel tests on macOS
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "gmclip-store-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    pub fn db(&self) -> PathBuf {
        self.0.join("project.sqlite")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
}

pub const CUT: u128 = 1;
pub const FRAMING: u128 = 2;

/// The transaction that follows `doc` in the session `seed`.
pub fn next_txn(doc: &Doc, seed: u64) -> Txn {
    let rev = doc.rev() + 1;
    let mut r = Rng(seed ^ rev.wrapping_mul(0x2545_f491_4f6c_dd1d));
    // ids minted from (seed, rev, counter): unique and replayable
    let mut k: u128 = 0;
    let base = (u128::from(rev) << 32) | (u128::from(seed as u16) << 16);
    let mut mint = || {
        k += 1;
        Ulid((1u128 << 100) | base | k)
    };
    let mut s = Staging::new(doc.clone(), Author::Human, &mut mint);
    let cut: Id<Cut> = Id::new(Ulid(CUT));
    let framing: Id<Framing> = Id::new(Ulid(FRAMING));
    if doc.get(cut).is_none() {
        s.create(fixtures::cut(CUT)).unwrap();
        s.create(fixtures::framing(FRAMING, cut)).unwrap();
        s.create(fixtures::caption_doc(3, cut)).unwrap();
    }
    for _ in 0..1 + r.below(3) {
        let parts: Vec<Id<Part>> = s.doc().children::<Part>(cut.entity()).collect();
        let pick = |r: &mut Rng| parts[r.below(parts.len() as u64) as usize];
        let _ = match r.below(10) {
            0..=2 => {
                let after = (!parts.is_empty() && r.below(4) > 0).then(|| pick(&mut r));
                let key =
                    edit::key_after(&s, cut, after, None).unwrap_or_else(|_| OrderKey::first());
                let id = s.mint();
                let t = r.below(1_000_000) as i64;
                let mut p = fixtures::part(0, cut, key, t, t + 2 + r.below(50_000) as i64);
                p.id = id;
                s.create(p).is_ok()
            }
            3 if !parts.is_empty() => {
                let p = pick(&mut r);
                let g = (r.below(41) as f64 - 20.0) / 2.0;
                s.update(p, |p| p.gain_db = g).is_ok()
            }
            4 if !parts.is_empty() => {
                let p = pick(&mut r);
                let (a, b) = {
                    let x = s.get(p).unwrap();
                    (x.src_in.0, x.src_out.0)
                };
                b - a >= 2
                    && edit::split_part(
                        &mut s,
                        p,
                        Flicks(a + 1 + r.below((b - a - 1) as u64) as i64),
                    )
                    .is_ok()
            }
            5 if parts.len() >= 2 => {
                let i = r.below(parts.len() as u64 - 1) as usize;
                edit::merge_parts(&mut s, parts[i], parts[i + 1], true).is_ok()
            }
            6 if !parts.is_empty() => {
                let p = pick(&mut r);
                let a = (r.below(3) > 0).then(|| pick(&mut r)).filter(|a| *a != p);
                edit::reorder_part(&mut s, p, a).is_ok()
            }
            7 if parts.len() > 3 => edit::delete_part(&mut s, pick(&mut r)).is_ok(),
            8 => {
                // a per-word map key on the caption doc
                let w = gmclip_model::anchor::WordId {
                    asr_revision: "r1".into(),
                    ordinal: r.below(50) as u32,
                };
                let drop = r.below(2) == 0;
                s.update(Id::<CaptionDoc>::new(Ulid(3)), |c| {
                    if drop {
                        c.edits.insert(w, CaptionEdit::Drop);
                    } else {
                        c.edits.remove(&w);
                    }
                })
                .is_ok()
            }
            9 if !parts.is_empty() => {
                let p = pick(&mut r);
                let at = s.get(p).unwrap().src_in;
                let id = s.mint();
                s.create(VisualSplit {
                    id,
                    part: p,
                    framing,
                    at_src: at,
                    overrides: Default::default(),
                    transition: Transition::Cut,
                    tag: None,
                })
                .is_ok()
            }
            _ => false,
        };
    }
    Txn {
        rev,
        id: TxnId(mint_txn(seed, rev)),
        author: Author::Human,
        run: None,
        command: CommandKey {
            name: "fuzz".into(),
            version: 1,
        },
        request_id: None,
        reverts: None,
        reads: vec![],
        ops: s.ops(),
        ts: i64::try_from(rev).unwrap(),
    }
}

fn mint_txn(seed: u64, rev: u64) -> Ulid {
    Ulid((u128::from(seed) << 64) | u128::from(rev))
}

/// The oracle: the session's document after `n` transactions.
pub fn oracle(seed: u64, n: u64) -> Doc {
    let mut d = Doc::new();
    for _ in 0..n {
        let t = next_txn(&d, seed);
        d.apply(&t).unwrap();
    }
    d
}
