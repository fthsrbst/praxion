# General state of the art: keeping a changing world belief fresh

Research note for session 3 ("Perception and world model"; open question: where and how world
memory is stored, and how remembered information ages). 2026-10-27. **Nothing here is a decision.**

Evidence tags:

- **[V]** primary source read in this session; URL or repo path is in "Sources".
- **[S]** secondary source read in this session (wiki, encyclopedia, vendor doc).
- **[U]** my own knowledge, not verified this session. Check it before relying on it.

Where I do not know, I write "unknown" instead of guessing.

## In one page

- The game world is the truth. Memory is a set of **time-stamped beliefs**, not a copy of the world.
- Most world facts (positions, counts, inventories, container contents) need an **exact key plus
  age and source**, not a similarity search.
- Keep an **append-only journal** of observations and project it into a **fact store with two time
  axes**: when the fact was true (or the bracket in which it changed), and when the agent recorded
  it.
- On every read, return one of three things: **fresh**, **stale with its age and a costed way to
  check**, or **unknown**. Never silently return an old value as current.
- Use **landmarks** (named places and containers) and an explicit "unknown" for places. Use an
  occupancy grid only for local navigation.
- Avoid a vector database as the world model, LLM-written facts, one global TTL, full SLAM/POMDP,
  and microservice CQRS.

**How to read it.** Five minutes: Section 0 (the chest), Section 4.2 (the RAG answer), and
Section 5.5 (the same chest after the design). The full catalogue is Section 2; the ranked
recommendation is Section 5; the traps are Section 6.

This file does not repeat the earlier proposal in `docs/research/memory-world-state.md`. It asks
what other fields already do, then ranks the pieces. The earlier conclusion — typed, keyed facts
with freshness metadata, not RAG — is the baseline here too.

The four fields are robotics, symbolic AI, data systems, and networked games. Section 0 fixes one
example. Section 2 is the catalogue. Section 4 answers the RAG question. Section 5 is my ranked
proposal, with the main alternative for each item.

---

## 0. One chest, three different things

At 10:00 the agent sees a chest at `(118, 64, -44)`. It contains 23 iron ingots and 6 bread.
The agent walks away. At 10:20, while the agent is 200 blocks away, the user moves the chest to
`(130, 64, -44)` and takes 10 iron. At 11:00 the agent is asked: "Bring me 23 iron."

There are three different layers here, and most memory bugs come from mixing them:

| Layer | Content | Nature |
|---|---|---|
| World truth | Chest at `130,64,-44`, 13 iron, 6 bread | The game's actual state. The agent cannot read it directly. |
| Agent belief | "At `118,64,-44` there was a chest with 23 iron, seen at 10:00. That block is now air. A chest is visible at `130,64,-44`. Its contents are unknown." | Time-stamped observations plus inferences. Some parts are certain, some are old, one is a guess (is it the same chest?). |
| What the LLM is told | "I last saw 23 iron an hour ago at `118,64,-44`; that chest is gone. I see a chest at `130,64,-44` but have not opened it. Checking costs about 20 seconds." | A compact, honest briefing built from the belief. |

The one-sentence lesson: **memory is not a copy of the world; it is a set of time-stamped beliefs
plus a way to improve them.**

Two words used throughout. A **fact** is one typed record: kind, key, value, the times involved,
and its source. A **belief** is the set of facts the agent currently holds, together with their
uncertainty; a fact is not necessarily true now. **Provenance** is where a fact came from: which
observation, which source, and when.

### 0.1 Why the chest case is hard

Four problems are usually confused. They need different mechanisms:

1. **Change.** The world changed while nobody was looking. The agent saw the "before" and the
   "after", never the change itself. So it cannot know *when* the move happened, only that it
   happened between two observations.
2. **Identity.** A chest at a new position may be the same chest moved, a different chest, or a
   new chest next to the old one. The observation carries a block type and a position, not an ID.
   "Same chest" is an inference, not a fact.
3. **Observation cost.** Blocks can be re-read for free when their chunk is loaded. Container
   contents cannot: the server sends them when a container window opens (`Set Container
   Content`) [S]. Refreshing a chest is an action, not a read.
4. **Age and truth are different.** A mountain's position can be old and still true. A chest's
   contents can be one minute old and already wrong. One global time-to-live (TTL: how long a
   value is trusted) cannot express that.

### 0.2 A detail that decides the whole design

In singleplayer, an unloaded chunk is not simulated: "Unloaded chunks are unprocessed by the game
and do not process any of the game aspects" [S, Minecraft Wiki, Chunk]. So a chest in an unloaded
chunk usually cannot change *by itself* [S, inference from the same quote]. It can still change
if the user visits that chunk, or if a command or another mod edits it.

So "the agent was away" does **not** mean "everything changed". It means "nobody was watching,
and the world may have been simulated elsewhere". For a singleplayer world this is a useful
prior: most unloaded terrain is unchanged. It is not a guarantee, and it must never be presented
as one.

### 0.3 Short answer to the RAG question

**No: semantic search is not the right tool for world facts.** Coordinates, counts, inventories
and container contents are exact keys that change over time. Similarity search has no concept of
"this value is newer" or "this value is false now". Embeddings earn a place for lessons,
episode recall and fuzzy references to places, which is a much smaller job. Section 4 gives the
reasoning and the boundary.

---

## 1. Six questions to compare any technique

Every technique below is scored on the same six questions. They are also a checklist for our own
design later.

| # | Question | Why it matters for the chest |
|---|---|---|
| Q1 | Does it store truth, belief, or text? | A store that only keeps "truth" has to lie when it is old. A belief store can say "probably" and "unknown". |
| Q2 | What time model does it have? | One `updated_at` cannot separate "when it was true" from "when I learned it". The chest moved at 10:20; the agent learned at 11:00. |
| Q3 | How does it detect staleness? | TTL, explicit invalidation, comparison with live state, or nothing. The chest is stale because its chunk unloaded, not because a timer expired. |
| Q4 | Can it say "unknown", distinct from "absent"? | "No chest row" must not become "there is no chest". The agent never entered that cave; that is not evidence of absence. |
| Q5 | What does a refresh cost? | Free read, a look, or a physical action. The chest costs walking plus a right-click. |
| Q6 | What is the failure mode and the operating cost? | Wrong facts, silent drift, storage growth, or architectural complexity. |

