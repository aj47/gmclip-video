#!/usr/bin/env python3
"""Writes the generated RenderPlan examples and invalid fixtures (W0-07).

Run from anywhere: `python3 -I schemas/examples/make_examples.py`. Deterministic, so a
rerun leaves `git status` clean. `crates/gmclip-plan/tests/{examples,invalid_fixtures}.rs`
check what it writes (valid plans accepted; each fixture refused with its pointer).

- h79-parts-1-3.renderplan.json   RP §2.3, copied verbatim from spec/renderplan-v0.md
- native-mode.renderplan.json     one source for picture and audio (RT `native` mode):
                                  a continuous join (no fades), hold / freeze_first,
                                  plate background, as_recorded loudness, no text
- bench-60-m0.renderplan.json     BENCH-60-M0-shaped (SPEC §17): 60 s, 40 discontinuous
                                  joins alternating (a) and (b) (MKV picture + proxy
                                  audio), 4 layers per part, 66 captions, 6 pills, a run
                                  clock, one gain envelope
- invalid/*.json + invalid/index.json   mutations of the h79 example, each with the
                                  pointer the validator must name
"""
import copy
import json
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "schemas", "examples")


def spec_h79():
    md = open(os.path.join(ROOT, "spec", "renderplan-v0.md"), encoding="utf-8").read()
    at = md.index("\n### 2.3")
    block = re.search(r"```json\n(.*?)```", md[at:], re.S).group(1)
    return block


