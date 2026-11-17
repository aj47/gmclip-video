//! Shared by core's tests: a project in a temp dir with a test clock and seeded ids,
//! and a small fixture (a cut, a 9:16 framing, a source, a screen layer, N parts).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use gmclip_core::services::TestClock;
use gmclip_core::{CallOk, CallRequest, Caller, CmdError, OpenOptions, ProjectHandle};
use gmclip_proto::Base;
use gmclip_store::{HolderKind, OpenOptions as StoreOpen};
use serde_json::{Value, json};

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
            "gmclip-core-{tag}-{}-{nanos}-{n}",
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

pub const T0: i64 = 1_760_000_000_000;
pub const SEC: i64 = 705_600_000;

pub struct Project {
    pub dir: TempDir,
    pub h: ProjectHandle,
    pub clock: Arc<TestClock>,
}

pub fn open(tag: &str) -> Project {
    let dir = TempDir::new(tag);
    let clock = Arc::new(TestClock::new(T0));
    let h = ProjectHandle::open(
        &dir.db(),
        OpenOptions {
            store: StoreOpen {
                kind: HolderKind::Test,
                host: Some("test-host".into()),
                force_other_host: false,
            },
            clock: clock.clone(),
            seed: Some(42),
        },
    )
    .unwrap();
    Project { dir, h, clock }
}

impl Project {
    pub fn reopen(self) -> Project {
        let Project { dir, h, clock } = self;
        h.close().unwrap();
        let h = ProjectHandle::open(
            &dir.db(),
            OpenOptions {
                store: StoreOpen {
                    kind: HolderKind::Test,
                    host: Some("test-host".into()),
                    force_other_host: false,
                },
                clock: clock.clone(),
                seed: Some(43),
            },
        )
        .unwrap();
        Project { dir, h, clock }
    }

    pub fn human(&self, cmd: &str, args: Value) -> CallOk {
        self.clock.advance(1);
        self.h
            .human(cmd, args)
            .unwrap_or_else(|e| panic!("{cmd}: {e:?}"))
    }

    pub fn try_human(&self, cmd: &str, args: Value) -> Result<CallOk, CmdError> {
        self.clock.advance(1);
        self.h.human(cmd, args)
    }

    pub fn agent(
        &self,
        conn: &str,
        cmd: &str,
        args: Value,
        base: Option<Base>,
    ) -> Result<CallOk, CmdError> {
        self.clock.advance(1);
        self.h.call(CallRequest {
            command: cmd.into(),
            args,
            base,
            request_id: None,
            caller: Caller::agent("local:claude-code", conn),
        })
    }

    pub fn as_of(&self) -> Option<Base> {
        Some(Base::AsOf {
            as_of: self.h.snapshot().doc.rev(),
        })
    }
}

/// Ids of the fixture.
pub struct Fx {
    pub cut: String,
    pub framing: String,
    pub source: String,
    pub screen: String,
    pub cam: String,
    pub parts: Vec<String>,
}

pub fn id(ok: &CallOk) -> String {
    ok.output["id"].as_str().unwrap().to_owned()
}

/// A cut with `n` parts of 2 s each, one source, a 9:16 framing with screen and cam.
pub fn fixture(p: &Project, n: usize) -> Fx {
    let c = p.human("cut_create", json!({"title": "fixture"}));
    let cut = c.output["cut"].as_str().unwrap().to_owned();
    let source = id(&p.human(
        "source_add",
        json!({"path": "/m/s.mkv", "fingerprint": "fp"}),
    ));
    let framing = id(&p.human(
        "framing_create",
        json!({"cut": cut, "name": "9:16", "w": 1080, "h": 1920, "fps": {"num": 30, "den": 1}}),
    ));
    let screen = id(&p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "screen"}, "crop": {"x": 0, "y": 0, "w": 1, "h": 1}, "place": {"x": 0, "y": 0.13, "w": 1}}),
    ));
    let cam = id(&p.human(
        "layer_add",
        json!({"owner": {"framing": framing}, "role": {"region": "cam"}, "crop": {"x": 0.8, "y": 0.44, "w": 0.19, "h": 0.24}, "place": {"x": 0.15, "y": 0.58, "w": 0.7}, "z": 1}),
    ));
    let mut parts: Vec<String> = Vec::new();
    for i in 0..n {
        let t = 100 * SEC + i as i64 * 3 * SEC;
        let after = parts.last().cloned();
        let mut a =
            json!({"cut": cut, "media": {"source": source}, "src_in": t, "src_out": t + 2 * SEC});
        if let Some(after) = after {
            a["after"] = json!(after);
        }
        parts.push(id(&p.human("part_add", a)));
    }
    Fx {
        cut,
        framing,
        source,
        screen,
        cam,
        parts,
    }
}
