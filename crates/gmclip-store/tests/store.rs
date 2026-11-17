//! Store behaviour: open/commit/reopen equals op replay, entity snapshot rows, the lock
//! (A07), epochs (R-MODEL-3), snapshots, idempotency records, settings (R-STORE-7).

mod common;

use common::{TempDir, next_txn, oracle};
use gmclip_model::Doc;
use gmclip_store::{
    Acquired, HolderKind, LockInfo, OpenOptions, RequestRecord, SnapshotKind, SnapshotRecord,
    Store, StoreError, lock_path,
};

fn opts() -> OpenOptions {
    OpenOptions {
        kind: HolderKind::Test,
        host: Some("host-a".into()),
        force_other_host: false,
    }
}

fn run(store: &mut Store, doc: &mut Doc, seed: u64, n: usize) {
    for _ in 0..n {
        let t = next_txn(doc, seed);
        store.commit(&t).unwrap();
        doc.apply(&t).unwrap();
    }
}

#[test]
fn reopen_equals_replay_with_and_without_checkpoints() {
    let dir = TempDir::new("reopen");
    let (mut s, mut doc, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.rev, 0);
    assert_eq!(rep.acquired, Acquired::Fresh);
    run(&mut s, &mut doc, 7, 120);
    s.checkpoint(&doc).unwrap();
    run(&mut s, &mut doc, 7, 80);
    s.close(&doc).unwrap();

    let (mut s, mut doc2, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.rev, 200);
    assert_eq!(rep.checkpoint_rev, 200, "close writes entity rows");
    assert_eq!(rep.replayed, 0);
    assert_eq!(doc2, doc);
    assert_eq!(doc2, oracle(7, 200));
    assert_eq!(s.replay_all().unwrap(), doc2);

    // ops after a checkpoint, then a crash-like drop (no close): replayed on open
    run(&mut s, &mut doc2, 7, 30);
    drop(s);
    let (s, doc3, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.checkpoint_rev, 200);
    assert_eq!(rep.replayed, 30);
    assert!(matches!(rep.acquired, Acquired::TookOverStale(_)));
    assert!(rep.integrity_checked);
    assert_eq!(doc3, doc2);
    assert_eq!(doc3, oracle(7, 230));
    s.close(&doc3).unwrap();
}

#[test]
fn settings_follow_r_store_7() {
    let dir = TempDir::new("pragmas");
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    // WAL, synchronous=NORMAL (1), fullfsync=OFF, checkpoint_fullfsync=ON, no
    // auto-checkpoint inside commits (the Checkpointer does it off the actor)
    assert_eq!(s.settings().unwrap(), ("wal".to_owned(), 1, 0, 1, 0));
    s.close(&doc).unwrap();
}

#[test]
fn a_second_opener_is_leased_and_the_lock_file_has_no_socket_path() {
    let dir = TempDir::new("lock");
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    match Store::open(&dir.db(), &opts()) {
        Err(StoreError::Leased { holder: Some(h) }) => {
            assert_eq!(h.pid, std::process::id());
            assert_eq!(h.host, "host-a");
            assert_eq!(h.kind, HolderKind::Test);
        }
        other => panic!("expected Leased, got {other:?}"),
    }
    // Windows locks are mandatory: while held, the lock file is read through its mirror
    let held = if cfg!(windows) {
        gmclip_store::holder_path(&dir.db())
    } else {
        lock_path(&dir.db())
    };
    let text = std::fs::read_to_string(held).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["host", "kind", "pid"]);
    assert!(!text.contains("sock") && !text.contains('/'), "{text}");
    s.close(&doc).unwrap();
    // a clean close empties the file
    assert_eq!(std::fs::read_to_string(lock_path(&dir.db())).unwrap(), "");
}

#[test]
fn stale_locks_from_this_host_are_taken_over_others_refused() {
    let dir = TempDir::new("stale");
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    s.close(&doc).unwrap();
    // a holder that died on this host
    let dead = LockInfo {
        pid: 999_999,
        host: "host-a".into(),
        kind: HolderKind::App,
    };
    std::fs::write(lock_path(&dir.db()), serde_json::to_string(&dead).unwrap()).unwrap();
    let (s, doc, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.acquired, Acquired::TookOverStale(dead));
    assert!(rep.integrity_checked);
    s.close(&doc).unwrap();
    // a holder that died on another host: refused unless forced
    let other = LockInfo {
        pid: 1,
        host: "host-b".into(),
        kind: HolderKind::App,
    };
    std::fs::write(lock_path(&dir.db()), serde_json::to_string(&other).unwrap()).unwrap();
    assert!(matches!(
        Store::open(&dir.db(), &opts()),
        Err(StoreError::StaleOtherHost { .. })
    ));
    let forced = OpenOptions {
        force_other_host: true,
        ..opts()
    };
    let (s, doc, rep) = Store::open(&dir.db(), &forced).unwrap();
    assert_eq!(rep.acquired, Acquired::ForcedOtherHost(other));
    s.close(&doc).unwrap();
}