---

## 2. The catalogue

Each technique gets the same shape: core idea in two sentences, the chest example, cost, and when
it fits or fails.

### Family A — Robotics: spatial belief with uncertainty

Robots have the same problem: a map built from sensors is partial, noisy and ages. Their answers
are the oldest and best-tested ones.

#### A1. Occupancy grid: one probability per cell, plus "unknown"

**Core idea.** Divide space into cells and store, for each cell, the probability that it is
occupied; sensor readings raise or lower that probability. Cells that were never observed stay
"unknown" instead of defaulting to "empty" [V, Moravec & Elfes 1985; V, Thrun et al., Table 9.1].

**Chest example.** The old chest cell at `118,64,-44` was occupied. At 11:00 the new chunk
snapshot says the block there is air, so the cell flips to "free with high probability". The new
chest cell at `130,64,-44` is occupied. The cave the agent has never entered is unknown, not free.

**Cost.** A Minecraft chunk column is `16 × 16 × 384 = 98,304` cells. At one byte per cell that
is 96 KiB per chunk; a 21×21 load area is about 42 MiB [U, arithmetic from the wiki's chunk
size]. Two bits per cell cuts it to about 24 KiB per chunk. Updates are cheap (one addition in
log-odds form [V, Thrun et al., Table 4.2]); the cost is memory and the work of deciding which
cells a sensor reading actually covers.

**Fits.** Local navigation, "can I walk here", cave exploration, and remembering which regions
have been seen at all. The `-1` convention in ROS 2 is a clean, common way to keep "unknown"
separate from "free" [V, `nav_msgs/OccupancyGrid.msg`].

**Fails.** It is a poor global memory. A 3D grid of the whole world is huge, and a probability
per cell invites fake precision. It also says nothing about identity ("this is the same chest"),
which is the actual question in the example.

#### A2. Landmark map with data association

**Core idea.** Store a short list of named landmarks (places, objects, doors) with an estimated
position, instead of a grid of cells. When a new observation arrives, decide which existing
landmark it belongs to; that decision is called **data association** [V, Thrun et al., §9 for
landmarks; §11–12 for data association].

**Chest example.** The chest at `130,64,-44` is a new observation. Is it the landmark
`base-chest` that moved, or a new landmark `chest-2`? The agent can use evidence: same block
type, same dimension, distance moved, whether the old position is now air, and whether the user
said "I moved the chest". The result is a hypothesis, not a fact.

**Cost.** Tens of bytes per landmark; thousands of landmarks are still small. The hard cost is
the matching logic and the mistakes it makes. A wrong match corrupts a trusted landmark; a missed
match creates duplicates ("my chest" and "chest near the wall" are the same chest).

**Fits.** Named places, bases, portals, farms, villages, and containers the agent cares about.
This is the right shape for "where is my base" and "which chest has the iron".

**Fails.** It does not cover terrain or arbitrary blocks, and it degrades when the world is full
of look-alike objects. Pure nearest-neighbour matching is not enough; the agent needs a rule for
"same type, close, old one gone" and a way to keep both hypotheses when unsure.

#### A3. POMDP belief state: a probability distribution over world states

**Core idea.** A partially observable Markov decision process (POMDP) keeps a **belief state**: a
probability distribution over all possible underlying world states, updated after each action and
observation [S, Wikipedia, POMDP]. The agent chooses actions that maximize expected reward, and
may take an information-gathering action purely to reduce uncertainty [S, same].

**Chest example.** The belief is not "the chest is at X". It is a distribution: 60% the chest
moved to `130,64,-44`, 30% the user took the iron and left the chest, 10% a different chest. The
agent can decide that opening the chest is worth 20 seconds because it changes the distribution
enough to matter.

**Cost.** The state space is the product of every variable; exact solutions are computationally
hard [U; the general result is standard, but I did not read the primary POMDP paper this
session]. Even a small Minecraft room has too many states for a global POMDP.

**Fits.** The *idea* fits: keep uncertainty, and treat looking as an action with a cost. It is
useful for a single decision ("open the chest or not") and for explaining why the agent spends
time verifying.

**Fails.** Solving a POMDP over the whole world is not a game-loop technique. Do not import the
solver; import the discipline of writing down uncertainty and the cost of reducing it.

#### A4. Dead reckoning and relocalization

**Core idea.** Estimate your current position by integrating movement from a known start
("dead reckoning"), and accept that error accumulates; when you see a known landmark, snap back
to it ("relocalization") [S, Wikipedia, Dead reckoning].

**Chest example.** The agent walks toward the remembered chest while predicting "40 blocks to go"
by counting its own movement. If it only counted, small errors would accumulate; when the chunk
loads and it sees the chest, the exact block coordinate corrects the estimate. The stored chest
coordinate is the anchor; the walked distance is not.

**Cost.** Cheap arithmetic per tick; the cost is drift and the need for a recognizable landmark.
Minecraft gives the agent exact coordinates [U], so its own position does not need dead reckoning;
the idea matters for predicting moving entities and for noticing teleports, respawns and
dimension changes.

**Fits.** Predicting where a moving mob or player will be between observations; detecting that a
position jumped.

**Fails.** As a memory model. It says nothing about whether the chest still contains iron. It
also tempts the agent to trust a predicted position over an observed one.

---

### Family B — Symbolic AI: changing beliefs without lying

This family is about the logic of belief change, not about storage. It supplies the rule for what
"update" means when the world itself moves.

#### B1. Belief revision versus belief update

**Core idea.** Belief **revision** handles new information about a world that did not change;
belief **update** handles a world that did change [V, SEP, §7.1]. The standard formal trick is to
attach a time index to each sentence, so the belief set contains pairs like "p at t1" rather than
"p" [V, SEP, §7.1, citing Katsuno & Mendelzon 1992].

**Chest example.** At 10:00 the agent believes "the chest at `118,64,-44` has 23 iron". At 11:00
it sees the chest is gone. This is **update**, not revision. It must not conclude "the chest
never had iron", and it must not conclude "the chest is empty"; it should conclude "at 10:00 it
had 23 iron; between 10:00 and 11:00 it was moved or removed".

The book/magazine example in SEP makes the difference concrete: if you are told a book was put on
a table *after* the first statement, you do not conclude there is no magazine; the new
information is about a later time [V, SEP, §7.1].

