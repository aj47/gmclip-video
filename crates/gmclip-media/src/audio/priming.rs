//! AAC priming compensation without edit lists (SPEC §6.5 step 3, R-EXP-5).
//!
//! An AAC encoder's output starts with `D` samples of priming (encoder delay:
//! AudioToolbox 2112, libavcodec 1024). Our files carry no `edts`, so a player plays
//! those `D` samples at t = 0. To keep cut sample `q` at time `q / rate`, the encoder is
//! fed the cut **from sample D on**: decoded position `q` (priming included) is then cut
//! sample `q` for every `q ≥ D`, and positions `[0, D)` are the encoder's warm-up. The
//! feed opens with a 5 ms linear fade so the step out of the warm-up does not click. The
//! muxer keeps `ceil(N / 1024)` packets and shortens the last one so the track lasts
//! exactly `N` samples, the cut's length, and writes the [`gapless`] iTunSMPB tag
//! (priming 0, length N): AVFoundation otherwise assumes 2112 samples of priming in an
//! edit-list-free file and plays the audio 44 ms early; ffmpeg honours the tag too.

use crate::mux::Gapless;

/// Samples per AAC-LC access unit.
pub const AAC_FRAME: u32 = 1024;

/// Fade-in at the start of the feed, in milliseconds.
pub const FEED_FADE_MS: u32 = 5;

/// The PCM to feed an encoder with priming `d`, from an interleaved cut.
#[must_use]
pub fn encoder_feed(cut: &[f32], channels: usize, d: usize, rate: u32) -> Vec<f32> {
    let ch = channels.max(1);
    let start = d.saturating_mul(ch).min(cut.len());
    let mut feed = cut.get(start..).unwrap_or(&[]).to_vec();
    let fade = (u64::from(rate) * u64::from(FEED_FADE_MS) / 1000) as usize;
    for (i, frame) in feed.chunks_mut(ch).take(fade).enumerate() {
        // Linear, 0 at the first sample: gain i/fade, as the reference de-click fade.
        let g = i as f64 / fade as f64;
        for s in frame {
            *s = (f64::from(*s) * g) as f32;
        }
    }
    feed
}

/// Durations of the AAC packets kept for a cut of `cut_frames` samples: `ceil(N/1024)`
/// packets of 1024, the last shortened so the durations sum to `N`.
#[must_use]
pub fn packet_durations(cut_frames: u64) -> Vec<u32> {
    let frame = u64::from(AAC_FRAME);
    let n = cut_frames.div_ceil(frame);
    (0..n)
        .map(|p| {
            let left = cut_frames - p * frame;
            u32::try_from(left.min(frame)).unwrap_or(AAC_FRAME)
        })
        .collect()
}

/// The gapless tag for a cut of `cut_frames` samples muxed as [`packet_durations`]:
/// priming 0 (decoded position q is already cut sample q), the cut's exact length, and
/// the rest of the last access unit as padding. Without it AVFoundation assumes 2112
/// samples of priming and plays the audio 44 ms early (B-05, measured).
#[must_use]
pub fn gapless(cut_frames: u64) -> Gapless {
    let total = cut_frames.div_ceil(u64::from(AAC_FRAME)) * u64::from(AAC_FRAME);
    Gapless {
        priming: 0,
        padding: u32::try_from(total - cut_frames).unwrap_or(0),
        length: cut_frames,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn feed_starts_at_d_with_a_fade() {
        let cut: Vec<f32> = (0..10_000).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let feed = encoder_feed(&cut, 2, 2112, 48_000);
        assert_eq!(feed.len(), (10_000 - 2112) * 2);
        assert_eq!(feed[0], 0.0, "faded in from 0");
        assert_eq!(feed[2 * 120], (2112.0 + 120.0) * 0.5);
        assert_eq!(feed[2 * 240], 2112.0 + 240.0, "fade over after 5 ms");
        assert_eq!(feed[2 * 240 + 1], -(2112.0 + 240.0));
    }

    #[test]
    fn packets_sum_to_the_cut() {
        assert_eq!(packet_durations(2048), vec![1024, 1024]);
        let p = packet_durations(96_000);
        assert_eq!(p.len(), 94);
        assert_eq!(p.last(), Some(&768));
        assert_eq!(p.iter().map(|&d| u64::from(d)).sum::<u64>(), 96_000);
        assert!(packet_durations(0).is_empty());
    }

    #[test]
    fn gapless_tag_covers_the_last_unit() {
        let g = gapless(96_000);
        assert_eq!((g.priming, g.padding, g.length), (0, 256, 96_000));
        assert_eq!(
            g.itunsmpb(),
            " 00000000 00000000 00000100 0000000000017700 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000"
        );
    }
}
