#!/bin/zsh
# H.07 / R-EXP-3: does the Reels preset's bitrate keep screen text? For three screen-text
# reels, VMAF of gmclip's export against x264 `-preset slow -crf 14` of the same frames
# straight from the compositor (no encoder before x264; `vmaf_reference` lab test).
# Target: mean ≥ 95, 1%-low (mean of the worst 1 % of frames) ≥ 90. ffmpeg is a dev tool.
#
#   tools/h07_vmaf.sh <gmclip CLI (release)> <work dir> [kbps …]   (default: 16000)
#
# Each rate is the plan's target; max is 1.5× it, capped at the validator's 25 Mbps.
set -eu
CLI=${1:?the gmclip CLI}; WORK=${2:?a work dir}; shift 2
RATES=(${@:-16000})
FF=/opt/homebrew/bin/ffmpeg
REPO=${0:A:h:h}
mkdir -p "$WORK"
plans=()
LAB=${GMCLIP_LAB_MEDIA:-$HOME/Development/gmclip-lab-media}
# a golden plan with its lab paths resolved
labplan() { /usr/bin/python3 - "$1" "$2" "$LAB" <<'PY'
import json, os, sys
src, dst, lab = sys.argv[1:4]
d = json.load(open(src))
fix = lambda p: p if os.path.isabs(p) else os.path.join(lab, p)
for s in d["sources"]: s["path"] = fix(s["path"])
for f in d.get("fonts") or []: f["path"] = fix(f["path"])
json.dump(d, open(dst, "w"))
PY
}
labplan "$REPO/schemas/examples/h79-v4.renderplan.json" "$WORK/h79.json"; plans+=(h79)
for h in loc_086da141_h03 loc_bd89f38b_h53; do
  labplan "$REPO/lab/fixtures/golden-11/$h.renderplan.json" "$WORK/${h##*_}.json"
  plans+=(${h##*_})
done
rate() { /usr/bin/python3 - "$1" "$2" "$3" <<'PY'
import json, sys
src, dst, t = sys.argv[1], sys.argv[2], int(sys.argv[3]) * 1000
d = json.load(open(src))
d["output"]["video"]["rate"] = {"mode": "vbr", "target_bps": t, "max_bps": min(25_000_000, t * 3 // 2)}
json.dump(d, open(dst, "w"))
PY
}
echo "| Reel | preset kbps | measured kbps | VMAF mean | 1%-low | x264 crf 14 kbps |"
echo "|---|---|---|---|---|---|"
for p in $plans; do
  ( cd "$REPO" && GMCLIP_VMAF_PLAN="$WORK/$p.json" GMCLIP_VMAF_REF="$WORK/$p.ref.mp4" \
      cargo test -q --release -p gmclip-render --test vmaf_reference -- --ignored ) >/dev/null 2>&1
  for k in $RATES; do
    rate "$WORK/$p.json" "$WORK/$p.$k.json" $k
    "$CLI" render "$WORK/$p.$k.json" --out "$WORK/$p.$k.mp4" >/dev/null
    $FF -v error -y -i "$WORK/$p.$k.mp4" -i "$WORK/$p.ref.mp4" \
      -lavfi "[0:v]setpts=PTS-STARTPTS[d];[1:v]setpts=PTS-STARTPTS[r];[d][r]libvmaf=log_fmt=json:log_path=$WORK/$p.$k.vmaf.json:n_threads=8" -f null - 2>/dev/null
    /usr/bin/python3 - "$WORK/$p.$k.vmaf.json" "$p" "$k" "$WORK/$p.$k.mp4" "$WORK/$p.ref.mp4" <<'PY'
import json, subprocess, sys
d = json.load(open(sys.argv[1]))
v = sorted(f["metrics"]["vmaf"] for f in d["frames"])
low = v[: max(1, len(v) // 100)]
def kbps(p):
    o = subprocess.run(["/opt/homebrew/bin/ffprobe", "-v", "error", "-select_streams", "v:0",
                        "-show_entries", "stream=bit_rate", "-of", "csv=p=0", p], capture_output=True, text=True).stdout
    return int(o.strip()) // 1000
print(f"| {sys.argv[2]} | {sys.argv[3]} | {kbps(sys.argv[4])} | {sum(v)/len(v):.2f} | {sum(low)/len(low):.2f} | {kbps(sys.argv[5])} |")
PY
    rm -f "$WORK/$p.$k.mp4"
  done
  rm -f "$WORK/$p.ref.mp4"
done
