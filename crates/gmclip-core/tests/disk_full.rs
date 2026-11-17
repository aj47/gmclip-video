//! R-ERR-2 / U.14 "a full-disk fault shows Not saved and retries without losing ops":
//! with the project's store at a simulated full disk (SQLite's page cap, so the write
//! fails inside SQLite with `SQLITE_FULL`), every edit still lands in the document and
//! answers OK, `saved()` reports `disk full` (the UI's red "Not saved: disk full"), undo
//! still works, a restore point is refused rather than written over a gap, and when the
//! space comes back every edit is written in order and survives a reopen.

mod common;

use std::time::{Duration, Instant};

use common::{fixture, open};
use gmclip_core::actor::DISK_FULL;
use serde_json::json;

fn gain(p: &common::Project, part: &str) -> f64 {
    let d = p.h.snapshot().doc.clone();
    d.get(gmclip_model::Id::<gmclip_model::entities::Part>::new(
        gmclip_model::Ulid::parse(part).unwrap(),
    ))
    .unwrap()
    .gain_db
}

#[test]
fn edits_on_a_full_disk_are_kept_shown_not_saved_and_written_when_space_returns() {
    let p = open("diskfull");
    let fx = fixture(&p, 4);
    assert_eq!(p.h.saved().1, None);
    p.h.simulate_disk_full(true).unwrap();
    // edit until one cannot be written (small ones may still fit in pages already there)
    let mut n = 0;
    let mut waiting = false;
    for i in 0..400 {
        let part = &fx.parts[i % fx.parts.len()];
        let r = p.human(
            "part_set_gain",
            json!({"part": part, "gain_db": f64::from(i as u32 % 20) / 2.0 - 5.0}),
        );
        assert!(
            r.committed.is_some(),
            "the edit is applied even on a full disk"
        );
        n += 1;
        if p.h.saved().1.as_deref() == Some(DISK_FULL) {
            waiting = true;
            break;
        }
    }
    assert!(waiting, "a full disk shows as Not saved after {n} edits");
    // more edits while full: each applies and the state stays "disk full"
    for (i, part) in fx.parts.iter().enumerate() {
        p.human(
            "part_set_gain",
            json!({"part": part, "gain_db": 1.0 + i as f64}),
        );
    }
    p.human(
        "part_set_gain",
        json!({"part": fx.parts[3], "gain_db": 9.0}),
    );
    let rev = p.h.snapshot().doc.rev();
    let (saved, err) = p.h.saved();
    assert_eq!(err.as_deref(), Some(DISK_FULL));
    assert!(saved < rev, "nothing past the full disk is reported saved");
    // still retrying a few seconds later; nothing is dropped
    std::thread::sleep(Duration::from_millis(1_200));
    assert_eq!(p.h.saved().1.as_deref(), Some(DISK_FULL));
    assert_eq!(p.h.snapshot().doc.rev(), rev);
    // undo works on the kept edits; a restore point waits for the space
    p.human("undo", json!({}));
    assert_eq!(gain(&p, &fx.parts[3]), 4.0);
    p.human("redo", json!({}));
    assert_eq!(gain(&p, &fx.parts[3]), 9.0);
    assert!(p.try_human("restore_point_create", json!({})).is_err());
    let expect = p.h.snapshot().doc.clone();
    // space returns: the waiting edits are written in order and become Saved
    p.h.simulate_disk_full(false).unwrap();
    let t0 = Instant::now();
    loop {
        let (saved, err) = p.h.saved();
        if err.is_none() && saved >= expect.rev() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{saved} {err:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    p.human("restore_point_create", json!({}));
    let p = p.reopen();
    let back = p.h.snapshot().doc.clone();
    for part in &fx.parts {
        let id = gmclip_model::Id::<gmclip_model::entities::Part>::new(
            gmclip_model::Ulid::parse(part).unwrap(),
        );
        assert_eq!(back.get(id), expect.get(id));
    }
    assert!(back.rev() >= expect.rev());
}

#[test]
fn closing_while_still_full_keeps_what_is_on_disk_consistent() {
    let p = open("diskfull-close");
    let fx = fixture(&p, 2);
    let on_disk = p.h.snapshot().doc.clone();
    p.h.simulate_disk_full(true).unwrap();
    for i in 0..400 {
        p.human(
            "part_set_gain",
            json!({"part": fx.parts[i % 2], "gain_db": (i % 7) as f64}),
        );
        if p.h.saved().1.is_some() {
            break;
        }
    }
    assert_eq!(p.h.saved().1.as_deref(), Some(DISK_FULL));
    // the close reports the edits it could not write and leaves a database that opens
    let err = p.h.close().unwrap_err();
    assert!(err.to_string().contains(DISK_FULL), "{err}");
    let h = gmclip_core::ProjectHandle::open(
        &p.dir.db(),
        gmclip_core::OpenOptions {
            store: gmclip_store::OpenOptions {
                kind: gmclip_store::HolderKind::Test,
                host: Some("test-host".into()),
                force_other_host: false,
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert!(h.snapshot().doc.rev() >= on_disk.rev());
    h.close().unwrap();
}
