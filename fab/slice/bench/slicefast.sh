#!/bin/bash
# SLICEFAST: timed slices of bench cases on a node (inside a devhub lease, from
# the shipped makepad tree; release build). One JSON line per run, appended to
# ../bench/out/slicefast.jsonl and printed.
#   bash apps/apps/fab/slice/bench/slicefast.sh "CASE..." "THREADS..." [RUNS] [GOLDEN] [OUT]
# GOLDEN (default local/fabgold, never committed): inputs/presets/ and
# slices/<case>/ (the vendor reference run). Models are generated into
# ../bench/models (outside the tree, not in git). With OUT, the G-code of each
# run is kept there (for byte-identity diffs) and compared with the vendor
# reference (layers, estimate, filament, extents, arcs).
set -eu
CASES=$1
THREADS=$2
RUNS=${3:-1}
GOLDEN=${4:-local/fabgold}
OUT=${5:-}
BIN=$CARGO_TARGET_DIR/release/fab-slicebench
M=../bench/models
O=../bench/out
mkdir -p "$M" "$O"
[ -f "$M/painted.3mf" ] || "$BIN" gen "$M"
for c in $CASES; do
  for t in $THREADS; do
    for r in $(seq 1 "$RUNS"); do
      if [ -n "$OUT" ]; then
        "$BIN" run "$c" --models "$M" --golden "$GOLDEN" --threads "$t" --out "$OUT/$c-t$t" | tee -a "$O/slicefast.jsonl"
        "$BIN" compare "$c" --out "$OUT/$c-t$t" --ref "$GOLDEN/slices"
      else
        "$BIN" run "$c" --models "$M" --golden "$GOLDEN" --threads "$t" | tee -a "$O/slicefast.jsonl"
      fi
    done
  done
done