**Cost.** No storage cost by itself; it is a rule. Computing a minimal change can be expensive in
full logic [U], but we do not need full AGM: a time-indexed fact row is enough.

**Fits.** This is the theoretical hinge of the whole design. Every world fact must carry the time
it was true, and a new observation must add a new time slice, not overwrite the old one.

**Fails.** AGM revision is the wrong operation for a changing world, and using it produces the
classic bug: "the chest is empty, so it never had iron". Also, full AGM needs a logically closed
belief set; our facts are simple records, so we take the time-index idea and leave the rest.

#### B2. Truth maintenance with justifications

**Core idea.** Every belief carries the reasons that support it (its **justification**); when a
reason is defeated, the belief is retracted and anything that depended on it is recomputed
[U, truth maintenance systems, Doyle 1979 and later work; not read this session].

**Chest example.** The belief "chest at `118,64,-44` has 23 iron" is justified by "I opened it at
10:00". At 11:00 the justification "the block at `118,64,-44` is a chest" is defeated by the chunk
snapshot showing air. The contents belief is retracted, but the historical event "I saw 23 iron at
10:00" remains as a past observation.

**Cost.** One justification record per belief, plus a dependency graph. Retraction can cascade;
in the worst case it is expensive, but for a few thousand facts it is trivial.

**Fits.** Keeping the *why* of a fact, and retracting derived facts when their base disappears.
This is exactly what "provenance" means in `systems-review.md`'s Craftonomous row.

**Fails.** It does not tell you when to re-check; it only tells you what to do when a reason is
already defeated. It also becomes bureaucratic if every fact needs a formal proof tree.

#### B3. Blackboard architecture

**Core idea.** Independent modules read and write one shared data structure, the **blackboard**,
instead of calling each other; the full model adds a control component that chooses which module
acts next [V, Nii 1986, abstract only; the control detail is U, not in the abstract I read]. It is
a coordination pattern, not a database.

**Chest example.** Perception writes "chest seen at `130,64,-44`" to the blackboard. The planner
reads it and writes "goal: open chest". The memory module reads both and records the attempt.
No module calls the others directly.

**Cost.** Cheap in a single process and a single thread; the cost is control policy and
consistency. With several writers, two modules can write conflicting values and nobody owns the
truth.

**Fits.** A small agent with a handful of modules and a human-readable shared state. It is a good
mental model for the "briefing" that the LLM reads.

**Fails.** As an architecture for a long-running, persistent memory: no history, no schema, no
aging. Also, "shared mutable state" is the opposite of an append-only journal. Take the
coordination idea, not the implementation.

#### B4. Behavior-tree memory nodes

**Core idea.** A behavior tree (BT) runs a tree of tasks; a **memory node** remembers which child
succeeded or is running, so the tree can resume instead of re-checking [V, Colledanchise & Ögren
2017, §1.3.2 and §3.6]. The same paper titles a section "Creating Un-Reactive BTs using Memory
Nodes" [V, same, table of contents].

**Chest example.** A "go to chest" branch remembers the target coordinate and keeps walking. If
the chest moved, the branch does not re-read the world; it arrives at the old spot and fails,
unless something else interrupts it.

**Cost.** Very cheap; one small state per node.

**Fits.** Resuming a multi-step task (mine three blocks, then place torches) without restarting.

**Fails.** Memory in the control layer hides staleness from the planner. The paper's own framing
is the warning: memory nodes make a BT less reactive. Use them for task progress, not for world
facts.

---

### Family C — Data systems: history, time and cache freshness

This is the family that has actually built "current view plus full history" at scale.

#### C1. Event sourcing

**Core idea.** Store every state change as an event in an append-only log, and derive the current
state by replaying the log [V, Fowler, Event Sourcing]. Because the log is the source of truth,
you can rebuild state, query the state at a past time, and replay after fixing a bad event
[V, same].

**Chest example.** The log holds: `10:00 container_opened(chest@118,64,-44, {iron:23,bread:6})`,
`10:05 chunk_unloaded(...)`, `11:00 chunk_loaded(...)`, `11:00 block_observed(118,64,-44, air)`,
`11:00 block_observed(130,64,-44, chest)`. Replaying it explains exactly what the agent knew and
when. The current fact store is a projection of these events.

**Cost.** The log grows forever. You need snapshots to avoid replaying from the beginning, a
schema for events, and a deterministic projection [V, Fowler, "Complete Rebuild" and
"Snapshots"]. Writing a full chunk snapshot per visit is large; store it compressed, and store
block changes as deltas.

**Fits.** The audit trail, debugging, "what happened while I was away", and the ability to change
the projection later without losing history. This is the backbone I recommend.

**Fails.** It is not a user-facing query language by itself, and it is easy to overdo: an event
for every block change in every loaded chunk is a firehose. Log *observations* (a chunk snapshot,
a container open, an inventory delta), not every internal tick.

#### C2. CQRS: separate write and read models

**Core idea.** Command Query Responsibility Segregation uses one model to update information and
a different model to read it [V, Fowler, CQRS]. The read model can be shaped for queries and
rebuilt from events.

**Chest example.** The write side appends observations. The read side is a small "current belief"
table plus a spatial index, optimized for "what is near me" and "where is the iron". They can be
updated independently.

**Cost.** Fowler's own warning: "for most systems CQRS adds risky complexity" [V, same]. Two
models mean two schemas, synchronization, and a window where the read side is behind.

**Fits.** Only if the read and write shapes genuinely diverge and the system is large. For a
single-process agent with one SQLite file, the separation is a *logical* one: journal table and
projection tables in the same database.

**Fails.** As a microservice pattern with an event bus. That is architecture for a company, not
for a Minecraft agent. Do not import the operational cost.

#### C3. Bitemporal tables

**Core idea.** Keep two independent time axes: **valid time** (when a fact was true in the
world) and **transaction time** (when it was stored in the database) [V, Snodgrass, Temporal
Databases, §1 and §2]. A bitemporal table has four timestamps: start and end on each axis
[V, same].

**Chest example.** The old fact has valid time starting at 10:00 and ending at the earliest known
moment it was gone, 11:00; its transaction time runs from 10:00 (recorded) to 11:00 (superseded).
A query "what did I believe at 10:30?" returns the old chest. A query "what was true at 10:30?"
returns **unknown**: the move happened somewhere between 10:00 and 11:00, and the agent never saw
the change. If the user says "I moved it at 10:20", valid time can be narrowed to 10:20, but that
is testimony, not observation.

