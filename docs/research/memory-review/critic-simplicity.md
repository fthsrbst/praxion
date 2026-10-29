# Simplicity review of the first memory proposal

Reviewer role: simplicity first (YAGNI). Date 2026-10-27.
Subject: [memory-world-state.md](../memory-world-state.md). Nothing here is a decision; every
paragraph is a proposal with its main alternative.

YAGNI means "You Aren't Gonna Need It": do not build a part until a real task needs it.
The project principles it enforces: question every addition, deleting beats adding, build in
dependency order, no speculative scaffolding [V: AGENTS.md §Engineering principles].

## 0. Tools and evidence

- Web tools worked in this session. I fetched the Minecraft Java protocol packet list and read
  the sections named below [V: https://minecraft.wiki/w/Java_Edition_protocol/Packets, page states
  protocol 777 for Java Edition 26.3].
- I did **not** read the academic papers (Generative Agents, Optimus-1, MrSteve, Zep) myself. When
  I mention them I cite the summary in `memory-world-state.md` and say so.
- I did **not** read the two dev.to critique posts. Anything from them is marked as second-hand.
- Repo files read: `AGENTS.md`, `docs/decisions.md`, `docs/research/memory-world-state.md`,
  `docs/research/systems-review.md`, `docs/research/README.md`, `docs/research/embeddings.md`,
  `docs/research/decision-models.md`, `docs/discussion-plan.md`, `backlog.md`, `notes.md`.

## 1. What the first demo actually asks of memory

The first demo is fixed: fresh survival world; hand-over; a verified chat command; instant
take-over; an autonomous first night [V: docs/decisions.md, 2026-10-23].

That is roughly one Minecraft day near the spawn point. A full day/night cycle is 20 real
minutes [U, not verified this session]. So the demo is a few tens of minutes of play, in one
small area, with one process running.

The memory questions the demo really generates:

| # | Question in plain words | Where the answer comes from |
|---|---|---|
| Q1 | "Where is my shelter / my bed?" | Memory: a named place with coordinates |
| Q2 | "What is in the chest by the door?" | Memory: contents from the last time it was opened |
| Q3 | "What did I just do, why did I die?" | A log of events, read by time |
| Q4 | "What do I need to make a stone pickaxe?" | Static game data, **not** memory |
| Q5 | "Where was that coal I saw?" | Memory of block positions, **optional** for the demo |

There is also a sixth need that the first proposal does not name: when control is handed over and
taken back, the agent must remember **what it was doing** (current goal, current sub-goal). That
is *session state*, not world memory. It is cheap and it must not be confused with a place or a
fact. See §5.

Everything below is judged against Q1–Q3 plus static data. Q4 and Q5 are the honest yardstick
for "do we need more".

## 2. Verdicts, one line each

| # | Component in the proposal | Verdict | Why |
|---|---|---|---|
| 1 | Live state (RAM, "now") | **Keep — but it is not memory** | It is perception. It must exist for any agent, memory or not. |
| 2 | Event journal as source of truth | **Shrink** | Keep an append-only log. Drop "the fact store is a projection of it". |
| 3 | Belief store (facts + age + provenance) | **Keep, smaller** | This is the user's requirement. Cut it to what the demo asks: places, containers. |
| 4 | Lessons | **Postpone** | Nothing in the demo reads them. The demo needs skills, not distilled lessons. |
| 5 | Static game data | **Keep, but outside the memory system** | Needed before memory: the first five minutes of play are crafting. |
| 6 | Resolver (read-through refresh) | **Shrink to a read rule** | "Live wins if it can be observed for free" is three lines, not a component. |
| 7 | Briefing | **Keep as one function with a token cap** | Needed, but it is a renderer of loaded data, not a subsystem. |
| 8 | Freshness classes (per-kind TTL table) | **Merge** | One stored timestamp plus three labels (live / seen / unknown) is enough. |
| 9 | Push invalidation from block packets | **Drop for now** | Inside loaded chunks live state already wins; outside loaded chunks those packets do not arrive. |
| 10 | Lesson scoring (recency/importance/relevance) | **Drop now** | Its input (lessons) does not exist yet, and the proposal itself says do not tune before measuring. |
| 11 | Vector search / embeddings | **Postpone, with an explicit trigger** | The demo's memory is a few dozen rows. A list in the prompt beats an index at that size. |

