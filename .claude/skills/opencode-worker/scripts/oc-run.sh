#!/usr/bin/env bash
# Run one OpenCode worker task under a watchdog, in the foreground.
# Launch this script with the harness's background runner (e.g. Claude Code
# Bash run_in_background) so the orchestrator is woken when it exits. Never `&` it.
#
# Usage: oc-run.sh <brief.md> <log-dir> [provider/model]
# Env:   OC_STALL_SECS  kill if the log stops growing this long (default 300)
#        OC_WALL_SECS   kill after this much wall time (default 1800)
# Exit:  0 finished with FINAL_REPORT   10 stalled   11 wall timeout
#        12 finished without FINAL_REPORT   13 could not start or worker error
# On 10 and 11 the stranded service session is exported to
# <log-dir>/<name>.session.json and deleted, so it stops working.
set -uo pipefail

brief=${1:?brief file required}
logdir=${2:?log dir required}
model=${3:-opencode-go/deepseek-v4.1-flash}
stall=${OC_STALL_SECS:-300}
wall=${OC_WALL_SECS:-1800}

[[ -r $brief ]] || { echo "oc-run: brief not readable: $brief" >&2; exit 13; }
mkdir -p "$logdir" || exit 13
name=$(basename "$brief" .md)
# Distinct suffixes so these never collide with files the worker itself creates.
log=$logdir/$name.events.jsonl
out=$logdir/$name.answer.txt

# macOS has no setsid, so walk the child tree instead of killing a process group.
# Git Bash on Windows has no pgrep; there, taskkill /T kills the native process tree.
kill_tree() {
  if [[ -r /proc/$1/winpid ]]; then
    taskkill //T //F //PID "$(cat "/proc/$1/winpid")" >/dev/null 2>&1
    return
  fi
  local child
  for child in $(pgrep -P "$1"); do kill_tree "$child"; done
  kill -TERM "$1" 2>/dev/null
}

# The shared service owns the session and keeps running it after its client is killed
# (measured 2026-10-31: 18 minutes of stray edits after a timeout). Killing the client
# alone is not enough, so find the session by the title the run was given, export it
# for evidence, then delete it. Best effort: never mask the watchdog's exit code.
stop_session() {
  local id dest
  dest=$logdir/$name.session.json
  id=$(opencode session list --format json -n 100 2>/dev/null \
    | node "$(dirname "$0")/oc-events.js" session-id "$name" 2>/dev/null)
  if [[ -z $id ]]; then
    echo "oc-run: WARNING no session titled '$name' to stop" >&2
    return 0
  fi
  if ! opencode session export "$id" >"$dest" 2>/dev/null || [[ ! -s $dest ]]; then
    echo "oc-run: WARNING export of session $id failed or is empty: $dest" >&2
  fi
  # Delete even when the export failed: stopping the work matters more than the record.
  if opencode session delete "$id" >/dev/null 2>&1; then
    echo "oc-run: stopped session $id, exported to $dest"
  else
    echo "oc-run: WARNING could not delete session $id" >&2
  fi
  return 0
}

opencode run --auto --format json --title "$name" --model "$model" "$(cat "$brief")" \
  </dev/null >"$log" 2>&1 &
pid=$!

start=$(date +%s)
last_size=-1
last_change=$start
while kill -0 "$pid" 2>/dev/null; do
  sleep 5
  now=$(date +%s)
  size=$(wc -c <"$log")
  if (( size != last_size )); then last_size=$size; last_change=$now; fi
  if (( now - last_change > stall )); then
    kill_tree "$pid"; echo "oc-run: STALLED (no output for ${stall}s), log: $log"
    stop_session; exit 10
  fi
  if (( now - start > wall )); then
    kill_tree "$pid"; echo "oc-run: WALL TIMEOUT (${wall}s), log: $log"
    stop_session; exit 11
  fi
done
wait "$pid"
rc=$?

# node, not jq: node ships wherever OpenCode was installed from npm; Windows has no jq.
node "$(dirname "$0")/oc-events.js" text "$log" >"$out" 2>/dev/null
errors=$(node "$(dirname "$0")/oc-events.js" error "$log" 2>/dev/null)
echo "oc-run: exit=$rc elapsed=$(( $(date +%s) - start ))s log=$log text=$out"

if (( rc != 0 )) || [[ -n $errors ]]; then
  echo "oc-run: WORKER ERROR ${errors:-"(see log)"}"; exit 13
fi
if ! grep -q '^FINAL_REPORT' "$out"; then
  echo "oc-run: NO FINAL_REPORT, treat as not done"; tail -20 "$out"; exit 12
fi
sed -n '/^FINAL_REPORT/,$p' "$out"
