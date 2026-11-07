#!/usr/bin/env bash
# Stop orphaned OpenCode worker sessions: the shared service keeps running a
# session after its client dies (sleep, reboot, a killed orchestrator), and an
# orphan keeps editing the tree (2026-11-07: one ran unsupervised for five days).
# oc-run.sh titles each session "<brief>.<watchdog pid>". A session updated in
# the last OC_REAP_MINUTES (default 60) whose watchdog is gone is exported to
# <log-dir>/<title>.session.json and deleted. Sessions without a pid suffix
# (older runs) are only listed. Run at the start of every orchestrator session.
#
# Usage: oc-reap.sh [log-dir]   (default logs/reaped)
set -uo pipefail
logdir=${1:-logs/reaped}
window=$(( ${OC_REAP_MINUTES:-60} * 60 * 1000 ))
now=$(( $(date +%s) * 1000 ))

opencode session list --format json -n 100 2>/dev/null \
  | node -e '
const s = JSON.parse(require("fs").readFileSync(0, "utf8"));
for (const x of s) console.log([x.id, x.updated ?? x.created ?? 0, x.title].join("\t"));' \
  | while IFS=$'\t' read -r id updated title; do
      (( now - updated > window )) && continue
      pid=${title##*.}
      if [[ $title == "$pid" || ! $pid =~ ^[0-9]+$ ]]; then
        echo "oc-reap: recent session without a watchdog pid, check by hand: $id $title"
        continue
      fi
      kill -0 "$pid" 2>/dev/null && continue
      mkdir -p "$logdir"
      opencode session export "$id" >"$logdir/$title.session.json" 2>/dev/null
      if opencode session delete "$id" >/dev/null 2>&1; then
        echo "oc-reap: stopped orphan $id $title (export: $logdir/$title.session.json)"
      else
        echo "oc-reap: WARNING could not delete orphan $id $title" >&2
      fi
    done