Score: 4 kept (1, 3, 5, 7), 2 shrank (2, 6), 1 merged (8), 1 postponed cheaply (11), 2 postponed
as features (4, 10), 1 dropped (9).

## 3. Component by component

### 3.1 Live state — needed, but stop calling it memory

Live state is the agent's copy of what the game client currently knows: blocks in loaded chunks,
entities the server is sending, the player's inventory and health. "Loaded chunk" means a
16×16 column of blocks the client currently has in memory and keeps updated.

This is perception, and the demo cannot start without it. The proposal's own freshness table
says "own inventory, health, position: live only, never stored as memory" [V:
memory-world-state.md §4]. That is correct and it is the biggest scope cut available: the most
frequently needed information is never memory at all.

Proposal: keep live state inside the perception layer, outside the memory system's boundary, so
the memory system only ever owns things the client *stopped* knowing.
Main alternative: one unified world model that mixes live and remembered data. Reason I advise
against it: then every consumer must ask "is this current?", and the answer is not in the data.
With a hard boundary, "remembered" means "not current by construction", which is a much safer
invariant. Cost of my choice: two APIs instead of one, and the read rule in §3.6.

### 3.2 Event journal — keep the log, drop the projection

An "append-only journal" is a file where lines are only ever added, never changed. "Projection"
means the fact store is rebuilt by replaying the journal through deterministic code.

What the journal buys us: replay for debugging, and "what happened last night" answers.
What the projection buys us: nothing the demo needs. The fact store has exactly one writer (our
own code) and is small. Rebuilding it from a log adds a second thing that can be wrong: when a
believed fact is false, we must now ask whether the observation was wrong or the rebuild was
wrong.

Proposal: keep one append-only file. Do **not** mark it as the source of truth for facts, and do
not build a projection step. Facts are written to their own small store at the moment of
observation; the log records significant events *and* every fact write.
Main alternative: full event sourcing (journal as truth, everything else derived). It is the
right long-term answer if we ever want exact time-travel over the agent's beliefs, which is
attractive for "the user must master every part". Cost: a rebuild path to build, test and
maintain from day one. My advice is to postpone it and keep the door open: because both files
are plain text and timestamped, adding a projection later is a refactor, not a rewrite.

Note: this section is where I disagree most with the first proposal, and it is a close call.

### 3.3 Belief store — keep, but cut the record shape

A "belief" here means: something the agent saw once and now holds with an age label.

The proposal's record is
`{world, kind, key, value, observed_tick, observed_wall, source, confidence, superseded_by, valid_to}` [V: memory-world-state.md §4].

Field by field, for the demo:

| Field | Proposal | Reasoning |
|---|---|---|
| `world` | keep | One file per world makes it redundant *inside* the file; keep it as the file name. |
| `kind` | keep as separate sections | With 2–3 kinds, separate small record types read better than one generic table. Generic wins only when kinds grow past ~10. |
| `key`, `value` | keep | That is the fact. |
| `observed_tick` | keep | Game time explains day/night and mob spawning. |
| `observed_wall` | keep | Wall clock explains process restarts and pauses. Cheap, and they answer different questions. |
| `source` (provenance tags) | **drop for now** | Our only sense is the proxy. Sight/hearing/testimony/inference tags exist in Craftonomous because it has several senses and a test harness that can cheat [V: systems-review.md §3.9]. Fair play is enforced by *one write API that only perception can call*, not by a label on every row. Add tags back when LLMs write lessons or when the user's chat becomes a "testimony" source. |
| `confidence` | **drop** | Code writes facts it observed: certainty is 1. Confidence only means something for model-written facts, which do not exist yet. |
| `superseded_by`, `valid_to` | **drop** | These two fields exist to keep fact history. The demo has no query that needs a fact's history; the *event log* answers "what happened". |

Removing those three fields removes the need for invalidation logic: a new observation of the
same key is simply appended later and the newest row wins.
Main alternative: keep the proposal's fields as they are. Cost: every reader must handle
"superseded" rows, and we must write code to maintain `valid_to` correctly, for zero demo queries.
Risk of my choice: a future query like "how much iron did the chest have yesterday" would need
the log instead of the store. That is acceptable; it is what the log is for.

### 3.4 Lessons — postpone

A lesson is a distilled rule in natural language, e.g. "creepers break through a dirt wall, use
stone". The proposal wants them embedded, scored and written by an LLM [V:
memory-world-state.md §4].

