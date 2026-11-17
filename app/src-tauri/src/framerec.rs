//! The frame socket's upstream records (R-FRAME-1, FZ14): parse, encode and coalesce.
//! Standard library only, so the fuzz target (`fuzz/fuzz_targets/fz14_frame.rs`) builds it
//! by path without the app's dependencies.
#![forbid(unsafe_code)]

/// Bytes per upstream record and per frame header.
pub const RECORD: usize = 32;

/// Upstream record kinds (the only ones accepted).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Down = 1,
    Move = 2,
    Up = 3,
    Wheel = 4,
    Key = 5,
    Viewport = 6,
    Cancel = 7,
}

/// One viewer-input record (32 bytes, little-endian):
/// `kind u8 · buttons u8 · mods u8 · pointer u8 · x f32 · y f32 · dx f32 · dy f32 ·
/// code u32 · t f64`. Pointer `x, y` are normalised to the drawn frame (0..1);
/// a viewport record carries device pixels in `x, y`; `t` is `performance.now()`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Record {
    pub kind: RecordKind,
    pub buttons: u8,
    pub mods: u8,
    pub pointer: u8,
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub code: u32,
    pub t: f64,
}

/// Transport keys a key record may carry (editing shortcuts go through `gm_call`).
pub mod keys {
    pub const PLAY_PAUSE: u32 = 1;
    pub const STEP_BACK: u32 = 2;
    pub const STEP_FORWARD: u32 = 3;
    pub const PAUSE: u32 = 4;
}

impl Record {
    /// Parses one record; `None` for anything not an enumerated kind with finite values.
    pub fn parse(b: &[u8]) -> Option<Record> {
        if b.len() != RECORD {
            return None;
        }
        let f = |i: usize| f32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let kind = match b[0] {
            1 => RecordKind::Down,
            2 => RecordKind::Move,
            3 => RecordKind::Up,
            4 => RecordKind::Wheel,
            5 => RecordKind::Key,
            6 => RecordKind::Viewport,
            7 => RecordKind::Cancel,
            _ => return None,
        };
        let code = u32::from_le_bytes([b[20], b[21], b[22], b[23]]);
        let mut t = [0u8; 8];
        t.copy_from_slice(&b[24..32]);
        let r = Record {
            kind,
            buttons: b[1],
            mods: b[2],
            pointer: b[3],
            x: f(4),
            y: f(8),
            dx: f(12),
            dy: f(16),
            code,
            t: f64::from_le_bytes(t),
        };
        let finite = [r.x, r.y, r.dx, r.dy].iter().all(|v| v.is_finite()) && r.t.is_finite();
        let key_ok = kind != RecordKind::Key || (1..=4).contains(&code);
        (finite && key_ok).then_some(r)
    }

    pub fn encode(&self) -> [u8; RECORD] {
        let mut b = [0u8; RECORD];
        b[0] = self.kind as u8;
        b[1] = self.buttons;
        b[2] = self.mods;
        b[3] = self.pointer;
        b[4..8].copy_from_slice(&self.x.to_le_bytes());
        b[8..12].copy_from_slice(&self.y.to_le_bytes());
        b[12..16].copy_from_slice(&self.dx.to_le_bytes());
        b[16..20].copy_from_slice(&self.dy.to_le_bytes());
        b[20..24].copy_from_slice(&self.code.to_le_bytes());
        b[24..32].copy_from_slice(&self.t.to_le_bytes());
        b
    }
}

/// Coalesces a batch: consecutive moves keep the newest, viewports keep the newest one;
/// downs, ups, cancels, wheels and keys are never dropped.
pub fn coalesce(batch: &mut Vec<Record>) {
    let last_viewport = batch.iter().rposition(|r| r.kind == RecordKind::Viewport);
    let mut out: Vec<Record> = Vec::with_capacity(batch.len());
    for (i, r) in batch.iter().enumerate() {
        match r.kind {
            RecordKind::Viewport if Some(i) != last_viewport => {}
            RecordKind::Move if out.last().is_some_and(|l| l.kind == RecordKind::Move) => {
                if let Some(l) = out.last_mut() {
                    *l = *r;
                }
            }
            _ => out.push(*r),
        }
    }
    *batch = out;
}

/// FZ14's harness body (SR §3.2, B.01), shared by `fuzz/fuzz_targets/fz14_frame.rs` and
/// `cargo xt fuzz-replay`. Any bytes, split into 32-byte records: no panic; a record
/// parses only to an enumerated kind with finite values (key records: transport codes
/// only); a parsed record re-encodes to the same bytes; coalescing keeps every down, up,
/// cancel, wheel and key, in order. Panics on a violation.
pub fn fuzz_check(data: &[u8]) {
    let mut batch = Vec::new();
    for c in data.chunks(RECORD) {
        let r = Record::parse(c);
        if c.len() != RECORD {
            assert!(r.is_none());
            continue;
        }
        let Some(r) = r else { continue };
        assert!((1..=7).contains(&c[0]));
        assert!([r.x, r.y, r.dx, r.dy].iter().all(|v| v.is_finite()) && r.t.is_finite());
        if r.kind == RecordKind::Key {
            assert!((1..=4).contains(&r.code));
        }
        assert_eq!(&r.encode()[..], c);
        batch.push(r);
    }
    let kept: Vec<Record> = batch
        .iter()
        .copied()
        .filter(|r| !matches!(r.kind, RecordKind::Move | RecordKind::Viewport))
        .collect();
    let mut b = batch.clone();
    coalesce(&mut b);
    let after: Vec<Record> = b
        .iter()
        .copied()
        .filter(|r| !matches!(r.kind, RecordKind::Move | RecordKind::Viewport))
        .collect();
    assert_eq!(kept, after);
    assert!(b.len() <= batch.len());
    assert!(b.iter().filter(|r| r.kind == RecordKind::Viewport).count() <= 1);
}