def write(name, obj):
    with open(os.path.join(OUT, name), "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=1, ensure_ascii=False)
        f.write("\n")


def S(f, fps=30):
    return f * 48000 // fps


def aspect_place(crop, x, y, w):
    """A place of width w with the crop's aspect (height rounded to a pixel)."""
    return {"x": x, "y": y, "w": w, "h": round(w * crop["h"] / crop["w"])}


FADE = {"samples": 144, "shape": "linear"}
STYLES = {
    "Speech": {"font": "neometric-heavy", "size": 86, "fill": "#FFFFFFFF",
               "outline": {"width": 12, "colour": "#000000FF", "join": "round"},
               "shadow": {"dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline"}, "align": 5},
    "Hook": {"font": "neometric-heavy", "size": 88, "fill": "#FFFFFFFF",
             "outline": {"width": 12, "colour": "#000000FF", "join": "round"},
             "shadow": {"dx": 7, "dy": 7, "blur": 0, "colour": "#000000FF", "of": "fill_and_outline"}, "align": 5},
    "Pill": {"font": "neometric-heavy", "size": 50, "fill": "#FFFFFFFF",
             "box": {"colour": "#101010CF", "pad_x": 16, "pad_y": 16, "radius": 0, "geometry": "libass_bs3"},
             "letter_spacing": 1, "align": 7},
}
FONT = {"id": "neometric-heavy", "family": "Neometric", "style": "Heavy",
        "path": "fonts/Neometric Heavy (Bold).otf",
        "sha256": "9ece81e8896ebc740c5256985dcbbeee0251011e6df73f5e637c8e599ad6a76e", "em_per_size": 0.8332}
OUTPUT = {"canvas": {"w": 1080, "h": 1920}, "fps": {"num": 30, "den": 1},
          "color": {"matrix": "bt709", "transfer": "bt709", "primaries": "bt709", "range": "limited"},
          "video": {"codec": "h264", "profile": "high", "level": "4.0", "bframes": 2, "gop_frames": 30, "closed_gop": True,
                    "rate": {"mode": "vbr", "target_bps": 16000000, "max_bps": 24000000}, "timescale": 15360},
          "audio": {"codec": "aac_lc", "rate": 48000, "channels": 2, "bitrate": 192000},
          "container": {"moov_first": True, "edit_lists": False}}


def native_mode():
    src = {"id": "a-prime", "path": "test-media/public/a-prime-1440p30-h264.mp4",
           "expect": {"video": {"codec": "h264", "w": 2560, "h": 1440, "fps": {"num": 30, "den": 1}, "timescale": 15360},
                      "audio": {"codec": "aac", "rate": 48000, "channels": 2}}}
    screen = {"x": 640, "y": 180, "w": 1280, "h": 900}
    cam = {"x": 2060, "y": 634, "w": 496, "h": 340}
    lens = [90, 120, 150]
    parts, f = [], 0
    # part 2 continues part 1 on the same source (continuous join: no fades there);
    # part 3 jumps back (discontinuous: fades allowed).
    first = [3000, 3090, 1200]
    for i, (n, ff) in enumerate(zip(lens, first)):
        layers = [{"id": "screen", "role": "screen", "crop": screen, "place": aspect_place(screen, 60, 260, 960), "z": 0},
                  {"id": "cam", "role": "cam", "crop": cam, "place": aspect_place(cam, 160, 1120, 760), "z": 1}]
        if i == 1:
            layers[0]["time_map"] = {"mode": "hold", "frame": 3095,
                                     "provenance": {"kind": "human", "detail": {"note": "screen held on the prompt"}}}
        if i == 2:
            layers[0]["time_map"] = {"mode": "freeze_first"}
            layers[0]["picture_source"] = "a-prime"
            layers[1]["opacity"] = 0.85
            layers[1]["resample"] = "lanczos3"
        audio = {"source": "a-prime", "first_sample": S(ff), "rate": 48000}
        if i == 0:
            audio["fade_in"] = {"samples": 192, "shape": "raised_cosine"}
            audio["fade_out"] = None
        if i == 2:
            audio["fade_in"] = FADE
            audio["fade_out"] = FADE
            audio["gain_db"] = -2.5
        parts.append({"id": f"n{i + 1}", "label": ["opening", "continues", "callback"][i], "out": [f, f + n],
                      "picture": {"source": "a-prime", "first_frame": ff,
                                  "expect_pts": {"ticks": ff * 512, "timescale": 15360}},
                      "audio": audio, "layers": layers})
        f += n
    out = dict(OUTPUT, frames=f)
    return {"schema": "gmclip.renderplan/0",
            "meta": {"title": "native-mode example (one source for picture and audio)",
                     "generator": {"name": "schemas/examples/make_examples.py", "version": "1"}},
            "output": out,
            "background": {"kind": "plate", "stops": ["#0c1118", "#1a2433"]},
            "sources": [src], "parts": parts,
            "audio": {"loudness": {"mode": "as_recorded"}},
            "checks": {"safe_zones": "instagram"}}


def bench60():
    frames, nparts = 1800, 41
    a = {"id": "a", "path": "test-media/lab/a-1440p30-h264.mp4",
         "expect": {"video": {"codec": "h264", "w": 2560, "h": 1440, "fps": {"num": 30, "den": 1}},
                    "audio": {"codec": "aac", "rate": 48000, "channels": 2}}}
    b = {"id": "b-mkv", "path": "test-media/lab/b-source.mkv",
         "expect": {"video": {"codec": "vp9", "w": 2560, "h": 1440, "fps": {"num": 30, "den": 1}}}}
    bp = {"id": "b-proxy", "path": "test-media/lab/loc_4fadb756.proxy.mp4",
          "expect": {"audio": {"codec": "aac", "rate": 48000, "channels": 2}}}
    screen = {"x": 770, "y": 220, "w": 1280, "h": 900}
    cam = {"x": 2060, "y": 634, "w": 496, "h": 340}
    d0 = {"x": 1074, "y": 250, "w": 425, "h": 35}
    d1 = {"x": 900, "y": 600, "w": 600, "h": 100}
    parts, f = [], 0
    for i in range(nparts):
        n = 44 if i < frames - nparts * 43 else 43
        use_a = i % 2 == 0
        native = 30000 + 900 * i
        if use_a:
            pic, aud, first_frame = "a", "a", native
        else:
            pic, aud, first_frame = "b-mkv", "b-proxy", native - 1642
        layers = [
            {"id": "screen", "role": "screen", "crop": screen, "place": aspect_place(screen, 160, 420, 760), "z": 0,
             "time_map": {"mode": "live"}},
            {"id": "cam", "role": "cam", "crop": cam, "place": aspect_place(cam, 160, 1120, 760), "z": 1},
            {"id": "detail-0", "role": "detail", "crop": d0, "place": aspect_place(d0, 30, 960, 1020), "z": 2},
            {"id": "detail-1", "role": "detail", "crop": d1, "place": aspect_place(d1, 240, 1040, 600), "z": 3},
        ]
        parts.append({"id": f"p{i + 1:02d}", "out": [f, f + n],
                      "picture": {"source": pic, "first_frame": first_frame},
                      "audio": {"source": aud, "first_sample": S(native), "rate": 48000, "gain_db": 0,
                                "fade_in": FADE, "fade_out": FADE},
                      "native": {"frames": [native, native + n], "fps": {"num": 30, "den": 1}},
                      "layers": layers})
        f += n
    assert f == frames
    events = [{"id": "hook", "role": "text_hook", "out": [0, 60], "layer": 2, "style": "Hook", "size": 88,
               "pos": {"x": 540, "y": 305}, "align": 5,
               "runs": [{"text": "60 seconds,\n"}, {"text": "40 joins.", "fill": "#FFD400FF"}]}]
    for i in range(6):
        events.append({"id": f"pill-{i + 1}", "role": "section_pill", "out": [60 + 290 * i, 60 + 290 * (i + 1)],
                       "layer": 3, "style": "Pill", "size": 50, "pos": {"x": 48, "y": 262}, "align": 7,
                       "runs": [{"text": f"SECTION {i + 1}  "}, {"text": "bench", "fill": "#FFD400FF"}]})
    for s in range(10):
        events.append({"id": f"clock-{s:02d}", "role": "run_clock", "out": [600 + 30 * s, 630 + 30 * s], "layer": 3,
                       "style": "Pill", "size": 50, "pos": {"x": 48, "y": 330}, "align": 7,
                       "runs": [{"text": "GENERATING  "}, {"text": f"0:{s:02d}", "fill": "#FFD400FF"}],
                       "provenance": {"kind": "measurement", "detail": {"measurement": "queue 0->1 at native 30600"}}})
    span = frames - 60
    for i in range(66):
        a0 = 60 + span * i // 66
        a1 = 60 + span * (i + 1) // 66
        runs = [{"text": f"caption {i + 1:02d}"}]
        if i % 5 == 0:
            runs = [{"text": "key", "fill": "#FFD400FF"}, {"text": f" word {i + 1:02d}"}]
        events.append({"id": f"c{i + 1:02d}", "role": "caption", "out": [a0, a1], "layer": 1, "style": "Speech",
                       "size": 86, "pos": {"x": 540, "y": 1065}, "runs": runs})
    return {"schema": "gmclip.renderplan/0",
            "meta": {"title": "BENCH-60-M0 (shape)", "generator": {"name": "schemas/examples/make_examples.py", "version": "1"}},
            "output": dict(OUTPUT, frames=frames),
            "background": {"kind": "colour", "rgb": "#0c1118"},
            "sources": [a, b, bp], "fonts": [FONT], "text_styles": STYLES,
            "parts": parts, "text_events": events,
            "audio": {"envelopes": [{"id": "lift", "out_samples": [S(300), S(600)], "gain_db": 10,
                                     "ramp_in_samples": 3840, "ramp_out_samples": 5760, "curve": "linear_db"}],
                      "loudness": {"mode": "static", "integrated_lufs": -16, "true_peak_dbtp": -1.5}},
            "checks": {"safe_zones": "instagram"}}


# ---------------------------------------------------------------------------------
# Invalid fixtures: (file stem, rule, expected code, expected pointer, mutation).
# Distinct from the RP §2.7 reject rows, which tests/reject_rows.rs replays in Rust.

def m_set(path, value):
    def f(p):
        *head, last = path
        node = p
        for k in head:
            node = node[k]
        node[last] = value
    return f


def m_del(path):
    def f(p):
        *head, last = path
        node = p
        for k in head:
            node = node[k]
        del node[last]
    return f


def m_raw(fn):
    """A mutation of the serialised text (for things JSON objects cannot hold)."""
    fn.raw = True
    return fn


def second_font_same_id(p):
    f = copy.deepcopy(p["fonts"][0])
    f["path"] = "fonts/other.otf"
    p["fonts"].append(f)


def second_envelope_same_id(p):
    e = copy.deepcopy(p["audio"]["envelopes"][0])
    e["out_samples"] = [200000, 300000]
    p["audio"]["envelopes"].append(e)


def overlapping_envelopes(p):
    e = copy.deepcopy(p["audio"]["envelopes"][0])
    e["id"] = "second"
    e["out_samples"] = [80000, 120000]
    p["audio"]["envelopes"].append(e)


FIXTURES = [
    ("range-empty", "1", "Invalid", "/text_events/2/out", m_set(["text_events", 2, "out"], [55, 55])),
    ("parts-overlap", "2", "Invalid", "/parts/1/out", m_set(["parts", 1, "out"], [50, 176])),
    ("first-part-not-zero", "2", "Invalid", "/parts/0/out", m_set(["parts", 0, "out"], [1, 55])),
    ("parts-past-frames", "2", "Invalid", "/output/frames", m_set(["output", "frames"], 396)),
    ("aspect-off", "5", "Invalid", "/parts/1/layers/0/place", m_set(["parts", 1, "layers", 0, "place", "h"], 560)),
    ("layer-role-unknown", "5", "Invalid", "/parts/0/layers/1/role", m_set(["parts", 0, "layers", 1, "role"], "camera")),
    ("dup-part-id", "10", "Invalid", "/parts/2/id", m_set(["parts", 2, "id"], "01-payoff-banana")),
    ("dup-layer-id", "10", "Invalid", "/parts/2/layers/2/id", m_set(["parts", 2, "layers", 2, "id"], "cam")),
    ("dup-source-id", "10", "Invalid", "/sources/1/id", m_set(["sources", 1, "id"], "mkv1440")),
    ("layer-source-dangling", "10", "Invalid", "/parts/2/layers/2/picture_source",
     m_set(["parts", 2, "layers", 2, "picture_source"], "mkv2160")),
    ("audio-source-dangling", "10", "Invalid", "/parts/0/audio/source", m_set(["parts", 0, "audio", "source"], "wav")),
    ("envelopes-overlap", "7", "Invalid", "/audio/envelopes/1/out_samples", overlapping_envelopes),
    ("envelope-past-end", "4", "Invalid", "/audio/envelopes/0/out_samples",
     m_set(["audio", "envelopes", 0, "out_samples"], [1600, 700000])),
    ("fade-longer-than-part", "7", "Invalid", "/parts/0/audio/fade_out",
     m_set(["parts", 0, "audio", "fade_out"], {"samples": 88001, "shape": "linear"})),
    ("sha256-uppercase", "schema", "Invalid", "/sources/0/fingerprint/sha256",
     m_set(["sources", 0, "fingerprint", "sha256"], "13ED3A2671AD918F9E1F79DEA18E2B44D30A4293CE106AE44E3BBBFA3F2DDB48")),
    ("canvas-odd", "schema", "Invalid", "/output/canvas/w", m_set(["output", "canvas", "w"], 1081)),
    ("fps-den-2", "4", "Invalid", "/output/fps/den", m_set(["output", "fps", "den"], 2)),
    ("frames-not-integer", "1", "Invalid", "/output/frames", m_set(["output", "frames"], 397.0)),
    ("id-with-space", "schema", "Invalid", "/parts/0/id", m_set(["parts", 0, "id"], "01 payoff")),
    ("schema-version", "12", "Invalid", "/schema", m_set(["schema"], "gmclip.renderplan/2")),
    ("styles-missing", "schema", "Invalid", "/text_styles", m_del(["text_styles"])),
    ("true-peak-positive", "8", "Invalid", "/audio/loudness/true_peak_dbtp",
     m_set(["audio", "loudness", "true_peak_dbtp"], 0.5)),
    ("time-map-mode-unknown", "schema", "Invalid", "/parts/0/layers/0/time_map/mode",
     m_set(["parts", 0, "layers", 0, "time_map"], {"mode": "loop"})),
    ("sources-empty", "schema", "Invalid", "/sources", m_set(["sources"], [])),
    ("negative-first-frame", "1", "Invalid", "/parts/0/picture/first_frame",
     m_set(["parts", 0, "picture", "first_frame"], -1)),
    ("duplicate-json-key", "11", "Invalid", "/output/frames",
     m_raw(lambda text: text.replace('"frames": 397', '"frames": 397, "frames": 398', 1))),
    ("long-unknown-key", "11", "Invalid", "/meta/a_very_long_unkn…",
     m_set(["meta", "a_very_long_unknown_key_that_should_be_cut"], 1)),
    ("fps-num-float", "1", "Invalid", "/output/fps/num", m_set(["output", "fps", "num"], 30.0)),
    ("audio-rate-float", "1", "Invalid", "/parts/1/audio/rate", m_set(["parts", 1, "audio", "rate"], 48000.0)),
    ("dup-font-id", "10", "Invalid", "/fonts/1/id", second_font_same_id),
    ("dup-envelope-id", "10", "Invalid", "/audio/envelopes/1/id", second_envelope_same_id),
    ("native-frames-empty", "1", "Invalid", "/parts/0/native/frames",
     m_set(["parts", 0, "native", "frames"], [81254, 81254])),
    ("first-frame-past-i64", "3", "Invalid", "/parts/0/picture/first_frame",
     m_set(["parts", 0, "picture", "first_frame"], 9223372036854775807)),
    ("ramp-in-too-long", "7", "Invalid", "/audio/envelopes/0/ramp_in_samples",
     m_set(["audio", "envelopes", 0, "ramp_in_samples"], 1000000)),
    ("picture-fps-mismatch", "3", "Unsupported", "/sources/0/expect/video/fps",
     m_set(["sources", 0, "expect", "video", "fps"], {"num": 60, "den": 1})),
    ("audio-rate-44k", "4", "Unsupported", "/sources/1/expect/audio/rate",
     m_set(["sources", 1, "expect", "audio", "rate"], 44100)),
    ("audio-track-zero", "4", "Invalid", "/sources/1/audio_track", m_set(["sources", 1, "audio_track"], 0)),
]


def main():
    text = spec_h79()
    with open(os.path.join(OUT, "h79-parts-1-3.renderplan.json"), "w", encoding="utf-8") as f:
        f.write(text)
    write("native-mode.renderplan.json", native_mode())
    write("bench-60-m0.renderplan.json", bench60())

    inv = os.path.join(OUT, "invalid")
    os.makedirs(inv, exist_ok=True)
    for name in os.listdir(inv):
        if name.endswith(".json"):
            os.remove(os.path.join(inv, name))
    index = []
    for stem, rule, code, path, fn in FIXTURES:
        base = json.loads(text)
        if getattr(fn, "raw", False):
            out = fn(json.dumps(base, indent=1, ensure_ascii=False))
        else:
            fn(base)
            out = json.dumps(base, indent=1, ensure_ascii=False)
        with open(os.path.join(inv, stem + ".json"), "w", encoding="utf-8") as f:
            f.write(out + "\n")
        index.append({"file": stem + ".json", "rule": rule, "code": code, "path": path})
    with open(os.path.join(inv, "index.json"), "w", encoding="utf-8") as f:
        json.dump(index, f, indent=1, ensure_ascii=False)
        f.write("\n")


if __name__ == "__main__":
    main()
