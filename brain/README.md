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

## Web console

`run` also serves a local console (127.0.0.1 only) at <http://127.0.0.1:8765/>:
the current mission, vitals, ladder, a live feed of decisions (click one to
see the full prompt, response, tokens and cost), skills with their admission
results, metrics, and a command bar (give a goal, pause/resume, stop the
goal). `--console-port N` changes the port, `--console-port 0` turns it off.
Your goals go before the proposer's next pick; pause stops new picks only.
"Stop goal" pops the running intent: the body cancels it and confirms with a
`settled outcome=cancelled` receipt; only then does the goal record close as
interrupted (`stopped_by_user`). A stop while a skill is being written marks the
background admission: when it ends its goal is recorded interrupted
(`stopped_by_user`) and is never played, though the skill may still be admitted
and stay active.

### Standalone console

One command runs the demo from the browser, without choreographing processes in
a terminal. From `brain/`:

```sh
python -m praxion_brain console --seed 303 --operator Fatih
```

It serves the same page on 127.0.0.1, default port 8765 (`--console-port N`
changes it; `--console-port 0` picks an ephemeral port), and puts the run
controls at the top of the page. The server and the bot are separate:

| Control | What it does |
|---|---|
| Start server | Opens one fresh-seed managed world and applies the survival or peaceful setup once, as soon as Java and RCON are ready. The server is joinable right away, before any bot exists. |
| Stop server | Stops the bot first, then the server. The console stays up so the next Start server works. |
| Start bot | Starts only a forwarder and body against the ready server. A bot restart reuses the same world and its brain memory, in a new attempt directory and with a fresh console. |
| Stop bot | Closes the bot, loop and console but keeps the server and the world running. |
| Join Minecraft | Launches the local Java 26.3 client straight into the running server through the watcher's existing Quick Play launch. It needs only the server, not the bot. |

A failed server start leaves nothing behind; a failed bot start or crash leaves
the healthy server ready for another Start bot. `--operator NAME` names the
player whose chat the bot obeys (default `Fatih`); `--peaceful` uses the
debugging world (clock frozen at 1000, no mobs) instead of the survival run.

The inventory panel draws the vanilla slot layout (9-35, 36-44, 5-8, offhand 45)
and reads item and block textures at runtime from the local 26.3 client. No
Mojang assets are shipped: a texture the local client cannot supply falls back
to a lettered tile for that item.

Prerequisites:

- A release body build: `cd body && cargo build --release`.
- Java 25 and the pinned server jar, as `harness/README.md` describes
  (`local/paths.toml` keys `java` and `server_jar`, or `PRAXION_JAVA` and
  `PRAXION_SERVER_JAR`).
- The machine-local 26.3 client files under `third_party/minecraft-client/`,
  the same files `python -m praxion_harness.watch_client` uses. They are ignored
  by git and nothing here downloads them: Join needs them, and the inventory
  textures come from the client's assets.
- `PRAXION_HARD_STOP_USD=0` runs code-only: no propose, route or write call, only
  the ladder fallback, so a demo costs nothing.

Everything binds to 127.0.0.1; the console adds no public network surface.

## Voice

The console speaks the bot's chat lines and transcribes the operator's speech
through ElevenLabs (decision 2026-11-12 "Voice"). Put the key in the
repository's `.env`:

```sh
ELEVENLABS_API_KEY=...
# optional
ELEVENLABS_TTS_MODEL=      # default eleven_flash_v2_5 (low latency, Turkish)
ELEVENLABS_STT_MODEL=      # default scribe_v2
ELEVENLABS_VOICE_ID=       # default: the account's first Turkish/multilingual
                           # voice, else the premade Sarah voice
```

The panel in the corner polls `/api/voice/lines` every second and speaks each
new narration line through `/api/voice/tts`; its speaker toggle is remembered
in `localStorage` and lines older than 15 s are dropped rather than replayed.
Hold the microphone button, or hold `V` while no text field has focus, to
record; on release the clip goes to `/api/voice/stt`, the transcript is shown,
and it is sent as a task down the same path as the task box
(`POST /api/control {"action": "task"}`). `/api/voice/status` reports
`{"enabled", "voice_id"}`. Without the key the panel says "voice off" and the
rest of the console is unchanged; the key never reaches the browser.

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
(TypeSafe System One), `practice.py` (one headless run, cancelled and killed
with its live stack), `admission.py` (write -> check -> practice -> active),
`admission_job.py` (one admission off the loop thread), `cancel.py` (its cancel
token), `port.py` (body port v0 client and
framing), `conditions.py` (inventory guard subset), `ladder.py` (the twelve
rungs), `memory.py` (goals, skill uses, failure memory, drift), `briefing.py`
(the capped briefing text), `curriculum.py` (proposal model and validation),
`proposer.py` (the structured proposal call), `governor.py` (cost warning, hard
stop, breaker, code-only mode), `session.py` (the body connection, reader thread,
reconnect, snapshots and sends), `records.py` (goal records, ids, recovery),
`loop.py` (the pick/route/write/play state machine that composes both),
`voice.py` (ElevenLabs speech out and in for the console) and `cli.py`.
`tests/fake_body.py` is the scriptable in-process body used by the
port and loop tests.

## Wiki RAG

A local, offline index of about 330 English minecraft.wiki article pages, built
for the planner: ores, tools, food, farming, mobs, structures, biomes, mining
and shelter tutorials, the Nether and the End. Install the optional extra once:

```sh
.venv/bin/pip install -e '.[wiki]'
python -m praxion_brain wiki fetch      # ~330 titles, at most one request / 1.5 s
python -m praxion_brain wiki build      # extract text, embed, write the index
python -m praxion_brain wiki search "demir hangi yükseklikte bulunur"
python -m praxion_brain wiki eval       # 20 questions, prints hit@1 / hit@3
```

The page list is `praxion_brain/wiki/pages.txt` (fixed, one title per line).
The index is local only, under `local/wiki/` (git-ignored): `raw/` HTML,
`text/` extracted JSON, `index.sqlite` chunks, `vectors.npy` the embedding
matrix and `models/` the embedding model. `search(query, k=3)` (from
`praxion_brain.wiki`) returns `Snippet(title, text, url, score)` or `[]` when
there is no index; it logs once and never raises. The index stores its
embedding model and dimension and refuses to answer when the model differs.

Licence: the text is CC BY-NC-SA 3.0 from minecraft.wiki. Only plain article
pages are downloaded (`https://minecraft.wiki/w/<Title>`), `robots.txt` is
checked before every request, requests are spaced 1.5 s apart, and the
user agent identifies the indexer. Nothing from the wiki is committed or
redistributed; the index stays on this machine.
