#!/usr/bin/env bash
# The lab media folder: symlinks (never copies) to AJ's own footage that the lab tests
# read. Every lab test finds its media through GMCLIP_LAB_MEDIA (default
# ~/Development/gmclip-lab-media): Rust `gmclip_model::lab::media`, the e2e specs'
# `tests/e2e/lab.ts`, the Python and shell tools' `GMCLIP_LAB_MEDIA`.
#
# usage: tools/lab_media.sh <clip-library> [<stream>…]
#   <clip-library>  AJ's clip library: the folder holding cache/preview, cache/asr,
#                   production/ and renders/ (read only; nothing is written there)
#   <stream>        a full-length recording to link under streams/ by its own file name
#                   (e.g. a restream the lab rows of test-media/manifest.toml name). The
#                   tests that read one skip by name when it is not linked.
#
# Re-running replaces the links. A target that is not on this machine is reported and
# left out; the tests that need it skip or fail by name.
set -euo pipefail

lib="${1:?usage: tools/lab_media.sh <clip-library> [<stream>…]}"
shift
lib="$(cd "$lib" && pwd)"
dest="${GMCLIP_LAB_MEDIA:-$HOME/Development/gmclip-lab-media}"
mkdir -p "$dest/streams" "$dest/fonts" "$dest/renders"

missing=0
link() { # <target> <name under dest>
  if [ -e "$1" ]; then
    ln -sfn "$1" "$dest/$2"
  else
    echo "missing: $2 -> $1" >&2
    missing=$((missing + 1))
  fi
}

# The clip library's folders the tests read whole.
link "$lib/cache/preview" preview                          # 720p proxies and previews
link "$lib/cache/asr" asr                                  # word-timed transcripts (out.json)
link "$lib/production/reels-2026-10-05" reels              # ten of the golden-11 cuts
link "$lib/production/h79-v3-2026-10-04" h79               # h79 v3/v4 and its spec, ASS, qa
link "$lib/production/hires-1440-2026-10-03" hires         # the 1440p alignment windows
link "$lib/data/caption_terms.json" caption-terms.json     # AJ's caption terms
link "$lib" clippy                                         # Clippy's library (retention data)
link "$lib/renders/r9FexJOoBgY/story__r9FexJOoBgY_sw08__why-i-give-every-project-its-own-vm__1080.mp4" renders/own-vm-1080.mp4

# A recording link whose file is gone is dropped, so a missing source reads as missing.
for l in "$dest"/streams/*; do
  if [ -L "$l" ] && [ ! -e "$l" ]; then
    echo "dropped: streams/$(basename "$l") (its file is gone)" >&2
    rm "$l"
  fi
done

# Full-length sources: (b), and whatever recordings were named on the command line.
# (AJ removed the Oct-01 and Oct-05 MMO restreams, (a) and mmo-oct05, on 2026-10-16; the
# rows that read them skip.)
link "$HOME/Documents/youtube/streams/20260915-comfyui-minimax-h3-from-scratch-jMcAmooE53I/source/source.mkv" streams/jMcAmooE53I-source.mkv
for f in "$@"; do
  if [ -e "$f" ]; then
    f="$(cd "$(dirname "$f")" && pwd)/$(basename "$f")"
  fi
  link "$f" "streams/$(basename "$f")"
done

# The caption face (commercial; never committed).
link "$HOME/Library/Fonts/Neometric Heavy (Bold).otf" "fonts/Neometric Heavy (Bold).otf"

echo "lab media: $dest ($missing missing)"
