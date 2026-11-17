//! R-STORE-1: kill -9 fuzz against the op-replay oracle.
//!
//! Each iteration runs one deterministic session (`common::next_txn`) in a child
//! process that commits op rows, writes entity snapshot rows every 37 ops and WAL
//! checkpoints every 101, printing `ACK <rev>` after each commit returns. The parent
//! SIGKILLs the child at a random point (several times per session), reopens, and
//! requires:
//! 1. the reopened rev ≥ the last acknowledged commit (crash-durable, R-STORE-7);
//! 2. the reopened document equals the oracle (the session replayed in memory) at that
//!    rev;
//! 3. it also equals a replay of every op row from empty (entity rows agree with the
//!    log).
//!
//! Then a new child continues the session from there, until it reaches its length.
//!
//! Sizes come from `GMCLIP_KILL_ITERS` × `GMCLIP_KILL_OPS` (default 4 × 150 for the
//! hosted-CI run). Local run per the brief: `GMCLIP_KILL_ITERS=50 GMCLIP_KILL_OPS=2000
//! cargo test --release -p gmclip-store --test kill_fuzz -- --nocapture`.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Rng, TempDir, next_txn};
use gmclip_model::Doc;
use gmclip_store::{HolderKind, OpenOptions, Store};

const CHILD_ENV: &str = "GMCLIP_KILL_CHILD";

fn env_or(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn opts() -> OpenOptions {
    OpenOptions {
        kind: HolderKind::Test,
        host: Some("fuzz-host".into()),
        force_other_host: false,
    }
}

/// The child: continue session `seed` in `db` up to `target` commits.
#[test]
fn kill_child() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let mut it = spec.split('|');
    let db = std::path::PathBuf::from(it.next().unwrap());
    let seed: u64 = it.next().unwrap().parse().unwrap();
    let target: u64 = it.next().unwrap().parse().unwrap();
    let (mut store, mut doc, _) = Store::open(&db, &opts()).expect("child open");
    // the app's durability path: a background checkpointer on its own connection
    let saver = store.checkpointer().expect("checkpointer");
    let out = std::io::stdout();
    // Paced by the parent: one stdin byte per commit, free-running after EOF, so the
    // parent's kill lands inside the run rather than after it.
    let mut stdin = std::io::stdin();
    let mut paced = true;
    while doc.rev() < target {
        if paced {
            let mut b = [0u8; 1];
            paced = matches!(stdin.read(&mut b), Ok(1));
        }
        let t = next_txn(&doc, seed);
        store.commit(&t).expect("commit");
        doc.apply(&t).expect("apply");
        {
            let mut o = out.lock();
            writeln!(o, "ACK {}", t.rev).unwrap();
            o.flush().unwrap();
        }
        if store.ops_since_checkpoint() >= 37 {
            store.checkpoint(&doc).expect("checkpoint");
        }
        saver.committed(t.rev);
        if doc.rev() % 101 == 0 {
            store.wal_checkpoint(false).expect("wal checkpoint");
        }
    }
    drop(saver);
    store.close(&doc).expect("close");
    writeln!(out.lock(), "DONE").unwrap();
}

struct Stats {
    kills: u64,
    reopens: u64,
    max_replayed: usize,
}

fn session(seed: u64, ops: u64, stats: &mut Stats) {
    let dir = TempDir::new("kill");
    let db = dir.db();
    let exe = std::env::current_exe().unwrap();
    let mut r = Rng(seed ^ 0xC0FFEE);
    let mut oracle = Doc::new();
    let mut last_ack: u64 = 0;
    loop {
        let mut child = Command::new(&exe)
            .args(["kill_child", "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, format!("{}|{seed}|{ops}", db.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // kill after a random number of further commits (or let it finish)
        let remaining = ops - last_ack;
        let kill_after = if r.below(8) == 0 {
            None
        } else {
            // about eight kills per session, anywhere in it
            Some(last_ack + 1 + r.below(remaining.min(ops / 8 + 1).max(1)))
        };
        // tokens: enough to run a little past the kill point; none planned → EOF (free)
        let mut stdin = child.stdin.take().unwrap();
        let _keep_open = match kill_after {
            Some(k) => {
                // enough slack that the kill usually lands mid-run, not on a child
                // blocked waiting for tokens
                let n = usize::try_from(k - last_ack + 1 + r.below(200)).unwrap();
                let _ = stdin.write_all(&vec![1u8; n]);
                let _ = stdin.flush();
                Some(stdin)
            }
            None => {
                drop(stdin);
                None
            }
        };
        let stdout = child.stdout.take().unwrap();
        let mut killed = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // libtest may print "test kill_child ... " on the same line as the first ack
            if let Some((_, rev)) = line.rsplit_once("ACK ") {
                let rev: u64 = rev.trim().parse().unwrap();
                assert_eq!(rev, last_ack + 1, "acks are contiguous (seed {seed})");
                last_ack = rev;
                if kill_after.is_some_and(|k| rev >= k) {
                    // land anywhere in the next commit, checkpoint or WAL checkpoint
                    let spin = Instant::now();
                    let wait = Duration::from_micros(r.below(1_500));
                    while spin.elapsed() < wait {
                        std::hint::spin_loop();
                    }
                    let _ = child.kill();
                    killed = true;
                    break;
                }
            }
        }
        let status = child.wait().unwrap();
        if killed {
            stats.kills += 1;
        } else {
            assert!(status.success(), "child failed (seed {seed})");
        }
        // reopen and compare with the oracle and with a full replay
        let (store, doc, rep) =
            Store::open(&db, &opts()).unwrap_or_else(|e| panic!("reopen (seed {seed}): {e}"));
        stats.reopens += 1;
        stats.max_replayed = stats.max_replayed.max(rep.replayed);
        assert!(
            doc.rev() >= last_ack,
            "lost an acknowledged commit: reopened at {} < ack {last_ack} (seed {seed})",
            doc.rev()
        );
        // the child may have committed past its last ack before dying
        last_ack = doc.rev();
        while oracle.rev() < doc.rev() {
            let t = next_txn(&oracle, seed);
            oracle.apply(&t).unwrap();
        }
        assert_eq!(
            doc,
            oracle,
            "reopened doc differs from the oracle at rev {} (seed {seed})",
            doc.rev()
        );
        assert_eq!(
            store.replay_all().unwrap(),
            doc,
            "entity rows disagree with the op log (seed {seed})"
        );
        // release like a crash or like a clean close
        if r.below(2) == 0 {
            store.close(&doc).unwrap();
        } else {
            drop(store);
        }
        if doc.rev() >= ops {
            break;
        }
    }
}

#[test]
fn kill9_fuzz_against_the_op_replay_oracle() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    let iters = env_or("GMCLIP_KILL_ITERS", 4);
    let ops = env_or("GMCLIP_KILL_OPS", 150);
    let base = env_or("GMCLIP_KILL_SEED", 0x6d63_6c69_7000);
    let mut stats = Stats {
        kills: 0,
        reopens: 0,
        max_replayed: 0,
    };
    let t0 = Instant::now();
    for i in 0..iters {
        session(base + i, ops, &mut stats);
    }
    eprintln!(
        "kill9 fuzz: {iters} sessions × {ops} ops, {} kills, {} reopens, max replayed {}, {:.1}s",
        stats.kills,
        stats.reopens,
        stats.max_replayed,
        t0.elapsed().as_secs_f64()
    );
    assert!(stats.kills >= iters, "the fuzz must actually kill");
}
