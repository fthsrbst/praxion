# Memory: a fresh, minimal design

Design draft, 2026-10-27. Not a decision. Session 3 (perception and world model) owns the real
decision; this file is input for it. Written without reading `memory-world-state.md`; the
comparison is at the end.

**Sources used.** I had web access this session and used it for a handful of facts (marked `[S]` or
`[V]`): Minecraft item despawn and death drops, the recovery compass, and SQLite's WAL guarantees.
Everything else is `[U]`: my own knowledge, unverified. Repo files are `[V]` with their path.

Tags: `[V]` primary source read this session (URL or repo path). `[S]` secondary source (wiki, blog).
`[U]` my knowledge, not verified.

---

## 0. The short version

One SQLite file. Three tables. Every row carries **how old it is, how it was learned, and how to
re-check it**. The agent never reads the database into a prompt; code renders a small **context card**
under a fixed character budget, and every line on that card carries a verdict (`now`, `fresh`,
`stale`, `unknown`). Stale is not deleted and not trusted: stale means "cheap to check, check it
before you rely on it".

Semantic search (embeddings) is the wrong tool for 95% of this. You look up "the chest at
(112, 63, -40)" by its key, not by meaning. Embeddings earn their place only for free text: lessons,
episodes, and game knowledge.

---

## 1. What the user asked for, turned into four requirements

The user's words (translated): real-time updates; knows the past; can pull stale data to current
instantly; gives the LLM the correct context; stores places, knowledge, inventory and "various other
information"; is RAG or semantic search the right tool?

I read that as four separate requirements, and they need four different mechanisms. Mixing them up is
what makes memory systems complicated.

| # | Requirement | Mechanism | Why not the others |
|---|---|---|---|
| R1 | **Real time**: memory matches the world now | Facts are written the moment perception sees a change; reads compute freshness arithmetically at query time | A background "cleanup job" is slower and can fail silently |
| R2 | **The past**: what happened before | Append-only `events` table, plus optional per-fact history for the kinds where change matters | Versioning every fact forever is expensive and mostly noise |
| R3 | **Stale → current instantly** | Every fact names the cheapest re-check action; `refresh(key)` runs it | Re-deriving the whole world from scratch costs seconds; one check costs ~0.5 s for the inventory and travel time for a chest |
| R4 | **Correct context for an LLM** | Deterministic renderer with tiers and a character budget; never dump the database | Semantic search picks *plausible* rows; the LLM needs *relevant and current* rows |

The user's question "should we search semantically?" maps to a fifth decision: **which lookups are by
identity and which are by meaning** (§6).

---

## 2. The one idea that makes this simple

Almost every fact about the game world has a **stable identity**: a coordinate, an item name, a
dimension, a player. If a fact has an identity, you look it up by key. That is an exact, instant,
indexed lookup. No embeddings, no ranking, no "did the search find it".

The only things without a useful identity are sentences: "in the Nether, piglins ignore you if you
wear gold armour", "last time I dug straight down at y=12 I fell in lava". Those are looked up by
meaning.

So the memory is **two storage shapes, not one**:

```
                  ┌─────────────────────────────────────────────┐
 identity lookups │  world facts (keyed, timestamped, fresh-     │  99% of queries
 (exact, indexed) │  ness rules)  +  event log                   │  "where / what / how many"
                  └─────────────────────────────────────────────┘
                  ┌─────────────────────────────────────────────┐
 meaning lookups  │  free text: lessons, episodes, knowledge     │  rare, small corpus
 (embeddings)     │  (a few thousand rows at most)               │  "what did I learn about X"
                  └─────────────────────────────────────────────┘
```

That split is the whole architectural argument. Everything below is detail.

---

## 3. Data: three tables and one file

### 3.1 Why SQLite (and the main alternative)

**Proposal: one SQLite database file, `praxion.db`.** SQLite is a library that stores a whole
relational database in a single ordinary file; you query it with SQL.

Why:

- **Crash safety is already solved.** In WAL (write-ahead log) mode a transaction is committed when a
  commit record is appended to a separate log file; on the next open after a crash SQLite replays
  that log automatically. `[V]` https://sqlite.org/wal.html (§2, §4, §9)
- **One writer, many readers, no blocking.** Readers do not block the writer and the writer does not
  block readers. `[V]` same page (§2.2) That matches us exactly: the agent process writes; an
  inspector window or a second tool reads while the game runs.
- **Exact and range queries without writing an index engine**: `WHERE dim=? AND x BETWEEN ...`,
  `ORDER BY observed_at DESC LIMIT 20`. `[U]`
- **One file to copy, inspect, delete.** A "fresh world" is a new file. A bug report is a file.
- **It is in the standard library of most languages** (Python ships `sqlite3`; Node has
  `node:sqlite` and mature packages). `[U]`

Two things to know before committing:

- Only **one writer at a time** is allowed. `[V]` sqlite.org/wal.html (§2.2) So exactly one process
  owns the write connection. Everything else opens read-only.
- There was a real WAL corruption bug (a "data race" — two connections doing things at the same
  instant) in versions from 3.7.0 through 3.51.2, fixed in **3.51.3 (2026-03-13)**. `[V]` same page
  (§11) It needs two connections writing at the same instant. Our design avoids that anyway, but pin
  SQLite ≥ 3.51.3.

**Main alternative: plain JSON files, one per fact, plus an append-only log.** Reasonable, and
trivially inspectable with a text editor. But: a crash in the middle of a file write leaves a
half-written file (you must write-temp-then-rename to stay safe), and "find all chests within 64
blocks" becomes a scan of everything. My recommendation is SQLite; the alternative is defensible if
we want zero dependencies and accept the extra care. `[U]`

Second alternative: a server database (Postgres/Redis). Rejected for now: another process to install,
run and keep alive on a PC that is supposed to be dedicated to the game. It becomes the right answer
only if we split into several machines. `[U]`

