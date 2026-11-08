# praxion-brain

The Python brain for the thin Demo-2 slice: one goal goes through the Jev
router, then the LLM writer, the body's static checker, three practice runs on
fresh seeds and one held-out seed, and finally to `local/skills/active/`.
Practice runs start the real headless body through `harness/`.

## Commands

Run from `brain/` (the package is not installed; pytest inserts `brain/` too):

```sh
python -m pytest -q
python -m praxion_brain spent
python -m praxion_brain route "have a furnace" --item furnace
python -m praxion_brain learn "have a furnace" --item furnace --model gpt-6-luna
python -m praxion_brain check local/skills/candidates
python -m praxion_brain run --port 25640 --world demo
```

`route` and `learn` cost money; the ledger stops a call before its provider
budget is crossed. `learn` routes first: a `play` decision prints the skill and
stops, otherwise the writer and admission run.

`check <dir>` calls `praxion-body check-skills --candidates <dir>` and prints
its JSON. Build the body first (`cd body && cargo build --release`).

`run` connects to a body that listens on `--port`, builds a briefing from each
snapshot, asks the curriculum model for one goal (or uses the code-only ladder
fallback under a cost hard stop), routes it with Jev, admits a new skill when
needed, and sends a command. Every state transition and wake goes to
`local/brain/brain.jsonl`; goals go to `local/worlds/<world>/goals.jsonl`.
`--world` defaults to `default`. The loop reconnects and resumes an open goal
after a brain restart, and the body keeps playing while the brain thinks: a
reader thread owns the socket and pings once a second even while a model call
blocks, so the body never sees a silent brain.

## Files it writes

Machine-local, ignored by git (all under `local/`):

| Path | Contents |
|---|---|
| `local/brain/ledger.jsonl` | one JSON line per paid request: tokens, cost, purpose |
| `local/brain/brain.jsonl` | full prompt/response evidence for every LLM and Jev call |
| `local/brain/attempts/<timestamp>-<n>/` | the candidate directory of one admission attempt |
| `local/brain/admission-<skill>-<timestamp>.json` | the admission report |
| `local/skills/candidates/<skill>.skill` | the latest candidate, for inspection |
| `local/skills/active/<skill>.skill` | an admitted skill; loaded with the core skills |
| `runs/test-body-*` (via the harness) | kept practice run directories |
| `local/worlds/<world>/goals.jsonl` | one line per goal, last line per `goal_id` |
| `local/worlds/<world>/skill_uses.jsonl` | append-only skill outcomes |

Budgets default to $4.50 per provider and can be changed with
`PRAXION_BUDGET_OPENAI` and `PRAXION_BUDGET_TYPESAFE`. The governor's hourly
warning and the hard stop come from `PRAXION_WARN_USD_PER_HOUR` (default 1.00)
and `PRAXION_HARD_STOP_USD` (default: the remaining ledger budget). The hard
stop counts the spend of the current run, not the whole ledger file, so an old
session's spend never trips it; the per-provider budget stays the absolute cap.

## Layout

`config.py` (paths, `.env`, harness on `sys.path`), `ledger.py` (cost and
budget), `evidence.py` (JSONL log), `goal.py`, `skills.py` (catalog parsing and
`check-skills`), `writer.py` (prompt plus `responses.parse`), `router.py`
(TypeSafe System One), `practice.py` (one headless run), `admission.py`
(write -> check -> practice -> active), `port.py` (body port v0 client and
framing), `conditions.py` (inventory guard subset), `ladder.py` (the twelve
rungs), `memory.py` (goals, skill uses, failure memory, drift), `briefing.py`
(the capped briefing text), `curriculum.py` (proposal model and validation),
`proposer.py` (the structured proposal call), `governor.py` (cost warning, hard
stop, breaker, code-only mode), `session.py` (the body connection, reader thread,
reconnect, snapshots and sends), `records.py` (goal records, ids, recovery),
`loop.py` (the pick/route/write/play state machine that composes both) and
`cli.py`. `tests/fake_body.py` is the scriptable in-process body used by the
port and loop tests.
