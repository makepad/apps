#!/bin/bash
# Fab AI eval: for each task a fresh, hidden Fab under makepad run with the AI hub's Claude as its
# model (fleet discovery on, pinned to claude-cli), its own settings, chat and autosave folders.
# Run from the checkout root on a node with a GPU, in a devhub lease, e.g.:
#   makepad devhub run <lane> screen -- bash apps/apps/fab/tools/ai_eval/run.sh <tag> 0 1 2 3 4
# Each task prints an `EVAL {json}` line (see run.py); compare tags across builds.
set -u
TAG=$1; shift
T=${CARGO_TARGET_DIR:-$PWD/target}
export RELAX_STD=$PWD/libs/relax/compiler/std
nice cargo build --release -p makepad-run --bin makepad -j8 2>&1 | grep -E "^(warning|error)|Finished" | head
nice cargo build --release -p makepad-remote --bin makepad-remote -j8 2>&1 | grep -E "^(warning|error)|Finished" | head
for i in "$@"; do
  S=${TMPDIR:-/tmp}/fabeval-$i; mkdir -p "$S/data" "$S/config" "$S/state"
  export XDG_DATA_HOME=$S/data XDG_CONFIG_HOME=$S/config XDG_STATE_HOME=$S/state FAB_AUTOSAVE_DIR=$S/autosave MAKEPAD_AICHAT_DIR=$S/aichat
  export MAKEPAD_LAN=1 MAKEPAD_AI_PROVIDER=local MAKEPAD_AI_MODEL=claude-cli MAKEPAD_XR_STREAM=off
  (cd apps/apps && exec "$T/release/makepad" run makepad-fab > "$S/app.log" 2>&1) &
  APP=$!
  python3 apps/apps/fab/tools/ai_eval/run.py "$T/release/makepad-remote" "$APP" apps/apps/fab/tools/ai_eval/tasks.json "$i" "$TAG"
  kill "$APP"; wait "$APP" 2>/dev/null
done
echo "EVAL-END $TAG"
