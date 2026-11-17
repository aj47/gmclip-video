#!/usr/bin/env python3
"""Builds the isolated h79 eval inputs (SPEC §17.1.1 "Task hygiene"):

  evals/h79/fixture/   the project inputs the agent's run starts from (a hash-listed
                       snapshot of AJ's clip library for loc_4fadb756, h79's cut excluded)
  evals/h79/style.md   AGENTS.md with every h79-specific passage redacted
  evals/h79/style.redactions.json   each redaction: the original text, the replacement, why

Excluded by construction (never read into the fixture): h79's story record and its
history, every RT plan/spec/receipt and render under production/, render records, and
h79's render/edit runs. The fixture is these files (gmclip keeps no importer for the
library they come from, decision 0023).

Large or lab-only inputs (the 1440p MKV, the proxy, the stream ASR word file) are listed
by path + sha256, never copied. Read-only on the library (the lab media folder's
`clippy/`, GMCLIP_LAB_MEDIA). Run from the repo root:
    python3 -I evals/h79/make_fixture.py
`gmclip-eval fixture check` then verifies the hashes and the forbidden-string grep.
"""
import hashlib, json, os, re
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
HL = Path(os.environ.get("GMCLIP_LAB_MEDIA") or Path.home() / "Development/gmclip-lab-media") / "clippy"
OUT = REPO / "evals/h79"
FIX = OUT / "fixture"
VID, H79 = "loc_4fadb756", "loc_4fadb756_h79"
SIBLINGS = ["loc_4fadb756_h34", "loc_4fadb756_h37", "loc_4fadb756_h48"]

def sha_bytes(b): return hashlib.sha256(b).hexdigest()
def sha(p): return sha_bytes(Path(p).read_bytes())
def rel(p): return "clip-library/" + str(Path(p).relative_to(HL))
def dump(p, obj):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(obj, indent=1, ensure_ascii=False) + "\n")

inputs = []
def read_input(p):
    b = Path(p).read_bytes()
    inputs.append({"path": rel(p), "sha256": sha_bytes(b), "bytes": len(b)})
    return b

