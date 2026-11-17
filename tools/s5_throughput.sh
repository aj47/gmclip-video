#!/bin/sh
# S5 / R-ML-1 throughput on a quiet machine (lane D; run by AJ when no lane is building).
#
#   tools/s5_throughput.sh            # waits for a quiet machine, then measures
#   tools/s5_throughput.sh --force    # measures now, load recorded
#
# Measures the whole loc_4fadb756 voice track (51.6 min; source (a)) through gmclip's
# ASR (Parakeet TDT v3 int8, parakeet-rs, ORT 1.30 CPU EP) three times at 8 and at 4
# intra-op threads, plus VAD and the analysis pass, and writes
# spikes/S5/results-<host>-<date>.txt with the machine, load averages before/after each
# run, × real time and the EP report. R-ML-1: ≥ 20× real time on mac-ref (the Air).
# On another Mac the lab media folder must hold preview/loc_4fadb756.proxy.mp4
# (GMCLIP_LAB_MEDIA, tools/lab_media.sh) and ffmpeg must be at
# /opt/homebrew/bin/ffmpeg (dev-only, decodes the test audio).
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0
host=$(scutil --get LocalHostName 2>/dev/null || hostname)
mkdir -p spikes/S5
out="spikes/S5/results-$host-$(date +%Y%m%d-%H%M).txt"
load() { sysctl -n vm.loadavg | awk '{print $2}'; }

sh tools/fetch_models.sh >/dev/null
cargo build -q --release -p gmclip-ml --features lab --tests

if [ "${1:-}" != "--force" ]; then
  echo "waiting for the 1-minute load average to stay under 1.5 for 2 minutes (Ctrl-C, or --force)…"
  quiet=0
  while [ $quiet -lt 4 ]; do
    if [ "$(load | awk '{print ($1 < 1.5)}')" = 1 ]; then quiet=$((quiet + 1)); else quiet=0; fi
    sleep 30
  done
fi

{
  echo "# S5 throughput, $(date)"
  echo "machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null)P+$(sysctl -n hw.perflevel1.physicalcpu 2>/dev/null)E cores, $(($(sysctl -n hw.memsize) / 1073741824)) GB, macOS $(sw_vers -productVersion)"
  echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo -dirty)"
} > "$out"
log="$out.log"
run() { # label, command…: full output to the log, the result lines to the summary
  label=$1; shift
  echo "## $label load-before=$(load)" >> "$out"
  if "$@" >> "$log" 2>&1; then status=ok; else status="FAILED (exit $?)"; fi
  echo "status: $status" >> "$out"
}
# alternate thread counts (no thermal ordering bias on the fanless Air), cool down between
for run_n in 1 2 3; do
  for threads in 8 4; do
    : > "$log.cur"
    run "ASR threads=$threads run=$run_n" sh -c "GMCLIP_ASR_THREADS=$threads cargo test -q --release -p gmclip-ml --features lab --test lab_asr full_stream -- --ignored --nocapture > '$log.cur' 2>&1"
    cat "$log.cur" >> "$log"
    grep -E 'real time|agreement|EP:' "$log.cur" >> "$out" || echo "no result lines (see $log)" >> "$out"
    echo "load-after=$(load)" >> "$out"
    sleep 120
  done
done
: > "$log.cur"
run "VAD + analysis" sh -c "cargo test -q --release -p gmclip-ml --features lab --test lab_vad -- --nocapture > '$log.cur' 2>&1 && cargo test -q --release -p gmclip-ml --features lab --test lab_edges whole -- --nocapture >> '$log.cur' 2>&1"
cat "$log.cur" >> "$log"
grep -E 'speech ratio|of audio:' "$log.cur" >> "$out" || echo "no result lines (see $log)" >> "$out"
rm -f "$log.cur"
echo "wrote $out"
cat "$out"
