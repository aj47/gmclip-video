#!/usr/bin/env python3
"""Writes evals/h79/truth.json and evals/h79/truth/*.json (SPEC §17.1.1 "Ground truth").

Reads (read-only) the h79 v4 build in the lab media folder and the repo's v4 RenderPlan; pins each
input by sha256. Run from the repo root:  python3 -I evals/h79/make_truth.py
Needs fontTools for the hook font's glyph metrics (G9's fill-ink bbox).
"""
import hashlib, json, os, sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HL = Path(os.environ.get("GMCLIP_LAB_MEDIA") or Path.home() / "Development/gmclip-lab-media") / "clippy"
PROD = HL / "production/h79-v3-2026-10-04"
FONT = Path.home() / "Library/Fonts/Neometric Heavy (Bold).otf"
OUT = REPO / "evals/h79"

def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()

def rel(p):
    p = Path(p)
    try:
        return "clip-library/" + str(p.relative_to(HL))
    except ValueError:
        return str(p).replace(str(Path.home()), "~")

plan_path = REPO / "schemas/examples/h79-v4.renderplan.json"
plan = json.loads(plan_path.read_text())
fps = plan["output"]["fps"]["num"]
part_frames = [p["out"][1] - p["out"][0] for p in plan["parts"]]
srt = sorted(part_frames)
n = len(srt)
median = (srt[n // 2] if n % 2 else (srt[n // 2 - 1] + srt[n // 2]) / 2) / fps

asr_v4 = PROD / "qa/asr-v4/out.json"
words = [
    {"word": w["word"], "start": round(w["start"], 3), "end": round(w["end"], 3)}
    for s in json.loads(asr_v4.read_text())["sentences"]
    for w in s["words"]
]
assert len(words) == 141, len(words)
(OUT / "truth").mkdir(parents=True, exist_ok=True)
(OUT / "truth/golden-words.json").write_text(json.dumps({
    "schema": "gmclip.eval.words/0",
    "source": {"path": rel(asr_v4), "sha256": sha(asr_v4)},
    "clock": "output seconds of h79-v4.mp4",
    "words": words,
}, indent=1) + "\n")

# Glyph metrics of the hook/caption font: advance and ink box per printable ASCII char,
# in font units. The grader lays text out with these (no kerning) to get G9's ink box.
from fontTools.ttLib import TTFont
f = TTFont(str(FONT))
cmap = f.getBestCmap()
gs = f.getGlyphSet()
from fontTools.pens.boundsPen import BoundsPen
glyphs = {}
for cp in range(32, 127):
    g = cmap.get(cp)
    if g is None:
        continue
    pen = BoundsPen(gs)
    gs[g].draw(pen)
    adv = f["hmtx"][g][0]
    glyphs[chr(cp)] = {"adv": adv, "ink": list(pen.bounds) if pen.bounds else None}
os2 = f["OS/2"]
(OUT / "truth/font-neometric-heavy.json").write_text(json.dumps({
    "schema": "gmclip.eval.font-metrics/0",
    "font": {"family": "Neometric", "style": "Heavy", "sha256": sha(FONT)},
    "units_per_em": f["head"].unitsPerEm,
    "win_ascent": os2.usWinAscent,
    "win_descent": os2.usWinDescent,
    "glyphs": glyphs,
}, indent=0) + "\n")

srcs = {s["id"]: s for s in plan["sources"]}
truth = {
    "schema": "gmclip.eval.h79.truth/0",
    "approved_by_aj": False,  # AJ-03: flip by hand after re-verifying the run clock on 1440p
    "note": "Values from the h79 v4 build (build_v4.py, render-receipt.json, AGENTS.md rule 6). The run clock was measured on the proxy; AJ re-verifies it on the 1440p picture (AJ-03) before the grader freezes.",
    "inputs": {
        "v4_renderplan": {"path": "schemas/examples/h79-v4.renderplan.json", "sha256": sha(plan_path)},
        "build_v4": {"path": rel(PROD / "build_v4.py"), "sha256": sha(PROD / "build_v4.py")},
        "render_receipt": {"path": rel(PROD / "render-receipt.json"), "sha256": sha(PROD / "render-receipt.json")},
        "render_spec_v3": {"path": rel(PROD / "render-spec-v3.json"), "sha256": sha(PROD / "render-spec-v3.json")},
        "captions_v3": {"path": rel(PROD / "captions-v3.ass"), "sha256": sha(PROD / "captions-v3.ass")},
    },
    "golden": {
        "path": rel(PROD / "h79-v4.mp4"),
        "sha256": plan["checks"]["reference"]["sha256"],
        "frames": plan["output"]["frames"],
        "fps": fps,
        "seconds": round(plan["output"]["frames"] / fps, 3),
        "parts": len(part_frames),
        "part_frames": part_frames,
        "median_part_s": round(median, 3),
        "min_part_s": round(min(part_frames) / fps, 3),
        "words": len(words),
    },
    "v3": {"path": rel(PROD / "h79-v3.mp4"), "sha256": sha(PROD / "h79-v3.mp4")},
    "moment_span_s": [2585.8, 2614.6],
    "payoff_region_s": [2679.0, 2730.0],
    "setup_region_s": [2585.8, 2611.5],
    "reaction_region_s": [2711.0, 2727.3],
    "result_window_s": [2679.0, 2700.0],
    "run_clock": {"start_s": 2610.833, "stop_s": 2678.267, "seconds": 67.434, "tolerance_s": 0.5},
    "cam_region_px": [2060, 634, 496, 340],
    "cam_place_px": [160, 1120, 760, 520],
    "sources": {
        "picture": {"video_id": "jMcAmooE53I", "path": srcs["mkv1440"]["path"].replace(str(Path.home()), "~"), "sha256": srcs["mkv1440"]["fingerprint"]["sha256"], "frame_offset": -1642, "offset_valid_from_s": 1196},
        "audio": {"video_id": "loc_4fadb756", "path": srcs["proxy"]["path"].replace(str(Path.home()), "~"), "sha256": srcs["proxy"]["fingerprint"]["sha256"]},
    },
    "golden_words_file": "truth/golden-words.json",
    "font_metrics_file": "truth/font-neometric-heavy.json",
    "thresholds": {
        "duration_s": [55.0, 65.0],
        "arc_monotone_share": 0.8,
        "median_part_s_min": 3.0,
        "part_s_min": 0.25,
        "parts_max": 25,
        "hook_end_s": [3.0, 8.0],
        "hook_words": [5, 8],
        "hook_lines_max": 2,
        "hook_chars_per_line_max": 22,
        "hook_band_y": [220, 400],
        "ig_top_px": 220,
        "ig_right": {"w": 120, "y": [1000, 1560]},
        "cue_words_max": 3,
        "cue4_chars_max": 18,
        "cue4_s_max": 0.9,
        "cue_frames_min": 6,
        "caption_coverage_min": 0.95,
        "evidence_frames_min": 2,
        "cam_iou_min": 0.8,
        "cam_place_tol": 0.1,
        "cam_share_min": 0.8,
        "screen_band_y": [250, 1010],
        "screen_share_min": 0.8,
        "layouts_max": 8,
        "content_recall_report_gate": 0.5,
        "loudness_tol_lu": 0.5,
        "true_peak_max_dbtp": -1.5,
    },
    "forbidden_fixture_strings": ["67", "1:07", "2610.8", "2678.2", "My PC made"],
}
(OUT / "truth.json").write_text(json.dumps(truth, indent=1) + "\n")
print("truth.json:", truth["golden"])