# ---- style.md: AGENTS.md redacted ------------------------------------------------
agents_b = read_input(HL / "AGENTS.md")
style = agents_b.decode()
REDACTIONS = [
    ("""Origin: AJ's brief while remaking `loc_4fadb756_h79` (talking banana). He rejected the 22.5 s v3
("a bit too jumpy, and hard to follow… we can make it go for ~1 minute") and approved the 60.2 s
v4 ("this is great"). Worked example: `production/h79-v3-2026-10-04/`. It contains `build_v4.py`
(spec + captions generator), `render-spec-v4.json`, `captions-v4.ass`, `h79-v4.mp4` and `qa/`. It
renders with `production/staged-reels-2026-10-01/loc_4fadb756_h06/reset-v2/render_reel.py`
(`--ffmpeg static_ffmpeg --ffprobe static_ffprobe`). Copy that build script as the starting point.""",
     "Origin: AJ's brief while remaking a staged hook (AJ found a ~20 s cut \"a bit too jumpy, and hard to follow… we can make it go for ~1 minute\" and approved a ~60 s cut). [redacted for an eval: the worked example, its files and build script]",
     "names the eval's own cut, its v3/v4 renders and the build script that holds the answer"),
    ("""Example: text "My PC made this in 67 seconds." over voice "we were able to
     generate that five-second video in less than two minutes".""",
     "[redacted for an eval: the example is the eval's own hook and voice line]",
     "the golden text hook and the spoken claim it compresses"),
    ("""never split "two minutes" or "NVIDIA 5080 GPU", and""",
     """never split a number from its unit or a product name, and""",
     "the examples are phrases from the eval's moment"),
    ("""First a context
   label ("MiniMax H3 running on my PC"), then a measured run clock ("GENERATING 0:23" →
   "DONE IN 1:07").""",
     """First a context
   label, then a measured run clock that counts while the job runs and ends on the measured total.
   [redacted for an eval: the example pill and clock texts]""",
     "the golden pill text and the golden run-clock values"),
    ("""(ComfyUI queue 0→1 active)""", """(for example a job queue going 0→1 active)""",
     "names the app in the eval's moment"),
    (""" For h79 that was 2610.833 → 2678.267, i.e. 67.4 s.""", """ [redacted for an eval: the measured example]""",
     "the golden run-clock measurement"),
    ("""   - Drop spoken lines the screen contradicts. h79 dropped "15%" (the screen said 57%/0%)
     and "maybe less than one minute" (the run was 67 s).""",
     """   - Drop spoken lines the screen contradicts. [redacted for an eval: the examples]""",
     "the golden cut's dropped lines and the measured total"),
    (""" and `production/h79-v3-2026-10-04/build_window.py` for""", """ for""",
     "the eval cut's production folder"),
    ("""with `program_gain_envelopes`;
      +10 dB worked for h79.""", """with `program_gain_envelopes`.
      [redacted for an eval: the gain used on the eval cut]""",
     "the golden gain envelope value"),
    ("""for a one-off
  build such as h79 v4.""", """for a one-off
  build.""", "names the eval cut"),
    ("""- 2026-10-04: v3 (`production/h79-v3-2026-10-04/h79-v3.mp4`, 22.5 s): "its a bit too jumpy, and
  hard to follow. we can make it go for ~ 1 minute".
- 2026-10-04: v4 (`production/h79-v3-2026-10-04/h79-v4.mp4`, 60.2 s): "ok this is great".""",
     """- 2026-10-04: a ~20 s cut: "its a bit too jumpy, and hard to follow. we can make it go for ~ 1 minute".
- 2026-10-04: a ~1 minute cut: "ok this is great".
  [redacted for an eval: which cut and render these were]""",
     "the feedback names the eval cut's renders; AJ's words are kept"),
]
log = []
for old, new, why in REDACTIONS:
    assert style.count(old) == 1, f"redaction anchor not found exactly once: {old[:60]!r}"
    style = style.replace(old, new)
    log.append({"original": old, "replacement": new, "why": why})
TRIGGERS = [r"h79", r"banana", r"(?<![\w.])67(?![\w])", r"2610", r"2678", r"my pc made", r"15%",
            r"one minute", r"minimax", r"generating", r"done in", r"10 db", r"two minutes", r"5080",
            r"comfyui", r"\bv3\b", r"\bv4\b"]
for t in TRIGGERS:
    m = re.search(t, style, re.I)
    assert not m, f"style.md still contains {t!r}: …{style[max(0, m.start()-60):m.end()+60]}…"
(OUT / "style.md").write_text(style)
dump(OUT / "style.redactions.json", {
    "schema": "gmclip.eval.redactions/0",
    "source": {"path": rel(HL / "AGENTS.md"), "sha256": sha_bytes(agents_b)},
    "style_sha256": sha_bytes(style.encode()),
    "approved_by_aj": False,
    "redactions": log,
})

# ---- the fixture --------------------------------------------------------------------
streams = json.loads(read_input(HL / "data/streams.json"))
stream = next(s for s in streams["streams"] if s.get("video_id") == VID)
mined = json.loads(read_input(HL / f"data/hooks/{VID}.json"))
derived = json.loads(read_input(HL / f"data/hooks/{VID}.derived.json"))
excerpts = json.loads(read_input(HL / "data/excerpts.json"))
text_hooks = json.loads(read_input(HL / "data/text-hooks.json"))
staged = json.loads(read_input(HL / "data/staged.json"))
stories = json.loads(read_input(HL / "data/stories.json"))
transcript_b = read_input(HL / f"data/transcripts/{VID}.txt")

def cards(doc):
    for k in ("hooks", "mined_lines", "snapped"):
        if isinstance(doc.get(k), list) and doc[k] and isinstance(doc[k][0], dict) and "id" in doc[k][0]:
            return doc[k]
    return []

