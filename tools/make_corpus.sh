#!/usr/bin/env bash
# Build the self-made corpus v0 files (W0-05, SPEC §13 and §17 Corpus) into
# test-media/local/ (git-ignored). Dev-only: shells out to ffmpeg, never linked.
#
#   tools/make_corpus.sh [--only ID[,ID...]]
#
# Every output is described by a row in test-media/manifest.toml (id, path, sha256,
# recipe). The bytes are reproducible with the ffmpeg build recorded in the manifest
# (`[corpus].ffmpeg`); a different ffmpeg/x264/libvpx build may produce different bytes,
# in which case rerun this script, then `tools/make_goldens.py`, and update the sha256s
# (`cargo xt corpus print` prints them).
#
# Inputs: (a′) is a stream-copy trim of source (a), the 2 h 1440p30 H.264 restream
# (lab-only, `A_SRC`). Everything else is synthesised from lavfi test sources.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${CORPUS_OUT:-$ROOT/test-media/local}"
FFMPEG="${FFMPEG:-/opt/homebrew/bin/ffmpeg}"
A_SRC="${A_SRC:-${GMCLIP_LAB_MEDIA:-$HOME/Development/gmclip-lab-media}/streams/i’m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4}"
ONLY=""
if [[ "${1:-}" == "--only" ]]; then ONLY="${2:?--only needs a comma-separated id list}"; fi
mkdir -p "$OUT" "$OUT/tmp"

want() { [[ -z "$ONLY" || ",$ONLY," == *",$1,"* ]]; }
# The build the manifest's bytes came from ([corpus].ffmpeg_version); another build may
# encode different bytes. ACCEPT_FFMPEG=1 overrides.
WANT_VERSION="$(sed -n 's/^ffmpeg_version = "\(.*\)"$/\1/p' "$ROOT/test-media/manifest.toml")"
HAVE_VERSION="$("$FFMPEG" -version | head -1 | sed 's/ Copyright.*//')"
if [[ -n "$WANT_VERSION" && "$HAVE_VERSION" != "$WANT_VERSION" && -z "${ACCEPT_FFMPEG:-}" ]]; then
  echo "make_corpus: $HAVE_VERSION is not the corpus build $WANT_VERSION (ACCEPT_FFMPEG=1 to override)" >&2
  exit 1
fi
ff() { "$FFMPEG" -hide_banner -nostdin -v error -y "$@"; }
# Bit-exact muxing: no encoder/version strings that change between runs.
BITEXACT=(-fflags +bitexact -flags:v +bitexact -flags:a +bitexact -map_metadata -1 -map_chapters -1)

# (a′) 3-min 1440p30 H.264 High with B-frames + AAC 48 kHz: stream copy of (a) from the
# keyframe at 1200.566 s (dts 1200.500 s), so no re-encode and no pre-roll.
if want a-prime; then
  [[ -f "$A_SRC" ]] || { echo "make_corpus: source (a) not found: $A_SRC" >&2; exit 1; }
  ff -ss 1200.566 -i "$A_SRC" -t 180 -map 0:v:0 -map 0:a:0 -c copy "${BITEXACT[@]}" \
    "$OUT/a-prime.mp4"
fi

# (b′) 3-min 1440p30 VP9 Profile 0 MKV with Cues + AAC-LC 48 kHz stereo, encoded here
# (not a trim of (b)). Good-quality one-pass libvpx with alt-ref enabled; on this
# synthetic content libvpx emits no hidden frames, so superframes (hidden ARF + shown
# frame) are exercised by (b), which has 1 396 of them.
if want b-prime; then
  ff -f lavfi -i "testsrc2=size=2560x1440:rate=30:duration=180" \
     -f lavfi -i "sine=frequency=440:beep_factor=4:sample_rate=48000:duration=180" \
     -map 0:v -map 1:a \
     -c:v libvpx-vp9 -deadline good -cpu-used 5 -row-mt 1 -threads 8 -b:v 6M -g 150 \
     -lag-in-frames 25 -auto-alt-ref 1 -pix_fmt yuv420p \
     -c:a aac -b:a 128k -ar 48000 -ac 2 \
     "${BITEXACT[@]}" "$OUT/b-prime.mkv"
fi