Nothing in the demo reads a lesson. The demo needs working skills and reflexes. The one place
the word "lesson" appears today is skill reliability, which belongs to the skill layer, not to
memory [V: systems-review.md §5]. Keeping both would double-book the same information.
Postpone, and do not reserve schema space for it.
Main alternative: build lessons first because they are the interesting research. That is a
research project, not the demo, and it would delay Q1–Q3.

### 3.5 Static game data — needed, and it is not memory

Recipes, block hardness, item names and mob stats come from Mojang's data generator reports,
which the backlog already lists as the planned source [V: backlog.md, "Session 2.2 decisions
pending"]. This is read-only reference data, identical in every world.

Two simplifications against the proposal:
- It is not part of the memory system. No ages, no provenance, no embeddings. A plain lookup plus
  a small recursive solver ("to get a stone pickaxe, get cobblestone and sticks…") is enough
  [V: systems-review.md §3.5, Odyssey's tables, ~1,265 lines of JS for 789 items].
- It is needed *before* memory in the build order. The first demo minutes are punching a tree and
  crafting. Memory has no role until something is out of sight.

Main alternative: build the recipe data as a graph with LLM-readable descriptions (Optimus-1
style). Cost: an index we do not need for a deterministic solver.

### 3.6 Resolver — shrink it to a read rule

The proposal's resolver, on every read, tries to refresh the fact from live perception and falls
back to age-labelled memory [V: memory-world-state.md §4]. As a *component* this is more than
needed. As a *rule* it is the whole design:

```
read(key) ->
  if live state can answer it now, for free  -> LIVE   (value, age 0)
  else if memory has it                     -> SEEN   (value, age = now - observed)
  else                                      -> UNKNOWN
```

Key point the first proposal misses: a live check does not exist for every kind of fact.

- Blocks in a loaded chunk: the client is being kept up to date, so the check is free and exact
  [V: "Block Update", "Update Section Blocks", "Chunk Data and Update Light" in
  https://minecraft.wiki/w/Java_Edition_protocol/Packets].
- Container contents: the server sends them when a container window is opened, and afterwards
  only to correct a mismatch [V: "Set Container Content", same URL]. Standing next to a chest
  does **not** tell you what is inside. Refreshing this fact costs an *action* (walk there, open
  it), not a read.

So "instant refresh" is partly free (loaded blocks, entities, own inventory) and partly an action
with a cost. The memory layer must never perform actions to refresh itself. Deciding "is it worth
walking over to check?" belongs to the brain.
Main alternative: let the resolver trigger a refresh action. Cost: memory silently moves the
player, which breaks take-over, breaks timing, and makes bugs hard to attribute.

### 3.7 Briefing — keep, as one function

The briefing is the text (or structured state) handed to Jev or an LLM. It is needed: a model
cannot read a database.

But it should be a *renderer* of data already in RAM, with a hard cap, not a subsystem:

```
brief(state) -> { goal, inventory_counts, health, time_of_day,
                  nearby (≤N blocks/entities), places[] with age, containers[] with age }
```

Why a hard cap: Jev is a single-pass decision model at ~130–300 ms and its quality drops with
long irrelevant state [V: docs/research/decision-models.md, citing vendor docs I did not read].
Also, Jev is weak at counting and arithmetic [V: same file], so the briefing must not ask it to
add up inventory; code computes "have 3 of 3 planks: enough".
Main alternative: no briefing function; give the LLM tool access to query memory directly. That
is fine for the slow LLM but impossible for Jev, which returns one typed answer per request and
cannot call tools. Keeping one renderer serves both.

### 3.8 Freshness classes — merge into one timestamp and three labels

The proposal has a nine-row table of kinds with different staleness rules [V:
memory-world-state.md §4]. All of it is a guess: there is no data yet on how fast a remembered
container becomes wrong, and the proposal itself says not to tune decay constants before there is
an evaluation.

The labels that matter are behavioural, not temporal:

| Label | Meaning | What the consumer does |
|---|---|---|
| LIVE | observed now, exact | use it |
| SEEN | observed at time T, may have changed | use it, and say the age |
| UNKNOWN | never observed, or out of range | ask, explore, or say "I don't know" |

The last one is the one systems forget: "unknown" is a value, not "absent" [V:
systems-review.md §3.9, Craftonomous]. Age is a number, so any consumer that wants a TTL can
apply its own threshold; storing the threshold per kind is an optimisation, not a requirement.
Main alternative: keep the per-kind table. Cost: nine rules to keep consistent, none measurable
today.

### 3.9 Push invalidation — drop for now, and note a factual worry

The proposal's push invalidation says: the proxy sees block-change packets, so a remembered
block can be contradicted the instant it changes [V: memory-world-state.md §4].

The packet exists ("Block Update", one block; "Update Section Blocks", many) [V: URL above].
The problem is where it can help. The server sends block updates only for chunks the player is
tracking [U, to verify in session 2]. That means:

- chunk loaded → live state already reflects the change, so memory does not need a push;
- chunk not loaded → no packets arrive, so a push cannot happen.

The case where the client *stops* being informed has its own packet: "Unload Chunk", which tells
the client to drop a chunk column entirely [V: URL above]. That is the exact moment a live fact
dies and memory becomes the only copy.

Proposal: drop push invalidation; if a hook is wanted later, hook "Unload Chunk" and snapshot the
few facts worth keeping (named-place anchors, containers, ores). That is one handler instead of
a general invalidation system.
Main alternative: keep push invalidation for correctness of nearby memory. If we later find that
memory is read for loaded chunks too (e.g. a path planner reading the memory store instead of
live state), the argument changes; the fix then is to make the planner read live state, not to
add invalidation.

### 3.10 Lesson scoring — drop now

The scoring formula (recency decay + importance + relevance) comes from Generative Agents [V via
memory-world-state.md §3; I did not read the paper]. It is a reasonable baseline *for lessons*.
Lessons are postponed, so the formula has no input. It is also untestable today: without a
labelled question set, every weight is a guess.
Main alternative: adopt it when lessons exist, and only against a no-scoring baseline.

### 3.11 Vector search — postpone, but answer the user's question plainly

Terms first. An **embedding** is a list of numbers that stands for a piece of text, produced by a
model, such that similar meanings get similar numbers. A **vector index** finds the nearest
embeddings to a query. **RAG** ("retrieval-augmented generation") means: find text passages by
similarity, paste them into the prompt, let the model answer from them.

Is RAG suitable here? Partly, and not for the parts the user listed first.

- Coordinates, counts, inventories, container contents: **no**. They have exact keys and a newest
  value, and the newest value must win. Similarity search ranks by wording, so two versions of the
  same fact are equally "similar" and an old one can rank above the current one [S, second-hand:
  the two dev.to posts cited in memory-world-state.md; I did not read them]. For these, a key
  lookup plus a timestamp is the correct operation.
- Static text knowledge (a wiki page about the End, a long guide): **yes**, RAG is the natural
  shape. Not needed for the demo, because the demo's knowledge is recipes and a handful of mobs.
- Lessons and "the cave where we found the village": **yes**, similarity helps. Both are
  postponed. For the fuzzy place query there is a cheaper 80% solution: keep a short place list
  with names and hand the whole list to the model, which can match "cave with the village" itself.
  That works while the list is small.

Concrete trigger for adopting vectors, to be agreed later: when the place/lesson list no longer
fits the briefing budget, or when measured retrieval failures per session exceed a threshold we
set. Until then, no embedding model, no vector store, no re-embedding pipeline.
Main alternative: adopt local Qwen3-Embedding now because it is free [V: embeddings.md]. Cost:
a model download, a Windows dependency, an unmeasured latency budget, vector storage, and the
versioning rule that every vector must record its model id [V: embeddings.md §Design implications
1]. That is a lot of moving parts for a demo memory of a few dozen rows.

## 4. "Pull stale data to current instantly" — the part that is physically impossible

This is worth saying clearly to the user, because the requirement sounds like a database
requirement and is actually a physics limit.

- Inside loaded chunks: the client is being kept current by the server, so "now" is exact and
  free. Nothing to design [V: Block Update / Chunk Data packets, URL above].
- Outside loaded chunks: the client receives nothing about those chunks, and when a chunk unloads
  it is explicitly told to forget it [V: "Unload Chunk", URL above]. After that moment, **no
  system can know the current state**. The information does not exist on this machine.
- Therefore "instant" can only mean: (a) it is exact for what is loaded, and (b) for the rest the
  agent knows *that* it is old, how old, and can decide to go and look.

Any design that promises instant freshness everywhere would have to cheat (ask the server
directly, or read the world files), which the fair-play decision forbids [V: docs/decisions.md,
"zero cheats"; AGENTS.md "an agent's action and perception APIs expose only what a player can
do"].
Main alternative: none. This is a limit, not a choice.

## 5. The smallest memory that meets the requirement

```
proxy ─► live state (RAM)  ──────────────► read(key): LIVE / SEEN / UNKNOWN
              │                                        │
              │ on observation worth keeping           │
              ▼                                        ▼
        world.jsonl (append-only)  ──fold at load──► in-RAM index ──► brief() ──► Jev / LLM
              │                                        │
              └──────────────► memory viewer (read-only) ◄──────────┘
```

Four parts, all small:

1. **One append-only text file per world** (`world.jsonl`, one JSON object per line). The last
   line for a key wins on load. That gives "current now" and "keeps the past" from one mechanism,
   with no projection code. "Fold at load" means: read the file top to bottom and keep the newest
   value per key.
2. **An in-RAM index** built from that fold: places, containers, and recent events. Everything
   the briefing needs is already in memory, so a decision costs microseconds, not a database
   query. This matters because Jev is called often at 130–300 ms per call [V:
   docs/research/decision-models.md].
3. **The read rule** from §3.6, in three branches.
4. **One briefing function** with a fixed token budget.

Record shapes, with the fields I would drop from the proposal's version:

```
place     {name, dim, pos, first_seen, last_seen}          // "shelter", "chest corner"
container {pos, dim, items[], last_opened}                 // items = "3 iron_ingot"
event     {t_tick, t_wall, kind, text?, refs?}             // death, item lost, command, place named
session   {goal, subgoal, owner: praxion|user}             // what it was doing at take-over
```

Example of "newest wins, history kept", in the file:

```
{"t":1200,"k":"container","pos":[12,64,-3],"items":["3 iron_ingot"]}
{"t":9000,"k":"container","pos":[12,64,-3],"items":["1 iron_ingot"],"note":"mine"}
```

The second line is the current belief; the first is still readable and answers "it used to have 3".

Storage proposal: plain JSON Lines while the file stays small (readable with any text editor,
which serves the "user masters every part" goal, and no dependency).
Main alternative: SQLite per world, as the first proposal suggests. It brings atomic writes,
indexes and (later) full-text and spatial queries [V: memory-world-state.md §4 states FTS5 and
R*Tree; I did not open sqlite.org]. Reason to wait: a database adds a query language, a schema
migration habit and an opaque binary file, in exchange for scale we do not have. Reason to switch
early: if the event log grows past a few hundred thousand lines, or if the viewer needs paging.
That is a measurable switch point, not a guess.

Note on session state: it lives in the same file but is not world memory. It is the answer to
"what was I doing?" across take-over.

## 6. What this smallest memory cannot do

Stated plainly, so nobody expects more:

- It cannot tell you what is in a chest without opening it. It reports the last time it looked,
  with the age [V: "Set Container Content" arrives when a window opens].
- It cannot make anything outside loaded chunks current. It can only label it old (§4).
- It cannot plan a path through terrain it has never seen. No block cache exists, so long-distance
  travel is planned from what is currently loaded, like a player with normal render distance.
- It cannot find a place by meaning ("the cave with the village") unless a name or the place list
  allows it. No embeddings.
- It cannot reuse lessons across worlds, or learn a tactic as text. Nothing distils experience.
- It cannot answer "how much iron did chest A have yesterday" from the store; the log has the
  writes, so the answer is a log query.
- It cannot dedupe near-duplicate places automatically. If the agent names a place every time it
  stops, the list fills with junk. Mitigation is a rule ("naming is a decision, not a side
  effect"), not clustering code.
- It cannot survive a corrupted line in the middle of the file unless loading skips bad lines.
  Cheap to do; must be done, because an append-only file that fails to load loses everything.

## 7. Staged build order

Each stage is verifiable on its own, and each one is a prerequisite of the next.

**Stage A — perception plus static data (not memory).**
Live state from the proxy, and recipe/item/block lookup from Mojang data.
*Check:* "what block is at (12,64,-3)?" returns the same answer the game shows; "what makes a
stick?" returns planks. No memory file exists yet.

**Stage B — belief store, places and containers only (Q1, Q2).**
`world.jsonl`, the fold, the read rule, the write API that only perception may call.
*Check:* name a place, fill a chest, walk beyond render distance, restart the process, ask both
questions. Then change the chest with the window closed and confirm the answer comes back marked
with its age, and that opening the chest corrects it. This is the whole "stale to fresh"
mechanism, tested without any of the machinery I dropped.

**Stage C — event log and viewer (Q3).**
Significant events appended (death, item gained/lost, place named, command received, control
handed over). A read-only viewer that prints the folded facts and the last N events.
*Check:* after a 20-minute session, "what happened in the first night" is answerable from the log,
and the viewer shows exactly what the briefing would show. The viewer is also how the user checks
his understanding of the system; it is cheap and worth doing early.
*Why before the briefing:* a wrong briefing is much easier to debug when you can see the facts it
was built from.

**Stage D — briefing with a budget (Q1–Q3 usable by models).**
One renderer, hard cap, no arithmetic asked of Jev.
*Check:* build 20 briefings from a recorded session; count tokens (must stay under the cap) and
count **unlabelled remembered facts** (must be zero). Then a scripted question set: 20 questions
with known answers; every answer either correct or explicitly uncertain. Target to be set with
the user.

**Stage E — only what measurement asks for.**
Candidates, each behind a stated trigger:
- block memory of unloaded chunks (snapshot on "Unload Chunk", or a Baritone-style chunk cache)
  → trigger: the demo's travel distance regularly exceeds view distance.
- lessons plus scoring → trigger: skills exist and a repeated mistake is observed twice.
- vector search → trigger: the place/lesson list no longer fits the briefing budget, or measured
  retrieval failures.
- SQLite → trigger: file size or viewer paging.

Stages A–D are the demo. If the demo lands well, E is the next project, chosen with data.

## 8. Where the first proposal is right, and I would keep it

- "Most of this is not RAG" is correct and is the most valuable sentence in the file [V:
  memory-world-state.md §2].
- Two time stamps (game tick and wall clock) per fact: keep both.
- "Never send a model a remembered fact without its age" and "a model may not overwrite a fact":
  keep as rules, they cost nothing and prevent confident nonsense.
- "Unknown is a value, not absent": keep.
- The memory test suite [V: memory-world-state.md §6]: keep, but shrink the scripted world to the
  demo scenario and add the two counters from stage D. Testing memory against a scripted world is
  the only way to know any of this works, and it is cheaper than the features it replaces.

## 9. Risks in my own proposal

- **Append-only JSON Lines can grow without bound.** Mitigation: compaction on load, and a size
  counter in the viewer. If it becomes a nuisance, switch to SQLite (stage E).
- **Folding at load costs startup time.** Fine for thousands of rows, unknown for a million. That
  is a measurement, not an assumption.
- **One generic read rule may hide per-kind differences** (chest refresh is an action). The rule
  keeps the distinction visible by returning SEEN for chests; a reviewer should check that no
  caller treats SEEN as LIVE.
- **Dropping provenance tags could weaken fair play later.** The mitigation is structural (single
  write API), which is stronger than a label *if* nobody bypasses the API. A test that fails when
  memory is written from outside perception should exist from stage B.
- **Dropping fact history loses "how much iron was in the chest yesterday" from the store.** The
  log still has it, but nobody will have written a query for it. Acceptable for the demo;
  write it to the backlog if it turns out to matter.
- I may be too strict. The counter-argument for building the journal-as-truth now is real
  (§3.2); if the user values exact time-travel debugging above everything else, that flips my
  recommendation, and it is his call.

## 10. Questions for session 3

Not decisions, just the five that change the shape most:

1. Is "the agent knows what it cannot see is old, and can go and look" an acceptable meaning of
   "instant" (§4)? If not, the requirement cannot be met without cheating.
2. Is a plain-text memory file (readable and editable by hand) worth more than a database, given
   the "user must master every part" goal?
3. Is session state (the current goal across take-over) part of the memory system, or a separate
   small file? I propose separate.
4. For the demo, is "remembered chest contents with an age label" enough, or must the agent verify
   by walking over before answering?
5. Should the viewer be part of stage C, so we can see inside the memory from the first day?

## 11. Unknown

- Block-update reach for chunks the player is not tracking: I claim no packets arrive [U]. Verify
  in session 2 against the protocol docs and a real capture.
- Whether the client keeps any chunk data after "Unload Chunk": unknown to me. If it does, our
  snapshot trigger changes.
- Real startup cost of folding a large JSON Lines file: unknown until measured.
- Whether Jev's quality actually degrades in our own briefings at N tokens: unknown, needs the
  stage D test.
- Everything about Minecraft day length, mob timing and night danger for the demo is [U] here.