moments = []
for c in cards(mined) + (derived if isinstance(derived, list) else derived.get("hooks", [])):
    m = dict(c)
    hid = m.get("id")
    m["state"] = "staged" if hid in staged else "new"
    if hid in excerpts:
        m["excerpt"] = excerpts[hid]["text"]
    if hid == H79 and H79 in text_hooks:
        m["text_hook"] = text_hooks[H79]["text"]
    moments.append(m)
assert any(m.get("id") == H79 for m in moments), "h79 card missing"
FIX.mkdir(parents=True, exist_ok=True)
dump(FIX / "moments.json", {"source_id": VID, "untrusted": True, "moments": moments})
(FIX / "transcript.txt").write_bytes(transcript_b)
dump(FIX / "cuts.json", {"untrusted": True, "cuts": {k: stories[k] for k in SIBLINGS if k in stories}})

asr = HL / f"cache/asr/{VID}/out.json"
plan = json.loads((REPO / "schemas/examples/h79-v4.renderplan.json").read_text())
srcs = {s["id"]: s for s in plan["sources"]}
dump(FIX / "sources.json", {
    "sync_group": {
        "id": "sg-loc_4fadb756", "method": "import", "master": "audio",
        "picture": {"source": "mkv1440", "video_id": "jMcAmooE53I", "lab_path": srcs["mkv1440"]["path"].replace(str(Path.home()), "~"),
                    "sha256": srcs["mkv1440"]["fingerprint"]["sha256"],
                    "offsets": [{"from_s": 195, "until_s": 307, "frames": -503}, {"from_s": 1196, "frames": -1642}]},
        "audio": {"source": "proxy", "video_id": VID, "lab_path": srcs["proxy"]["path"].replace(str(Path.home()), "~"),
                  "sha256": srcs["proxy"]["fingerprint"]["sha256"]},
    },
    "active_asr": {"lab_path": rel(asr), "sha256": sha(asr), "note": "stream ASR word times (lab-only, by hash)"},
    "stream": {"video_id": VID, "title": stream.get("title"), "duration_s": stream.get("duration")},
})
dump(FIX / "layout.json", {"stream_layout": {"source": "mkv1440", "canvas": [2560, 1440],
                                             "regions": [{"role": "cam", "rect": [2060, 634, 496, 340]}]}})
dump(FIX / "framing.json", {"id": "9x16", "canvas": [1080, 1920], "fps": 30,
                            "captions": {"preset": "story_v5", "seat_y": 1065},
                            "loudness": {"mode": "static", "integrated_lufs": -16.0, "true_peak_dbtp": -1.5}})
dump(FIX / "profile.json", {"name": "AJ", "style_file": {"path": "evals/h79/style.md", "sha256": sha_bytes(style.encode()), "accepted": True}})
dump(FIX / "cut.json", {"id": "h79-eval", "title": "h79 eval", "moment": H79, "framing": "9x16",
                        "created_by": "human", "brief_file": "evals/h79/brief.md", "parts": []})

files = sorted(p for p in FIX.rglob("*") if p.is_file() and p.name != "manifest.json")
dump(FIX / "manifest.json", {
    "schema": "gmclip.eval.h79.fixture/0",
    "built_by": "evals/h79/make_fixture.py",
    "inputs": inputs,
    "excluded": [f"stories.json[{H79}]", f"story-history/{H79}.jsonl", "production/** (RT plans, specs, receipts, renders)",
                 "renders.json", f"edit-runs/{H79}/**"],
    "files": [{"path": str(p.relative_to(FIX)), "sha256": sha(p)} for p in files]
                + [{"path": "../style.md", "sha256": sha(OUT / "style.md")}, {"path": "../brief.md", "sha256": sha(OUT / "brief.md")}],
})
print("fixture:", len(files), "files;", len(moments), "moments;", len(inputs), "inputs")
