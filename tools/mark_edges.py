#!/usr/bin/env python3
"""Dev-only R-ML-2 marking tool (SPEC §9; F18): AJ hand-marks word edges, which
gmclip's edge refinement (audible edges + the arbiter) is then scored against.

    tools/mark_edges.py build <vid> [<vid> …] --out DIR [--n 60] [--seed 1]
    tools/mark_edges.py serve DIR [--port 8790]        # open http://127.0.0.1:8790/
    cargo run -p gmclip-ml --release --features lab --example score_edges -- DIR/marks-*.json

`build` picks, per stream, `n` edges from the stream's transcript in the lab media
folder (`asr/<vid>/out.json`, read-only) whose gaps are **measured** on the audio (the
longest run 18 dB under the pair's peak; that transcript stretches word ends to the next
word's start, so its own gaps say nothing): half abutting pairs (gap < 0.3 s, sentence
boundaries included), a quarter word ends before a pause (≥ 0.4 s) and a quarter word
starts after one, spread over the stream, skipping fillers and one-letter words. It cuts a WAV clip per edge from the stream's
proxy (`cache/preview/<vid>.proxy.mp4`) with ffmpeg and writes `marks-<vid>.json`
(format `gmclip.edge-marks.v1`) and the page.

The page shows the clip's waveform and the two words' **text only**: no ASR time and no
gmclip guess is shown or pre-filled, so the marks are independent of what is measured.
Click to put the cursor (snapped to 5 ms), Space plays the second before the cursor
and stops on it ("play to cursor"), P plays from the cursor, ←/→ move it 5 ms
(Shift: 50 ms), Enter saves and goes to the next edge, K marks the item unusable, Z
zooms. Marks are saved into marks-<vid>.json by the local server on every Enter.

What to mark (also shown on the page):
  abutting  the latest point that keeps word 1 whole and nothing of word 2
  end       where word 1's sound ends
  start     where word 2's sound starts
"""
import argparse
import hashlib
import http.server
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

LAB = Path(os.environ.get("GMCLIP_LAB_MEDIA") or Path.home() / "Development/gmclip-lab-media")
FFMPEG = "/opt/homebrew/bin/ffmpeg"
FILLERS = {"um", "umm", "ummm", "uh", "uhh", "uhhh", "uhm", "erm", "mm", "mhm"}
PAD = 1.0  # seconds of audio either side of the pair


def norm(w):
    return re.sub(r"[^a-z0-9' ]", "", w.lower()).strip()


def words_of(vid):
    d = json.loads((LAB / "asr" / vid / "out.json").read_text())
    return [(w["word"], float(w["start"]), float(w["end"])) for s in d["sentences"] for w in s["words"]]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def levels(audio):
    """10 ms RMS level (dBFS) of the whole stream's first audio track, 16 kHz mono."""
    import numpy as np
    raw = subprocess.run([FFMPEG, "-v", "error", "-nostdin", "-i", str(audio), "-map", "0:a:0", "-vn",
                          "-ac", "1", "-ar", "16000", "-f", "f32le", "-"], check=True, capture_output=True).stdout
    x = np.frombuffer(raw, dtype=np.float32)
    n = len(x) // 160
    ms = (x[: n * 160].astype(np.float64).reshape(n, 160) ** 2).mean(axis=1)
    return 10 * np.log10(np.maximum(ms, 1e-12))


def gap_ms(db, a, b):
    """The longest quiet run between two words, measured: frames 18 dB under the pair's
    loudest frame (gmclip's audible-edge rule), searched between the words' middles."""
    f = lambda t: int(round(t * 100))
    peak = db[f(a[1]):max(f(b[2]), f(a[1]) + 1)].max()
    best = run = 0
    for v in db[f((a[1] + a[2]) / 2):f((b[1] + b[2]) / 2)]:
        run = run + 1 if v < peak - 18 else 0
        best = max(best, run)
    return best * 10