### 3.2 The fact envelope

Every world fact is one row with the same envelope. The envelope is what makes freshness uniform;
`data` is what makes facts different.

```sql
CREATE TABLE facts (
  key          TEXT PRIMARY KEY,  -- stable identity, e.g. 'container:0:112:63:-40'
  kind         TEXT NOT NULL,     -- 'container' | 'place' | 'inventory' | 'resource' | ...
  dim          TEXT,              -- 'overworld' | 'nether' | 'end'
  x INTEGER, y INTEGER, z INTEGER,-- NULL for non-spatial facts
  data         TEXT NOT NULL,     -- JSON payload, shape depends on kind
  source       TEXT NOT NULL,     -- 'sight' | 'ui' | 'chat' | 'inference'
  observed_at  INTEGER NOT NULL,  -- wall clock, ms since epoch (when we saw it)
  game_time    INTEGER,           -- world age in ticks, for "how long ago in game"
  revision     INTEGER NOT NULL DEFAULT 1,
  note         TEXT               -- optional: why this value changed
);
CREATE INDEX facts_kind ON facts(kind);
CREATE INDEX facts_pos  ON facts(dim, x, z);
```

Two deliberate choices, both overrulable:

1. **JSON in `data`, typed columns for the hot fields.** Position and dimension are the most common
   filter, so they are real columns. Everything else is JSON, so adding a fact kind needs no schema
   migration. The alternative — a column per field per kind — is stricter and faster but grows a
   migration every time we learn something new. My call: envelope columns + JSON.
2. **A fact is not deleted when it becomes wrong.** It is updated in place (`revision + 1`,
   `observed_at` bumped) or marked dead (§4.4). Rows are only physically deleted by an explicit
   "forget" rule (§8.4).

A field I deliberately left out: **`confidence`**. It sounds prudent, but a number between 0 and 1
that nothing computes from evidence is decoration, and an LLM reading "confidence 0.7" will treat it
as information. `source` (how we know) plus the verdict (how old) says the same thing honestly. Add it
only if we find a case where a real probability is available — e.g. a Jev estimate. `[U]`

Where the fields come from: `source` is filled by the perception layer (which window, which packet,
or which chat line produced the value); `game_time` is the world time the client reports in its time
packets. Both are `[U]` and belong to the session-2/3 interface; if the body cannot supply `game_time`,
`observed_at` alone is enough for everything except the "world rolled back" check (§8.3).

### 3.3 The event log

Facts say "what is true". Events say "what happened". You cannot reconstruct the second from the
first, and the user explicitly wants the past.

```sql
CREATE TABLE events (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  at        INTEGER NOT NULL,   -- wall clock ms
  game_time INTEGER,
  kind      TEXT NOT NULL,      -- 'death' | 'container_opened' | 'chunk_left' | 'item_gained'
                                -- | 'command' | 'danger' | 'handover' | 'skill_result'
  dim TEXT, x INTEGER, y INTEGER, z INTEGER,
  data      TEXT NOT NULL,      -- JSON
  importance INTEGER NOT NULL DEFAULT 1  -- 1 normal, 2 keep long / never prune
);
CREATE INDEX events_at ON events(at DESC);
```

Events are append-only (never updated). This is the table that answers "what did the agent do in the
last hour", "what did I die to", "did the user move my chest". It is also the raw material for
lessons (§9).

### 3.4 Text memory (the only place embeddings belong)

```sql
CREATE TABLE notes (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  at        INTEGER NOT NULL,
  kind      TEXT NOT NULL,   -- 'lesson' | 'episode' | 'knowledge' | 'preference'
  text      TEXT NOT NULL,   -- one plain sentence, written for a human to read
  tags      TEXT,            -- JSON array, e.g. ["nether","piglin","danger"]
  evidence  TEXT,            -- JSON: event ids or fact keys that support it
  embedding BLOB,            -- optional, NULL until we decide to embed
  model     TEXT             -- which embedding model produced it
);
```

`notes` is small on purpose. If it grows past a few thousand rows, the problem is not the search — it
is that we are writing junk.

### 3.5 Example rows

Concrete rows for the six kinds the user named. `data` is JSON; `…` replaces fields that do not
matter for the point.

**Places** — where the agent has been, and what is notable there.

| key | kind | dim | x | y | z | data | source | observed_at |
|---|---|---|---|---|---|---|---|---|
| `place:0:112:64:-40` | `place` | overworld | 112 | 64 | -40 | `{"name":"base","note":"2 furnaces, bed at 114,64,-40","seen_count":14}` | sight | 1793000000000 |

**Containers** — what is in a chest, and whether we may still believe it.

| key | kind | dim | x | y | z | data | source | observed_at |
|---|---|---|---|---|---|---|---|---|
| `container:0:118:64:-44` | `container` | overworld | 118 | 64 | -44 | `{"type":"chest","items":{"iron_ingot":23,"bread":6},"slots_used":2,"last_opened_by":"agent"}` | ui | 1793000600000 |
| `container:0:203:12:88` | `container` | overworld | 203 | 12 | 88 | `{"type":"chest","items":{"raw_iron":41,"coal":12},"slots_used":2}` | ui | 1792980000000 |

The second one was opened 5.7 hours ago. It is *not* wrong; it is *unconfirmed*.

**Inventory snapshots** — one fact, replaced on every change, with the previous values kept only as
events.

| key | kind | dim | data | source | observed_at |
|---|---|---|---|---|---|
| `inv:player` | `inventory` | — | `{"slots":[{"slot":0,"item":"iron_pickaxe","count":1,"damage":42},{"slot":1,"item":"bread","count":12},{"slot":9,"item":"iron_ingot","count":7}],"empty":24}` | sight | 1793001000000 |

**Resources** — a standing need or a known supply. This is the kind that answers "bring me iron".

