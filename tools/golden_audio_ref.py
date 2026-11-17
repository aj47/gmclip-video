#!/usr/bin/env python3
"""Dev-only (tools/, never shipped): references for the audio graph (B-11) and loudness
(B-12) from a golden cut's render receipt.

  golden_audio_ref.py RECEIPT OUT_DIR [--no-ref]

Writes to OUT_DIR:
  plan-audio.json  a valid RenderPlan v0 carrying the cut's audio (picture refs point at
                   the audio member and every part has no layers: the picture half is
                   B-13's converter's job), by RP §2.4:
                   part edges from the receipt's native frames, first_sample = native
                   frame x 1600 on the audio member's clock (stream-sources: loc_4fadb756's
                   1440p windows take their audio from the proxy; native sources from the
                   file itself), fades {144, linear} where the receipt says, envelopes
                   from program_gain_envelopes.
  slices/<part>.wav  each part's source samples [first_sample, first_sample + n), f32
                   stereo, decoded by ffmpeg: the input our graph mixes (decode is Lane H's;
                   these stand in for it).
  ref.wav          (unless --no-ref) RT's own audio graph without loudnorm, run by ffmpeg
                   over RT's own per-part intermediates (receipt segments[].intermediate):
                   aformat fltp, volume, afade ns=144 at the flagged joins, concat, the
                   aeval linear-dB envelopes, aformat stereo. Exactly render_reel_rt.py's
                   chain, minus loudnorm and the AAC encode.
  slices/join-<part>.wav  one decode of ±4800 samples around each continuous join.
  slices-vs-rt.json  per part, max |our slice - RT's intermediate| (proves the plan's
                   sample addressing equals what RT rendered).

Uses /opt/homebrew/bin/ffmpeg. Reads the lab media folder (GMCLIP_LAB_MEDIA); writes only
OUT_DIR.
"""
import json
import math
import os
import subprocess
import sys
from pathlib import Path

import numpy as np

FFMPEG = os.environ.get("FFMPEG", "/opt/homebrew/bin/ffmpeg")
SR, SPF = 48000, 1600
FADE = 144
JOIN_HALF = 4800  # samples either side of a continuous join
LAB = Path(os.environ.get("GMCLIP_LAB_MEDIA") or Path.home() / "Development/gmclip-lab-media")
# stream-sources.toml (W0-12) in miniature: where each stream's 48 kHz audio member lives
# in the lab media folder (never the receipt's own absolute path, which names wherever the
# file was when RT rendered).
AUDIO_MEMBER = {
    "loc_4fadb756": LAB / "preview/loc_4fadb756.proxy.mp4",
    "loc_086da141": LAB / "preview/loc_086da141.proxy.mp4",
    "loc_bd89f38b": LAB / "streams/i\u2019m-building-an-mmo-where-bots-are-welcome-Oct-01-2026-restream.mp4",
}


def run(cmd):
    p = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if p.returncode:
        sys.exit(f"{cmd[:6]}...: {p.stderr.decode()[-2000:]}")
    return p.stdout


