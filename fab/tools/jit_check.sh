#!/bin/bash
# jit_check.sh: Fab under the JIT, from the root of a makepad tree with apps/apps (a devhub
# lease's shipped tree): `makepad check` of the Fab app, `makepad test` of its model crates, and a
# headless cold run of the app that must report `present`. Prints "FABJIT-OK" or "FABJIT-FAIL: <steps>".
#   makepad devhub run <lane> test --needs arch=x64 -- bash apps/apps/fab/tools/jit_check.sh
T=${CARGO_TARGET_DIR:-$PWD/target}
B=$T/release/makepad
log=$(mktemp -d "${TMPDIR:-/tmp}/fabjit.XXXXXX") || exit 2
trap 'rm -rf -- "$log"' EXIT
fail=""
export RELAX_STD=$PWD/libs/relax/compiler/std

CARGO_INCREMENTAL=0 cargo build --release -p makepad-run -p makepad-remote --bin makepad --bin makepad-remote -j8 > "$log/build" 2>&1 || { tail -20 "$log/build"; echo "FABJIT-FAIL: build"; exit 1; }

PKG=${FABJIT_PKG:-makepad-fab}; DIR=${FABJIT_DIR:-apps/apps}
if [ -n "${FABJIT_RUN_ONLY:-}" ]; then :; else
full=$(cd apps/apps && $B check makepad-fab 2>&1)
echo "$full" | grep -B2 -A12 -E "^error|: error" | head -60
line=$(echo "$full" | grep -E " errors$|load error" | tail -1)
echo "check makepad-fab: $line"
echo "$line" | grep -qE "(^| )0 errors$" || fail="$fail check"

for pkg in fab-plate fab-model fab-ai; do
  out=$(cd apps/apps && $B test $pkg 2>&1)
  res=$(echo "$out" | grep -E "^test result" | tail -1)
  echo "test $pkg: $res"
  echo "$res" | grep -q "^test result: ok" || { fail="$fail test-$pkg"; echo "$out" | grep -E "panicked|FAILED|error" | head -10; }
done

fi
s=$log/run; mkdir -p "$s"
(cd $DIR && exec env RELAX_HOME=$s/home RELAX_CACHE_DIR=$s/home/cache RELAX_TARGET_DIR=$s/home/target \
  MAKEPAD_HIDE_WINDOWS=1 MAKEPAD_MUTE=1 MAKEPAD_TRACE=startup $B run --report-to $s/report $PKG > $s/out.log 2>&1) &
pid=$!
for i in $(seq 1 360); do
  grep -q present $s/report 2>/dev/null && break
  kill -0 $pid 2>/dev/null || break
  sleep 0.5
done
sleep 3
n=$(grep -cx present $s/report 2>/dev/null)
kill -0 $pid 2>/dev/null && kill $pid
wait $pid 2>/dev/null
echo "makepad run $PKG present: $n"
grep -E "\[E\]|panic|fault" $s/out.log | head -10
[ "$n" = 1 ] || fail="$fail present"
grep -qE "\[E\]|panicked" $s/out.log && fail="$fail run-errors"

if [ -n "$fail" ]; then echo "FABJIT-FAIL:$fail"; exit 1; fi
echo "FABJIT-OK"