| key | kind | dim | data | source | observed_at |
|---|---|---|---|---|---|
| `resource:iron_ingot` | `resource` | — | `{"want":3,"have":7,"known_supply":[{"where":"container:0:118:64:-44","count":23,"confirmed":true},{"where":"ore_vein:0:140:29:-70","count":8,"confirmed":false}]}` | inference | 1793001000000 |

**Dangers** — places that hurt, with the evidence.

| key | kind | dim | x | y | z | data | source | observed_at |
|---|---|---|---|---|---|---|---|---|
| `danger:0:151:31:-64` | `danger` | overworld | 151 | 31 | -64 | `{"kind":"lava","radius":4,"evidence":"agent took 7 damage at 1792990000000"}` | inference | 1792990000000 |
| `danger:0:112:64:-40` | `danger` | overworld | 112 | 64 | -40 | `{"kind":"mob_spawn","what":"zombie","count":3,"night":true}` | sight | 1793002000000 |

**Lessons** — text, in `notes`, not `facts`, because there is no identity and nothing to re-check.

| kind | text | tags | evidence |
|---|---|---|---|
| `lesson` | "Digging straight down at y=12 in this world hits lava; mine a staircase instead." | `["lava","mining"]` | `[event 4412, fact danger:0:151:31:-64]` |
| `preference` | "The user wants iron delivered to the base chest, not dropped on the ground." | `["user","delivery"]` | `[event 4501]` |

---

## 4. Freshness: how stale is detected and repaired

This is the part the user cares about most, so it gets the most care. Four mechanisms, each simple
and independent.

### 4.1 Facts never say "true". They say "true as of".

A read does not return a value. It returns a **value plus an age plus a verdict**:

```
Fact(container:0:118:64:-44)  value={iron_ingot:23, bread:6}  age=4m12s  verdict=Fresh
```

The verdict is computed by a pure function at read time, from `kind` + `observed_at` + invalidation
flags. No background thread, no timers, nothing that can silently stop running. `[U]`

### 4.2 The freshness policy table

One small table of policy, indexed by `kind`, written by hand. This is the whole "AI memory aging"
system and it fits on a screen.

| kind | Fresh while | Then | Cheapest re-check |
|---|---|---|---|
| `inventory` | 0 s (it is only true at the instant we read it) | always `Stale` | look at the HUD/inventory screen; the client is sent full inventory state on open `[U]` |
| `container` | the chunk stayed loaded **and** nobody else touched it | `Stale` | walk to it and open it (one `ui` read) |
| `place` | forever (a mountain does not move) | — | nothing; but re-confirm `name`/`note` when nearby |
| `resource` | derived; fresh if its `known_supply` entries are fresh | `Stale` | re-read the supplies |
| `danger` | 30 min game time for `mob_spawn`, forever for `lava` | `Stale` | look around, or don't bother |
| `player_state` (health, food, position) | 0 s | `Stale` | read the HUD |

Three rules that keep this honest:

- **Loaded-chunk rule.** A container fact is only `Fresh` if the chunk it lives in was loaded and
  entity-ticking for the whole time since we read it, and the agent saw no other actor near it. The
  moment the chunk unloads, every fact inside it drops to `Stale` — not because it is wrong, but
  because anything could have happened. `[U]`
- **No silent upgrades.** Only a new observation makes a fact Fresh again. Time passing never does.
  `[U]`
- **No fact is not the same as no thing.** If there is no row for `container:0:118:64:-44`, the answer
  is `Unknown` ("never looked"), never "empty". This distinction is cheap to keep and expensive to
  lose: a missing fact read as an absence is how an agent walks to a spot and claims a chest vanished.

### 4.3 Invalidation: what makes a fact stale without asking

Some events are strong evidence that a set of facts changed. Each one is a cheap hook:

| Trigger | Facts marked `Stale` | Why |
|---|---|---|
| Chunk unloaded | every fact inside it | nobody was watching `[U]` |
| Hand-over from user (§12.3) | everything except `place` (world terrain) | an hour of unknown play |
| Death | `inv:player`, all `container` facts in the death dimension older than the death | items were dropped and inventory emptied `[S]` |
| Block at a fact's position is no longer that block | that fact → `Gone` | the chest was broken/moved |
| Player count/tab list change, dimension change | all spatial facts of the old dimension → `Stale` | different world region `[U]` |
| User says "I moved the chest" | that container + `place` note | testimony, tagged as such |

### 4.4 The refresh action — the "instantly pull stale to current" answer

Every fact kind declares **how to re-check it with the body we have**. This is a small registry, and
it is where the phrase "pull stale data to current" becomes a concrete action:

| kind | refresh action | cost |
|---|---|---|
| `inventory` | open the inventory screen (E), read slots, close | ~0.5 s, no movement |
| `container` | path to the block, right-click, read the window contents, close | seconds + travel |
| `place` | nothing needed; if nearby, look at it | free |
| `resource` | refresh its `known_supply` entries | as above |
| `danger` | look around (sight), or skip | free |

This must respect the decided body: **actions are OS keyboard and mouse, and perception is read-only
from the proxy** `[V]` `docs/decisions.md` (2026-10-23, body direction). So the refresh action is
always *something a player could do*: walk, look, press E, right-click a chest. The agent cannot ask
the server "what is in that chest" — it must physically go and open it. That constraint is a feature:
it keeps memory honest and keeps the agent fair-play.

Result of a refresh is a new observation, written to `facts`, with an `event` row explaining why.
A refresh that finds something different is a **contradiction event** — always logged, because those
are exactly the moments worth learning from.

### 4.5 What the agent does with a Stale fact

Never silently use it. Three options, in this order:

1. **Refresh** if cheap (inventory, nearby container).
2. **Plan around the uncertainty**: "I believe 23 iron is in the base chest, unconfirmed; I will check
   it on the way."
