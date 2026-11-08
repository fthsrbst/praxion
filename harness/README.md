# praxion_harness

Demo-0 test harness: start a fresh-seed Minecraft server, drive it through a logged
fixture channel over RCON, read facts with a referee, and follow the server log.
Standard library only. English docs; agent-oriented.

## Requirements

- Python >= 3.11 (tested with 3.14).
- Java 25 (`java -version` must say 25).
- The pinned server jar (`third_party/minecraft/server-26.3.jar` after setup).

## Paths

Resolved per call, in this order:

1. Environment variables `PRAXION_JAVA` and `PRAXION_SERVER_JAR`.
2. `local/paths.toml` keys `java` and `server_jar` (relative paths are resolved
   from the repository root; the file is git-ignored and written by `setup.py`).
3. Otherwise `PathResolutionError` naming what to set.

The repository root is the first ancestor of `harness/praxion_harness` holding
`.git` or `versions/manifest.toml`.

## Commands

Run from `harness/` (or anywhere: `pythonpath` is configured in `pyproject.toml`).

```sh
cd harness

# Unit tests: no server, fast, default.
python -m pytest -q

# Live-server tests: fresh seeds, random ports, ~2 min.
python -m pytest -q -m server

# Same, with the measured startup lines visible.
python -m pytest -q -m server -s

# Start a server and wait for Ctrl+C. Prints run dir, game port and RCON port.
python -m praxion_harness serve --seed 123
python -m praxion_harness serve --seed 123 --keep   # keep runs/<id>/ after stop

# Query the running server (RCON port from `serve`).
python -m praxion_harness query --rcon-port 34567 time
python -m praxion_harness query --rcon-port 34567 day
python -m praxion_harness query --rcon-port 34567 gametime
python -m praxion_harness query --rcon-port 34567 is-night
python -m praxion_harness query --rcon-port 34567 block 0 64 0
python -m praxion_harness query --rcon-port 34567 is-block 0 64 0 minecraft:stone
python -m praxion_harness query --rcon-port 34567 gamerule keep_inventory
python -m praxion_harness query --rcon-port 34567 player Steve health
python -m praxion_harness query --rcon-port 34567 raw list

# Watch the body play from a real Minecraft 26.3 client (Direct Connection to the printed address).
# The body waits --wait-seconds so you can join; you are teleported next to it, in your own mode.
python -m praxion_harness watch --seed 303 --goal get_stone_pickaxe --wait-seconds 240
python -m praxion_harness watch --spectate   # lock your view to the body instead

# Delete stale harness run dirs (only test-*, measure-*, explore-*).
python -m praxion_harness clean-runs
python -m praxion_harness clean-runs --older-than-hours 24 --dry-run
```

`clean-runs` skips a directory whose `server/world/session.lock` is locked by a live
server, and never touches `fwd-*`, `raw-*`, `manual-*` or `cli-*`.

## The body

`body/target/release/praxion-body` is the one process that plays a goal. It
connects to the forwarder as a game client and to its tee, runs
tee -> decoder -> world -> perception, and drives the headless session's 20 Hz
tick through reflexes, the arbiter, the interpreter and the primitives.

```sh
praxion-body --forwarder 127.0.0.1:25640 --tee 127.0.0.1:25641 \
             --name Praxion --goal get_stone_pickaxe \
             --run-dir runs/body-pickaxe \
             [--goal-args ...] [--max-ticks 6000] \
             [--settings body/config/reflexes.toml] [--skills body/skills] \
             [--candidates DIR] [--start-delay-ticks 60] [--linger-ticks 60] [--verbose]
```

- `--forwarder` is the forwarder's game listener, `--tee` its tee listener.
- `--goal` names a skill in `body/skills`; `--goal-args` are typed from the
  skill's parameters (int, item, block, entity_kind or bool).
- `--candidates DIR` loads every `.skill` in DIR next to the core skills; a
  candidate that redefines a core skill exits 2 with an error. In a port
  session the body watches DIR: a new or changed file reloads the program
  before the next command, so a skill admitted mid-run is playable without a
  restart (`start_session` watches `active_skills_dir()`:
  `PRAXION_SKILLS_ACTIVE`, default `local/skills/active`, created when missing).
- `praxion-body check-skills --candidates DIR [--skills body/skills]` checks
  them offline and prints one JSON report (exit `0` pass, `1` fail, `2` usage/IO).
- `--start-delay-ticks` waits after the first usable tick so fixtures can give
  items, set the time and summon mobs.
- `--linger-ticks` keeps the player connected after the goal settles so the
  referee can read the inventory while the body is still in the world.
- `--run-dir` gets `events.jsonl` (the first event is `run_started` with the
  manifest pins and the git commit) and `summary.json`.