#[test]
fn a_corrupt_database_fails_the_takeover_integrity_check() {
    let dir = TempDir::new("corrupt");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    run(&mut s, &mut doc, 3, 200);
    s.close(&doc).unwrap();
    // scribble over the middle of the file, then leave a stale lock
    let mut bytes = std::fs::read(dir.db()).unwrap();
    let mid = bytes.len() / 2;
    let end = mid + 4096.min(bytes.len() - mid);
    for b in &mut bytes[mid..end] {
        *b = 0xA5;
    }
    std::fs::write(dir.db(), &bytes).unwrap();
    let dead = LockInfo {
        pid: 1,
        host: "host-a".into(),
        kind: HolderKind::App,
    };
    std::fs::write(lock_path(&dir.db()), serde_json::to_string(&dead).unwrap()).unwrap();
    assert!(matches!(
        Store::open(&dir.db(), &opts()),
        Err(StoreError::IntegrityFailed)
    ));
}

#[test]
fn torn_lock_content_is_taken_over_as_our_own() {
    let dir = TempDir::new("torn");
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    s.close(&doc).unwrap();
    std::fs::write(lock_path(&dir.db()), b"{\"pid\": 12\0\0\0").unwrap();
    let (s, doc, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert!(matches!(rep.acquired, Acquired::TookOverStale(_)));
    assert!(rep.integrity_checked);
    assert!(s.close(&doc).unwrap());
}

#[test]
fn the_holder_is_mirrored_for_readers_that_cannot_open_the_lock() {
    let dir = TempDir::new("mirror");
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    let h: LockInfo =
        serde_json::from_slice(&std::fs::read(gmclip_store::holder_path(&dir.db())).unwrap())
            .unwrap();
    assert_eq!(h.host, "host-a");
    assert_eq!(gmclip_store::peek(&dir.db()), Some(h));
    s.close(&doc).unwrap();
    assert!(!gmclip_store::holder_path(&dir.db()).exists());
    assert_eq!(gmclip_store::peek(&dir.db()), None);
}

#[test]
fn epochs_keep_a_full_set_of_rows() {
    let dir = TempDir::new("epoch");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    run(&mut s, &mut doc, 11, 60);
    s.begin_epoch(&doc, 1, "E1", 1).unwrap();
    let at_epoch = doc.clone();
    run(&mut s, &mut doc, 11, 20);
    assert_eq!(s.epoch_doc("E1").unwrap(), {
        // index revs are not part of an epoch's rows; compare contents
        let mut d = at_epoch.clone();
        d.restore_index_revs(Vec::new());
        let mut e = s.epoch_doc("E1").unwrap();
        e.restore_index_revs(Vec::new());
        assert_eq!(e.rev(), d.rev());
        assert_eq!(e.entity_ids(), d.entity_ids());
        for id in d.entity_ids() {
            assert_eq!(e.encode(id), d.encode(id));
        }
        s.epoch_doc("E1").unwrap()
    });
    let snaps = s.snapshots().unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].kind, SnapshotKind::Epoch);
    assert_eq!(snaps[0].name.as_deref(), Some("epoch 1 start"));
    s.close(&doc).unwrap();
}