**Cost.** Four timestamps and two indexes; roughly double the bookkeeping. Queries become more
complex, and every write must decide both intervals.

**Fits.** Exactly our problem: the agent often learns about a change long after it happened.
It also gives a clean answer to "what did I know before the restart?".

**Fails.** Full bitemporal modeling is overkill for a first version. A reduced form —
`observed_at`, `superseded_at`, `superseded_by` — captures most of the value. The full model
matters when the user asks historical questions ("was the chest there yesterday?").

#### C4. HTTP cache freshness: TTL, age, validation, invalidation

**Core idea.** A cached response has a **freshness lifetime**; while it is fresh it can be reused
without asking the origin, and after that it is **stale** and should be validated before reuse
[V, RFC 9111, §4.2]. The cache tracks **age**, invalidates stored responses when an unsafe request
changes the resource, and supports extensions such as `stale-while-revalidate` (return stale now,
refresh in the background) and `stale-if-error` (keep serving stale when the origin fails)
[V, RFC 9111, §4.2.4 and §4.4; V, RFC 5861].

**Chest example.** The chest fact is fresh for as long as the chunk stays loaded and nobody else
touches it. When the chunk unloads, the fact becomes stale. The resolver can either return it
with an age and a warning, or schedule a revalidation (walk over and open it). While the agent is
walking, `stale-while-revalidate` is the right behavior: keep planning with "23 iron, unconfirmed"
and correct when the chest opens.

**Cost.** One policy table keyed by fact kind, plus an age calculation. Almost no infrastructure.

**Fits.** The read rule. It gives precise words for what we need: fresh, stale, age, validate,
invalidate. The per-kind lifetime is the important part.

**Fails.** HTTP assumes the origin can always answer a validation request. A chest cannot be
validated without a physical action, and a chunk cannot be validated without loading it. So the
"validation" step must be allowed to fail, and the stale value must remain usable with a label.
Also, one global TTL is exactly the mistake the RFC's per-response lifetime avoids.

#### C5. Materialized views and spatial indexes

**Core idea.** A materialized view stores the result of a query physically, so reads are fast, and
it must be refreshed when the base data changes [V, PostgreSQL docs, Materialized Views]. A
spatial index such as SQLite's R*Tree stores bounding rectangles and answers "what is inside this
rectangle" quickly [V, SQLite R*Tree].

**Chest example.** The "current belief" table is a materialized view over the journal: one row
per live fact. The R*Tree index answers "which known containers are within 64 blocks of
`(120,64,-40)`?" without scanning every fact.

**Cost.** Refresh policy (on write, on read, or periodically) and index maintenance. The read
model can be briefly behind the journal; that is acceptable if the briefing says so.

**Fits.** The projection layer and all spatial queries. It is the cheapest way to make "nearest
known iron" fast.

**Fails.** A materialized view is not a truth source; it is a cache. If it is refreshed
incorrectly, it silently lies. Keep the journal as the source and make the projection
rebuildable.

---

### Family D — Networked games: authority, interest and reconciliation

Multiplayer games already run a shared, changing world over a network with clients that see only
part of it. The details below are general practice; the Valve wiki was blocked, so most of this is
tagged [U].

#### D1. Server authority, client prediction, reconciliation

**Core idea.** The server owns the true state; clients predict locally for responsiveness and
correct themselves when the server's authoritative update arrives [U, general game networking;
the prediction idea is the same as dead reckoning, S, Wikipedia]. The client never treats its
prediction as truth.

**Chest example.** The agent right-clicks the chest and the client predicts "window opened". The
server then sends `Set Container Content` with the real slots. If the prediction was wrong, the
server's packet wins. The fact is written only after the authoritative packet arrives [S,
Minecraft protocol, `Set Container Content`].

**Cost.** Prediction and reconciliation code, plus a way to detect divergence. In our case the
game client already does this [U]; the agent should not duplicate it. Read the authoritative
packets.

**Fits.** The mental model for the whole system: the game server is the authority; the agent's
memory is a client-side belief. It also justifies "never write a fact from a prediction".

**Fails.** As a reason to build our own netcode. We are a read-only proxy plus OS input; the game
already reconciles.

#### D2. Interest management (subscriptions)

**Core idea.** A server sends a client only the part of the world it is interested in, usually
based on distance, and stops sending when the client leaves [U, general practice; the exact
mechanism is engine-specific]. In Minecraft terms, chunks are sent when they enter view distance
and unloaded chunks are not processed [S, Minecraft Wiki, Chunk; S, protocol].

**Chest example.** When the agent walks away and the chest's chunk unloads, the agent
"unsubscribes" from that chunk. The chest fact becomes stale, not false. When the chunk loads
again, a full chunk snapshot is the new baseline, and block updates are deltas from it.

**Cost.** A subscription table keyed by chunk, and a rule for what happens on unsubscribe. Cheap.

**Fits.** Exactly how to decide which facts are "live" and which are "remembered". It also gives
a natural trigger for invalidation: chunk unload.

**Fails.** Interest management is about *delivery*, not *truth*. A chunk can be loaded (so the
client receives it) while the player cannot see through a wall. Do not confuse "subscribed" with
"seen"; that is a fair-play decision (D4).

#### D3. Snapshot plus delta, with periodic baseline resync

**Core idea.** Send a full snapshot occasionally and small deltas in between; if a delta is lost or
a client drifts, the next snapshot repairs it [U, general practice]. This is the network version
of "full rebuild plus incremental updates" [V, Fowler, Event Sourcing, "Complete Rebuild"].

**Chest example.** On chunk load, the agent gets the full chunk (baseline). A block update packet
says "the block at `118,64,-44` is now air" (delta). If a packet is missed or a decode bug
occurs, the next full chunk load corrects the local copy.

**Cost.** Baseline snapshots are large; deltas are small. You need a rule for when to force a
baseline (chunk load, decode error, version change).

**Fits.** Keeping live block state correct without a per-block event log. It is also the answer
to silent drift: when in doubt, re-read the chunk.

**Fails.** A delta alone is not trustworthy forever. Without a baseline, one lost packet leaves a
permanent wrong belief. Always keep a way to resync.

#### D4. Per-player visibility (fog of war)