- `--verbose` shows the body's debug diagnostics (packet traces); `RUST_LOG`
  overrides the level, for example `RUST_LOG=praxion_body=debug`.

Exit codes: `0` goal `done`, `1` `failed(kind)`, `2` `--max-ticks`, `3`
Ctrl+C. Every exit disconnects cleanly.

`summary.json` carries the goal and its outcome, ticks, tick-time p50/p99 for
the body's work and the tick period, receipts by outcome, reflex activations,
deaths seen and world warnings. Ticks over 25 ms also become `slow_tick` events
in `events.jsonl`.

The Python helper `praxion_harness/body.py` starts a server, a forwarder and the
body for one seed and returns the run, the referee and the fixture channel:

```python
from praxion_harness.body import start_body

def setup(fixture, referee, run):
    fixture.give("Praxion", "oak_log", 3)

with start_body(101, "get_wooden_pickaxe", setup=setup) as run:
    summary = run.wait_for_outcome(timeout=240)
    assert run.referee.player_inventory()["minecraft:wooden_pickaxe"] >= 1
```

It uses only ports 25640-25659. `tests/test_body_smoke.py` is the level-2 smoke
test (`-m server`).

## Test markers

| Marker | Meaning |
|---|---|
| `server` | starts a real server; deselected by default |
| `needs_player` | needs a connected player; also `server`; skipped when nobody is online |

`pyproject.toml` sets `addopts = "-m 'not server and not needs_player'"`, so the
default run starts no server. `-m server` on the command line overrides it.
Thread exceptions and misnamed test classes are errors, not warnings.

Every server in the live tests comes from the `server_factory` fixture (see
`tests/conftest.py`): it is stopped even when a test fails, and a failed test keeps
its run directory and prints the path. A session fixture stops any server still
alive when the session ends.

## Run directories

```
runs/<run_id>/
  server/              eula.txt, server.properties, world/, logs/latest.log, server.out
  fixtures.jsonl       one JSON line per fixture command: {t, run_id, command, response}
```

`TestServer` picks a random free game port and RCON port at start; it never uses
25565, 25566, 25570, 25571 or 25575. It writes `eula.txt`, `server.properties`
(offline mode, 127.0.0.1, difficulty normal, survival, spawn-protection 0, the
given `level-seed`) and a fresh `world/` for every start.

On a clean `stop()` the whole `runs/<run_id>/` is deleted unless `keep=True` or
`PRAXION_KEEP_RUNS=1` is set. Tests print kept paths; use `clean-runs` afterwards.

## 26.3 command facts (verified)

Captured with the real server in `tests/fixtures/observed-26.3.txt`.

- `time query day` -> `Timeline minecraft:day is at 1002 tick(s)` (time of day).
- `time query gametime` -> `The game time is 48000 tick(s)`; day = gametime // 24000.
- `time query daytime` is **invalid** in 26.3 (no `minecraft:daytime` timeline).
- Gamerules are snake_case: `advance_time` (daylight cycle), `spawn_mobs`
  (general mob spawning), `keep_inventory`. `doDaylightCycle`/`doMobSpawning` fail.
- `execute if block X Y Z <block>` -> `Test passed` / `Test failed`.
- `setblock` -> `Changed the block at X, Y, Z`; `data get block` only works for
  block entities (`The target block is not a block entity` otherwise), so
  `Referee.block_at` probes candidates and `block_is` is the exact check.
- `scoreboard objectives add <name> deathCount` -> `Created new objective [<name>]`;
  `scoreboard players get <player> <name>` -> `<player> has 5 [<name>]` or
  `Can't get value of <name> for <player>; none is set` (zero deaths).

## Layout

| File | Role |
|---|---|
| `praxion_harness/paths.py` | Java/jar resolution, repository root |
| `praxion_harness/rcon.py` | RCON client, multi-packet replies, auth |
| `praxion_harness/server.py` | `TestServer`, active-server registry, startup timing |
| `praxion_harness/logtail.py` | log lines -> `ServerDone`/`PlayerJoined`/`PlayerLeft`/`PlayerDied`/`ChatMessage`; death patterns from the jar's `en_us.json` |
| `praxion_harness/fixtures.py` | logged fixture channel and verified gamerule names |
| `praxion_harness/referee.py` | server facts and verdicts (health, inventory, time, blocks, deaths, first night) |
| `praxion_harness/cleanup.py` | stale run-dir deletion and session-lock check |
| `praxion_harness/cli.py` | `serve`, `query`, `clean-runs` |
| `praxion_harness/body.py` | server + forwarder + `praxion-body` for one seed; summary and events |
| `tests/` | unit tests (default) and live-server tests (`-m server`) |
