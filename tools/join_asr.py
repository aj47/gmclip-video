#!/usr/bin/env python3
"""Dev-only (tools/, never shipped): R-EXP-2's join-word check (B-15).

  join_asr.py --plan PLAN.json --render R --reference REF [--out report.json]

For every discontinuous join of PLAN (RP rule 7: a different audio source, or samples
that are not adjacent), cuts ±1.5 s of the output clock around the join from the render
(an MP4 or WAV) and from the reference (RT's MP4 where one exists, else the planned mix
as WAV), transcribes each side once with transcribe-anything (parakeet; windows joined
with 1 s of silence) and compares the words:

  missing  a reference word with no render word of the same normalised text (edit
           distance <= 1) within +-0.25 s
  split    a missing word that two adjacent render words spell together
  added    a render word (not a filler) with no reference word within +-0.25 s

Words within 80 ms of a window edge are not expected (the window may cut them); matching
is one to one, so a word said twice at a join is "added". Words of <= 2 letters match
exactly. A join passes when it has none of the three; a join whose reference window has no
words is reported `checked: false`. Exit 0: all joins pass; 1: a join fails; 2: a tool
error (ffmpeg or ASR failed, or the reference has no words at all) — the report says why. Uses
/opt/homebrew/bin/ffmpeg and transcribe-anything (both dev-only).
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

FFMPEG = os.environ.get("FFMPEG", "/opt/homebrew/bin/ffmpeg")
SR = 16000
FILLERS = {"uh", "um", "uhh", "umm", "er", "hmm", "ah", "mm"}


def joins(plan):
    fps = plan["output"]["fps"]["num"]
    spf = 48000 // fps
    out = []
    parts = plan["parts"]
    for a, b in zip(parts, parts[1:]):
        na = (a["out"][1] - a["out"][0]) * spf
        cont = a["audio"]["source"] == b["audio"]["source"] and a["audio"]["first_sample"] + na == b["audio"]["first_sample"]
        if not cont:
            out.append({"part": b["id"], "t": b["out"][0] / fps})
    return out, plan["output"]["frames"] / fps


def windows_wav(media, wins, path):
    """Each window [t0, t1) of `media`'s audio at 16 kHz mono, then 1 s of silence."""
    parts = []
    for i, (t0, t1) in enumerate(wins):
        parts.append(f"[0:a]atrim=start={t0:.6f}:end={t1:.6f},asetpts=N/SR/TB,aresample={SR},"
                     f"pan=mono|c0=0.5*c0+0.5*c1,apad=pad_dur=1[w{i}]")
    graph = ";".join(parts) + ";" + "".join(f"[w{i}]" for i in range(len(wins))) + f"concat=n={len(wins)}:v=0:a=1[out]"
    subprocess.run([FFMPEG, "-v", "error", "-y", "-i", str(media), "-filter_complex", graph, "-map", "[out]",
                    "-c:a", "pcm_s16le", str(path)], check=True, stderr=subprocess.PIPE)


