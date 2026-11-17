//! Shared helpers for the demux golden tests: the manifest lookup, SHA-256 and the
//! comparison against `test-media/goldens/<id>.json` (W0-06).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

pub mod build;

use std::path::{Path, PathBuf};

use gmclip_media::demux::{self, Demuxed, View, io::FileSource};

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `id -> path` from test-media/manifest.toml (`id = "…"` then `path = "…"` per row):
/// `lab:<rel>` is in the lab media folder, otherwise absolute or relative to test-media.
pub fn manifest_path(id: &str) -> Option<PathBuf> {
    let raw = manifest_field(id, "path")?;
    if let Some(rel) = raw.strip_prefix("lab:") {
        return Some(gmclip_model::lab::media(rel));
    }
    let p = PathBuf::from(raw);
    Some(if p.is_absolute() {
        p
    } else {
        repo().join("test-media").join(p)
    })
}

/// The manifest rows among `ids` whose files are on this machine. A row that is not gets
/// one skip line: a lab row "skipped: lab source … not on this machine", a corpus row the
/// same with its recipe (`tools/make_corpus.sh`).
pub fn on_this_machine<'a>(ids: &[&'a str]) -> Vec<&'a str> {
    ids.iter()
        .copied()
        .filter(|id| {
            let path = manifest_path(id).unwrap_or_else(|| panic!("{id} not in manifest"));
            if path.exists() {
                return true;
            }
            if manifest_field(id, "class").as_deref() == Some("corpus") {
                eprintln!(
                    "skipped: corpus file test-media/{} not on this machine (tools/make_corpus.sh)",
                    manifest_field(id, "path").unwrap_or_default()
                );
            } else {
                eprintln!("{}", gmclip_model::lab::skip_line(&path));
            }
            false
        })
        .collect()
}

/// The goldens JSON of a manifest row (its `goldens` field).
pub fn goldens_for(id: &str) -> serde_json::Value {
    let rel = manifest_field(id, "goldens")
        .unwrap_or_else(|| panic!("{id} has no goldens in the manifest"));
    let p = repo().join("test-media").join(rel);
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

/// A string field of a manifest row (rows are `[[file]]` blocks of `key = value` lines).
pub fn manifest_field(id: &str, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(repo().join("test-media/manifest.toml")).ok()?;
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[file]]" {
            current = None;
        } else if let Some(v) = line.strip_prefix("id = ") {
            current = serde_json::from_str::<String>(v).ok();
        } else if let Some(v) = line.strip_prefix(&format!("{key} = "))
            && current.as_deref() == Some(id)
        {
            return serde_json::from_str::<String>(v).ok();
        }
    }
    None
}

pub fn goldens(file: &str) -> serde_json::Value {
    let p = repo().join("test-media/goldens").join(file);
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

/// Index `id` and compare every stream and view with its goldens. Returns a report
/// line per stream/view; panics on the first mismatch with the first differing line.
pub fn check(id: &str) -> Vec<String> {
    let path = manifest_path(id).unwrap_or_else(|| panic!("{id} not in manifest"));
    assert!(
        path.exists(),
        "{id}: {} missing (tools/make_corpus.sh, or the lab sources)",
        path.display()
    );
    let g = goldens_for(id);
    let mut src = FileSource::open(&path).unwrap();
    let d: Demuxed = demux::index(&mut src).unwrap_or_else(|e| panic!("{id}: {e}"));
    let streams = g["streams"].as_array().unwrap();
    assert_eq!(d.tracks.len(), streams.len(), "{id}: track count");
    let mut report = Vec::new();
    for (t, s) in d.tracks.iter().zip(streams) {
        let idx = s["index"].as_u64().unwrap();
        for (view_name, want) in s["packets"].as_object().unwrap() {
            let view = if view_name == "raw" {
                View::Raw
            } else {
                View::Edited
            };
            let packets = t
                .packets(view)
                .unwrap_or_else(|e| panic!("{id} s{idx} {view_name}: {e}"));
            let mut text = String::new();
            for p in &packets {
                text.push_str(&p.golden_line());
                text.push('\n');
            }
            let sha = sha256_hex(text.as_bytes());
            let count = want["count"].as_u64().unwrap();
            if sha != want["sha256"].as_str().unwrap() {
                let dump = repo().join(format!(
                    "test-media/local/goldens/{id}/s{idx}.{view_name}.txt"
                ));
                let detail = match std::fs::read_to_string(&dump) {
                    Ok(ref_text) => first_diff(&ref_text, &text),
                    Err(_) => format!(
                        "(no local dump at {}; rerun tools/make_goldens.py)",
                        dump.display()
                    ),
                };
                panic!(
                    "{id} s{idx} {view_name}: {} packets vs golden {count}: {detail}",
                    packets.len()
                );
            }
            report.push(format!("{id} s{idx} {view_name}: {count} packets equal"));
        }
    }
    report
}

fn first_diff(want: &str, got: &str) -> String {
    for (n, (a, b)) in want.lines().zip(got.lines()).enumerate() {
        if a != b {
            return format!("line {}: golden '{a}' vs ours '{b}'", n + 1);
        }
    }
    format!(
        "lengths differ: golden {} lines, ours {}",
        want.lines().count(),
        got.lines().count()
    )
}

/// SHA-256 (FIPS 180-4), for comparing dumps without a new dependency.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_known_answers() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(&[b'a'; 1000]),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}