3. **Ask the user** if the plan depends on it and the trip is expensive.

The LLM is told the verdict, so *the LLM makes this choice with the facts in front of it* — the
memory does not hide uncertainty from it.

---

## 5. Real time: two speeds

The user said "changes in real time". There are two different speeds, and conflating them is the
usual mistake.

**Fast path (every tick, in RAM, no disk).** The agent already needs a live view of the world to act:
position, health, what is in the loaded chunks, entities nearby. That lives in memory, refreshed by
perception at network speed. Memory writes to disk are **batched**: at most once per second, and
immediately on any event that matters (death, container opened, chunk left, command received). `[U]`

**Slow path (durable).** The SQLite write. A crash loses at most the last second of *facts*, and
nothing of the *events* that were already committed. `[U]`

Why not write every block change to disk? Because it is thousands of rows per second and buys nothing:
if we lose the position of a block we re-see it the moment we look. The rule: **write what we cannot
re-observe cheaply; buffer what we can.**

---

## 6. Semantic search and RAG: what fits, what does not

Short answer: **no for world facts, yes for a small text corpus, and never as the first step.**

### 6.1 Why not for world facts

- **Wrong failure mode.** A vector search returns the *most similar* row, always — even when the right
  answer is "I have never seen a chest here". Exact lookup can return zero rows; similarity search
  cannot, so it hallucinates relevance. `[U]`
- **No notion of "current".** "Chest with iron" matches 6 chests; the ranking knows nothing about
  which one still exists or is 40 blocks away. You would immediately add filters — i.e. rebuild the
  keyed lookup. `[U]`
- **Cost and latency where we cannot afford it.** The decision layer must answer in ~130–300 ms
  `[V]` `docs/research/README.md` point 3; embed-then-search on every query spends that budget on
  something a `WHERE` clause does in microseconds. `[U]`
- **The inventory is numbers, not language.** "How many iron do I have" is a count. Counting is
  exactly where embeddings are useless.

### 6.2 Where embeddings do earn their place

Three narrow uses, plus one that only appears once the user talks to the agent in natural language:

1. **Lessons and episodes** (`notes`): "have I learned something about piglins?" — a few thousand
   short sentences, retrieval by meaning. The research already recommends structured filters first,
   then embeddings, then a reranker `[V]` `docs/research/embeddings.md` (§Design implications 4).
2. **Game knowledge** (recipes, mob behaviour, version notes): the session-12 topic. Same shape:
   text corpus, not world state.
3. **Near-duplicate detection** when writing lessons, so we do not accumulate 40 versions of "lava is
   dangerous" `[U]`.
4. **Fuzzy user references**: "go back to that cave with the village". The user is naming a place by
   description, not by coordinates. Embed the place `note` text, match, then confirm the coordinate
   with the user before walking 800 blocks. This is the one case where meaning beats identity, and it
   must end with a confirmation, never an assumption. `[U]`

Even here, add metadata filters first: `tag=nether`, `at > last_week`. Cheapest model first — the
repo notes Qwen3-Embedding 0.6B runs free and local `[V]` `docs/research/embeddings.md` (§Qwen
models), with local latency still unmeasured.

### 6.3 The resulting retrieval order (cheap → expensive)

```
1. exact key lookup           (microseconds, exact)      "what is at 118,64,-44"
2. SQL range / filter         (microseconds, exact)      "chests within 64 blocks, kind=container"
3. SQL full-text (FTS5)       (fast, exact words)        "chests whose note mentions 'furnace'"
4. embeddings + filter        (tens–hundreds ms)         "lessons about the Nether"
5. LLM reads the top few      (seconds)                  only for judgement, never for lookup
```

RAG is step 4. It is a small part of a system whose correctness comes from steps 1–2.

---

## 7. Context assembly: how the LLM gets the right context

The LLM never touches the database. A **renderer** builds a plain-text card, deterministic, under a
character budget. If the same state is rendered twice, the same bytes come out — which makes bugs
reproducible and the prompt cacheable. `[U]`

### 7.1 Budget and tiers

Total ~4,000 characters by default (`[U]` my proposal; tune by measurement). Filled in this order,
each tier cut off independently so a huge tier cannot starve a vital one:

| Tier | Content | Budget | Rule |
|---|---|---|---|
| 0 | Task and rules: what the user asked, protected items, "never dig straight down" | 400 | always complete |
| 1 | Self: position, dimension, health, food, time of day, held item, inventory totals | 600 | `now`, always |
| 2 | The 8 nearest *relevant* facts (containers, dangers, places) | 1,200 | sorted by relevance × freshness |
| 3 | The last 6 events, newest first | 800 | includes death, commands, failures |
| 4 | Up to 3 retrieved lessons, with a relevance score | 600 | only if the task hints at a topic |
| 5 | Standing goals and the current step | 400 | always |

If tier 2 overflows, it drops the *least relevant* fact, not the oldest — an old fact about the chest
in front of you is worth more than a fresh fact about a chest 900 blocks away.

### 7.2 Every line carries its verdict

Three markers, chosen so an LLM cannot confuse them:

- `[now]` — read this instant.
- `[fresh]` — read N minutes ago, nothing could have changed it.
- `[stale]` — read N minutes ago, unconfirmed; cheap to check.

`gone` and `unknown` are never printed as if they were facts: a `gone` fact becomes a recent event
("the chest at 118,64,-44 is no longer there") and an `unknown` simply does not appear. A card must
never make absence look like a finding.

Tier 2 sorting is one line of code, not a model: `score = kind_weight + 1/(1 + distance/32)`, and any
fact that is not `fresh` is multiplied by 0.5 so a confirmed close thing beats an unconfirmed one at
the same distance. `[U]`

Numbers are pre-computed. The repo already notes that the decision model is weak at arithmetic
`[V]` `docs/research/README.md` point 3, so the card says "23 iron" and "7 in your inventory", never
"23 - 7 = ?".