def transcribe(wav, outdir):
    subprocess.run(["transcribe-anything", str(wav), "--output_dir", str(outdir), "--language", "en",
                    "--model", "parakeet-v3", "--device", "parakeet"],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if not (Path(outdir) / "out.json").exists():
        raise subprocess.CalledProcessError(1, ["transcribe-anything"], stderr=b"no out.json written")
    data = json.loads((Path(outdir) / "out.json").read_text())
    words = []
    for s in data.get("sentences") or data.get("segments") or []:
        for w in s.get("words", []):
            n = norm(w["word"])
            if n:
                words.append((n, float(w["start"]), float(w["end"])))
    return words


def norm(w):
    return re.sub(r"[^a-z0-9]", "", w.lower())


def edit1(a, b):
    """Same word: equal, or edit distance <= 1 for words longer than 2 characters
    (short words like "we"/"me", "a"/"i" must match exactly)."""
    if a == b:
        return True
    if min(len(a), len(b)) <= 2:
        return False
    if abs(len(a) - len(b)) > 1:
        return False
    if len(a) == len(b):
        return sum(x != y for x, y in zip(a, b)) <= 1
    if len(a) > len(b):
        a, b = b, a
    for i in range(len(b)):
        if b[:i] + b[i + 1:] == a:
            return True
    return False


def per_window(words, starts, dur):
    """Words per window, with window-local times."""
    out = [[] for _ in starts]
    for n, s, e in words:
        for i, ws in enumerate(starts):
            if ws - 0.05 <= s < ws + dur[i] + 0.05:
                out[i].append((n, s - ws, e - ws))
                break
    return out


def compare(ref, ren, dur, edge=0.08, tol=0.25):
    """Matches words one to one (each reference and each render word used once)."""
    inside = lambda w: w[1] >= edge and w[2] <= dur - edge  # noqa: E731
    used_ren, used_ref = set(), set()
    missing, split = [], []
    for i, w in enumerate(ref):
        if not inside(w):
            continue
        hit = next((k for k, r in enumerate(ren) if k not in used_ren and edit1(w[0], r[0]) and abs(r[1] - w[1]) <= tol), None)
        if hit is not None:
            used_ren.add(hit)
            used_ref.add(i)
            continue
        pair = next((k for k in range(len(ren) - 1)
                     if k not in used_ren and k + 1 not in used_ren and ren[k][0] + ren[k + 1][0] == w[0]
                     and abs(ren[k][1] - w[1]) <= tol), None)
        if pair is not None:
            used_ren.update({pair, pair + 1})
            used_ref.add(i)
            split.append(w[0])
        else:
            missing.append(f"{w[0]}@{w[1]:.2f}")
    # Edge words the reference has but the window cut: they may pair with render words.
    for i, w in enumerate(ref):
        if i in used_ref or inside(w):
            continue
        hit = next((k for k, r in enumerate(ren) if k not in used_ren and edit1(w[0], r[0]) and abs(r[1] - w[1]) <= tol), None)
        if hit is not None:
            used_ren.add(hit)
            used_ref.add(i)
    added = [f"{r[0]}@{r[1]:.2f}" for k, r in enumerate(ren)
             if k not in used_ren and inside(r) and r[0] not in FILLERS]
    return missing, split, added


def fail(out, msg):
    """A tool error (not a join failure): exit 2, the report says why."""
    report = {"error": msg, "ok": False}
    if out:
        Path(out).write_text(json.dumps(report, indent=1) + "\n")
    print(msg, file=sys.stderr)
    sys.exit(2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", required=True)
    ap.add_argument("--render", required=True)
    ap.add_argument("--reference", required=True)
    ap.add_argument("--out")
    ap.add_argument("--half", type=float, default=1.5)
    a = ap.parse_args()
    try:
        plan = json.loads(Path(a.plan).read_text())
        js, total = joins(plan)
    except (OSError, ValueError, KeyError) as e:
        fail(a.out, f"plan: {e}")
    if not js:
        report = {"joins": [], "ok": True}
        if a.out:
            Path(a.out).write_text(json.dumps(report, indent=1) + "\n")
        print(json.dumps(report))
        return
    wins = [(max(0.0, j["t"] - a.half), min(total, j["t"] + a.half)) for j in js]
    dur = [t1 - t0 for t0, t1 in wins]
    starts, t = [], 0.0
    for d in dur:
        starts.append(t)
        t += d + 1.0
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        sides = {}
        for side, media in (("render", a.render), ("reference", a.reference)):
            wav = tmp / f"{side}.wav"
            try:
                windows_wav(media, wins, wav)
                (tmp / side).mkdir()
                sides[side] = per_window(transcribe(wav, tmp / side), starts, dur)
            except subprocess.CalledProcessError as e:
                fail(a.out, f"{side}: {e.cmd[0]} failed: {(e.stderr or b'').decode(errors='replace')[-2000:]}")
    if sum(len(w) for w in sides["reference"]) == 0:
        fail(a.out, "the reference transcribed to no words at all (wrong file, silence, or an out.json format change)")
    rows = []
    for i, j in enumerate(js):
        missing, split, added = compare(sides["reference"][i], sides["render"][i], dur[i])
        checked = len(sides["reference"][i]) > 0
        rows.append({"part": j["part"], "t": round(j["t"], 3), "reference_words": len(sides["reference"][i]),
                     "checked": checked, "missing": missing, "split": split, "added": added,
                     "ok": not (missing or split or added)})
    report = {"joins": rows, "ok": all(r["ok"] for r in rows),
              "unchecked": [r["part"] for r in rows if not r["checked"]]}
    text = json.dumps(report, indent=1)
    if a.out:
        Path(a.out).write_text(text + "\n")
    print(text)
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
