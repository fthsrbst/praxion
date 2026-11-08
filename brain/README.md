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
```

`route` and `learn` cost money; the ledger stops a call before its provider
budget is crossed. `learn` routes first: a `play` decision prints the skill and
stops, otherwise the writer and admission run.

`check <dir>` calls `praxion-body check-skills --candidates <dir>` and prints
its JSON. Build the body first (`cd body && cargo build --release`).

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

Budgets default to $4.50 per provider and can be changed with
`PRAXION_BUDGET_OPENAI` and `PRAXION_BUDGET_TYPESAFE`.

## Layout

`config.py` (paths, `.env`, harness on `sys.path`), `ledger.py` (cost and
budget), `evidence.py` (JSONL log), `goal.py`, `skills.py` (catalog parsing and
`check-skills`), `writer.py` (prompt plus `responses.parse`), `router.py`
(TypeSafe System One), `practice.py` (one headless run), `admission.py`
(write -> check -> practice -> active), `port.py` (body port v0 client and
framing) and `cli.py`. `tests/fake_body.py` is the scriptable in-process body
used by the port tests.
