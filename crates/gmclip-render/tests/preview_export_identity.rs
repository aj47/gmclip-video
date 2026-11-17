//! Decision 0053 (R-COMP-1, standing): the preview is the export, frame for frame, on a
//! small fixture cut that needs none of AJ's media (`identity`: two ffmpeg-made sources,
//! six parts, every kind of join). The cut is exported with the encoder's input hashed
//! per frame; a canvas-size preview session (the viewer's full-quality frame) then
//! - seeks to both sides of every join and the cut's ends,
//! - plays the whole cut in order, as the viewer plays it,
//! - and does the same after `set_plan` from the cut before a trim (an edit while the
//!   session is open),
//!
//! and every frame's NV12 must be byte for byte the encoder's. The export's mix hashes
//! are checked against the plan's mix read in ranges (`VoiceMix`); the app's
//! `playback_export_identity` compares playback's own mix with them. VideoToolbox and
//! Metal, as `rate_conversion`: runs wherever export runs; skipped without ffmpeg.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Arc;

use gmclip_render::export::preview::PreviewSession;
use gmclip_render::export::{
    DemuxInputs, ExportOptions, MIX_HASH_BLOCK, VoiceMix, export, mix_block_hashes,
};
use gmclip_render::sha256;

mod identity;

fn repo() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Frames of `order` whose preview differs from the export's encoder input.
fn differ(s: &mut PreviewSession, order: &[u64], want: &[String]) -> Vec<u64> {
    let mut out = Vec::new();
    for &f in order {
        let p = s
            .render(f, &[], &[])
            .unwrap_or_else(|e| panic!("frame {f}: {e}"));
        let mut all = p.y;
        all.extend_from_slice(&p.uv);
        if sha256::hex(&all) != want[f as usize] {
            out.push(f);
        }
    }
    out
}

#[test]
fn preview_frames_and_mix_equal_the_export_on_the_fixture_cut() {
    let dir = std::env::temp_dir().join(format!("gmclip-identity-{}", std::process::id()));
    let Some((a, b)) = identity::sources(&dir) else {
        return;
    };
    let plan = identity::plan(&repo(), &a, &b, 0);
    let out = dir.join("out.mp4");
    let rep = export(
        &plan,
        &mut DemuxInputs::new(None),
        &out,
        &ExportOptions {
            hash_encoder_input: true,
            ..ExportOptions::default()
        },
    )
    .unwrap();
    let n = plan.output.frames;
    assert_eq!(rep.encoder_input_hashes.len() as u64, n);
    let size = (
        plan.output.canvas.w.get() as u32,
        plan.output.canvas.h.get() as u32,
    );
    let seeks = identity::seeks(&plan);
    let play: Vec<u64> = (0..n).collect();
    let plan = Arc::new(plan);

    // a session opened on this cut: seeks, then a play through
    let mut s = PreviewSession::open(Arc::clone(&plan), &mut DemuxInputs::new(None), size).unwrap();
    assert_eq!(s.size(), size, "full quality: the canvas size");
    let at_seeks = differ(&mut s, &seeks, &rep.encoder_input_hashes);
    let playing = differ(&mut s, &play, &rep.encoder_input_hashes);
    drop(s);

    // a session opened before a trim, then edited into this cut
    let before = Arc::new(identity::plan(&repo(), &a, &b, 4));
    let mut s =
        PreviewSession::open(Arc::clone(&before), &mut DemuxInputs::new(None), size).unwrap();
    for f in identity::seeks(&before) {
        s.render(f, &[], &[]).unwrap();
    }
    s.set_plan(Arc::clone(&plan), &mut DemuxInputs::new(None))
        .unwrap();
    let edited = differ(&mut s, &seeks, &rep.encoder_input_hashes);
    let edited_playing = differ(&mut s, &play, &rep.encoder_input_hashes);

    // the mix the export encoded, against the plan's mix read in playback-sized ranges
    let mut vm = VoiceMix::open(&plan, &mut DemuxInputs::new(None)).unwrap();
    let total = vm.total() as usize;
    let mut mix = vec![0.0f32; total * 2];
    for (k, c) in mix.chunks_mut(MIX_HASH_BLOCK * 2).enumerate() {
        vm.read((k * MIX_HASH_BLOCK) as u64, c).unwrap();
    }
    let ranged = mix_block_hashes(&mix);
    let mix_differ: Vec<usize> = (0..ranged.len().max(rep.mix_hashes.len()))
        .filter(|&k| ranged.get(k) != rep.mix_hashes.get(k))
        .collect();
    let heard = mix.iter().filter(|x| x.abs() > 0.01).count();
    eprintln!(
        "preview = export (decision 0053): {n} frames, joins {:?}; differ at seeks {at_seeks:?}, \
         playing {playing:?}, after an edit {edited:?} / {edited_playing:?}; \
         mix {} blocks ({heard} samples heard), differ {mix_differ:?}",
        identity::joins(&plan),
        rep.mix_hashes.len(),
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(heard > total / 2, "the fixture's sound is silent");
    assert!(
        at_seeks.is_empty(),
        "preview ≠ export at seeks {at_seeks:?}"
    );
    assert!(playing.is_empty(), "preview ≠ export playing {playing:?}");
    assert!(
        edited.is_empty() && edited_playing.is_empty(),
        "preview ≠ export after an edit: {edited:?} / {edited_playing:?}"
    );
    assert!(
        mix_differ.is_empty(),
        "the export's mix ≠ the plan's mix in ranges, blocks {mix_differ:?}"
    );
}