**Core idea.** Some games have the server filter state per player, so a client cannot read through
walls; others send nearby data and only hide it on the client, which leaks [U, general practice;
the exact filter is game-specific]. This is the network analogue of a fair-play perception policy.

**Chest example.** The agent is in a sealed room. The chest is one wall away, in a loaded chunk.
The client may receive the chunk data, but a player could not see the chest. Does the agent's
memory get to know it? That is a policy question, not a technical one, and it changes what
"observed" means.

**Cost.** A visibility test per observation (a raycast or a line-of-sight check), plus a rule for
entities and block entities. It is not free, and it interacts with every write.

**Fits.** If we want the agent to know only what a player could know, this is the honest way to
enforce it.

**Fails.** If we accept a chunk-level policy ("the agent knows everything in a loaded chunk"), the
agent effectively has X-ray vision over time. That may be acceptable for a singleplayer assistant,
but it must be a stated decision, not an accident.

---

## 3. Side-by-side

Short verdicts are for Praxion, not general use.

| # | Technique | Uncertainty? | Time model | Staleness / repair | Main cost | Verdict |
|---|---|---|---|---|---|---|
| A1 | Occupancy grid | Yes, per cell | Last update | Sensor update; unknown cells | 24–96 KiB per chunk column | Partly: local navigation only |
| A2 | Landmark map | Yes, per landmark | Last seen | Re-observe; data association | Matching logic and mistakes | Yes: core for places/containers |
| A3 | POMDP belief | Yes, full distribution | Belief update | Information actions | State explosion | No: take the idea, not the solver |
| A4 | Dead reckoning | Position error | Integrated | Relocalize at landmarks | Drift | Partly: movement only |
| B1 | Revision vs update | Yes, time-indexed | Valid time per sentence | New time slice | None (rule) | Yes: the core rule |
| B2 | Truth maintenance | Yes, justifications | Event order | Retract defeated beliefs | Dependency graph | Partly: provenance |
| B3 | Blackboard | No | None | None | Control and consistency | Partly: mental model |
| B4 | BT memory nodes | No | Task state | None (hidden) | Cheap | Caution: hides staleness |
| C1 | Event sourcing | Historical | Event order | Replay / rebuild | Log growth, snapshots | Yes: the journal |
| C2 | CQRS | No | Two models | Projection refresh | Operational complexity | No: logical split only |
| C3 | Bitemporal tables | Yes, two axes | Valid + transaction | Supersession | Double bookkeeping | Yes: reduced form first |
| C4 | HTTP cache rules | Yes, stale label | Age + lifetime | Validate / invalidate | Policy table | Yes: the read rule |
| C5 | Materialized view + R*Tree | No | Refresh time | Refresh policy | Index maintenance | Yes: projection + index |
| D1 | Server authority | Yes, prediction | Server tick | Reconciliation | None new | Yes: worldview |
| D2 | Interest management | No | Subscription | Chunk unload | Subscription table | Yes: live vs remembered |
| D3 | Snapshot + delta | Drift detection | Baseline + deltas | Full resync | Snapshot size | Yes: block state |
| D4 | Visibility filter | Yes, policy | Observation time | Re-look | Raycast per write | Decision needed: fair-play |

---

## 4. Is semantic search / RAG suitable?

### 4.1 The words, once

- An **embedding** is a list of numbers that represents the meaning of a piece of text (or an
  image). Texts with similar meaning get similar numbers.
- A **vector database** stores embeddings and answers "which entries are nearest to this one".
- **RAG** (retrieval-augmented generation) means: search a store for relevant text, then put that
  text into the LLM's prompt.

All three are tools for *finding relevant text*. They are not tools for *representing a world*.

### 4.2 The answer

**For world facts: no.** Use typed, keyed records with time and provenance.

Four concrete reasons, using the chest:

1. **Exact key versus similarity.** "What is in the chest at `118,64,-44`?" is a key lookup. An
   embedding search returns the *most similar* chest description, which may be a different chest
   in a different base. Similarity is not identity.
2. **No supersession.** The old fact ("23 iron at `118`") and the new fact ("13 iron at `130`")
   are nearly identical in meaning. A vector search happily returns both, and the LLM has no
   reliable way to know which one is current. A keyed store can mark one as `superseded_by` the
   other; an embedding index has no such concept.
3. **Numbers and coordinates are weak signals.** `23` and `13` differ by one digit, and embeddings
   tend to place them close; `(118,64,-44)` and `(130,64,-44)` are almost the same vector
   [U, general property of embeddings]. Embeddings are designed to ignore surface details, which
   is exactly what we need to preserve.
4. **Negation and absence.** "There is no chest here" and "there is a chest here" are close in
   embedding space. A similarity search cannot reliably distinguish a fact from its negation.

There is also a cost reason: every write would need an embedding call (or a local model), and
every read a nearest-neighbour search, to answer questions that a primary-key lookup answers
exactly and in microseconds.

### 4.3 Where embeddings do earn their place

Three narrow jobs, all text-shaped:

1. **Lessons and episodes.** "What did I learn about creepers in caves?" The entries are
   paragraphs; meaning-based retrieval is appropriate. Even here, filter first by kind, area and
   tags, then rank by BM25 plus embedding similarity plus recency (hybrid retrieval), and keep
   the lesson's metadata.
2. **Fuzzy references from the user.** "Go back to that cave with the village." The user's words
   are a description, not a key. Embed the place descriptions, retrieve candidates, then confirm
   with coordinates or by asking.
3. **Static game knowledge.** Wiki pages, crafting notes, patch notes. This is classic RAG:
   document-shaped and mostly unchanging.

### 4.4 The rule of thumb

> If the answer must be exact and can change, it is a **lookup**.
> If the question is "which text is relevant", it is **retrieval**.

A useful corollary: the vector index should store *pointers with metadata*, never the only copy
of a mutable fact. The fact store remains authoritative; the index helps you find the paragraph
that explains it.

---

## 5. Ranked proposal (for discussion, not a decision)

I rank by "what must exist before the rest can work, and how much of the user's problem it
solves". Each item includes the main alternative, because the alternative is a real choice the
user may prefer.

