#!/usr/bin/env python3
"""Writes evals/h79/selftest/v3.plan.json: h79 v3 (22.5 s, "too jumpy") in the RenderPlan
shape the grader reads (parts with out/native/layers, text events), for self-test (2).

The B-13 converter refuses v3's spec (`run_clock` is not a key it knows), so this is a
grader-shaped reduction, not a validated RenderPlan: part frames follow build_v3.py's
quantisation (int(t*30+.5)), layers come from the spec's crops/places (the dummy camera is
hidden), text events from captions-v3.ass. Run: python3 -I evals/h79/make_v3_plan.py
"""
import hashlib, json, os, re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
P = Path(os.environ.get("GMCLIP_LAB_MEDIA") or Path.home() / "Development/gmclip-lab-media") / "h79"
spec_b = (P / "render-spec-v3.json").read_bytes()
ass_b = (P / "captions-v3.ass").read_bytes()
spec = json.loads(spec_b)
FPS = 30
DUMMY = [200, 1350, 32, 20]
rect = lambda r: {"x": r[0], "y": r[1], "w": r[2], "h": r[3]}
parts, cursor = [], 0
for s in spec["segments"]:
    q0, q1 = int(s["native_start"] * FPS + .5), int(s["native_end"] * FPS + .5)
    n = q1 - q0
    layers = [
        {"id": "screen", "role": "screen", "crop": rect(s["screen_crop"]), "place": rect(s["screen_place"]), "z": 0},
        {"id": "cam", "role": "cam", "crop": rect(s["camera_crop"]), "place": rect(s["camera_place"]), "z": 1,
         "visible": s["camera_crop"] != DUMMY},
    ]
    for i, d in enumerate(s.get("detail_layers", [])):
        layers.append({"id": f"detail-{i}", "role": "detail", "crop": rect(d["crop"]), "place": rect(d["place"]), "z": 2})
    parts.append({"id": s["id"], "out": [cursor, cursor + n], "native": {"frames": [q0, q1], "fps": {"num": FPS, "den": 1}}, "layers": layers})
    cursor += n

def t(s):
    h, m, rest = s.split(":")
    return round((int(h) * 3600 + int(m) * 60 + float(rest)) * FPS)

events = []
for i, line in enumerate(l for l in ass_b.decode().splitlines() if l.startswith("Dialogue:")):
    f = line.split(",", 9)
    style, text = f[3], f[9]
    pos = re.search(r"\\pos\(([\d.]+),([\d.]+)\)", text)
    plain = re.sub(r"\{[^}]*\}", "", text).replace("\\N", "\n")
    role = {"Hook": "text_hook", "Speech": "caption", "Pill": "section_pill"}.get(style, "note")
    events.append({
        "id": "hook" if role == "text_hook" else f"ev-{i}", "role": role, "style": style,
        "out": [t(f[1]), min(t(f[2]), cursor)], "size": 88.0 if style == "Hook" else (50.0 if style == "Pill" else 86.0),
        "pos": {"x": float(pos.group(1)), "y": float(pos.group(2))} if pos else {"x": 540.0, "y": 1065.0},
        "align": 7 if style == "Pill" else 5, "runs": [{"text": plain}], "provenance": {"kind": "import"},
    })
plan = {
    "schema": "gmclip.renderplan/0",
    "meta": {"title": spec["title"], "hook_id": spec["hook_id"],
             "import_report": ["grader-shaped reduction of h79 v3 for the h79 self-test; not a validated RenderPlan"],
             "inputs": [{"role": "spec", "path": "production/h79-v3-2026-10-04/render-spec-v3.json", "sha256": hashlib.sha256(spec_b).hexdigest()},
                        {"role": "ass", "path": "production/h79-v3-2026-10-04/captions-v3.ass", "sha256": hashlib.sha256(ass_b).hexdigest()}]},
    "output": {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": FPS, "den": 1}, "frames": cursor},
    "parts": parts,
    "text_styles": {"Hook": {"font": "neometric-heavy", "size": 88.0, "align": 5}, "Speech": {"font": "neometric-heavy", "size": 86.0, "align": 5},
                    "Pill": {"font": "neometric-heavy", "size": 50.0, "align": 7, "letter_spacing": 1.0}},
    "text_events": events,
}
out = REPO / "evals/h79/selftest/v3.plan.json"
out.write_text(json.dumps(plan, indent=1) + "\n")
print(out, cursor, "frames =", round(cursor / FPS, 2), "s,", len(parts), "parts")
