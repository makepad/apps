#!/bin/bash
# SLICEFAST: perf profile of one bench case (Linux, perf installed): self time
# by symbol, inclusive time by symbol, and folded stacks for a flame graph
# (../bench/prof/CASE-tN.folded: "frame;frame;... count" per line).
#   bash apps/commercial/fab/slice/bench/slicefast_prof.sh CASE [THREADS] [GOLDEN]
set -eu
CASE=$1
T=${2:-1}
GOLDEN=${3:-local/fabgold}
# A frame-pointer build with line tables in its own target dir, so perf can
# walk the stacks (fp) and attribute inlined code to source lines.
FP=$CARGO_TARGET_DIR/../fp-target
RUSTFLAGS="-C force-frame-pointers=yes" CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release -p makepad-fab-slice --bin fab-slicebench -j8 --target-dir "$FP" 2>&1 | tail -1
BIN=$FP/release/fab-slicebench
M=../bench/models
P=../bench/prof
mkdir -p "$M" "$P"
[ -f "$M/painted.3mf" ] || "$BIN" gen "$M"
D="$P/$CASE-t$T.data"
perf record -q -F 199 --call-graph fp -o "$D" -- "$BIN" run "$CASE" --models "$M" --golden "$GOLDEN" --threads "$T"
echo "--- self"
perf report -i "$D" --no-children --sort symbol --percent-limit 0.7 --stdio -g none 2>/dev/null | grep -v '^#' | grep -v '^$' | head -40 | cut -c1-200
echo "--- self by source line (top)"
perf report -i "$D" --no-children --sort srcline --percent-limit 0.5 --stdio -g none 2>/dev/null | grep -v '^#' | grep -v '^$' | head -60 | cut -c1-160
echo "--- inclusive"
perf report -i "$D" --children --sort symbol --percent-limit 1.5 --stdio -g none 2>/dev/null | grep -v '^#' | grep -v '^$' | head -70 | cut -c1-200
perf script -i "$D" 2>/dev/null | awk '
  /^\S/ { if (n) { s=""; for (i=n; i>=1; i--) s = s (s==""?"":";") st[i]; c[s]++ } n=0; next }
  /^\s+[0-9a-f]+ / { f=$0; sub(/^\s+[0-9a-f]+ /, "", f); sub(/ \(.*\)$/, "", f); sub(/\+0x[0-9a-f]+$/, "", f); gsub(/;/, ":", f); st[++n]=f; next }
  END { if (n) { s=""; for (i=n; i>=1; i--) s = s (s==""?"":";") st[i]; c[s]++ } for (k in c) print k, c[k] }' > "$P/$CASE-t$T.folded"
echo "folded: $P/$CASE-t$T.folded ($(wc -l < "$P/$CASE-t$T.folded") stacks)"
rm -f "$D"