| Rank | Piece | Job | If we did not build it |
|---|---|---|---|
| 1 | Observation journal + bitemporal fact store | History, replay, honest time brackets | A mutable table plus a text log |
| 2 | Read-through resolver + per-kind policy | fresh / stale / unknown at read time | A background refresher that guesses what to check |
| 3 | Spatial belief layer | Places, identity, "have I been here" | Plain coordinates plus an R*Tree |

### 5.1 First: an observation journal plus a bitemporal fact store

**What it is.** One append-only table of observations (chunk snapshot, container open, inventory
delta, block change, chat, death, skill result). Each observation belongs to an **epoch**, a
labelled run of the agent (one process start or world join), so observations from different runs
can be ordered. One fact table is projected from it, where each row has `valid_from`, `valid_to`,
`recorded_at`, `superseded_at`, `source`, `kind`, `key`, `value`. The current-belief view is "rows
with no `valid_to`", but each row is still labelled with when it was recorded and how fresh it is.

**Why first.** It is the foundation the other two sit on. It answers "what happened while I was
away", survives restarts, makes debugging possible, and lets us change the projection later
without losing history [V, Fowler]. The bitemporal split, read through the update rule, is what
prevents the classic lie: "the chest is empty, so it never had iron" [V, SEP, §7.1;
V, Snodgrass].

**What it buys for the chest.** The move is bracketed, not invented: "at `118` from 10:00; gone
by 11:00". The agent can say "sometime in that hour", which is the truth.

**Main alternative.** A single mutable `facts` table with `updated_at` plus a plain text log.
Simpler to build and query; loses temporal queries, replay, and the ability to explain a past
briefing. If the user wants the smallest possible v1, this is the honest fallback.

**Cost and failure mode.** Write amplification and log growth. The fix is to journal
*observations*, not every tick: one compressed chunk snapshot per visit, container snapshots on
open, inventory deltas, block updates as deltas. Add snapshots so replay does not start at zero.
The dangerous failure is treating the projection as the source of truth; it must always be
rebuildable.

### 5.2 Second: a read-through resolver with a per-kind freshness policy

**What it is.** A single function that turns a question into one of three answers:

- `fresh(value)` — observed now, inside the perception policy.
- `stale(value, age, source, verify_action)` — a remembered value plus the concrete action that
  would refresh it ("open the chest at `130,64,-44`, about 20 s").
- `unknown(reason)` — never observed, outside the policy, or from before a known gap.

The freshness policy is a small table keyed by fact kind, not one global TTL. **Invalidation**
means marking a stored value as no longer trustworthy; invalidation hooks mark facts stale when
their chunk unloads, a block changes, the agent dies, the user hands control back, or the
dimension changes.

**Why second.** It is the piece the user actually feels: the difference between "the chest has 23
iron" (a lie) and "I last saw 23 iron an hour ago; there is a chest over there; checking takes 20
seconds" (useful). The HTTP cache vocabulary is a small, standard model for this [V, RFC 9111;
V, RFC 5861].

**What it buys for the chest.** It refuses to answer "23 iron" as current. It returns the old
value with its age, the new block observation as fresh, and a costed action for the contents.

**The honest answer to "refresh instantly".** A block in the current chunk is instant: the data is
already in the client's memory. A block across the room costs a look. Container contents cost an
action. The resolver must say which case it is, instead of promising one speed for all.

**Main alternative.** A background refresher that periodically re-reads everything and keeps a
warm cache. It sounds simpler, but it spends actions the planner did not choose, it cannot refresh
container contents without opening them, and it can violate fair-play by looking at things the
player would not see. Push invalidation plus pull-on-demand is cheaper and more honest.

**Cost and failure mode.** The policy table and the refresh actions. The main failure is
"stale forever": if nothing ever schedules the `verify_action`, the chest stays unconfirmed. The
resolver must hand the action to the planner with a cost, and the planner must be allowed to
choose it. The second failure is pretending a refresh is free when it is an action.

### 5.3 Third: a spatial belief layer (landmarks plus a coarse occupancy map)

**What it is.** Two structures: (a) a landmark table for named places and containers, with
position, type, last seen, confidence and aliases; (b) a coarse per-chunk occupancy/seen map for
local navigation and "have I been here". The landmark table is the important one; the occupancy
map is a helper.

**Why third.** It answers the questions the LLM asks most often: "where is my base", "which chest
is closest", "is this the cave I explored". Robotics has the right primitives: landmarks,
data association, and unknown space [V, Moravec & Elfes; V, Thrun et al.]. It also gives the
resolver a spatial index for cheap queries [V, SQLite R*Tree].

**What it buys for the chest.** It stores `base-chest` as a landmark. When a chest appears at
`130,64,-44`, the agent can form the hypothesis "same chest, moved" and keep the old name as an
alias. It can also say "I have not seen this chunk before", which is different from "empty".

**Main alternative.** Plain coordinates in the fact table plus an R*Tree, with no landmark
identity and no occupancy grid. Much simpler, and probably enough for v1. The cost is duplicate
landmarks and weaker answers to "is this the same place".

**Cost and failure mode.** Data association mistakes and map drift. A permanent ore map is also
an X-ray advantage over time; whether that is allowed is a fair-play decision for the user, not a
technical default. Do not build full SLAM: a voxel world with exact coordinates does not need it
[U].

### 5.4 How they fit together

```
 game server (authority)
        │  packets: chunk data, block updates, container contents, inventory
        ▼
 read-only proxy ──► perception policy ──► OBSERVATION JOURNAL (append-only, epochs)
                                                │ deterministic projection
                    ┌───────────────────────────┼──────────────────────────┐
                    ▼                           ▼                          ▼
              LIVE STATE (RAM)            FACT STORE                 TEXT MEMORY
              "now" only                  valid + recorded time      lessons, notes
                    │                     + provenance               (embeddings here)
                    │                           │
                    └──────────────► RESOLVER ◄─┘
                                      │  fresh | stale(value, age, verify) | unknown
                                      ▼
                                   BRIEFING ──► Jev (typed) / LLM (prose)
```

Read path: question → typed key → resolver → live refresh if free → otherwise stale with age and a
verify action → compact briefing.

Write path: observation → journal → projection. Chunk load/unload is subscribe/unsubscribe. A
block update is a delta. A container open is an explicit refresh, not passive perception.

### 5.5 The chest, end to end