# Shared 20 s H.264 (B-frames 3, closed 2 s GOP) + AAC source for the edit-list files.
# Muxed without faststart: `moov` is last, which tools/elst_patch.py relies on.
zoo_src() {
  local size="$1" out="$2"
  ff -f lavfi -i "testsrc2=size=$size:rate=30:duration=20" \
     -f lavfi -i "sine=frequency=440:beep_factor=4:sample_rate=48000:duration=20" \
     -map 0:v -map 1:a -c:v libx264 -preset veryfast -threads 4 -bf 3 -g 60 -keyint_min 60 \
     -sc_threshold 0 -pix_fmt yuv420p -c:a aac -b:a 128k -ac 2 "${BITEXACT[@]}" "$out"
}

# Edit-listed MOV, the way ffmpeg writes one: a stream-copy trim from mid-GOP (2.5 s),
# so video starts at the previous keyframe with discarded pre-roll and audio starts
# inside an AAC packet.
if want editlist-mov; then
  zoo_src 1280x720 "$OUT/tmp/el-src.mp4"
  ff -ss 2.5 -i "$OUT/tmp/el-src.mp4" -t 10 -c copy "${BITEXACT[@]}" "$OUT/editlist.mov"
fi

# Edit-list zoo: one 320x240 source, edit lists rewritten in place (tools/elst_patch.py)
# for the cases ffmpeg's muxer does not write. Video timescale 15360 (512/frame), audio
# 48000; keyframes every 60 frames; frame n has media pts 512 n + 1024.
if want zoo; then
  zoo_src 320x240 "$OUT/tmp/zoo-src.mp4"
  P="python3 $ROOT/tools/elst_patch.py"
  # video: start at frame 130 (mid-GOP, on a frame edge), 5 s long (end trim)
  $P "$OUT/tmp/zoo-src.mp4" "$OUT/zoo-v-midgop.mp4" 0 '5000:67584'
  # video: start 256 ticks into frame 120 (not on a frame edge)
  $P "$OUT/tmp/zoo-src.mp4" "$OUT/zoo-v-offframe.mp4" 0 '20000:62720'
  # audio: start inside an AAC packet, 5.003 s long (end inside a packet)
  $P "$OUT/tmp/zoo-src.mp4" "$OUT/zoo-a-straddle.mp4" 1 '5003:100000'
  # audio: start 4.375 s in (pre-roll window)
  $P "$OUT/tmp/zoo-src.mp4" "$OUT/zoo-a-late.mp4" 1 '5000:210000'
  # empty edits: video 0.5 s, audio 0.25 s, then the whole media
  $P "$OUT/tmp/zoo-src.mp4" "$OUT/tmp/zoo-empty.mp4" 0 '500:-1,20000:1024'
  $P "$OUT/tmp/zoo-empty.mp4" "$OUT/zoo-empty.mp4" 1 '250:-1,20000:1024'
  # negative composition offsets (ctts v1, no B-frame delay edit): dts shifts earlier
  ff -i "$OUT/tmp/zoo-src.mp4" -c copy -movflags +negative_cts_offsets "${BITEXACT[@]}" "$OUT/zoo-neg-cts.mp4"
  # ISO PCM: 'ipcm' sample entry with pcmC, 24-bit stereo 48 kHz (RT windows use QuickTime in24)
  ff -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=3" -ac 2 -c:a pcm_s24le \
     "${BITEXACT[@]}" "$OUT/zoo-ipcm.mp4"
fi

# VFR phone-style clip: portrait 1080x1920 H.264 (B-frames) + AAC, 10 s, frames kept
# pseudo-randomly from a 60 fps source so stts has many distinct durations.
if want vfr; then
  ff -f lavfi -i "testsrc2=size=1080x1920:rate=60:duration=10" \
     -f lavfi -i "sine=frequency=660:beep_factor=2:sample_rate=48000:duration=10" \
     -map 0:v -map 1:a \
     -vf "select='not(mod(n\,2))+lt(mod(n*7919\,13)\,3)'" -fps_mode vfr \
     -c:v libx264 -preset veryfast -threads 4 -bf 2 -g 60 -pix_fmt yuv420p \
     -c:a aac -b:a 128k -ac 2 -movflags +faststart "${BITEXACT[@]}" "$OUT/vfr.mp4"
fi

rm -rf "$OUT/tmp"
echo "make_corpus: done -> $OUT"
