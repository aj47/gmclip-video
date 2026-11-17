//! R-ERR-2 at the store: a full disk (simulated with SQLite's own page cap, so the write
//! fails inside SQLite with `SQLITE_FULL` as on a real full disk) refuses the commit
//! cleanly — nothing half-written, the rev unchanged — and the same transaction commits
//! once space returns. R-STORE-5: `backup_into` writes a consistent copy while the
//! holder keeps writing, never over an existing file, and the copy opens to the same
//! document.

mod common;

use common::{TempDir, next_txn};
use gmclip_model::Doc;
use gmclip_store::{HolderKind, OpenOptions, Store, backup_into};

fn opts() -> OpenOptions {
    OpenOptions {
        kind: HolderKind::Test,
        host: Some("host-a".into()),
        force_other_host: false,
    }
}

#[test]
fn a_full_disk_refuses_cleanly_and_the_same_commit_lands_when_space_returns() {
    let dir = TempDir::new("diskfull");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    for _ in 0..20 {
        let t = next_txn(&doc, 3);
        s.commit(&t).unwrap();
        doc.apply(&t).unwrap();
    }
    s.simulate_disk_full(true).unwrap();
    // commits that fit in pages already there may still land; one needing a new page
    // fails with SQLITE_FULL
    let mut failed = None;
    for _ in 0..2_000 {
        let t = next_txn(&doc, 3);
        match s.commit(&t) {
            Ok(()) => doc.apply(&t).unwrap(),
            Err(e) => {
                assert!(e.is_disk_full(), "{e}");
                failed = Some(t);
                break;
            }
        }
    }
    let t = failed.expect("the page cap makes a commit fail");
    assert_eq!(s.rev(), doc.rev(), "a refused commit leaves the rev alone");
    // still full: the retry fails the same way
    assert!(s.commit(&t).unwrap_err().is_disk_full());
    s.simulate_disk_full(false).unwrap();
    s.commit(&t).unwrap();
    doc.apply(&t).unwrap();
    s.close(&doc).unwrap();
    let (_s, back, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(back, doc);
}

#[test]
fn a_backup_is_a_consistent_copy_taken_beside_the_writer() {
    let dir = TempDir::new("backup");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    for _ in 0..150 {
        let t = next_txn(&doc, 11);
        s.commit(&t).unwrap();
        doc.apply(&t).unwrap();
    }
    let at = doc.clone();
    let b1 = dir.0.join("b1.sqlite");
    let n = backup_into(&dir.db(), &b1).unwrap();
    assert!(n > 0);
    assert!(
        backup_into(&dir.db(), &b1).is_err(),
        "never over an existing copy"
    );
    assert!(!dir.0.join("b1.sqlite.part").exists());
    // the writer goes on; the copy stays what it was
    for _ in 0..30 {
        let t = next_txn(&doc, 11);
        s.commit(&t).unwrap();
        doc.apply(&t).unwrap();
    }
    let copy = dir.0.join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(&b1, copy.join("p.sqlite")).unwrap();
    let (c, back, _) = Store::open(&copy.join("p.sqlite"), &opts()).unwrap();
    assert_eq!(back, at, "the copy is the document as of the backup");
    c.close(&back).unwrap();
    // a replay of the copy's op rows from empty agrees with it
    let (c, back, _) = Store::open(&copy.join("p.sqlite"), &opts()).unwrap();
    assert_eq!(c.replay_all().unwrap(), back);
    let _: Doc = back;
}