| Time | Event | Belief after the event |
|---|---|---|
| 10:00 | Container opened at `118,64,-44` | Chest `118` has 23 iron, 6 bread. Fresh, source `container_open`. |
| 10:05 | Chunk unloads | Same value, now `stale` (not watched). Age starts. |
| 10:20 | User moves the chest; agent sees nothing | No event. The belief is unchanged and increasingly old. |
| 11:00 | Chunk loads; full snapshot | Block at `118` is air → old fact `valid_to` bracketed to (10:00, 11:00). Chest at `130` → new landmark observation. Contents `unknown`. |
| 11:00 | Question: "bring 23 iron" | Resolver: old value stale and gone; new chest fresh as a block, contents unknown; verify action "open chest at `130`, ~20 s". Briefing says exactly that. |

The important line is the 11:00 briefing. It contains no invented number, no claim that the chest
is empty, and one concrete way to improve the answer.

**The smallest useful slice.** If I had to build one vertical slice before the full design: the
journal, a `container` fact table, the resolver, and this chest scenario as an automated test. It
exercises the four hard parts (history, bracket, refresh action, unknown) with one container and
no pathfinding.

**How to test it.** The earlier file's scripted-world test still applies: move a chest, break a
block, restart the process, let the user play alone, then ask questions with known answers. Add
one metric the earlier plan lacks: **refresh cost** — how many actions and seconds the agent
spends verifying each answer. A memory that is always correct because it checks everything is too
expensive to use.

---

## 6. Ideas to avoid

Each of these is attractive for a good reason; each fails for a specific one.

1. **A vector database as the world model.** Similarity cannot express "newer", "superseded" or
   "exact". Keep vectors for text, not for facts.
2. **Letting the LLM write facts.** An LLM may propose a lesson or a summary; it must not write
   "the chest has 13 iron". Facts come from observations. Otherwise the memory inherits
   hallucination and you cannot trace it.
3. **Full SLAM or a global POMDP.** The world already gives exact coordinates, and the state space
   is too large. Take the ideas (uncertainty, landmarks, information actions), not the solvers.
4. **Microservice CQRS and an event bus.** Fowler's own warning is that CQRS adds risky complexity
   for most systems [V]. In one process, use a journal table and projection tables in one SQLite
   file.
5. **Graphiti or Mem0 as the memory layer.** They target conversation memory and run an LLM per
   ingest [U; from the earlier review, not re-read here]. Our observations are already structured;
   the LLM step is cost without benefit. Take the "invalidate, never delete" idea, not the
   product.
6. **CRDTs for a single-writer world.** Conflict-free replicated data types solve concurrent
   writes from many replicas. We have one writer and one game server [U, design assumption]; they
   add complexity for a conflict that does not exist.
7. **One global TTL or one decay constant.** A mountain, a chest and a mob age at completely
   different rates. A single number either expires mountains or trusts mobs for hours.
8. **A journal row for every block change.** It is a firehose. Journal chunk snapshots and
   container/inventory events; treat block updates as deltas that update the live state and
   invalidate facts.
9. **Uncalibrated confidence numbers.** "0.87" looks scientific and is usually made up. Until
   there is a measured error rate, use labels (`fresh`, `stale`, `unknown`) and provenance.
10. **The closed-world assumption.** "No row" does not mean "no chest". It means "never observed".
    This single mistake causes agents to walk to a place and declare a chest vanished.
11. **Treating every loaded chunk as seen.** A loaded chunk includes blocks behind walls. If the
    agent may use them, it has X-ray vision. Decide this explicitly (chunk policy or visibility
    filter), do not let it happen by accident.
12. **Promising instant refresh for everything.** Some facts are free to refresh (a block in the
    current chunk), some need a look, and some need an action (open the chest). Say which is
    which; do not hide an action inside a "read".

---

## 7. What I could not verify

Honest gaps, so nobody mistakes them for findings:

- **POMDP primary text.** The URL I had was the 1996 reinforcement-learning survey, not the 1998
  POMDP paper. The POMDP claims are tagged [S] (Wikipedia) or [U], not [V]. Unknown: the exact
  complexity statement in the original paper.
- **F.E.A.R. GOAP article** (`gamedeveloper.com`): blocked. Any GOAP claim would be [U], so I left
  GOAP out rather than pad the file.
- **Unreal Engine Blackboard docs**: blocked (JavaScript-only page). The blackboard description
  relies on Nii's abstract [V, abstract only] and general knowledge.
- **Valve networking wiki**: blocked by an anti-bot page. The networked-game section (D1–D4) is
  general practice [U], except where a wiki or RFC is cited.
- **Entity resolution literature** (VLDB PDF): blocked. The landmark-identity discussion is
  therefore [U]; it is the right shape, but the formal methods are not surveyed here.
- **Nii 1986 full paper**: only the abstract was readable. The blackboard description is
  correspondingly shallow.
- **Web search**: the search tool failed in this session. No claim here depends on a search
  snippet.
- **Occupancy-grid parameters** (cell size, prior, sensor model) were not tested on Minecraft
  data. The storage arithmetic is mine [U].

---

## 8. Open questions for the user (session 3)

These are decisions, not research gaps. My recommendation is in brackets; the user decides.

1. **Fair-play policy for memory.** May the agent remember everything in a loaded chunk, or only
   what a player could see? [Recommendation: start with the chunk policy for terrain, but require
   line-of-sight for containers and entities, and state the difference in the briefing.] The
   alternative is a full visibility filter, which costs a raycast per observation.
2. **Permanent map or decaying map.** May the agent keep a permanent ore/chest map across
   sessions? [Recommendation: no permanent ore map; demote unreobserved resource facts after a
   few game days, and keep named places.] The alternative is a full map, which is stronger but
   less fair-play.
3. **Verify actions in the plan.** May the agent detour to open a chest or look around, and what
   is the time budget? [Recommendation: yes, with a cost attached and a per-task budget.] The
   alternative is "never detour", which leaves container facts stale forever.
4. **Epoch boundaries.** When does a new epoch start: process start, world join, death, or user
   hand-over? [Recommendation: process start and world join; death and hand-over mark a gap
   inside the epoch.] The alternative is one epoch per session, which loses the "user played alone
   for an hour" case.
5. **Cross-world memory.** Do lessons and place knowledge carry across worlds? [Recommendation:
   lessons yes, world facts no.] The alternative is per-world everything, which is simpler but
   forgets useful lessons.
