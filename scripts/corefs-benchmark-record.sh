#!/usr/bin/env bash
# Run the CoreFS-vs-ext4 benchmark, archive the TSV under perf-history/
# with a timestamp + label, and diff the corefs-fuse rows against the
# current baseline (perf-history/baseline.tsv).  Non-zero exit if any
# workload regressed beyond the threshold.
#
# Usage:
#   scripts/corefs-benchmark-record.sh <label> [--threshold-pct N]
#
# <label> is a short slug (e.g. "p2-append", "post-commit-abc123").
#
# The threshold (default: 15 %) applies to the numeric perf metric
# (ops/s or MiB/s).  A workload regresses if the new number is lower
# than baseline × (1 - threshold).  Workloads without a numeric metric
# (stat, delete) are compared on ms (lower = better).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PERF_DIR="${REPO_ROOT}/perf-history"
BASELINE="${PERF_DIR}/baseline.tsv"

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <label> [--threshold-pct N]" >&2
  exit 2
fi

LABEL="$1"; shift
THRESHOLD_PCT=15
while [[ $# -gt 0 ]]; do
  case "$1" in
    --threshold-pct)
      THRESHOLD_PCT="$2"; shift 2 ;;
    *)
      echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if ! [[ "$LABEL" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "label must be [A-Za-z0-9._-]+ (got: $LABEL)" >&2
  exit 2
fi

mkdir -p "$PERF_DIR"
STAMP="$(date -u +%Y-%m-%d_%H%M%S)"
ARTEFACT="${PERF_DIR}/${STAMP}_${LABEL}.tsv"

echo "[*] running benchmark (label=${LABEL})"
"${SCRIPT_DIR}/corefs-benchmark-vs-ext4.sh"

WORK="${WORK:-/tmp/corefs-bench}"
RESULTS="${WORK}/results.tsv"
if [[ ! -s "$RESULTS" ]]; then
  echo "no results file produced at $RESULTS" >&2
  exit 1
fi

cp "$RESULTS" "$ARTEFACT"
echo "[*] archived to $ARTEFACT"

if [[ ! -s "$BASELINE" ]]; then
  echo "[!] no baseline to compare against (${BASELINE}); skipping diff"
  exit 0
fi

echo
echo "==== REGRESSION CHECK vs baseline (threshold: ${THRESHOLD_PCT}%) ===="

# Compare only corefs-fuse rows.  For each workload, extract the
# numeric perf metric (ops/s or MiB/s) and compare.  Rows without a
# numeric metric (delete, stat) fall back to comparing ms.
python3 - "$BASELINE" "$ARTEFACT" "$THRESHOLD_PCT" <<'PY'
import sys, re, os

baseline_path, new_path, threshold_pct = sys.argv[1], sys.argv[2], float(sys.argv[3])
threshold = threshold_pct / 100.0

def parse(p):
    rows = {}
    with open(p) as f:
        next(f, None)  # header
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 4: continue
            fs, wl, ms, metric = parts[0], parts[1], parts[2], parts[3]
            if fs != "corefs-fuse": continue
            rows[wl] = (int(ms), metric)
    return rows

base = parse(baseline_path)
new = parse(new_path)

def perf_value(metric, ms):
    # Prefer the explicit ops/s or MiB/s number; fall back to 1/ms.
    m = re.match(r"\s*(\d+(?:\.\d+)?)\s*(ops/s|MiB/s)\s*$", metric)
    if m:
        return float(m.group(1)), m.group(2), "higher_better"
    return float(ms), "ms", "lower_better"

regressions = []
improvements = []
same = []
width = max((len(w) for w in set(list(base) + list(new))), default=0)

for wl in sorted(set(list(base) + list(new))):
    if wl not in base:
        print(f"  NEW     {wl:<{width}}  only in new run")
        continue
    if wl not in new:
        print(f"  MISSING {wl:<{width}}  only in baseline")
        continue
    base_ms, base_metric = base[wl]
    new_ms, new_metric = new[wl]
    bv, bu, bdir = perf_value(base_metric, base_ms)
    nv, nu, ndir = perf_value(new_metric, new_ms)
    if bu != nu or bdir != ndir:
        print(f"  UNIT?   {wl:<{width}}  base={bv}{bu} new={nv}{nu}")
        continue
    if bv == 0:
        pct = float("inf") if nv > 0 else 0.0
    else:
        pct = (nv - bv) / bv if bdir == "higher_better" else (bv - nv) / bv

    # Noise floor: sub-10 ms workloads are dominated by scheduler jitter,
    # FUSE wakeup latency, and /usr/bin/date granularity.  Reporting them
    # as regressions on a 1-2 ms swing produces more false positives than
    # useful signal.  Below the floor we classify by absolute-ms delta
    # (≥ 3 ms) rather than relative %.
    noise_floor_ms = 10
    if base_ms < noise_floor_ms and new_ms < noise_floor_ms:
        abs_delta_ms = new_ms - base_ms
        if abs_delta_ms >= 3:
            tag = "REGRESS"
            regressions.append(wl)
        elif abs_delta_ms <= -3:
            tag = "BETTER"
            improvements.append(wl)
        else:
            tag = "SAME"
            same.append(wl)
    elif pct >= threshold:
        tag = "BETTER"
        improvements.append(wl)
    elif pct <= -threshold:
        tag = "REGRESS"
        regressions.append(wl)
    else:
        tag = "SAME"
        same.append(wl)
    arrow = "↑" if pct > 0 else ("↓" if pct < 0 else "=")
    print(f"  {tag:<7} {wl:<{width}}  {bv:>10g}{bu} → {nv:>10g}{nu}  {arrow}{pct*100:+7.1f}%")

print()
print(f"summary: {len(improvements)} better, {len(regressions)} regress, {len(same)} same (±{threshold_pct}%)")

if regressions:
    print(f"regressions: {', '.join(regressions)}")
    sys.exit(1)
PY