### 7.3 A full example: `the user said: bring me iron`

World state: agent at the base, 7 iron ingots in the inventory, a chest 6 blocks away last opened 4
minutes ago, a second chest 900 blocks away last opened yesterday, a lava pool on the way to the mine.

```
=== TASK ===
User (chat, 12s ago): "bring me iron"
Interpretation: deliver iron ingots to the user. Ambiguous amount; assume "as much as you can
without breaking tools".
Rules: do not touch the user's diamond chest (protected). Do not dig straight down.

=== SELF [now] ===
Position: 112, 64, -40 (overworld, in the base). Health 20/20, food 18/20.
Time: day 41, 09:20 (morning). Held: iron_pickaxe (58% durability).
Inventory: iron_ingot x7, raw_iron x2, bread x12, cobblestone x64, torch x9. Free slots: 24.

=== NEARBY AND KNOWN [fresh/stale] ===
[fresh] chest at 118,64,-44 (6 blocks, base) - 23 iron_ingot, 6 bread. Checked 4m ago, chunk
        loaded since; nobody else was near.  <- enough to satisfy the request
[fresh] base at 112,64,-40 - bed, 2 furnaces, 1 crafting table.
[stale] chest at 203,12,88 (900 blocks, mine entrance) - 41 raw_iron, 12 coal. Checked 27h ago,
        chunk unloaded since. Unconfirmed.
[stale] furnace at 114,64,-41 - last known: 3 iron_ingot smelting. Checked 27h ago.
[fresh] danger at 151,31,-64 - lava pool, radius 4. On the route to the mine.
[stale] danger at 112,64,-40 - 3 zombies last night (mob danger expired at sunrise).

=== RECENT EVENTS (newest first) ===
09:18  user said: "bring me iron"
08:55  agent smelted 5 raw_iron -> 5 iron_ingot (furnace 114,64,-41)
08:40  agent returned to base, deposited 23 iron_ingot into chest 118,64,-44
07:02  night started; 3 zombies approached the base; agent fought them (took 2 damage)

=== LESSONS (tag-filtered, not ranked by a model) ===
[tag: user, delivery] The user wants iron delivered to the base chest, not dropped on the ground.
[tag: iron, mining]  Iron ore in this world is mostly at y=16..40 under the hill east of the base.

=== PLAN ===
Goal: deliver iron. Next check: open chest 118,64,-44 (6 blocks) to confirm the 23 - one action.
If confirmed: take 23, walk to the user, drop. If empty: re-plan (mine, or check the far chest).
```

Note what the card does *not* contain: the other 40 known containers, block-level chunk data, the
full event log, or any database rows in raw form. It contains the decision, not the archive.

### 7.4 Why not just put everything in a big prompt

Long context is not free: cost per turn, latency, and — the part that matters — a model reading 200
irrelevant facts makes worse choices than one reading 12 good ones. `[U]` The budget is the feature.

---

## 8. Crash and restart

Four different "crashes", four answers. This is where most memory designs quietly lose data.

### 8.1 The process dies (power loss, kill, exception)

- Facts and events are already committed to SQLite: WAL gives atomic commit and automatic recovery on
  next open. `[V]` https://sqlite.org/wal.html (§2, §9)
- Batching means at most ~1 s of *facts* is lost. Events are committed when they happen, so nothing
  important is lost.
- On start: open the DB, read `meta.last_session`, and **re-derive the truth from the body**: log in,
  read position/health/inventory/dimension/time. Those are `[now]` facts. Everything else keeps its
  age.

### 8.2 The game crashes or the world is reloaded

`meta` holds `world_id` (a hash of the level name + seed) and `session_id`. If `world_id` differs, the
memory is from another world: mark **everything** `Stale` or archive it to `praxion-<world_id>.db`.
Never mix two worlds in one truth. `[U]`

### 8.3 The server restarts and the world is rolled back

The dangerous case: memory says "23 iron in that chest", the world was rolled back to before we put it
there. Detection: `meta` stores `game_time` (world age). If the world age went *backwards*, we are
looking at an earlier state of the world. Mark all facts `Stale` and say so in the context card
(`world time moved backwards: memories from the future are untrusted`). `[U]` Cheap and catches a real
class of confusion.

### 8.4 Corruption or a bad migration

- A copy-on-write backup before any schema migration (`praxion.db.bak-<ts>`). `[U]`
- If the DB will not open: move it aside, start empty, and tell the user. A memory that refuses to
  start is worse than a memory that admits amnesia.
- Optional: SQLite backup API / `PRAGMA integrity_check` on startup when the file is small. `[U]`

### 8.5 What "restart" does *not* need

No journal replay of our own, no "did I flush" logic, no second log file — SQLite already is the
journal. That is the reason to pick it.

---

## 9. Turning experience into lessons (kept deliberately small)

The user said "knowledge it gained". The tempting version — an LLM summarising everything every N
minutes — is expensive and produces vague text. The minimal version:

- **Deterministic triggers**, not a timer: after a death, after a contradicting refresh, after a task
  fails 3 times for the same reason, after the user corrects the agent.
- The trigger hands an LLM a *small* packet: the last ~20 relevant events plus the facts involved.
- The LLM returns **one sentence** plus tags plus the evidence ids. One sentence is a constraint that
  prevents mush.
- Dedup by embedding similarity before insert (`notes` §3.4).
- Nothing is written as a `lesson` if it restates a fact ("the chest has 23 iron"): that belongs in
  `facts`, and lessons that duplicate facts go stale in the worst way.

Example output (real shape): `"Digging straight down at y=12 in this world hits lava; mine a staircase
instead."` with `evidence=[event 4412, fact danger:0:151:31:-64]`.

---

## 10. How the user inspects it

Inspection is a requirement, not a nicety: the user must understand the system `[V]` `AGENTS.md`.

