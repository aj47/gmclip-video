#!/usr/bin/env python3
"""Make the reference goldens for every manifest row that names a `goldens` file (W0-06).

Dev-only: shells out to ffprobe/ffmpeg (SPEC §3.1 `tools/`), never linked. For each
source it writes

  test-media/goldens/<id>.json            committed: hashes, counts, small samples
  test-media/local/goldens/<id>/*.txt     git-ignored: the full packet dumps

Packet dumps, one per stream and view, one line per packet in ffprobe's order (decode
order within the stream):

  "<pts> <dts> <size> <pos> <flags>"   pts/dts in the stream time base ("-" if unset),
                                       flags = K|_ then D|_ (keyframe, discard)

- view `edited`: ffprobe's default (edit lists applied; discarded pre-roll flagged D).
- view `raw`: MP4/MOV only, `-ignore_editlist 1` (the sample tables as stored).

The JSON records count, discard count, sha256 of the dump text and its first/last three
lines. The demux tests (B-01, B-02) rebuild the same text from their index and compare
the sha256; on a mismatch they diff against the full dump if it is present. Packet
durations are not in the line (the index carries them; ffprobe's MKV durations are not
specified).

Per stream the JSON also keeps ffprobe's codec facts (profile, level, has_b_frames,
pix_fmt, colour fields) and, for VP9, `vpcc_ref`: the vpcC payload ffmpeg's MP4 muxer
writes for a stream copy, the reference for B-03's vpcC synthesis.

Frames: on the first video stream, up to 200 presentation timestamps from the edited
view's non-discarded packets: always the first and last, the first 16 keyframes with
the frames just before and after each (the seek and edit-list edges), then filled from
splitmix64 seeded by fnv1a64(id) (draw `next() % n`, skip repeats); sorted. Each is
decoded by ffmpeg on its own (input seek 2 s before it, -copyts, select on the exact
pts, showinfo checked) and hashed as md5 of the luma plane and md5 of the interleaved
CbCr plane (NV12 layout, display size, no row padding). The chosen pts are in the JSON,
so a consumer need not replay the selection.

PCM: the first audio stream decoded by ffmpeg (-xerror) to f32le interleaved at its own
rate and channel count, edit lists and priming honoured: md5 of each whole second of
samples from the first decoded sample (the last chunk may be partial), plus the first
decoded frame's pts and sample count, so a consumer knows where sample 0 sits. For PCM
sources the md5s are decoder-independent (integer PCM scaled by 1/2^(bits-1)); for
lossy codecs (AAC) they hold for this ffmpeg build only (`bit_exact_across_decoders`).

`MP4Box -diso` box trees (W0-06): MP4Box is not installed on the lab, so MP4/MOV goldens
carry `"iso_tree": {"skipped": "MP4Box not installed"}`; with MP4Box on PATH they carry
the sha256 of its `-diso` XML instead.

The ffmpeg/ffprobe build must be the one `[corpus]` in the manifest names (another
build can apply edit lists differently); `--accept-version` overrides. Files are
written atomically (temp + rename, UTF-8, LF). Rerunning on the same inputs with the
same build rewrites byte-identical files.

usage: make_goldens.py [--only ID[,ID...]] [--jobs N] [--no-frames] [--accept-version]
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TM = ROOT / "test-media"
FFPROBE = os.environ.get("FFPROBE", "/opt/homebrew/bin/ffprobe")
FFMPEG = os.environ.get("FFMPEG", "/opt/homebrew/bin/ffmpeg")
N_FRAMES = 200
# Stream facts the codec readers (B-03) are checked against.
FACTS = ["profile", "level", "has_b_frames", "pix_fmt", "bits_per_raw_sample", "color_range", "color_space",
         "color_primaries", "color_transfer", "chroma_location"]
MASK = (1 << 64) - 1


def fnv1a64(s):
    h = 0xCBF29CE484222325
    for b in s.encode():
        h = ((h ^ b) * 0x100000001B3) & MASK
    return h


def splitmix64(state):
    while True:
        state = (state + 0x9E3779B97F4A7C15) & MASK
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        yield z ^ (z >> 31)


def choose(id_, candidates, keyframes):
    """Edges and keyframe neighbours first, then PRNG fill up to N_FRAMES."""
    n = len(candidates)
    if n <= N_FRAMES:
        return list(candidates)
    where = {p: i for i, p in enumerate(candidates)}
    picked = {0, n - 1}
    for k in sorted(keyframes)[:16]:
        i = where.get(k)
        if i is not None:
            picked.update(j for j in (i - 1, i, i + 1) if 0 <= j < n)
    for r in splitmix64(fnv1a64(id_)):
        if len(picked) >= N_FRAMES:
            break
        picked.add(r % n)
    return [candidates[i] for i in sorted(picked)]


def ffversion(tool):
    out = subprocess.run([tool, "-version"], capture_output=True, text=True, check=True).stdout
    return out.splitlines()[0].split(" Copyright")[0]


def write_atomic(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + f".{os.getpid()}.tmp")
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    os.replace(tmp, path)


def probe_streams(path):
    out = subprocess.run([FFPROBE, "-v", "error", "-show_entries",
                          "stream=index,codec_type,codec_name,time_base,width,height,sample_rate,channels,"
                          + ",".join(FACTS),
                          "-show_entries", "format=format_name", "-of", "json", str(path)],
                         capture_output=True, text=True, check=True).stdout
    return json.loads(out)


def packets(path, raw):
    cmd = [FFPROBE, "-v", "error"]
    if raw:
        cmd += ["-ignore_editlist", "1"]
    cmd += ["-show_entries", "packet=stream_index,pts,dts,size,pos,flags", "-of", "compact=p=0", str(path)]
    per = {}
    with subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True) as p:
        for line in p.stdout:
            kv = dict(x.split("=", 1) for x in line.strip().split("|") if "=" in x)
            flags = kv["flags"]
            canon = "{} {} {} {} {}{}".format(
                "-" if kv["pts"] == "N/A" else kv["pts"], "-" if kv["dts"] == "N/A" else kv["dts"],
                kv["size"], "-" if kv["pos"] == "N/A" else kv["pos"], "K" if flags[0] == "K" else "_",
                "D" if len(flags) > 1 and flags[1] == "D" else "_")
            per.setdefault(int(kv["stream_index"]), []).append(canon)
        if p.wait() != 0:
            raise RuntimeError(f"ffprobe failed on {path}")
    return per


def summarize(lines):
    text = "".join(l + "\n" for l in lines)
    return text, {
        "count": len(lines),
        "discarded": sum(1 for l in lines if l.endswith("D")),
        "sha256": hashlib.sha256(text.encode()).hexdigest(),
        "head": lines[:3],
        "tail": lines[-3:],
    }


def one_frame(path, vstream, pts, tb, w, h):
    """Decode exactly the frame with presentation timestamp `pts` (stream time base `tb`).

    Input-seeks to 2 s before it with -copyts, so timestamps stay the stream's own, and
    keeps the single frame whose pts equals the target (showinfo confirms which)."""
    num, den = (int(x) for x in tb.split("/"))
    t = pts * num / den - 2.0
    cmd = [FFMPEG, "-hide_banner", "-nostdin", "-v", "info", "-copyts"]
    if t > 0:
        cmd += ["-ss", f"{t:.6f}"]
    cmd += ["-i", str(path), "-map", f"0:{vstream}", "-vf", f"select='eq(pts,{pts})',showinfo",
            "-frames:v", "1", "-fps_mode", "passthrough", "-pix_fmt", "nv12", "-f", "rawvideo", "-"]
    r = subprocess.run(cmd, capture_output=True)
    ysize = w * h
    csize = 2 * ((w + 1) // 2) * ((h + 1) // 2)
    err = r.stderr.decode(errors="replace")
    if r.returncode != 0 or len(r.stdout) != ysize + csize:
        raise RuntimeError(f"{path}: frame at pts {pts} not decoded ({len(r.stdout)} bytes):\n{err[-1500:]}")
    shown = [int(m) for m in re.findall(r"\] n:\s*\d+ pts:\s*(-?\d+)", err)]
    if shown != [pts]:
        raise RuntimeError(f"{path}: wanted pts {pts}, showinfo saw {shown}")
    return hashlib.md5(r.stdout[:ysize]).hexdigest(), hashlib.md5(r.stdout[ysize:]).hexdigest()


def frame_hashes(path, vstream, pts_list, tb, w, h):
    with ThreadPoolExecutor(8) as ex:
        got = list(ex.map(lambda p: one_frame(path, vstream, p, tb, w, h), pts_list))
    return [g[0] for g in got], [g[1] for g in got]


def first_audio_frame(path, astream):
    out = subprocess.run([FFPROBE, "-v", "error", "-select_streams", str(astream), "-read_intervals", "%+2",
                          "-show_entries", "frame=pts,nb_samples", "-of", "json", str(path)],
                         capture_output=True, text=True, check=True).stdout
    f = (json.loads(out).get("frames") or [{}])[0]
    return {"pts": f.get("pts"), "samples": f.get("nb_samples")}


def pcm_hashes(path, astream, rate, channels):
    cmd = [FFMPEG, "-hide_banner", "-nostdin", "-v", "error", "-xerror", "-i", str(path), "-map",
           f"0:{astream}", "-c:a", "pcm_f32le", "-f", "f32le", "-"]
    chunk = rate * channels * 4
    out, total = [], 0
    with subprocess.Popen(cmd, stdout=subprocess.PIPE) as p:
        while True:
            b = p.stdout.read(chunk)
            if not b:
                break
            # read() may return short only at EOF on a pipe
            while len(b) < chunk:
                more = p.stdout.read(chunk - len(b))
                if not more:
                    break
                b += more
            out.append(hashlib.md5(b).hexdigest())
            total += len(b) // (4 * channels)
        if p.wait() != 0:
            raise RuntimeError(f"ffmpeg audio decode failed on {path}")
    return out, total


def boxes(data, start, end):
    off = start
    while off + 8 <= end:
        size, kind = struct.unpack(">I4s", data[off:off + 8])
        head = 8
        if size == 1:
            size = struct.unpack(">Q", data[off + 8:off + 16])[0]
            head = 16
        elif size == 0:
            size = end - off
        if size < head or off + size > end:
            raise RuntimeError(f"bad box at {off}")
        yield kind, off + head, off + size
        off += size


def find_box(data, start, end, path):
    for kind, b, e in boxes(data, start, end):
        if kind == path[0]:
            return (b, e) if len(path) == 1 else find_box(data, b, e, path[1:])
    return None


def vpcc_reference(path, idx):
    """The vpcC payload ffmpeg's MP4 muxer writes for this VP9 stream (stream copy of the
    first 2 s): the reference B-03's vpcC synthesis must equal. Found by walking
    moov/trak/mdia/minf/stbl/stsd/vp09/vpcC, never by searching the bytes."""
    tmpdir = TM / "local" / "tmp"
    tmpdir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=tmpdir) as d:
        out = Path(d) / "vpcc.mp4"
        subprocess.run([FFMPEG, "-hide_banner", "-nostdin", "-v", "error", "-y", "-t", "2", "-i", str(path),
                        "-map", f"0:{idx}", "-c", "copy", str(out)], check=True)
        data = out.read_bytes()
    stsd = find_box(data, 0, len(data), [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"])
    if not stsd:
        raise RuntimeError(f"{path}: no stsd in the remux")
    entry = find_box(data, stsd[0] + 8, stsd[1], [b"vp09"])
    if not entry:
        raise RuntimeError(f"{path}: no vp09 sample entry in the remux")
    vpcc = find_box(data, entry[0] + 78, entry[1], [b"vpcC"])
    if not vpcc:
        raise RuntimeError(f"{path}: no vpcC in the remux")
    return data[vpcc[0]:vpcc[1]].hex()


def iso_tree(path):
    exe = shutil.which("MP4Box")
    if not exe:
        return {"skipped": "MP4Box not installed"}
    xml = subprocess.run([exe, "-std", "-diso", str(path)], capture_output=True, check=True).stdout
    return {"tool": "MP4Box -std -diso", "bytes": len(xml), "sha256": hashlib.sha256(xml).hexdigest()}


def sha256_file(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def make(row, version, frames=True):
    id_ = row["id"]
    path = Path(row["path"])
    if not path.is_absolute():
        path = TM / path
    if not path.exists():
        if row["class"] == "corpus":
            raise RuntimeError(f"{id_}: corpus file missing ({path}); run tools/make_corpus.sh")
        return f"{id_}: SKIP (lab file not on this machine: {path})"
    sha = sha256_file(path)
    if sha != row["sha256"]:
        raise RuntimeError(f"{id_}: sha256 {sha} != manifest {row['sha256']}")
    info = probe_streams(path)
    is_mov = "mov" in info["format"]["format_name"]
    dump_dir = TM / "local" / "goldens" / id_
    shutil.rmtree(dump_dir, ignore_errors=True)
    dump_dir.mkdir(parents=True, exist_ok=True)
    views = {"edited": packets(path, False)}
    if is_mov:
        views["raw"] = packets(path, True)
    streams = []
    for s in info["streams"]:
        idx = s["index"]
        e = {"index": idx, "type": s["codec_type"], "codec": s.get("codec_name"), "time_base": s["time_base"]}
        for k in ("width", "height", "sample_rate", "channels"):
            if k in s:
                e[k] = int(s[k])
        facts = {k: s[k] for k in FACTS if k in s and s[k] not in ("unknown", "N/A")}
        if facts:
            e["ffprobe"] = facts
        if s.get("codec_name") == "vp9":
            e["vpcc_ref"] = vpcc_reference(path, idx)
        e["packets"] = {}
        for view, per in views.items():
            text, summ = summarize(per.get(idx, []))
            write_atomic(dump_dir / f"s{idx}.{view}.txt", text)
            e["packets"][view] = summ
        streams.append(e)
    doc = {
        "schema": "gmclip.goldens/0",
        "id": id_,
        "tool": "tools/make_goldens.py",
        "ffmpeg": version,
        "source": {"name": path.name, "size": path.stat().st_size, "sha256": sha},
        "packet_line": "<pts> <dts> <size> <pos> <K|_><D|_>",
        "streams": streams,
    }
    if is_mov:
        doc["iso_tree"] = iso_tree(path)
    v = next((s for s in streams if s["type"] == "video"), None)
    if v and frames:
        lines = [l for l in views["edited"][v["index"]] if not l.endswith("D") and not l.startswith("-")]
        cand = sorted({int(l.split()[0]) for l in lines})
        keys = {int(l.split()[0]) for l in lines if l.endswith("K_")}
        pts = choose(id_, cand, keys)
        ys, uvs = frame_hashes(path, v["index"], pts, v["time_base"], v["width"], v["height"])
        doc["frames"] = {
            "stream": v["index"],
            "format": "md5 of the Y plane and of the interleaved CbCr plane (NV12), display size, no padding",
            "selection": f"{len(pts)} of {len(cand)} presentable pts: first, last, the first 16 keyframes and "
                         f"their neighbours, then splitmix64(fnv1a64({id_!r}))",
            "pts": pts, "y_md5": ys, "uv_md5": uvs,
        }
    a = next((s for s in streams if s["type"] == "audio"), None)
    if a and frames:
        md5s, total = pcm_hashes(path, a["index"], a["sample_rate"], a["channels"])
        lossless = (a.get("codec") or "").startswith("pcm_")
        doc["pcm"] = {
            "stream": a["index"],
            "format": "f32le interleaved, native rate and channels, md5 per whole second",
            "decoder": "ffmpeg " + ("(integer PCM / 2^(bits-1))" if lossless else "(libavcodec, lossy)"),
            "bit_exact_across_decoders": lossless,
            "first_frame": first_audio_frame(path, a["index"]),
            "sample_rate": a["sample_rate"], "channels": a["channels"], "samples": total,
            "md5_per_second": md5s,
        }
    write_atomic(TM / row["goldens"], json.dumps(doc, indent=1, ensure_ascii=False) + "\n")
    return f"{id_}: " + ", ".join(
        f"s{s['index']} {s['type']} " + "/".join(f"{k} {p['count']}" for k, p in s["packets"].items())
        for s in streams)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only")
    ap.add_argument("--jobs", type=int, default=2)
    ap.add_argument("--no-frames", action="store_true")
    ap.add_argument("--accept-version", action="store_true")
    args = ap.parse_args()
    manifest = tomllib.loads((TM / "manifest.toml").read_text(encoding="utf-8"))
    rows = [r for r in manifest["file"] if r.get("goldens")]
    if args.only:
        want = set(args.only.split(","))
        rows = [r for r in rows if r["id"] in want]
    corpus = manifest.get("corpus", {})
    versions = []
    for tool, key in ((FFPROBE, "ffprobe_version"), (FFMPEG, "ffmpeg_version")):
        have = ffversion(tool)
        if corpus.get(key) and have != corpus[key] and not args.accept_version:
            sys.exit(f"make_goldens: {have!r} is not the corpus build {corpus[key]!r} (--accept-version)")
        versions.append(have)
    version = "; ".join(versions)
    with ThreadPoolExecutor(args.jobs) as ex:
        for msg in ex.map(lambda r: make(r, version, not args.no_frames), rows):
            print(msg, flush=True)


if __name__ == "__main__":
    sys.exit(main())
