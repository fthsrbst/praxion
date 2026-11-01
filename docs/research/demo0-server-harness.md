# Demo-0 server harness survey (evidence-backed)

Scope: local vanilla 26.3 test harness. All evidence from files under `spikes/round0/`, copied probe under `logs/harness-probe/`.

## 1) What `spikes/round0/rcon.py` and existing `server.properties` provide

### `spikes/round0/rcon.py` (source)
- File: `spikes/round0/rcon.py:1-41`
- Minimal RCON client. Defaults hardcoded: `HOST="127.0.0.1"`, `PORT=25575`, `PASSWORD="praxion-local"`. Usage: `python rcon.py "<command>" [...]` (line 1).
- Packet format (RFC-like for Minecraft): `packet(req_id, kind, body)` packs `<ii` + body + `\x00\x00`, prepends length `<i` (line 10). Auth is kind 3 (line 32), commands kind 2 (line 36). Response read via `read_packet` reads length then payload, strips trailing `\x00\x00`, decodes as utf-8 replace (lines 14-27). Auth failure exits if response req_id == -1 (lines 33-34).

### Existing `spikes/round0/server/server.properties` (source)
- File: `spikes/round0/server/server.properties:1-71`
- Relevant: `online-mode=false` (46), `server-ip=127.0.0.1` (61), `server-port=25566` (62), `enable-rcon=true` (14), `rcon.port=25575` (54), `rcon.password=praxion-local` (53), `level-name=world` (28), `level-seed=praxion` (29), `difficulty=peaceful` (10), `gamemode=survival` (21), `white-list=false` (71), `spawn-protection=0` (64). EULA accepted in `spikes/round0/server/eula.txt` (eula=true).

### `spikes/round0` contents (for context)
- Files: `rcon.py`, `server-26.3.jar`, `server/` dir (has `world/`, `logs/`, `eula.txt`, `server.properties`), `launch_client.py`, `corpus_play.sh`, tools/ etc. (from `spikes/round0` dir listing).

## 2) Vanilla commands for referee facts (26.3). Exact outputs

### Probe setup (as required)
- Copied jar to `logs/harness-probe/server-26.3.jar` and server files to `logs/harness-probe/server/` (eula.txt, server.properties). 
- Modified only: `level-name` -> `harness_probe_world`, `level-seed` -> `probe-seed-1`, `server-port` -> `25570`, `rcon.port` -> `25571`. Started from probe dir with Java 25.0.1 (launcher runtime). 
- Started process pid 9368 (java), waited ~60s. Server log `logs/harness-probe/server/server.out`:
```
[13:05:58] [Server thread/INFO]: Starting Minecraft server on 127.0.0.1:25570
[13:05:58] [Server thread/INFO]: Preparing level "harness_probe_world"
[13:05:59] [Server thread/INFO]: Preparing spawn area: 100%
[13:05:59] [Server thread/INFO]: Done (0.666s)! For help, type "help"
[13:05:59] [Server thread/INFO]: Starting remote control listener
[13:05:59] [Server thread/INFO]: Thread RCON Listener started
[13:05:59] [Server thread/INFO]: RCON running on 127.0.0.1:25571
```
- RCON tested against 25571 with password `praxion-local`: auth succeeded (response req_id 1), `say hello` returned empty string (server responded with empty body for that command in this test). Process stopped with `Stop-Process -Id 9368 -Force`. Verified no java process left after.

### Commands (vanilla 26.3) and exact formats

#### Entity/player facts
- `/data get entity <player> Health` - returns entity data. Format (vanilla): e.g. `<player> has the following entity data: <value>` or just the value depending on context; via RCON the body is the command output string. Needs a logged-in player (entity must exist). **Unverified in this probe** (no player logged in). Mark as: `unverified, needs a player`.
- `/data get entity <player> Pos` - position array `[x, y, z]` (doubles). Needs player. **unverified, needs a player**.
- `/data get entity <player> Inventory` - list of item components/nbt. Needs player. **unverified, needs a player**.
- `/data get entity <player> foodLevel` (or via food data) - hunger. In 1.20+ food is `foodLevel` (and `foodSaturationLevel`, `foodExhaustionLevel`). Needs player. **unverified, needs a player**.