def read_wav_f32(path):
    """f32 WAV (as ffmpeg writes it) -> (frames, 2) float32."""
    b = Path(path).read_bytes()
    at = 12
    while at < len(b):
        cid, n = b[at:at + 4], int.from_bytes(b[at + 4:at + 8], "little")
        if cid == b"data":
            d = np.frombuffer(b[at + 8:], dtype="<f4")
            return d[: len(d) // 2 * 2].reshape(-1, 2)
        at += 8 + n + (n & 1)
    raise ValueError(f"{path}: no data chunk")


def decode_slice(src, s0, n, out):
    """Samples [s0, s0+n) of src's first audio stream at 48 kHz, f32 stereo WAV.
    Seeks to a whole second before s0 (ffmpeg's accurate seek trims to that timestamp
    exactly), then trims by sample count."""
    t = max(0, s0 // SR - 1)
    m = s0 - t * SR
    run([FFMPEG, "-v", "error", "-y", "-ss", str(t), "-i", str(src), "-map", "0:a:0",
         "-af", f"aresample={SR},aformat=sample_fmts=flt:channel_layouts=stereo,"
                f"atrim=start_sample={m}:end_sample={m + n}",
         "-c:a", "pcm_f32le", "-f", "wav", str(out)])
    got = read_wav_f32(out)
    if len(got) != n:
        sys.exit(f"{out}: decoded {len(got)} samples, wanted {n}")


def audio_member(seg):
    vid = seg.get("source_video_id")
    if vid in AUDIO_MEMBER:
        path = AUDIO_MEMBER[vid]
        if not path.exists():
            sys.exit(f"skipped: lab source {path.relative_to(LAB)} not on this machine")
        return vid, path
    if seg.get("source_native_origin", 0) != 0:
        sys.exit(f"{seg['id']}: windowed source with no known audio member")
    return Path(seg["source"]).stem, Path(seg["source"])


def program_gain_filter(envelopes):
    """render_reel_rt.py's program_gain_filter, verbatim."""
    if not envelopes:
        return ""
    terms = []
    for e in envelopes:
        start, end = e["output_samples"]
        weight = (f'max(0,min(1,min((n-{start})/{e["ramp_in_samples"]},'
                  f'({end}-n)/{e["ramp_out_samples"]})))')
        terms.append(f'({e["gain_db"]}*{weight})')
    multiplier = "pow(10,(" + "+".join(terms) + ")/20)"
    return "aeval=exprs='" + f"val(0)*{multiplier}|val(1)*{multiplier}" + "':channel_layout=stereo,"


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    receipt_path, out = Path(sys.argv[1]), Path(sys.argv[2])
    want_ref = "--no-ref" not in sys.argv
    r = json.loads(receipt_path.read_text())
    if r["fps"] != 30:
        sys.exit("RT receipts here are 30 fps")
    segs = r["segments"]
    out.mkdir(parents=True, exist_ok=True)
    (out / "slices").mkdir(exist_ok=True)

    sources, parts = {}, []
    for g in segs:
        _, path = audio_member(g)
        # RenderPlan ids are ^[A-Za-z0-9_.:-]{1,96}$; file stems are not.
        sid = sources.setdefault(str(path), f"src{len(sources)}")
        n = g["audio_samples"]
        assert n == g["n"] * SPF and g["output_frames"][1] - g["output_frames"][0] == g["n"]
        fade = {"samples": FADE, "shape": "linear"}
        parts.append({
            "id": g["id"], "out": g["output_frames"],
            "picture": {"source": sid, "first_frame": g["native_frames"][0]},
            "layers": [],
            "audio": {"source": sid, "first_sample": g["native_frames"][0] * SPF, "rate": SR,
                      "gain_db": g.get("audio_gain_db", 0.0),
                      "fade_in": fade if g["fade_in"] else None,
                      "fade_out": fade if g["fade_out"] else None}})
    envelopes = [{"id": e["id"], "out_samples": e["output_samples"], "gain_db": e["gain_db"],
                  "ramp_in_samples": e["ramp_in_samples"], "ramp_out_samples": e["ramp_out_samples"],
                  "curve": e["curve"]} for e in r.get("program_gain_envelopes") or []]
    plan = {"schema": "gmclip.renderplan/0",
            "meta": {"title": receipt_path.parent.name, "generator": {"name": "tools/golden_audio_ref.py", "version": "1"},
                     "import_report": ["audio-only plan from tools/golden_audio_ref.py: parts carry no layers"]},
            "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
                       "frames": r["output"]["frames"], "video": {"codec": "h264"},
                       "audio": {"codec": "aac_lc", "rate": SR, "channels": 2}},
            "background": {"kind": "colour", "rgb": "#0c1118"},
            "sources": [{"id": v, "path": k} for k, v in sources.items()],
            "parts": parts,
            "audio": {"envelopes": envelopes,
                      "loudness": {"mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5}}}
    (out / "plan-audio.json").write_text(json.dumps(plan, indent=1) + "\n")

    for p in parts:
        wav = out / "slices" / f"{p['id']}.wav"
        if not wav.exists():
            path = next(k for k, v in sources.items() if v == p["audio"]["source"])
            decode_slice(path, p["audio"]["first_sample"],
                         (p["out"][1] - p["out"][0]) * SPF, wav)

    # One decode across each continuous join (same source, adjacent samples): the mix
    # there must equal it bit for bit (no fade, no step, no lost or doubled sample).
    for a, b in zip(parts, parts[1:]):
        na = (a["out"][1] - a["out"][0]) * SPF
        if a["audio"]["source"] == b["audio"]["source"] and a["audio"]["first_sample"] + na == b["audio"]["first_sample"]:
            wav = out / "slices" / f"join-{b['id']}.wav"
            if not wav.exists():
                path = next(k for k, v in sources.items() if v == b["audio"]["source"])
                decode_slice(path, b["audio"]["first_sample"] - JOIN_HALF, 2 * JOIN_HALF, wav)

    if not want_ref:
        return
    inputs, chains = [], []
    for i, g in enumerate(segs):
        inputs += ["-i", g["intermediate"]["path"]]
        samples = g["audio_samples"]
        fade_in = f",afade=t=in:ss=0:ns={FADE}" if g["fade_in"] else ""
        fade_out = f",afade=t=out:ss={samples - FADE}:ns={FADE}" if g["fade_out"] else ""
        chains.append(f"[{i}:a]aformat=sample_fmts=fltp:sample_rates={SR}:channel_layouts=stereo,"
                      f"asetpts=N/SR/TB,volume={g.get('audio_gain_db', 0.0):.6f}dB{fade_in}{fade_out}[a{i}]")
    chains.append("".join(f"[a{i}]" for i in range(len(segs))) + f"concat=n={len(segs)}:v=0:a=1[ac]")
    total = r["output"]["frames"] * SPF
    chains.append("[ac]aformat=channel_layouts=stereo," + program_gain_filter(r.get("program_gain_envelopes") or [])
                  + f"aformat=sample_fmts=flt,atrim=end_sample={total}[aout]")
    run([FFMPEG, "-v", "error", "-y", *inputs, "-filter_complex", ";".join(chains), "-map", "[aout]",
         "-c:a", "pcm_f32le", "-f", "wav", str(out / "ref.wav")])

    # Our slices vs RT's own intermediates (pcm_s24le): equal up to 24-bit quantisation.
    check = {}
    for g, p in zip(segs, parts):
        rt = out / "slices" / f"rt-{p['id']}.wav"
        if not rt.exists():
            run([FFMPEG, "-v", "error", "-y", "-i", g["intermediate"]["path"], "-map", "0:a:0",
                 "-c:a", "pcm_f32le", "-f", "wav", str(rt)])
        a, b = read_wav_f32(out / "slices" / f"{p['id']}.wav"), read_wav_f32(rt)
        check[p["id"]] = {"samples": [len(a), len(b)],
                          "max_abs_diff": float(np.abs(a - b).max()) if len(a) == len(b) else None}
    (out / "slices-vs-rt.json").write_text(json.dumps(check, indent=1) + "\n")


if __name__ == "__main__":
    main()
