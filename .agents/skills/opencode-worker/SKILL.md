---
name: opencode-worker
description: Orchestrator-side guide for delegating work to an OpenCode v2 worker on the free MiniMax M3.1 Flash model. Use when handing a well-specified coding, test, refactor, docs, digest or review task to OpenCode, or when checking OpenCode model and service health. Not for an agent that is itself running inside OpenCode.
---

# OpenCode worker (v2)

Facts below were measured on this machine on 2026-10-21 with `opencode v2.1.2`.
Re-measure after an OpenCode upgrade; v2 changed a lot from v1.

## When to delegate

Delegate when the task is clearly specified and verifiable: implementing a designed unit,
writing tests, mechanical refactors, docs drafts, summarizing large files or logs, a second-opinion review.
Keep decisions, architecture, integration and final verification with the orchestrator.

Do not delegate when the spec is still open. Close the open questions first, then write the brief.

## Models

| Model | Use | Measured |
|---|---|---|
| `opencode/minimax-m3.1-flash-free` | Default worker. Free (cost 0 in event log). | PONG 5 s; spotted a planted bug in 5 s |
| `opencode-go/minimax-m3.1-flash-free` | Same model through OpenCode Go. Fallback when the Zen route fails. | PONG 2 s |

Check availability: `opencode models | grep minimax`. Liveness test before a batch:

```sh
perl -e 'alarm 90; exec @ARGV' opencode run --auto -m opencode/minimax-m3.1-flash-free "Reply with exactly one word: PONG" </dev/null
```

macOS has no `timeout`; `perl -e 'alarm N; exec @ARGV'` is the portable substitute.

## How OpenCode v2 works (what matters here)

- One shared **background service** per user owns sessions, config and tool execution.
  `opencode run` connects to it automatically. `opencode service status` prints its URL.
  `--standalone` uses a private server instead.
- `opencode run [flags] "<message>"` is the non-interactive entry point. Key flags:
  `--auto` (approve everything not explicitly denied), `-m provider/model#variant`,
  `--format json` (one JSON event per line), `--title`, `-f <file>` (attach), `-c` / `-s <id>` / `--fork`, `--agent <id>`.
- Instructions: OpenCode v2 reads **only `AGENTS.md`** (global `~/.config/opencode/AGENTS.md`, then every
  `AGENTS.md` from the workspace up to `$HOME`). It never reads `CLAUDE.md`.
- Skills: discovered in `.opencode/skills`, `.claude/skills`, `.agents/skills` (project) and
  `~/.config/opencode/skills`, `~/.claude/skills`, `~/.agents/skills` (global). Skills without a `description` are not advertised.
- Agents: built-in `build` and `plan` (primary), `general` and `explore` (subagents). Custom agents live in
  `.opencode/agents/<name>.md` with frontmatter `description`, `mode`, `model`, `permissions`.
- Config: project `opencode.json` at the repo root. Permissions are an ordered list of
  `{action, resource, effect}`; the last matching rule wins. Actions include `read`, `edit`, `shell`,
  `external_directory`, `subagent`, `skill`, `webfetch`, `websearch`.

## Safety rails (this repo's `opencode.json`)

Verified under `--auto`:
- Denied: `git push`, `git commit`, `git reset`, `git checkout`, `git rebase`, `rm -rf`.
  Each git rule also has a `git * <cmd> *` twin, because `git commit *` alone misses
  global options such as `git -c user.name=x commit ...`.
- Denied: the read/edit tools on any path outside the repo, and shell commands with absolute outside paths.
- **Not caught:** shell commands using `~` paths (e.g. `ls ~/Desktop` ran). v2 shell path checks are best effort.
  So the brief must still forbid touching anything outside the repo.

## Running a worker

1. Write the brief to a file (template below). Put every requirement and decision in it.
   Never relay new decisions to a running worker; start a new run instead.
2. Run it under the watchdog, with the harness's background runner so you get woken on exit:

   ```sh
   .claude/skills/opencode-worker/scripts/oc-run.sh <brief.md> <log-dir> [model]
   ```

   It writes `<log-dir>/<brief-name>.events.jsonl` (raw events) and `<brief-name>.answer.txt` (text only).
   Exit codes: `0` done with `FINAL_REPORT`, `10` stalled, `11` wall timeout,
   `12` finished without `FINAL_REPORT`, `13` could not start or worker error.
   Tune with `OC_STALL_SECS` (default 300) and `OC_WALL_SECS` (default 1800).
   Do not `&` workers or "check back later": nothing wakes you when they die.
3. Verify the result yourself. Exit 0 is not success. Success means the expected diff, no out-of-scope
   changes (`git status --porcelain`), and your own run of the build and tests passing.
4. Have a different model review non-trivial output before it is committed.

Parallel runs: two trivial runs in parallel both finished (1 s and 2 s). Heavy parallel load is unmeasured.
Start with one run at a time and measure before fanning out.

## Reading the event log

`--format json` events: `step_start`, `tool_use`, `step_finish`, `text`, `error`.

```sh
jq -r 'select(.type=="text") | .part.text' run.jsonl                    # final answer text
jq -r 'select(.type=="tool_use") | "\(.part.tool) \(.part.state.status)"' run.jsonl
jq -r 'select(.type=="error") | .error.message' run.jsonl               # e.g. provider.no-route
jq -s 'map(select(.type=="step_finish") | .part.cost) | add' run.jsonl  # total cost
```

A bad model id exits `1` with `{"type":"error","error":{"type":"provider.no-route",...}}`.

## Brief template

```md
# Task: <one line>

## Context
<what exists, which files matter, links to decisions>

## Do
<exact, verifiable steps>

## Done when
<observable acceptance criteria, including the build/test commands to run>

## Rules
- Touch only these files: <list>. Do not write outside this repository, not even to /tmp.
- Do not commit, push or change branches.
- If something is unclear or blocked, stop and say so in FINAL_REPORT. Do not guess.
- Do not claim success unless the build and tests you were told to run pass.

## Report
End with a line starting `FINAL_REPORT`, then: what changed (files), commands run and their results,
anything unfinished or uncertain.
```

## Sessions and housekeeping

```sh
opencode session list                 # top-level sessions in this project, newest first
opencode session export <id>          # JSON export
opencode run -s <id> "<follow-up>"    # continue a session (add --fork to branch it)
opencode stats                        # usage
```

An MCP bridge for OpenCode exists in some clients. In v1 it duplicated prompts and dropped long runs;
it is untested on v2, so prefer the CLI.