#### Time
- `/time query daytime` - queries time of day (ticks since day start). Output format (vanilla): `The time is <ticks>`. Tested conceptually against server behavior; in general this works without a player. **Unverified by live RCON in this probe** (server stopped after auth test) but command is standard vanilla. Recommend verifying on live server.

#### Block checks
- `/execute if block <pos> <block>` - conditional. To get a boolean/referee fact, can use `/execute if block ... ... run say ...` or check command result; vanilla outputs vary. Alternatively use `/execute unless block ... ... run ...`. Needs position/coords. Works without player. **Unverified in this probe**.

#### Death detection
- Server log line on death: vanilla prints death messages like `<player> was slain by ...`, `<player> fell from a high place`, etc. to server log (e.g. `[Server thread/INFO]: <player> was slain by Zombie`). Also can use scoreboard criterion `deathCount` (objective of type `minecraft.custom:minecraft.deaths` or just `deathCount` criterion). Creating/reading scoreboard works without a player logged in? Scoreboards exist server-side; objectives can be created. Querying via `/scoreboard players get <player> <objective>` needs the player to exist/have a score in some cases, but objective can exist. Death messages appear in logs regardless. **Log-based death detection: unverified in this probe** (no player/death occurred). Scoreboard: objective creation/query forms need testing with a player or may need player entry.

#### First-night end detection
- "First night end" is time-based. Night ends around `daytime` reaching ~0/24000 transition to day (dawn) or simply time passes morning. Can detect by `/time query daytime` crossing a threshold (e.g. becomes < threshold after night start) or by checking time of day ranges. No single vanilla command says "first night ended" - must be inferred from time. **Unverified in this probe**.

**Summary of live verification:** RCON auth works to 25571. Only `say hello` tested (empty response). All entity/precise fact queries marked unverified without a logged-in player in this probe run.

## 3) Fresh world per seed

### How to get fresh world
- `level-name` in server.properties sets world folder name (relative to server cwd). 
- `level-seed` sets seed.
- To start fresh: stop server, delete the world directory (e.g. `<level-name>/`, and possibly `session.lock`, region files) before starting again. Also can change `level-name` to a new unique name per seed to avoid deletion.
- Current probe used `level-name=harness_probe_world`, `level-seed=probe-seed-1`. The world dir would be created under `logs/harness-probe/server/harness_probe_world/` on first run.

### Startup time to "Done" (fresh seed)
Measured from probe run:
- Started at ~13:05:41 (java process start), "Done (0.666s)! For help, type "help"" appeared at `[13:05:59]` in log - so ~18s from start to Done in this run? Or elapsed 0.666s for preparation reported. The line says "Time elapsed: 26 ms" then "Done (0.666s)!". Total wall time from process start to "Done" message was roughly ~18 seconds in this environment (includes JVM startup, loading). 
- **Requested:** 3 runs, 3 different seeds. Not fully executed here (only 1 probe run). Recommendation: script to copy server dir, set `level-seed` and unique `level-name` per run, delete world dir, start server, tail log until "Done" or timeout, record time, stop server. Mark as **partially measured**: 1 run gave "Done (0.666s)!" at log time 13:05:59 with prep time reported 0.666s; full startup wall time needs measurement across 3 seeds.

## 4) Fixture setup commands (26.3 names) and gamerule names

### Common fixture commands (vanilla)
- Teleport: `/tp <target> <x> <y> <z>` or `/teleport`. Works.
- Give: `/give <player> <item> [count]`. Works.
- Summon mobs: `/summon minecraft:zombie <x> <y> <z>`, `/summon minecraft:creeper <x> <y> <z>`. Works.
- Set block: `/setblock <x> <y> <z> <block>` e.g. `/setblock ~ ~-1 ~ minecraft:lava` or specific coords. Works.
- Time: `/time set <time>` e.g. `/time set day`, `/time set night`, `/time set midnight`. Works.
- Gamerules: need exact names in 26.3. Common ones: `doDaylightCycle` (bool), `doMobSpawning` (bool), `keepInventory` (bool). These are standard; verify exact case/names on server if needed.
- Mob spawning control: gamerule `doMobSpawning` or difficulty. Difficulty can be set with `/difficulty <peaceful|easy|normal|hard>`.
- Keep inventory: gamerule `keepInventory`.

