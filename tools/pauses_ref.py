#!/usr/bin/env python3
"""Dev-only (tools/, never shipped): an independent reference for SPEC §5.5's pause sweep
(backlog D.03), in plain Python (no numpy, no ffmpeg), for crates/gmclip-cli/tests/
sweeps_lab.rs to compare gmclip's fragments against.

  pauses_ref.py PARTS_JSON

PARTS_JSON lists parts: {"id", "pcm": <raw little-endian f32 mono file>, "rate": 16000,
"min_s", "keep_s", "words": [[start_s, end_s], ...]} (times from the buffer's first
sample). Prints {"<id>": {"threshold_db", "cuts": [[from_s, to_s]], "skipped": [...]}}.

The rule, from the spec and gmclip-analysis' documented conventions:
- 40 ms RMS frames from the buffer's first sample (the last may be partial), dBFS
  floored at -120; the span's floor and speech levels are the 5th and 95th nearest-rank
  percentiles of the frames inside it; threshold = clamp(max(floor + 8, speech - 28),
  -55, -22); a frame is quiet below it; a pause is a run of quiet frames >= min_s.
- A pause never removes a word: each word is protected over its audible extent (18 dB
  under its loudest 10 ms frame, 5 ms hop, held 60 ms, the scan bounded by the
  neighbouring words' peaks and 1 s), falling back to its ASR times; a word whose
  loudest frame is under the threshold is not heard at all (decision 0048).
- The span's edges sit on its voice (decision 0049): its head goes up to 40 ms before
  its first heard word (a run above the threshold leading into the voice within 250 ms
  kept), its tail from 120 ms after its last, short of any frame as loud as that word's
  peak - 18 dB; each is cut when at least 0.12 s.
- Inside, each pause keeps keep_s of silence on each side (none at a span edge with no
  word heard); a piece shorter than min_s before that, or under 0.12 s after it, is not
  cut.
"""
import json
import math
import sys
from array import array
from pathlib import Path

FLOOR_DB = -120.0


def db_of(ms):
    return FLOOR_DB if ms <= 1e-12 else max(10.0 * math.log10(ms), FLOOR_DB)


def frames(x, window, hop):
    """dB of frames [i*hop, i*hop + window) (the last one partial)."""
    n = len(x)
    if n == 0:
        return []
    count = (max(n - window, 0) + hop - 1) // hop + 1
    pre = [0.0] * (n + 1)
    acc = 0.0
    for i, v in enumerate(x):
        acc += float(v) * float(v)
        pre[i + 1] = acc
    out = []
    for i in range(count):
        a = i * hop
        b = min(a + window, n)
        out.append(db_of((pre[b] - pre[a]) / (b - a)))
    return out