**Level 0 — the file.** `praxion.db` is one file. `sqlite3 praxion.db` opens it. `[U]`

**Level 1 — canned queries, shipped in the repo** (`tools/mem.sh`):

```sh
mem.sh facts container          # every chest we know, with age and verdict
mem.sh near 112 64 -40 64      # everything within 64 blocks
mem.sh inv                     # inventory as last seen
mem.sh why container:0:118:64:-44  # full history of that fact + supporting events
mem.sh card --task "bring me iron" # exactly what the LLM would see
```

`why` is the important one: it prints the fact, its revision history, the events that changed it, and
the observation that created it. That is how a wrong belief gets debugged.

**Level 2 — the context card viewer.** A tiny read-only page (or terminal view) that shows the card
the LLM saw for each decision, with the character budget per tier. When the agent does something
silly, the first question is "what did it actually see?", and this answers it.

**Level 3 — the map.** Places, containers and dangers as a top-down plot (matplotlib/HTML canvas) per
dimension, with fresh/stale colour. Cheap to build from `facts`, and it is the fastest way to see
"the agent thinks there is a chest in a lake".

**Read-only enforcement.** The inspector opens the DB read-only (`file:praxion.db?mode=ro`) so it can
never corrupt a live run; WAL allows it to read while the agent writes. `[V]` sqlite.org/wal.html
(§2.2, §5)

---

## 11. Diagram

```
                       user's real client  ── keyboard/mouse ──> game
                              │                      ▲
                              │ packets              │ OS input
                              ▼                      │
                    ┌──────────────────┐    ┌────────┴────────┐
                    │ read-only proxy  │    │  action layer   │
                    │  (perception)    │    │  (press/click)  │
                    └────────┬─────────┘    └────────┬────────┘
                             │  observations         │
                             ▼                       │
        ┌────────────────────────────────────────────────────────┐
        │                     MEMORY  (one process)              │
        │                                                        │
        │  live view (RAM)         ┌──────────────────────────┐  │
        │  position, health,  ────>│ writer: batch 1/s,       │  │
        │  loaded chunks,          │ commit on every event    │  │
        │  entities                └───────────┬──────────────┘  │
        │                                      ▼                 │
        │  refresh(key) ──> action layer   ┌──────────────────┐  │
        │  "go open that chest"            │  praxion.db      │  │
        │                                  │  facts           │  │
        │  renderer ──> context card       │  events          │  │
        │  (budget, [now/fresh/stale])     │  notes           │  │
        │        │                         │  meta            │  │
        │        ▼                         └────────┬─────────┘  │
        └────────┼──────────────────────────────────┼────────────┘
                 │                                  │ read-only
                 ▼                                  ▼
        ┌─────────────────┐              ┌──────────────────────┐
        │  LLM / Jev      │              │ inspector: CLI, map, │
        │  (decides)      │              │ card history         │
        └─────────────────┘              └──────────────────────┘
```

Data flow in one line: **perception → live view → batched facts → renderer → LLM → action → new
observation → back into perception.** A closed loop, with the database on the side of it, not in the
middle of it.

---

## 12. Three scenarios, step by step

### 12.1 A chest moved while the agent was away

Setup: agent stored 23 iron in the chest at `118,64,-44`, then went mining for an hour.

1. **Leaving.** The chunk containing `118,64,-44` unloads. The invalidation hook (§4.3) marks that
   container fact `Stale`, and writes an event: `chunk_left (118,64,-44)`.
2. **Away.** The user breaks the chest and rebuilds it 3 blocks east at `121,64,-44`. Nothing is
   recorded — the agent is not looking. This is the honest normal case: *memory is ignorance, not
   truth.*
3. **Returning.** The chunk reloads. Perception says: block at `118,64,-44` is now `air`. The
   observation contradicts the fact. Memory writes `event(kind=contradiction, reason=block_gone)`,
   marks the container `Gone` (kept as history, not deleted), and — this is the part that matters —
   **does not guess** where it went.
4. **Discovery.** The agent walks toward the base and sees a chest at `121,64,-44`. New fact
   `container:0:121:64:-44` with `source=sight`, `data.items=null`, `revision=1`. It knows a chest
   exists; it does not know the contents until it opens it.
5. **Refresh.** Task "bring me iron" → the renderer sees `resource:iron_ingot` whose `known_supply`
   points at a `Gone` chest. Refresh action: path 3 blocks, right-click, read window → 23 iron.
   Fact updated (`revision 2`), event `container_opened`, and a **link**: `moved_from=118,64,-44`.
6. **Learning (optional, deterministic).** Second time this happens, the lesson trigger fires: "the
   user moves chests near the base; re-check before relying on a chest's contents."

What a bad design would do here: trust the stored coordinates and walk to an empty spot, then report
failure as "cannot find iron". The difference is one boolean (`verdict`), not a smarter model.

### 12.2 The agent died and must recover its items

Facts that matter (verified): on death, inventory items drop at the death spot and decay after five
minutes *while that chunk is loaded* `[S]` https://minecraft.wiki/w/Death (Player effects) and
https://minecraft.wiki/w/Item_(entity) (Despawning: 6000 ticks, timer paused when the chunk is
unloaded). A recovery compass (1.19+) points to the last death location in the same dimension; that
location is stored as the player's `LastDeathLocation` tag, which is only readable with cheats `[S]`
https://minecraft.wiki/w/Recovery_Compass — and we play with zero cheats `[V]` `docs/decisions.md`
(2026-10-21), so memory, not that tag, is our source for the coordinates.

1. **Death detected.** Health hits 0 (perception), or the death screen appears. The agent writes
   `event(kind=death, pos, cause, game_time, importance=2)` **before anything else** — this is the one
   write that must never be batched.
2. **Snapshot.** The last `inv:player` fact is copied into the death event (`items_at_death`). That
   copy is why the agent can later tell "the pickaxe is missing" rather than "something is missing".
