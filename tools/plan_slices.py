#!/usr/bin/env python3
"""Dev-only (tools/, never shipped): decode every part's audio slice of a RenderPlan.

  plan_slices.py PLAN.json OUT_DIR

For each part writes OUT_DIR/slices/<part id>.wav: samples
[audio.first_sample, + S(out1) - S(out0)) of its source, f32 stereo 48 kHz, decoded by
ffmpeg (standing in for Lane H's decoders; the same seek-then-trim as golden_audio_ref.py).
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from golden_audio_ref import decode_slice  # noqa: E402


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    plan = json.loads(Path(sys.argv[1]).read_text())
    out = Path(sys.argv[2])
    (out / "slices").mkdir(parents=True, exist_ok=True)
    spf = 48000 // plan["output"]["fps"]["num"]
    paths = {s["id"]: s["path"] for s in plan["sources"]}
    for p in plan["parts"]:
        wav = out / "slices" / f"{p['id']}.wav"
        if not wav.exists():
            n = (p["out"][1] - p["out"][0]) * spf
            decode_slice(paths[p["audio"]["source"]], p["audio"]["first_sample"], n, wav)


if __name__ == "__main__":
    main()