6. **Confidence format.** Are labels (`fresh`, `stale`, `unknown`) enough, or does the user want
   numeric probabilities? [Recommendation: labels first; add numbers only after calibration.]
   The alternative is numbers now, which look precise and are not.

---

## 9. Glossary

| Term | Meaning in one line |
|---|---|
| Belief | What the agent thinks is true, with uncertainty and time attached. |
| Fact | A typed record: kind, key, value, time, source. Not necessarily true now. |
| Valid time | When a fact was true in the world. |
| Transaction time | When the agent recorded the fact. |
| Bitemporal | Keeping both valid time and transaction time. |
| Epoch | A labelled run of the agent, used to order observations across restarts. |
| Freshness | Whether a fact may be used as current, given its kind and events. |
| TTL | Time-to-live; a fixed age after which a value is no longer trusted. |
| Invalidation | Marking a fact stale because an event makes it doubtful. |
| Push invalidation | The system tells memory that something changed, instead of memory polling. |
| Read-through | On a read, try to refresh the value before returning it. |
| Data association | Deciding which observation belongs to which known landmark. |
| Occupancy grid | A map of cells, each with a probability of being occupied, plus unknown. |
| POMDP | A decision process where the agent keeps a probability distribution over hidden states. |
| SLAM | Simultaneous localization and mapping; building a map while tracking position. |
| Event sourcing | Storing every change as an event and deriving state by replay. |
| CQRS | Separate models for writes and reads. |
| Materialized view | A stored query result that must be refreshed. |
| Interest management | Sending a client only the part of the world it is near. |
| Dead reckoning | Estimating position by accumulating movement, with growing error. |
| Embedding | A vector of numbers representing meaning. |
| RAG | Retrieve text, then put it in the LLM prompt. |
| Closed-world assumption | Treating "not known" as "false"; dangerous here. |

---

## Sources

Read in this session unless marked otherwise.

**Primary**

- Moravec, H. and Elfes, A. (1985), *High Resolution Maps from Wide Angle Sonar*.
  https://www.ri.cmu.edu/pub_files/pub4/moravec_hans_1985_1/moravec_hans_1985_1.pdf [V, abstract,
  introduction and method]
- Thrun, S., Burgard, W. and Fox, D., *Probabilistic Robotics*. https://docs.ufpr.br/~danielsantos/ProbabilisticRobotics.pdf
  [V, §4.2 binary Bayes filter in log-odds, Table 4.2; §9 occupancy grid, Table 9.1; §11–12
  data association]
- ROS 2, `nav_msgs/OccupancyGrid.msg`.
  https://raw.githubusercontent.com/ros2/common_interfaces/rolling/nav_msgs/msg/OccupancyGrid.msg
  [V, `0` free, `1` occupied, `-1` unknown]
- Nav2, `nav2_costmap_2d` README. https://raw.githubusercontent.com/ros-navigation/navigation2/main/nav2_costmap_2d/README.md
  [V, layered costmap]
- Fowler, M., *Event Sourcing*. https://martinfowler.com/eaaDev/EventSourcing.html [V, complete
  rebuild, temporal query, event replay, snapshots]
- Fowler, M., *CQRS*. https://martinfowler.com/bliki/CQRS.html [V, "for most systems CQRS adds
  risky complexity"]
- RFC 9111, *HTTP Caching*. https://www.rfc-editor.org/rfc/rfc9111.txt [V, freshness lifetime,
  age, stale, validation, invalidation]
- RFC 5861, *HTTP Cache-Control Extensions for Stale Content*.
  https://www.rfc-editor.org/rfc/rfc5861.txt [V, `stale-while-revalidate`, `stale-if-error`]
- Snodgrass, R. T., *Temporal Databases* (book draft).
  https://www2.cs.arizona.edu/~rts/tdbbook.pdf [V, valid time vs transaction time, four
  timestamps, "when scheduled vs when recorded"]
- Stanford Encyclopedia of Philosophy, *Logic of Belief Revision*.
  https://plato.stanford.edu/entries/logic-belief-revision/ [V, §7.1 revision vs update,
  time-indexed sentences, Katsuno & Mendelzon 1992]
- Nii, H. P. (1986), *Blackboard Systems*, AI Magazine. Abstract read via
  https://ojs.aaai.org/aimagazine/index.php/aimagazine/article/view/537 [V, abstract only]
- Colledanchise, M. and Ögren, P. (2017), *Behavior Trees in Robotics and AI*.
  https://arxiv.org/pdf/1709.00084 [V, §1.3.2 control-flow nodes with memory; §3.6 "Creating
  Un-Reactive BTs using Memory Nodes"]
- SQLite, *The SQLite R*Tree Module*. https://sqlite.org/rtree.html [V, rectangle index,
  overlap queries]
- PostgreSQL, *Materialized Views*.
  https://www.postgresql.org/docs/current/rules-materializedviews.html [V, stored query result,
  refresh]

**Secondary**

- Minecraft Wiki, *Chunk*. https://minecraft.wiki/w/Chunk [S, unloaded chunks are unprocessed;
  load levels; simulation distance]
- Minecraft Wiki, *Java Edition protocol/Packets*.
  https://minecraft.wiki/w/Java_Edition_protocol/Packets [S, `Set Container Content` on window
  initialisation; `Block Update` within render distance]
- Wikipedia, *Dead reckoning*. https://en.wikipedia.org/wiki/Dead_reckoning [S, accumulated
  error, position fix]
- Wikipedia, *Partially observable Markov decision process*.
  https://en.wikipedia.org/wiki/Partially_observable_Markov_decision_process [S, belief state as
  a distribution, information-gathering actions]

**Tried and blocked**

- `gamedeveloper.com` F.E.A.R. GOAP article.
- Epic Games, Unreal Engine Blackboard documentation (JavaScript-only page).
- Valve Developer Community, networking wiki (anti-bot page).
- VLDB entity-resolution PDF.
- AAAI Nii 1986 full PDF (abstract page only).
- Web search tool (cancelled).

**Not read**

- Kaelbling, Littman and Cassandra (1998), *Planning and Acting in Partially Observable Stochastic
  Domains*. The URL I had was a different paper (the 1996 RL survey), so nothing here is tagged
  [V] for it.
- Doyle (1979) and the truth-maintenance literature; B2 is [U].
- Full SLAM literature; A4 uses only the dead-reckoning idea.