3. **Invalidate.** `inv:player` → empty/`Stale`. Every container fact in that dimension older than
   the death → `Stale`. The world is now less known than it was a second ago, and memory says so.
4. **Respawn.** New position, full health, empty inventory: all `[now]`. The renderer puts the death at
   the top of the card with an **age in seconds** — because the decay clock is the whole problem.
5. **Triage under the deadline.** The plan is chosen against 5 minutes `[S]`:
   - Distance from spawn to death spot is known (both are facts) → travel time estimate.
   - If it fits with margin: go. Refresh `inv:player` only if something is picked up en route.
   - If it does not fit: take what is cheap (armour, food, tool from a chest) and *say so*, because a
     doomed sprint that arrives 40 s late is worse than an honest "too far".
6. **Recovery.** Items are entities; picking them up is a sight-plus-inventory change. Each pickup is
   an event; the death event gets `recovered: [iron_pickaxe, 12 bread]`. Diffing `items_at_death`
   against the final inventory gives the loss list, which is the input to the lesson trigger.
7. **The compass is a bonus, not the plan.** If the agent has a recovery compass in a known chest, it
   can fetch it and hold it to point at the death spot (same dimension only) `[S]`
   https://minecraft.wiki/w/Recovery_Compass. But the compass is itself an item that dropped at the
   death spot, and fetching it costs time from the same 5 minutes — so the coordinates remembered in
   step 1 remain the primary source and the compass is an extra confirmation when it is already in
   hand.

Note the subtlety that makes the "5 minutes" honest: if the death chunk is *not* loaded, the timer is
paused `[S]` — so the real deadline depends on whether anything is keeping that chunk alive, which
memory also knows (loaded-chunk facts). This is exactly the kind of thing a time-decay-only design
gets wrong.

### 12.3 The user hands control back after playing alone for an hour

This is the hardest case, because the agent's belief is not merely old — a *human* was in the world
changing it, and humans break things silently.

1. **Before hand-over (user starts playing).** A `handover` event. Memory writes a **snapshot marker**:
   `meta.cold_since = now`. It stops trusting everything spatial. Cheap alternative if the proxy keeps
   running: keep observing (the proxy sees the user's packets `[V]` `docs/decisions.md` body
   direction), in which case the hour is *not* unknown and nothing is invalidated. Which of the two
   happens depends on whether Praxion stays attached — undecided in session 2. **The memory design
   must not assume either**; it needs the `cold_since` marker for the detached case and works with
   live observation in the attached case.
2. **During the hour (detached case).** Nothing recorded. No timer writes anything. There is no
   "the agent thinks about what it missed" — it genuinely does not know.
3. **Hand-over back.** `event(handover_back)`. Memory invalidates by rule (§4.3): all `container`,
   `inventory`, `resource`, `player_state` facts → `Stale`. `place` facts survive (terrain rarely
   moves), `danger(lava)` survives, `danger(mob_spawn)` does not.
4. **Rebuild the cheap truth first (about one second).** Read position, dimension, health, food, time
   of day, inventory, held item. That is ~90% of what decisions need and it needs no travel.
5. **Reconcile the expensive truth lazily.** Do not tour 40 chests. Instead: the first task's renderer
   shows the few stale facts that matter, each with its age, and the agent checks them *when the plan
   touches them*. A "bring me iron" one minute after hand-over re-checks exactly one chest.
6. **Ask one question if the world contradicts itself.** If the agent is now somewhere it has no
   `place` fact for, or the inventory contains items it never had, that is a `contradiction` event:
   the card says "you are in an area I have not seen; I will explore before assuming". An honest
   "I don't know where I am" is a valid state — and it is exactly what a player would say.
7. **What the user sees.** A one-line report: "Back. I checked: I am at the base, I have 7 iron, 3
   chests are unconfirmed. Want me to re-check them?" No wall of text, no invented certainty.

---

## 13. What this design deliberately does not do

Each of these is a real option someone will propose; each is out of scope *for the minimal version*
and can be added later without changing the shape.

| Not doing | Why | When to revisit |
|---|---|---|
| Storing every block of every chunk (Baritone-style 2-bit cache) | The proxy gives exact perception of loaded chunks; we re-see instead of remember. Huge data, little gain. `[U]` | Long-distance travel through unloaded terrain |
| Embedding world facts | §6.1 — wrong failure mode, no notion of current | Probably never |
| Summarising everything with an LLM on a timer | Cost, vagueness, and it hides the source | If lessons prove too sparse (§9) |
| Versioning every fact forever | Storage noise; chest positions change constantly | If debugging needs it — add per-kind history |
| A graph database for relationships | "which chest has iron" is a filter, not a traversal | Multi-step causal reasoning (a later concern) |
| Multi-agent concurrent writers | One agent plays at a time `[V]` `docs/decisions.md` (player-bound demo) | Companion mode with two bodies |
| Learned decay rates | We do not have data yet | After we log real contradictions |

---

## 14. Open questions I cannot answer alone

1. **Does the proxy stay attached while the user plays?** This decides whether scenario 12.3 is
   "observe everything" or "invalidate everything". Body decision, session 2. `[unknown]`
2. **Does the agent see the HUD or only the packet stream?** Inventory reads via `ui` assume we can
   open the inventory screen and read window contents — true for a player, but the *proxy* snapshot vs
   the *screen* reading path differ in fidelity. Session 2/3. `[unknown]`
3. **Where does the code run** (same PC as the game, or a Pi)? Affects whether SQLite is enough or a
   server DB is needed. Session 15. `[unknown]`
4. **Is one snapshot per second the right rate?** Needs a measurement of how often facts actually
   change during normal play. `[unknown]`
5. **How much of the "past" does the user want?** Full event log forever, or a rolling window (e.g. 30
   days, then a summary)? This is a product question for the user, not a technical one.