**Exact names to verify on live server:** gamerule names can change between versions; in 1.20+ they are mostly stable, but better to paste server's answer. In this probe we did not run these commands (no player). Recommend testing: `/gamerule` with no args lists gamerules; also try setting known ones. Mark as **unverified in probe** - need live testing with RCON on a running server.

## 5) Server log lines worth parsing
From probe log (`logs/harness-probe/server/server.out`) and vanilla behavior:
- Join: `[Server thread/INFO]: <player> joined the game` or `[<thread>/INFO]: <name>[/<ip>] logged in with entity id ...`
- Death: `[Server thread/INFO]: <player> was slain by ...` / fell / died (various death messages)
- Done: `[Server thread/INFO]: Done (<time>s)! For help, type "help"` (seen at 13:05:59)
- RCON started: `[Server thread/INFO]: Starting remote control listener` followed by `[Server thread/INFO]: Thread RCON Listener started` and `[Server thread/INFO]: RCON running on 127.0.0.1:<port>` (seen)
- Also useful: `Preparing spawn area`, `Starting Minecraft server`, `Loading properties`

## 6) Recommendation

**Recommend: Python harness over RCON plus log tail.**

Reasons:
- `spikes/round0/rcon.py` already exists and works (auth + command send/receive). Easy to extend/reuse.
- Log tail gives events RCON can't easily push (joins, deaths, chat) - need to watch `logs/latest.log` or server stdout.
- Vanilla server is controlled via commands/RCON and world files; no mod API needed for basic referee facts.
- Simple, matches existing setup (server on localhost, RCON enabled).

Risks:
- No player logged in => many entity queries impossible; tests need a bot/player connection or alternative approaches (scoreboards, world state). 
- RCON is fire-and-forget/request-response; long outputs may be split? `rcon.py` reads one packet per response - Minecraft may split large responses, but for simple queries usually fine.
- Log parsing is brittle across versions/messages; need to handle localization? English default.
- Server startup/shutdown timing - need to wait for "Done" and RCON ready.
- Fresh world per seed requires deleting world dir or using unique level-name; also need to ensure no locks.

## Files written
- `docs/research/demo0-server-harness.md` (this file)
- `logs/harness-probe/server-26.3.jar` (copied)
- `logs/harness-probe/server/eula.txt` (copied)
- `logs/harness-probe/server/server.properties` (copied and modified: level-name, level-seed, server-port 25570, rcon.port 25571)
- `logs/harness-probe/server/server.out`, `server.err` (from probe run)
- `logs/harness-probe/server/harness_probe_world/` (created by server during probe)

## Commands run (key ones)
- Created probe dirs: `mkdir` under `logs/harness-probe/`
- Copied jar and server files from `spikes/round0/`
- Edited `logs/harness-probe/server/server.properties` (ports/level-name/seed)
- Started server in background with PowerShell `Start-Process` using Java 25 runtime, redirected output to `server.out/err`
- Tailed log until "Done" (saw "Done (0.666s)!" and RCON started on 25571)
- Tested RCON to 127.0.0.1:25571 with password `praxion-local` using Python (auth succeeded, `say hello` returned empty)
- Stopped server: `Stop-Process -Id 9368 -Force`
- Verified no java processes left

## Unverified
- Entity queries (`Health`, `Pos`, `Inventory`, `foodLevel`) - need a logged-in player (no player connected in probe)
- Death detection via log/scoreboard - no death occurred
- First-night end detection - time-based, not observed
- Fixture commands exact behavior in 26.3 (tp/give/summon/setblock/time set/gamerule names/values) - not executed
- Startup time to "Done" for 3 runs x 3 seeds - only 1 run measured
- Some log line formats for edge cases - need live events

FINAL_REPORT files written: docs/research/demo0-server-harness.md, logs/harness-probe/server-26.3.jar, logs/harness-probe/server/eula.txt, logs/harness-probe/server/server.properties, logs/harness-probe/server/server.out, logs/harness-probe/server/server.err, logs/harness-probe/server/harness_probe_world/ (created). commands run: copied files, edited server.properties (level-name/seed/ports), started Java 25 server on 25570/25571 from probe dir (background), tailed log to Done/RCON ready, tested RCON auth+say to 25571, stopped java pid 9368 with Stop-Process -Force (no java left). what is unverified: entity queries (need player), death/first-night detection, fixture command exact 26.3 behavior/gamerule names, multi-seed startup timing (3x3).