def pick(words, db, n, rng):
    """Half abutting pairs (measured gap < 0.3 s), a quarter word ends and a quarter
    word starts at a measured pause (≥ 0.4 s), spread over the stream."""
    ok = lambda w: len(norm(w[0])) > 1 and norm(w[0]) not in FILLERS
    ab, pauses = [], []
    for i in range(1, len(words) - 2):
        a, b = words[i], words[i + 1]
        if not (ok(a) and ok(b)) or b[2] - a[1] > 3.0 or b[1] < a[1]:
            continue
        g = gap_ms(db, a, b)
        if g < 300:
            ab.append((i, g))
        elif g >= 400:
            pauses.append((i, g))
    quotas = [("abutting", ab, n // 2), ("end", pauses, n // 4), ("start", pauses, n - n // 2 - n // 4)]
    chosen, used = [], set()
    for kind, pool, q in quotas:
        # spread: split the pool into q strata in stream order, one pick per stratum
        pool = [c for c in pool if not ({c[0] - 1, c[0], c[0] + 1} & used)]
        if len(pool) < q:
            sys.exit(f"only {len(pool)} {kind} candidates")
        for k in range(q):
            lo, hi = len(pool) * k // q, len(pool) * (k + 1) // q
            i, g = pool[rng.randrange(lo, hi)]
            if i in used:
                continue
            used.add(i)
            chosen.append((kind, i, g))
    chosen.sort(key=lambda c: c[1])
    return chosen


def build(args):
    out = Path(args.out).resolve()
    (out / "clips").mkdir(parents=True, exist_ok=True)
    for vid in args.vids:
        f = out / f"marks-{vid}.json"
        if f.exists():
            sys.exit(f"{f} exists: refusing to overwrite marks")
        rng = random.Random(f"{args.seed}/{vid}")  # per stream: order-independent
        audio = LAB / "preview" / f"{vid}.proxy.mp4"
        words = words_of(vid)
        db = levels(audio)
        items = []
        for kind, i, g in pick(words, db, args.n, rng):
            a, b = words[i], words[i + 1]
            cs, ce = max(0.0, a[1] - PAD), b[2] + PAD
            iid = f"{vid}#{i}"
            wav = out / "clips" / f"{vid}-{i}.wav"
            if not wav.exists():
                subprocess.run([FFMPEG, "-v", "error", "-nostdin", "-ss", f"{cs:.3f}", "-t", f"{ce - cs:.3f}",
                                "-i", str(audio), "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "48000",
                                "-c:a", "pcm_s16le", "-y", str(wav)], check=True)
            items.append({"id": iid, "kind": kind,
                          "w1": {"text": a[0], "start": a[1], "end": a[2]},
                          "w2": {"text": b[0], "start": b[1], "end": b[2]},
                          "clip_start": cs, "clip_end": ce, "clip": f"clips/{wav.name}", "gap_ms": g,
                          "mark": None, "skipped": False, "note": ""})
        f.write_text(json.dumps({"format": "gmclip.edge-marks.v1", "stream": vid,
                                 "audio_path": str(audio), "audio_sha256": sha256(audio),
                                 "items": items}, indent=1))
        print(f"{vid}: {len(items)} edges → {f}")
    (out / "index.html").write_text(PAGE)
    print(f"serve with: {sys.argv[0]} serve {out}")


LOCK = threading.Lock()


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_POST(self):
        name = self.path.lstrip("/")
        port = self.server.server_address[1]
        origin = self.headers.get("Origin")
        if origin and origin not in (f"http://127.0.0.1:{port}", f"http://localhost:{port}"):
            return self.send_error(403, "foreign origin")
        if self.headers.get("Content-Type", "").split(";")[0].strip() != "application/json":
            return self.send_error(415)  # forces a CORS preflight from any other page
        if not re.fullmatch(r"marks-[A-Za-z0-9_-]+\.json", name):
            return self.send_error(400)
        path = Path(self.directory) / name
        try:
            doc = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            with LOCK:
                cur = json.loads(path.read_text())
                if doc.get("format") != "gmclip.edge-marks.v1" or doc.get("stream") != cur["stream"]:
                    raise ValueError("not this stream's marks")
                if [i["id"] for i in doc["items"]] != [i["id"] for i in cur["items"]]:
                    raise ValueError("items differ from the file")
                for new, old in zip(doc["items"], cur["items"]):
                    m = new.get("mark")
                    if m is not None and not (isinstance(m, (int, float)) and old["clip_start"] <= m <= old["clip_end"]):
                        raise ValueError(f"mark outside the clip: {old['id']}")
                    if not isinstance(new.get("skipped"), bool):
                        raise ValueError("skipped")
                    # only AJ's fields change
                    old["mark"], old["skipped"], old["note"] = m, new["skipped"], str(new.get("note", ""))[:500]
                fd, tmp = tempfile.mkstemp(dir=self.directory, prefix=".marks-", suffix=".tmp")
                with os.fdopen(fd, "w") as f:
                    json.dump(cur, f, indent=1)
                os.replace(tmp, path)
                with open(path.with_suffix(".history.jsonl"), "a") as h:
                    h.write(json.dumps({"t": time.time(), "items": [[i["id"], i["mark"], i["skipped"]] for i in cur["items"]]}) + "\n")
        except (ValueError, KeyError, TypeError) as e:
            return self.send_error(400, str(e)[:200])
        self.send_response(204)
        self.end_headers()

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def serve(args):
    d = str(Path(args.dir).resolve())
    files = sorted(p.name for p in Path(d).glob("marks-*.json"))
    (Path(d) / "files.json").write_text(json.dumps(files))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", args.port),
                                          lambda *a, **k: Handler(*a, directory=d, **k))
    print(f"http://127.0.0.1:{args.port}/  ({len(files)} streams)")
    srv.serve_forever()


PAGE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Mark edges</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root{--bg:#fafafa;--fg:#111;--mute:#666;--wave:#3a6ea5;--cur:#d33;--box:#fff;--line:#ddd}
@media (prefers-color-scheme:dark){:root{--bg:#141414;--fg:#eee;--mute:#999;--wave:#7fb0e6;--cur:#ff6b6b;--box:#1e1e1e;--line:#333}}
body{background:var(--bg);color:var(--fg);font:15px/1.4 system-ui,sans-serif;margin:0;padding:16px}
canvas{width:100%;height:220px;background:var(--box);border:1px solid var(--line);cursor:crosshair;display:block}
.row{display:flex;gap:12px;align-items:center;flex-wrap:wrap;margin:8px 0}
.words{font-size:26px} .w1{font-weight:700} .w2{font-weight:700} .kind{color:var(--mute)}
button,select{font:inherit;padding:4px 10px} .mute{color:var(--mute)} kbd{border:1px solid var(--line);padding:0 4px;border-radius:3px}
</style></head><body>
<div class="row"><select id="file"></select><span id="prog" class="mute"></span><span id="saved" class="mute"></span></div>
<div class="row words"><span class="w1" id="w1"></span><span class="mute">|</span><span class="w2" id="w2"></span></div>
<div class="row kind" id="what"></div>
<canvas id="c"></canvas>
<div class="row"><span>cursor <b id="cur">—</b></span><span class="mute" id="zoom"></span>
<button id="prev">◀ prev</button><button id="play">▶ to cursor</button><button id="save">save ↵</button><button id="skip">unusable</button><button id="next">next ▶</button></div>
<div class="row mute">click: cursor (5 ms snap) · <kbd>Space</kbd> play the 1 s before the cursor · <kbd>P</kbd> play from cursor · <kbd>←</kbd>/<kbd>→</kbd> 5 ms (<kbd>Shift</kbd> 50 ms) · <kbd>Z</kbd> zoom · <kbd>Enter</kbd> save + next · <kbd>K</kbd> unusable</div>
<script>
const WHAT={abutting:"Mark the LATEST point that keeps word 1 whole and NOTHING of word 2.",
 end:"Mark where word 1's sound ENDS (the pause follows).",start:"Mark where word 2's sound STARTS (after the pause)."};
let doc=null,name=null,idx=0,buf=null,pcm=null,cur=null,zoom=false,actx=new AudioContext(),src=null,busy=false,gen=0;
const $=id=>document.getElementById(id), cv=$("c"), g=cv.getContext("2d");
async function load(n){name=n;doc=await (await fetch(n)).json();idx=doc.items.findIndex(i=>i.mark==null&&!i.skipped);if(idx<0)idx=0;await show();}
function snap(t){return Math.round(t*200)/200}
function view(){const d=buf.duration;if(!zoom)return[0,d];const c=cur??d/2;return[Math.max(0,c-0.4),Math.min(d,c+0.4)]}
function draw(){const W=cv.width=cv.clientWidth*devicePixelRatio,H=cv.height=cv.clientHeight*devicePixelRatio;
 const st=getComputedStyle(document.documentElement);g.clearRect(0,0,W,H);const[a,b]=view(),sr=buf.sampleRate;
 g.fillStyle=st.getPropertyValue('--wave');for(let x=0;x<W;x++){const i0=Math.floor((a+(b-a)*x/W)*sr),i1=Math.max(i0+1,Math.floor((a+(b-a)*(x+1)/W)*sr));
 let lo=1,hi=-1;for(let i=i0;i<i1&&i<pcm.length;i++){const v=pcm[i];if(v<lo)lo=v;if(v>hi)hi=v}if(hi<lo)continue;
 const s=Math.max(Math.abs(lo),Math.abs(hi));const db=Math.max(0,1+20*Math.log10(s+1e-9)/60);g.fillRect(x,H/2-db*H/2,1,Math.max(1,db*H))}
 if(cur!=null){g.fillStyle=st.getPropertyValue('--cur');g.fillRect((cur-a)/(b-a)*W-1,0,2,H)}
 $("cur").textContent=cur==null?"—":(cur*1000).toFixed(0)+" ms into the clip";$("zoom").textContent=zoom?"zoom ±400 ms (log level)":"whole clip (log level)"}
async function show(){const my=++gen,it=doc.items[idx];buf=null;$("w1").textContent=it.w1.text;$("w2").textContent=it.w2.text;
 $("what").textContent=`[${it.kind}] `+WHAT[it.kind]+(it.skipped?"  (marked unusable)":"");
 $("prog").textContent=`${idx+1} / ${doc.items.length} · ${doc.items.filter(i=>i.mark!=null||i.skipped).length} done`;
 const b=await actx.decodeAudioData(await (await fetch(it.clip)).arrayBuffer());if(my!==gen)return;buf=b;pcm=buf.getChannelData(0);
 cur=it.mark==null?null:it.mark-it.clip_start;zoom=false;draw()}
function stop(){if(src){try{src.stop()}catch(e){}src=null}}
async function play(from,to){await actx.resume();stop();src=actx.createBufferSource();src.buffer=buf;src.connect(actx.destination);from=Math.max(0,from);src.start(0,from,Math.max(0.01,to-from))}
cv.onclick=e=>{const r=cv.getBoundingClientRect(),[a,b]=view();cur=snap(a+(b-a)*(e.clientX-r.left)/r.width);draw()};
async function step(d){if(busy||!buf)return;const it=doc.items[idx],dirty=cur!=null&&(it.mark==null||Math.abs(it.mark-(it.clip_start+cur))>1e-6);
 if(dirty&&!confirm("Leave this edge without saving the cursor?"))return;busy=true;try{const j=idx+d;if(j>=0&&j<doc.items.length){idx=j;await show()}}finally{busy=false}}
async function save(next){if(busy||!buf)return;busy=true;try{const it=doc.items[idx];if(cur==null&&!it.skipped)return;
 if(cur!=null){it.mark=+(it.clip_start+cur).toFixed(3);it.skipped=false}
 let ok=false;try{const r=await fetch(name,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(doc)});ok=r.ok}catch(e){}
 $("saved").textContent=ok?"saved "+new Date().toLocaleTimeString():"SAVE FAILED: not saved";if(ok&&next&&idx<doc.items.length-1){idx++;await show()}}finally{busy=false}}
$("save").onclick=()=>save(true);$("next").onclick=()=>step(1);$("prev").onclick=()=>step(-1);$("play").onclick=()=>{if(cur!=null)play(cur-1,cur)};
$("skip").onclick=async()=>{if(busy||!buf)return;doc.items[idx].skipped=true;doc.items[idx].mark=null;cur=null;await save(true)};
document.onkeydown=async e=>{if(!buf||e.repeat)return;const k=e.key;if((k==="Enter"||k===" ")&&e.target.tagName==="BUTTON"){e.preventDefault();e.target.blur()}if(k===" "){e.preventDefault();if(cur!=null)play(cur-1,cur)}
 else if(k==="p"||k==="P"){if(cur!=null)play(cur,buf.duration)}else if(k==="ArrowLeft"||k==="ArrowRight"){e.preventDefault();
 cur=snap(Math.min(buf.duration,Math.max(0,(cur??0)+(k==="ArrowLeft"?-1:1)*(e.shiftKey?0.05:0.005))));draw()}
 else if(k==="Enter"){await save(true)}else if(k==="k"||k==="K"){$("skip").onclick()}else if(k==="z"||k==="Z"){zoom=!zoom;draw()}};
(async()=>{const files=await (await fetch("files.json")).json();for(const f of files){const o=document.createElement("option");o.textContent=f;$("file").append(o)}
 $("file").onchange=e=>{if(!busy)load(e.target.value)};if(files.length)await load(files[0])})();
addEventListener("resize",()=>buf&&draw());
</script></body></html>
"""


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("vids", nargs="+")
    b.add_argument("--out", required=True)
    b.add_argument("--n", type=int, default=60)
    b.add_argument("--seed", type=int, default=1)
    s = sub.add_parser("serve")
    s.add_argument("dir")
    s.add_argument("--port", type=int, default=8790)
    a = ap.parse_args()
    build(a) if a.cmd == "build" else serve(a)


if __name__ == "__main__":
    main()