6. **Do lessons carry across worlds?** A fresh world is a new `facts` world, but "do not dig straight
   down" is true everywhere. Keeping lessons global means the agent starts every new world smarter;
   the risk is that advice about *this* world's lava leaks into another. My default would be global
   lessons tagged with the world they came from, but this is a product choice. `[unknown]`

---

## 15. Comparison with the first proposal (`memory-world-state.md`)

Read after finishing the design above.

### 15.1 Why it was hard to read

The proposal is good work and most of its conclusions match mine. The problem is the order it presents
them in. Three concrete causes:

1. **It starts with literature.** Five systems (Generative Agents, Optimus-1, MrSteve, Zep, plus the
   repo reviews) arrive before any single record shape. A beginner has to hold five designs in their
   head before seeing one row they could write down.
2. **Its central mechanism is abstract.** "An append-only event journal is the source of truth; the
   fact store is a projection of it" requires the reader to hold two representations of the same
   knowledge and understand a derivation step. True and useful, but it is the *second* thing to
   explain, not the first.
3. **No worked example.** No example rows, no example context card, no walkthrough. Every claim is
   correct but none is shown operating on a real chest and a real request.

My file leads with one row, one verdict, one card, one scenario; the literature is pushed to this
section. Same content, different order.

### 15.2 Where the two designs genuinely differ

| Point | First proposal | This proposal | Why I chose mine (user overrules) |
|---|---|---|---|
| **Source of truth** | Append-only journal; beliefs are a projection | Facts are written directly; events are a separate log for "what happened" | Fewer moving parts, no rebuild step, nothing to get out of sync. Cost: I lose "replay the journal to rebuild beliefs". Adopt theirs if replay/debug value beats simplicity — it is a close call. |
| **Refresh timing** | Read-through: the resolver tries to refresh on *every read* | Refresh is an explicit action the planner chooses, with a known cost | A read must be fast and side-effect free. A read that can silently start a 200-block walk is a hidden action, and hidden actions are how agents do surprising things. |
| **Staleness model** | Decay constants per kind (Generative Agents style) | Verdict from a policy table + invalidation flags; no decay formula | No data to fit decay constants to. A boolean plus an age is testable today; tuned decay is a later optimisation. |
| **Own inventory / health / position** | "Live only, never stored as memory" | Stored as a fact whose verdict is `Stale` the instant it is written | We need the pre-death inventory to know what was lost (§12.2). Storing it with `fresh while 0 s` gets both. |
| **Scoring of lessons** | recency + importance + relevance, min-max scaled | Not scored; filtered by tags, ranked by distance and verdict | Scoring is the part most likely to be wrong and hardest to debug. Revisit when there is data. |
| **Storage specifics** | SQLite + FTS5 (BM25) + R*Tree + optional `sqlite-vec` | SQLite; JSON payloads; FTS5 only if needed | Same choice, fewer modules to start. Their FTS5 note is the right next step for `notes`. |
| **Evaluation** | A scripted-world test suite with a vector-store baseline | Not covered here | **Genuinely better.** Their §6 is the part I would keep first; my design is unusable if contradictions are not tested. |
| **Fuzzy user references** ("that cave with the village") | Handled by embedding over place descriptions | Was missing from my draft; added to §6.2 item 4 after reading theirs | **Their gap-filling wins.** Meaning beats identity when the user names a place by description, with a confirmation before walking. |
| **Cross-world lessons** | Named as an open question | World facts are isolated by `world_id`, but `notes` has no `world_id`, so lessons silently carry across worlds | I did not think about it — theirs asks the right question. Decision needed (§14 Q6). |

### 15.3 Things both designs agree on

Not RAG for world state; embeddings only for text; never show an LLM a fact without its age; never let
a model overwrite a fact; SQLite per world; keep the history; let the user inspect the memory. Where
two independent passes agree, the decision is probably right.

### 15.4 Two ideas I should have stated more explicitly

Both come from the first proposal and are worth copying into the minimal version:

- **"Unknown" is a value, not an error** (Craftonomous, via the first proposal). A missing fact must
  never be read as "there is nothing there". My first draft had only `fresh`/`stale`/`gone`; after
  reading theirs I added `unknown` to §4.2, because `gone` covers a broken chest but not "I have never
  looked here".
- **Invalidate, never delete.** I keep `Gone` rows and revisions but delete by policy (§8.4). Their
  rule, "supersede, keep the history", is stricter and better for auditing a wrong belief later.

---

## 16. Beginner glossary

- **Fact**: one claim about the world with an age, e.g. "the chest at 118,64,-44 had 23 iron 4 minutes
  ago".
- **Key**: the identity of a fact, so it can be found without searching, e.g.
  `container:0:118:64:-44`.
- **Stale**: still possibly true, but unconfirmed; anything could have changed it.
- **Verdict**: the label a read attaches — `now`, `fresh`, `stale`, `gone`, `unknown`, or `guess`
  (from testimony).
- **Refresh**: the cheapest player action that turns a stale fact into a fresh one.
- **Invalidation**: marking facts stale because an event (chunk unload, death, hand-over) means they
  may have changed.
- **Context card**: the small text an LLM receives, built from memory under a character budget.
- **Embedding**: a list of numbers representing meaning, so that similar sentences are close together.
  Used only for text here (§6).
- **RAG** (retrieval-augmented generation): fetch relevant text first, then let the model read it. A
  technique for text, not a way to store world state.
- **WAL** (write-ahead log): SQLite writes changes to a log and appends a commit record; after a crash
  it replays the log. `[V]` https://sqlite.org/wal.html
- **Chunk**: a 16×16 column of the world. The game only simulates (ticks) chunks near the player;
  unloaded chunks are frozen. `[U]` (the item-timer behaviour in §12.2 is the source-backed example
  `[S]`)