#[test]
fn snapshots_requests_and_runs_persist() {
    let dir = TempDir::new("records");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    run(&mut s, &mut doc, 5, 3);
    s.add_snapshot(&SnapshotRecord {
        id: "R1".into(),
        kind: SnapshotKind::RestorePoint,
        name: None,
        rev: 3,
        run: Some("run1".into()),
        ts: 10,
    })
    .unwrap();
    let t = next_txn(&doc, 5);
    let req = RequestRecord {
        identity: "local:claude-code".into(),
        scope: "conn:1".into(),
        request_id: "r-1".into(),
        ts: 1_000,
        rev: Some(t.rev),
        result: b"{\"ok\":true}".to_vec(),
    };
    s.commit_with(Some(&t), Some(&req)).unwrap();
    doc.apply(&t).unwrap();
    // the store refuses a second record under the same key (R-CMD-7: the original stays)
    assert!(
        s.commit_with(
            None,
            Some(&RequestRecord {
                result: b"other".to_vec(),
                ..req.clone()
            })
        )
        .is_err()
    );
    s.put_run("run1", b"body").unwrap();
    s.close(&doc).unwrap();

    let (mut s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(s.snapshots().unwrap()[0].id, "R1");
    assert_eq!(
        s.request("local:claude-code", "conn:1", "r-1", 0).unwrap(),
        Some(req.clone())
    );
    // a different connection or identity never shares the key space
    assert_eq!(
        s.request("local:claude-code", "conn:2", "r-1", 0).unwrap(),
        None
    );
    assert_eq!(s.request("local:codex", "conn:1", "r-1", 0).unwrap(), None);
    // older than the window: not returned, then pruned
    assert_eq!(
        s.request("local:claude-code", "conn:1", "r-1", 1_001)
            .unwrap(),
        None
    );
    assert_eq!(s.prune_requests(2_000).unwrap(), 1);
    assert_eq!(
        s.runs().unwrap(),
        vec![("run1".to_owned(), b"body".to_vec())]
    );
    assert_eq!(s.txns(2, 4).unwrap().len(), 2);
    s.close(&doc).unwrap();
}

#[test]
fn commits_must_be_contiguous() {
    let dir = TempDir::new("gap");
    let (mut s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    let mut t = next_txn(&doc, 1);
    t.rev = 2;
    assert!(matches!(s.commit(&t), Err(StoreError::Gap(1))));
    s.close(&doc).unwrap();
}

#[test]
fn the_checkpointer_reports_saved() {
    let dir = TempDir::new("saver");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    let saver = s.checkpointer().unwrap();
    run(&mut s, &mut doc, 9, 10);
    saver.committed(s.rev());
    // within the 2 s debounce
    let t0 = std::time::Instant::now();
    while saver.saved() < 10 && t0.elapsed() < std::time::Duration::from_secs(3) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(saver.saved(), 10, "saved within the debounce");
    run(&mut s, &mut doc, 9, 5);
    saver.committed(s.rev());
    assert_eq!(saver.flush(), Some(15));
    assert_eq!(saver.last_error(), None);

    // a reader holding the WAL: the checkpoint cannot finish, is reported, and the
    // saver backs off instead of spinning
    let reader = rusqlite::Connection::open(dir.db()).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM ops;")
        .unwrap();
    run(&mut s, &mut doc, 9, 5);
    saver.committed(s.rev());
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    assert_eq!(saver.saved(), 15);
    assert_eq!(
        saver.last_error().as_deref(),
        Some("checkpoint blocked by a reader")
    );
    reader.execute_batch("COMMIT;").unwrap();
    drop(reader);
    let t0 = std::time::Instant::now();
    while saver.saved() < 20 && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(saver.saved(), 20);
    assert_eq!(saver.last_error(), None);
    drop(saver);
    assert!(
        s.close(&doc).unwrap(),
        "close reports a completed checkpoint"
    );

    // reopened cleanly: everything is already saved
    let (s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(s.checkpointer().unwrap().saved(), 20);
    s.close(&doc).unwrap();
}

#[test]
fn analysis_blobs_survive_reopen_and_are_immutable() {
    let dir = TempDir::new("analysis");
    let (mut s, doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(s.analysis("g", "words", "import:1").unwrap(), None);
    s.put_analysis("g", "words", "import:1", b"one").unwrap();
    s.put_analysis("g", "lines", "import:1", b"lines").unwrap();
    s.put_analysis("g", "words", "import:1", b"one").unwrap();
    assert!(matches!(
        s.put_analysis("g", "words", "import:1", b"two"),
        Err(StoreError::Invalid(_))
    ));
    s.close(&doc).unwrap();
    let (s, _, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(
        s.analysis("g", "words", "import:1").unwrap().as_deref(),
        Some(&b"one"[..])
    );
    assert_eq!(
        s.analysis_keys().unwrap(),
        vec![
            ("g".into(), "lines".into(), "import:1".into()),
            ("g".into(), "words".into(), "import:1".into())
        ]
    );
}

/// R-MODEL-3 / decisions 0011, 0015: the first open of a project from before this
/// build's model epoch starts "epoch k start" (a full set of rows); later opens and fresh
/// projects do not.
#[test]
fn an_older_project_starts_the_model_epoch_once() {
    let dir = TempDir::new("model-epoch");
    let (mut s, mut doc, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(
        rep.epoch_started, None,
        "a fresh project has no history to keep"
    );
    assert_eq!(s.model_epoch().unwrap(), gmclip_model::MODEL_EPOCH);
    run(&mut s, &mut doc, 13, 40);
    s.close(&doc).unwrap();
    // as a pre-epoch build left it: no model_epoch row
    {
        let c = rusqlite::Connection::open(dir.db()).unwrap();
        c.execute("DELETE FROM meta WHERE key = 'model_epoch'", [])
            .unwrap();
    }
    let (s, doc2, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.epoch_started, Some(gmclip_model::MODEL_EPOCH));
    assert_eq!(doc2.rev(), 40, "older ops replay unchanged");
    let snaps = s.snapshots().unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].kind, SnapshotKind::Epoch);
    let want = format!("epoch {} start", gmclip_model::MODEL_EPOCH);
    assert_eq!(snaps[0].name.as_deref(), Some(want.as_str()));
    assert_eq!(snaps[0].rev, 40);
    let e = s.epoch_doc(&snaps[0].id).unwrap();
    assert_eq!(e.entity_ids(), doc2.entity_ids());
    s.close(&doc2).unwrap();
    let (s, doc3, rep) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(rep.epoch_started, None);
    assert_eq!(s.snapshots().unwrap().len(), 1);
    // a project from a newer epoch is refused, not half-loaded
    s.close(&doc3).unwrap();
    {
        let c = rusqlite::Connection::open(dir.db()).unwrap();
        c.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('model_epoch', ?1)",
            [gmclip_model::cbor::to_vec(&(gmclip_model::MODEL_EPOCH + 1))],
        )
        .unwrap();
    }
    assert!(matches!(
        Store::open(&dir.db(), &opts()),
        Err(StoreError::TooNew(_))
    ));
}

/// L.01: a read-only view beside the live holder sees the last commit (entity rows and
/// replayed ops) and the analysis blobs, takes no lock, and reads a closed project too.
#[test]
fn a_read_only_view_reads_beside_the_holder() {
    let dir = TempDir::new("readonly");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    run(&mut s, &mut doc, 11, 40);
    s.checkpoint(&doc).unwrap();
    run(&mut s, &mut doc, 11, 15);
    s.put_analysis("g", "words", "r1", b"w").unwrap();
    let ro = gmclip_store::ReadOnlyStore::open(&dir.db()).unwrap();
    assert_eq!(ro.doc().unwrap(), doc, "rows + replayed ops while held");
    assert_eq!(
        ro.analysis("g", "words", "r1").unwrap().as_deref(),
        Some(&b"w"[..])
    );
    assert_eq!(ro.analysis_keys().unwrap().len(), 1);
    // the holder keeps committing: a fresh read sees it
    run(&mut s, &mut doc, 11, 5);
    assert_eq!(ro.doc().unwrap(), doc);
    s.close(&doc).unwrap();
    drop(ro);
    // closed: still readable, and the holder can open again (no lock was taken)
    let ro = gmclip_store::ReadOnlyStore::open(&dir.db()).unwrap();
    assert_eq!(ro.doc().unwrap(), doc);
    let (s, d2, _) = Store::open(&dir.db(), &opts()).unwrap();
    assert_eq!(d2, doc);
    s.close(&d2).unwrap();
}

/// L.06: a fresh database written from a document is one file (no WAL beside it), opens
/// as that document with its analysis, and takes commits after it; an existing file is
/// never overwritten.
#[test]
fn a_fresh_database_holds_the_document_as_one_checkpoint() {
    let dir = TempDir::new("fresh");
    let (mut s, mut doc, _) = Store::open(&dir.db(), &opts()).unwrap();
    run(&mut s, &mut doc, 5, 60);
    s.close(&doc).unwrap();
    let out = dir.0.join("copy.sqlite");
    let analysis = vec![(
        "g".to_owned(),
        "words".to_owned(),
        "r1".to_owned(),
        b"w".to_vec(),
    )];
    gmclip_store::write_fresh(&out, &doc, &analysis).unwrap();
    assert!(!dir.0.join("copy.sqlite-wal").exists());
    assert!(
        gmclip_store::write_fresh(&out, &doc, &analysis).is_err(),
        "never overwrites"
    );
    let (mut s2, mut d2, rep) = Store::open(&out, &opts()).unwrap();
    assert_eq!(d2, doc);
    assert_eq!(rep.replayed, 0);
    assert_eq!(
        s2.analysis("g", "words", "r1").unwrap().as_deref(),
        Some(&b"w"[..])
    );
    run(&mut s2, &mut d2, 5, 5);
    s2.close(&d2).unwrap();
    let (_, d3, _) = Store::open(&out, &opts()).unwrap();
    assert_eq!(d3, d2);
}
