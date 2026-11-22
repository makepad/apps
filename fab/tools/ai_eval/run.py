# Fab AI eval, one task: the task's prompt and one "check your work" follow-up through Fab's chat
# (the bridge's `ai say`), then a mechanical score from Fab's own tools (fab.inspect, fab.check,
# fab.measure where the build has it): part count, closed meshes, no check errors, each expected part's
# size (sorted dimensions within size_tol) and volume range, and for gears a clearance without overlap.
# Prints one `EVAL {json}` line: success, wall seconds, tool calls, failed calls, assistant turns, score.
# Usage: run.py <makepad-remote> <fab pid> <tasks.json> <task index> <tag>   (run.sh drives it)
import json, subprocess, sys, time
remote, target, task_path, index, tag = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5]
task = json.load(open(task_path))[index]

def app(*a, timeout=60):
    # A Fab whose model worker hangs must not end the run: an unanswered call reads as empty.
    try:
        r = subprocess.run([remote, "app", target, *a], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return '{"err":"timeout"}'
    return r.stdout.strip()

def j(text):
    try:
        return json.loads(text[text.find("{"):])
    except Exception:
        return {}

def transcript():
    d = j(app("ai", "transcript"))
    return None if not d or "err" in d else d

def call(tool, args="{}"):
    d = j(app("call", f"fab.{tool}", args, timeout=300))
    # A service reply: {"Result":[{"text": "<the tool's JSON>", ...}]}.
    res = d.get("Result")
    if isinstance(res, list) and res and isinstance(res[0].get("text"), str):
        try:
            return json.loads(res[0]["text"])
        except Exception:
            return {"raw": res[0]["text"]}
    for k in ("text", "result"):
        if isinstance(d.get(k), str):
            try:
                return json.loads(d[k])
            except Exception:
                return {"raw": d[k]}
    return d

t0 = time.time()
while time.time() - t0 < 600 and transcript() is None:
    time.sleep(3)

def say(text, wait=1500):
    n = len((transcript() or {}).get("entries", []))
    app("ai", f"say={text}")
    t = time.time(); seen = False
    while time.time() - t < wait:
        time.sleep(4)
        tr = transcript()
        if not tr:
            continue
        e = tr.get("entries", []); busy = tr.get("status") not in ("idle", "closed")
        # A card waiting for the person (save, export, new): answer it as the
        # person would after asking for the work ("always in this chat").
        if any(x.get("kind") == "tool" and x.get("status") == "confirm" for x in e[n:]):
            app("ai", "confirm=always")
        if str(tr.get("status", "")).startswith("error"):
            return e[n:], time.time() - t, tr.get("status")
        seen = seen or busy or len(e) > n + 1
        if seen and not busy and len(e) > n + 1:
            return e[n:], time.time() - t, "idle"
    return [], time.time() - t, "timeout"

started = time.time()
log = []
new, secs, status = say(task["prompt"])
log.append({"say": task["prompt"], "secs": secs, "status": status, "entries": new})
# The hub failed the turn (busy, no progress): send it again, as a person would.
retries = 0
while status.startswith("error") and retries < 2:
    retries += 1
    more, s2, status = say(task["prompt"])
    new += more
    secs += s2
followup = "Check your work: render it, run fab.check, and fix anything that does not match what I asked or would not print. Do not save or export. Say DONE when it is finished."
new2, secs2, status2 = say(followup)
log.append({"say": followup, "secs": secs2, "status": status2, "entries": new2})
wall = time.time() - started
entries = new + new2
tools = [e for e in entries if e.get("kind") == "tool"]
failed_tools = [e for e in tools if e.get("status") not in ("ok", "done")]
assistant_turns = sum(1 for e in entries if e.get("kind") == "assistant")

# Score from Fab's own state.
inspect = call("inspect", '{"program":false}')
parts = inspect.get("plate", {}).get("parts", [])
check = call("check", "{}")
errors = check.get("errors")
score = {"parts": len(parts), "want_parts": task["parts"]}
ok = len(parts) == task["parts"]
closed = all(p.get("closed") for p in parts) and bool(parts)
ok = ok and closed
score["closed"] = closed
score["check_errors"] = errors
ok = ok and (errors == 0)
# Sizes and volumes: match each expected part to the closest actual one.
used = set()
matches = []
for k, want in enumerate(task["size"]):
    best = None
    for i, p in enumerate(parts):
        if i in used:
            continue
        got = sorted(p.get("size_mm", [0, 0, 0]))
        d = max(abs(a - b) for a, b in zip(got, sorted(want)))
        if best is None or d < best[0]:
            best = (d, i)
    if best is None:
        matches.append({"want": want, "got": None}); ok = False; continue
    used.add(best[1])
    p = parts[best[1]]
    vol = p.get("volume_mm3", 0)
    lo, hi = task["volume"][k]
    size_ok = best[0] <= task["size_tol"]
    vol_ok = lo <= vol <= hi
    ok = ok and size_ok and vol_ok
    matches.append({"want": want, "got": p.get("size_mm"), "size_ok": size_ok, "volume": vol, "volume_ok": vol_ok, "object": p.get("object")})
score["matches"] = matches
if task.get("gears") and len(parts) >= 2:
    m = call("measure", json.dumps({"what": "clearance", "a": parts[0]["object"], "b": parts[1]["object"]}))
    c = m.get("clearance_mm")
    score["gear_clearance"] = c if c is not None else m.get("error", "n/a")
    if isinstance(c, (int, float)):
        ok = ok and 0.0 < c < 0.6 and not m.get("overlap_mm3")
result = {"tag": tag, "task": task["id"], "success": ok, "hub_retries": retries, "wall_s": round(wall), "tool_calls": len(tools), "failed_tool_calls": len(failed_tools), "assistant_turns": assistant_turns, "status": [status, status2], "score": score}
print("EVAL " + json.dumps(result), flush=True)
last = " ".join(e.get("text", "") for e in new2 if e.get("kind") == "assistant")[-400:]
print("EVAL-LAST " + json.dumps({"task": task["id"], "tag": tag, "last": last}), flush=True)
