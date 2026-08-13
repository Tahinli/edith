#!/usr/bin/env bash
# The decode/encode baseline: runs `examples/bench` over a list of real files
# and writes one TSV of medians. Every later codec change is compared against
# the file this produces, so it is meant to be run twice -- before and after --
# on an otherwise idle machine.
#
#   scripts/bench.sh <out_dir> [file ...]
#
# With no files it reads them from $BENCH_LIST (one path per line, `#` comments
# allowed), and failing that from assets/test_av.mp4. The list lives *outside*
# this repo on purpose: what is measured here is a personal media library and
# its file names are not this project's to record.
#
# Output in <out_dir>: baseline.tsv (the data) and bench.log (every run's
# stderr, decoder panics included).
#
# Methodology, so a rerun means the same thing:
#   * cold = the file's pages dropped with posix_fadvise(DONTNEED) immediately
#     before the measurement -- no root, nothing else on the machine evicted,
#     and advisory, so it is the optimistic end of cold.
#   * warm = the same call repeated straight after, cache as the cold run left it.
#   * medians of 5 for open and seek, 20 steps for a scrub, and for export as
#     many reps as $BENCH_EXPORT_CAP seconds leave room for (at least one,
#     which is then marked CAPPED if it did not finish).
#   * every metric runs as its own process: a decoder that panics costs one
#     row, not the run.
#   * the media files are opened read only; only <out_dir> is written to.
set -uo pipefail
cd "$(dirname "$0")/.."

OUT_DIR=${1:-}
if [ -z "$OUT_DIR" ]; then
    echo "usage: scripts/bench.sh <out_dir> [file ...]" >&2
    exit 2
fi
shift
mkdir -p "$OUT_DIR"
TSV="$OUT_DIR/baseline.tsv"
LOG="$OUT_DIR/bench.log"

# Absolute, because $CARGO_TARGET_DIR may already be one and the plugin path
# below must not become "$PWD/$HOME/...".
TARGET=$(realpath -m "${CARGO_TARGET_DIR:-target}")
cargo build --release -p engine -p engine-hw --example bench || exit 1
BENCH="$TARGET/release/examples/bench"
# The plugin is looked for beside the running executable, and an example binary
# lives one directory below the plugin: without this, hardware decode and
# hardware encode silently fall back to software and the baseline is a lie.
# Measured on the control file: seek to 600 s is 23 ms on the plugin and
# 1359 ms without it, and every export seat reads "SW".
export LD_LIBRARY_PATH="$TARGET/release${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

files=("$@")
if [ ${#files[@]} -eq 0 ] && [ -n "${BENCH_LIST:-}" ]; then
    while IFS= read -r line; do
        [ -z "$line" ] && continue
        case "$line" in \#*) continue ;; esac
        files+=("$line")
    done <"$BENCH_LIST"
fi
[ ${#files[@]} -eq 0 ] && files=(assets/test_av.mp4)

{
    echo "# bench run $(date -Is)"
    echo "# rev $(git rev-parse --short HEAD 2>/dev/null || echo '?')"
    echo "# $(uname -sr), $(nproc) cpus, $(free -g | awk '/^Mem:/ {print $2 " GiB ram"}')"
    echo "# caps: export ${BENCH_EXPORT_CAP:-300}s/${BENCH_EXPORT_REPS:-5} reps, ttff ${BENCH_TTFF_TIMEOUT:-180}s"
} | tee -a "$LOG"
printf 'file\tmetric\tunit\tn\tmedian\tmin\tmax\tnote\n' >"$TSV"

# One metric, one process, one row -- even when the process dies.
run() { # <timeout_s> <label> <file> <metric> [args...]
    local limit=$1 label=$2 file=$3 metric=$4
    shift 4
    local name=${file##*/} out rc
    echo "== $(date +%T) $metric ${*:-} $name" | tee -a "$LOG" >&2
    out=$(timeout -k 30 "$limit" "$BENCH" "$metric" "$file" "$@" 2>>"$LOG")
    rc=$?
    [ -n "$out" ] && printf '%s\n' "$out" >>"$TSV"
    if [ $rc -ne 0 ] && [ -z "$out" ]; then
        local why="exit $rc"
        [ $rc -eq 124 ] && why="timeout after ${limit}s"
        [ $rc -ge 128 ] && why="signal $((rc - 128)) (panic/abort — see bench.log)"
        printf '%s\t%s\t\t0\t\t\t\tFAIL(%s)\n' "$name" "$label" "$why" >>"$TSV"
    fi
}

export_cap=${BENCH_EXPORT_CAP:-300}
export_limit=$((export_cap * ${BENCH_EXPORT_REPS:-5} + 600))

for file in "${files[@]}"; do
    if [ ! -f "$file" ]; then
        printf '%s\tMISSING\t\t0\t\t\t\tFAIL(no such file)\n' "${file##*/}" >>"$TSV"
        continue
    fi
    run 3600 open "$file" open
    for t in 30 600 5400; do
        run 1800 "seek_ttff_${t}s" "$file" seek "$t"
    done
    run 3600 scrub "$file" scrub
    for seat in h264sw h264hw av1 hevc hevchw; do
        run "$export_limit" "export_fps_$seat" "$file" export "$seat" "$OUT_DIR"
    done
done

echo "# done $(date -Is)" | tee -a "$LOG"
column -t -s $'\t' "$TSV"
