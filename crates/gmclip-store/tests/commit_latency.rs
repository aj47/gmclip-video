//! R-STORE-7 / A06: commit latency on this machine. Ignored by default (hosted CI
//! machines say nothing about mac-ref); run in release:
//! `cargo test --release -p gmclip-store --test commit_latency -- --ignored --nocapture`.
//! Transactions are generated first (the session generator, 1–3 edits each), then only
//! `Store::commit` is timed, with the background checkpointer running as in the app.

mod common;

use std::time::{Duration, Instant};

use common::{TempDir, next_txn};
use gmclip_model::Doc;
use gmclip_store::{HolderKind, OpenOptions, Store};

fn pct(sorted: &[Duration], p: f64) -> Duration {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

#[test]
#[ignore = "benchmark; run in release with --ignored"]
fn commit_p95() {
    let n: usize = std::env::var("GMCLIP_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let mut doc = Doc::new();
    let mut txns = Vec::with_capacity(n);
    for _ in 0..n {
        let t = next_txn(&doc, 42);
        doc.apply(&t).unwrap();
        txns.push(t);
    }
    let bytes: usize = txns.iter().map(|t| t.to_cbor().len()).sum();
    let dir = TempDir::new("latency");
    let opts = OpenOptions {
        kind: HolderKind::Test,
        host: Some("bench".into()),
        force_other_host: false,
    };
    let (mut store, _, _) = Store::open(&dir.db(), &opts).unwrap();
    let saver = store.checkpointer().unwrap();
    let mut times = Vec::with_capacity(n);
    let mut d = Doc::new();
    for t in &txns {
        let t0 = Instant::now();
        store.commit(t).unwrap();
        times.push(t0.elapsed());
        saver.committed(t.rev);
        d.apply(t).unwrap();
        if store.ops_since_checkpoint() >= 500 {
            store.checkpoint(&d).unwrap();
        }
    }
    let saved = saver.flush();
    times.sort();
    eprintln!(
        "commit latency over {n} commits (avg op row {} B): p50 {:?}  p95 {:?}  p99 {:?}  max {:?}; saved rev {saved:?}",
        bytes / n,
        pct(&times, 0.50),
        pct(&times, 0.95),
        pct(&times, 0.99),
        times[n - 1]
    );
    assert!(
        pct(&times, 0.95) <= Duration::from_millis(2),
        "R-STORE-7: p95 ≤ 2 ms"
    );
    drop(saver);
    store.close(&d).unwrap();
}
