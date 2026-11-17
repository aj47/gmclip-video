#!/bin/zsh
# M2 performance rows (SPEC §13 [M2], backlog U.18 / A2.05 / AJ.02), run on the Air from a
# copied release build. Writes a Markdown table you can paste into milestones/m2/perf.md.
#
#   [ASR_VIDEO=<a long stream>] [SCRUB_A=<(a)> SCRUB_B=<(b)>] tools/m2_perf.sh /Applications/gmclip.app /path/to/any.mp4 [project-home.gmclip] > perf-air.md
#
# What it measures:
#   1. cold start — process start to the UI's first render (5 launches, median); ≤ 1.5 s
#   2. `gmclip-app perf --video …` — open a 500-part project (≤ 1 s), the hot commands'
#      p99 (≤ 2 ms, R-CMD-8), a 1 000-op agent burst with the human's p99 (≤ 16.7 ms,
#      R-EVT-1, core side), resident size after opening
#   3. idle CPU (< 1 %) and memory of the windowed app on Home, 30 s after a 10 s settle
#   4. with a project home: memory while S2a plays the preview (≤ 1.2 GB)
#   4b. memory with ASR (≤ 2.2 GB, S11): `gmclip-app perf --asr` runs the whole analysis of
#      ASR_VIDEO (default: the video above) and samples the resident size every 100 ms.
#      A 1 h stream adds about 40 min on the dev machine.
#   5. H.05, when SCRUB_A / SCRUB_B name sources (a) and (b) (test-media/manifest.toml):
#      `gmclip-app perf --scrub` — the whole scrub proxy of (a) (≤ 10 min), scrub frames
#      per second from the proxy (≥ 20), exact seek p50/p95 by GOP class (≤ 2 s: 60/150 ms,
#      ≤ 5 s: 150/400 ms). Adds ~10 min.
# The window opens and closes by itself; leave the Mac alone while it runs (~4 min).
set -eu
APP=${1:?usage: m2_perf.sh <gmclip.app|gmclip-app> <video.mp4> [project.gmclip]}
VIDEO=${2:?a video}
PROJ=${3:-}
BIN=$APP
[[ -d $APP ]] && BIN="$APP/Contents/MacOS/gmclip-app"
[[ -x $BIN ]] || { echo "no gmclip-app at $BIN" >&2; exit 2; }

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

echo "# M2 performance on $(sysctl -n hw.model) ($(sw_vers -productVersion)), $(date '+%Y-%m-%d %H:%M')"
echo
echo "Build: $("$BIN" --version 2>/dev/null || shasum -a 256 "$BIN" | cut -c1-16)"
echo

# 1. cold start
starts=()
for i in 1 2 3 4 5; do
  ms=$("$BIN" --perf-cold-start 2>/dev/null | sed -n 's/^GMCLIP_PERF cold_start_ms=//p' | head -1)
  starts+=(${ms:-NaN})
  sleep 2
done
cold=$(printf '%s\n' "${starts[@]}" | median)

# 2. in-process rows
perf=$("$BIN" perf --video "$VIDEO")
j() { print -r -- "$perf" | /usr/bin/python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }

# 3. idle CPU and memory on Home
"$BIN" >/dev/null 2>&1 &
pid=$!
sleep 10
cpu=(); rss=()
for i in $(seq 30); do
  read c r <<< "$(ps -o %cpu=,rss= -p $pid)"
  cpu+=($c); rss+=($r); sleep 1
done
kill $pid 2>/dev/null || true; wait $pid 2>/dev/null || true
idle=$(printf '%s\n' "${cpu[@]}" | awk '{s+=$1} END {printf "%.2f", s/NR}')
home_mb=$(printf '%s\n' "${rss[@]}" | sort -n | tail -1 | awk '{printf "%.0f", $1/1024}')

# 4. playing (S2a) memory
play_mb="—"
if [[ -n $PROJ ]]; then
  "$BIN" --open "$PROJ" --s2a >/dev/null 2>&1 &
  pid=$!
  peak=0
  while kill -0 $pid 2>/dev/null; do
    r=$(ps -o rss= -p $pid 2>/dev/null || echo 0); (( r > peak )) && peak=$r; sleep 0.5
  done
  play_mb=$(( peak / 1024 ))
fi

# 4b. ASR memory
aperf=$("$BIN" perf --asr "${ASR_VIDEO:-$VIDEO}")
ja() { print -r -- "$aperf" | /usr/bin/python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }

# 5. scrub proxy, scrub, exact seek (H.05)
scrub_rows=""
js() { print -r -- "$sperf" | /usr/bin/python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
for pair in "a:${SCRUB_A:-}" "b:${SCRUB_B:-}"; do
  name=${pair%%:*}; src=${pair#*:}
  [[ -z $src ]] && continue
  full=(); [[ $name == a ]] && full=(--full-proxy)
  sperf=$("$BIN" perf --scrub "$src" "${full[@]}")
  if [[ $name == a ]]; then
    scrub_rows+="| Scrub proxy for ($name): $(js 'd["proxy"]["frames"]') frames | $(js 'round(d["proxy"]["seconds"],1)') s ($(js 'round(d["proxy"]["fps"])') fps) | ≤ 600 s |"$'\n'
  fi
  scrub_rows+="| Scrub from the proxy ($name) | $(js 'round(d["scrub"]["frames_per_s"])') distinct frames/s | ≥ 20 |"$'\n'
  scrub_rows+="| Exact seek ($name, GOP ≤ $(js 'round(d["exact_seek"]["class_s"])') s; longest $(js 'round(d["gop_s"]["longest"],2)') s) p50 / p95 | $(js 'round(d["exact_seek"]["p50_ms"])') / $(js 'round(d["exact_seek"]["p95_ms"])') ms | ≤ $(js 'round(d["exact_seek"]["budget_ms"][0])') / $(js 'round(d["exact_seek"]["budget_ms"][1])') ms |"$'\n'
done

cat <<MD
| Row | Measured | Target |
|---|---|---|
| Cold start (median of 5) | ${cold} ms (${starts[*]}) | ≤ 1500 ms |
| Open project, $(j 'd["open_project"]["parts"]') parts (open / open+plan+view) | $(j 'round(d["open_project"]["open_ms"])') / $(j 'round(d["open_project"]["open_plan_view_ms"])') ms | ≤ 1000 ms |
| Hot commands p99 (worst of trim, reorder, crop, split+undo) | $(j 'round(d["hot_commands"]["worst_p99_ms"],3)') ms | ≤ 2 ms (R-CMD-8) |
| R-EVT-1: human p99 during a 1 000-op agent burst (core) | $(j 'round(d["r_evt_1"]["human_p99_ms"],3)') ms ($(j 'd["r_evt_1"]["agent_ops"]') agent ops in $(j 'round(d["r_evt_1"]["agent_s"],2)') s) | ≤ 16.7 ms |
| Idle CPU on Home (30 s mean) | ${idle} % | < 1 % |
| Memory on Home / while playing (S2a) | ${home_mb} MB / ${play_mb} MB | ≤ 1.2 GB editing + playing |
| Memory with ASR running, peak ($(ja 'round(d["source_s"]/60,1)') min source, $(ja 'd["phase"]'), $(ja 'round(d["real_time_factor"],1)')× real time) | $(ja 'round(d["rss_mb"]["peak"])') MB (transcribing $(ja 'round(d["rss_mb"]["by_phase"].get("transcribing",0))'), analysing $(ja 'round(d["rss_mb"]["by_phase"].get("analysing",0))')) | ≤ 2200 MB |
${scrub_rows%$'\n'}
MD