def frames_in(db, window, hop, start, end):
    first = -(-start // hop)
    last = (end - window) // hop + 1 if end >= window else 0
    first, last = min(first, len(db)), min(last, len(db))
    if last > first:
        return first, last
    if not db:
        return 0, 0
    i = min(start // hop, len(db) - 1)
    return i, i + 1


def percentile(v, p):
    v = sorted(v)
    rank = -(-(p * len(v)) // 100)
    return v[min(max(rank, 1), len(v)) - 1]


def ms(m, rate):
    return (m * rate + 500) // 1000


def peak(fine, w, s, e):
    a, b = frames_in(fine, 160, 80, s, e)
    best = None
    for i in range(a, b):
        if best is None or fine[i] > fine[best]:
            best = i
    return best


def kept_start(fine, voice, quiet, rate):
    """Where a part whose voice starts at `voice` starts: 40 ms before the run of frames
    above the room leading into the voice, when that run is at most 250 ms long."""
    win, hop = ms(10, rate), ms(5, rate)
    onset = voice
    i = max(voice - win, 0) // hop
    while 0 <= i < len(fine):
        if fine[i] <= quiet:
            break
        if voice - i * hop > ms(250, rate):
            onset = voice
            break
        onset = i * hop
        if i == 0:
            break
        i -= 1
    return max(min(onset, voice) - ms(40, rate), 0)


def audible(fine, words, span_end, rate):
    """(start, end) samples of each word's sound."""
    win, hop = ms(10, rate), ms(5, rate)
    hold = -(-(ms(60, rate) - win) // hop) + 1
    scan = ms(1000, rate)
    peaks = [peak(fine, None, s, e) for s, e in words]
    centre = [None if p is None else p * hop + win // 2 for p in peaks]
    out = []
    for k, (ws, we) in enumerate(words):
        p = peaks[k]
        if p is None:
            out.append((ws, we, None))
            continue
        thr = fine[p] - 18.0
        prev = centre[k - 1] if k > 0 and centre[k - 1] is not None else 0
        nxt = centre[k + 1] if k + 1 < len(words) and centre[k + 1] is not None else span_end
        # the end
        limit = min(nxt, we + scan)
        end = we
        i = p + 1
        while i < len(fine) and i * hop < limit:
            if fine[i] < thr and all(fine[j] < thr for j in range(i, min(i + hold, len(fine)))):
                end = i * hop
                break
            i += 1
        # the start
        limit = max(prev, ws - scan)
        start = ws
        i = p
        while i > 0:
            i -= 1
            if i * hop < limit:
                break
            if fine[i] < thr and all(fine[j] < thr for j in range(max(i - hold + 1, 0), i + 1)):
                start = i * hop + win
                break
        out.append((min(start, end), max(start, end), fine[p]))
    return out


def part(p, base):
    rate = p["rate"]
    x = array("f")
    x.frombytes((base / p["pcm"]).read_bytes())
    if sys.byteorder != "little":
        x.byteswap()
    n = len(x)
    cell = ms(40, rate)
    db = frames(x, cell, cell)
    s, e = 0, n
    a, b = frames_in(db, cell, cell, s, e)
    inside = db[a:b]
    thr = min(max(max(percentile(inside, 5) + 8.0, percentile(inside, 95) - 28.0), -55.0), -22.0)
    min_len = round(p["min_s"] * rate)
    keep = round(p["keep_s"] * rate)
    shortest = round(0.12 * rate)
    # quiet runs
    pauses, open_at = [], None
    for i, d in enumerate(inside + [None]):
        q = d is not None and d < thr
        if q and open_at is None:
            open_at = i
        elif not q and open_at is not None:
            pauses.append(((a + open_at) * cell, (a + i) * cell))
            open_at = None
    pauses = [(x0, x1) for x0, x1 in pauses if x1 - x0 >= min_len]
    fine = frames(x, ms(10, rate), ms(5, rate))
    words = [(min(max(round(ws * rate), 0), n), min(max(round(we * rate), 0), n)) for ws, we in p["words"]]
    words = [(ws, max(we, ws)) for ws, we in words if we > 0 and ws < n]
    # a word whose loudest frame is under the threshold is not heard (decision 0048)
    heard = [h for h in audible(fine, words, e, rate) if h[2] is None or h[2] >= thr]
    cuts, skipped = [], []
    # the part's edges on its voice (decision 0049): 40 ms before the first word is heard
    # (a soft onset within 250 ms kept), 120 ms after the last, short of a sound as loud
    # as the voice that is no word
    head, tail = s, e
    win, hop = ms(10, rate), ms(5, rate)
    # the words the span plays some of; an edge inside a word keeps that side whole
    played = [h for h in heard if h[1] > s and h[0] < e]
    first = played[0] if played and played[0][0] >= s else None
    last = played[-1] if played and played[-1][1] <= e else None
    if first is not None and first[2] is not None:
        a0, a1 = frames_in(fine, win, hop, s, first[0])
        loud = next((i for i in range(a0, a1) if fine[i] >= first[2] - 18.0), None)
        k = kept_start(fine, first[0], thr, rate)
        if loud is not None:
            k = min(k, max(loud * hop - ms(40, rate), 0))
        if k >= s + shortest:
            cuts.append([s / rate, k / rate])
            head = k
    if last is not None and last[2] is not None:
        a0, a1 = frames_in(fine, win, hop, last[1], e)
        loud = next((i for i in reversed(range(a0, a1)) if fine[i] >= last[2] - 18.0), None)
        k = last[1] + ms(120, rate)
        if loud is not None:
            k = max(k, loud * hop + win + ms(120, rate))
        if k + shortest <= e and k >= head:
            cuts.append([k / rate, e / rate])
            tail = k
    for x0, x1 in pauses:
        pa, pb = max(x0, s, head), min(x1, e, tail)
        if pb <= pa:
            continue
        gaps = [(pa, pb)]
        for ws, we, _ in heard:
            if not (ws < pb and pa < we):
                continue
            nxt = []
            for g0, g1 in gaps:
                if ws > g0:
                    nxt.append((g0, min(ws, g1)))
                if we < g1:
                    nxt.append((max(we, g0), g1))
            gaps = [(g0, g1) for g0, g1 in nxt if g1 > g0]
        for g0, g1 in gaps:
            if g1 - g0 < min_len:
                continue
            c0 = g0 if g0 <= s else g0 + keep
            c1 = g1 if g1 >= e else g1 - keep
            if c1 - c0 < shortest:
                skipped.append([g0 / rate, g1 / rate])
                continue
            cuts.append([c0 / rate, c1 / rate])
    return {"threshold_db": thr, "cuts": cuts, "skipped": skipped}


def main():
    path = Path(sys.argv[1])
    parts = json.loads(path.read_text())
    json.dump({p["id"]: part(p, path.parent) for p in parts}, sys.stdout)


if __name__ == "__main__":
    main()
