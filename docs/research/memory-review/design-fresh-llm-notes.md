# Memory as a notebook: a small set of files that the agent and the user both edit

Research date: 2026-10-27. Phase 0, session-3 input. **Nothing here is a decision.**

Web access: available and used. Sources are tagged and listed at the end.
Two gaps: the Minecraft Wiki returned HTTP 403 for every page I tried, so all Minecraft
mechanics below are marked `[U]` (my own knowledge, unverified this session). I also did not
read `docs/research/memory-world-state.md` until my own design was finished; the comparison is
in section 18.

**Notation**

- `[V]` — I read this in a primary source this session. The file path or URL is given.
- `[S]` — secondary source (blog, docs page that is not the upstream spec).
- `[U]` — my own knowledge, not verified this session.
- No tag — a design proposal or my reasoning. Proposals are not facts; you decide.

---

## 0. Answer in one page

1. **Memory is not a database of the world. It is the part of the world that observation cannot
   give you right now.** Position, health, inventory and the blocks in loaded chunks are already
   known exactly through the proxy `[V docs/decisions.md]`. Writing them into files would be a
   second copy of the truth that can only ever be more wrong. Memory exists for what is *not*
   currently observable: a chest in a chunk that is not loaded, a lesson from three days ago, a
   promise the user made.
2. **Two speeds, not one.** The world changes 20 times per second; a human notebook changes a few
   times per hour. Keep the fast part in memory as code (the live world model) and write to disk
   only what a careful player would write down.
3. **No semantic search for the world facts. Exact keys and filters.** A chest has an id and a
   position. Similarity search returns "the chest that looks relevant", not "the chest that is
   there now". For a corpus of a few hundred entries, a name lookup plus a short index is both
   cheaper and correct.
4. **A code gate between notes and prompts.** The agent may write claims. Code checks each claim
   against live observation where it can, and marks it `fresh`, `stale 3d` or `unchecked` where it
   cannot. The prompt never carries an unmarked claim.
5. **Embeddings: not now.** Add them when the *notes* (journal, lessons) pass a few hundred
   entries, or when the agent must answer "where did I once see…". That is a different corpus from
   the facts, and it is the only corpus where similarity search earns its keep.

The rest of this document explains each of these and shows the files.

---

## 1. Words first

Short definitions, because everything else depends on them.

- **Observation** — a fact read from the network stream: a block at a position, the contents of an
  open container, the player's inventory, a position, an entity. Exact, but only for what the client
  currently receives `[V docs/decisions.md]`.
- **Live world model** — the code structure that holds recent observations in RAM. It is
  authoritative while the agent plays and is thrown away on exit (or rebuilt from a chunk cache).
  Not memory. Not files.
- **Fact** — one line in a file describing something in the world: `chest-2 is at 131,68,-64`.
- **Claim** — a statement written by the agent or the user that code has not checked yet.
- **Provenance** — where a fact came from and how good that source is. Craftonomous tags every
  fact `proprioception`, `sight`, `hearing`, `memory`, `inference`, `testimony` or `privileged`
  `[V docs/research/systems-review.md §3.9]`. That idea is the backbone of this design.
- **Staleness** — how long ago a fact was last confirmed. Not "old", but "unconfirmed since".
- **Context pack** — the exact text block handed to an LLM for one decision. Built by code, never
  by the LLM itself.
- **Notebook** — the set of files in section 4.
- **Bi-temporal** — keeping two times per fact: when it was true in the world, and when the system
  learned or retired it. Zep tracks four timestamps per fact for exactly this reason
  `[V https://arxiv.org/html/2501.13956v1]`.

---

## 2. The core idea

A person playing Minecraft keeps a mental notebook. It contains things like:

- "my base is at 128, 68, -64, next to the big oak"
- "the iron is in the shaft under that oak, around y=30"
- "sand falls; dig from the side"
- "the user hates when I break the floor"

It does **not** contain "I am holding 14 cobblestone". A person looks at their hand.

That split is the whole design. The notebook holds what you cannot see. The live model holds what
you can. Any memory system that stores the current inventory in a file is doing work that the game
already does, better.

This also answers the "changes in real time" requirement honestly: a file on disk can never be
real-time. What can be real-time is the *live model*. The notebook is the slower layer, and the
code that builds prompts is what makes the slow layer *safe* to use — by stamping every entry with
its age and by re-checking it the moment the world makes checking possible.

Three consequences:

1. **Facts are cheap to re-check, expensive to trust.** Every design choice below follows from that.
2. **The number of facts must stay small.** A hundred facts can be read in full by a model. Ten
   thousand cannot, and then you are back to retrieval, which is the thing we are trying to avoid.
3. **The agent's own notes are not automatically true.** An LLM that writes "the iron is in chest-2"
   may be wrong, or may be repeating something it read in its own earlier note. Notes need evidence.

---

## 3. Three layers, two speeds

```
  speed            layer                     owner     lifetime        example
  ---------------------------------------------------------------------------------------
  20 / s     A. live world model (RAM)       code      session         block at (96,31,-139)
  ~1 / min   B. notebook (files on disk)     mixed     the save        chest-2 contents, day 14
  forever    C. history (append-only)        code      the save        "chest-2 was at 131,... until d14"
```

- **A** is not this document's subject, but it is the source of every fact. It must be able to
  answer "is this position loaded right now?" and "what block is at this position?".
- **B** is the notebook. Human readable. Both the agent and the user edit it.
- **C** is the trail. Never edited, only appended. It is what lets us rebuild B and explain a
  wrong belief.

```
   proxy stream ──► live world model (A) ───┐
                        │                   │ validate
                        ▼                   ▼
                  fact writer ────────► notebook (B)  ◄── user edits
                        │                   │
                        ▼                   ▼
                    history (C)      context builder ──► LLM
```

The arrow that matters is the one into the LLM. **The LLM never reads the notebook directly.**
It reads a context pack that code assembled, with every entry stamped. That is the "check claims
against live observation before they reach a prompt" requirement, and it is one function, not a
policy.

---

## 4. File layout

One folder per save, because coordinates mean nothing across worlds. One shared folder for things
that are true in every world.

```
praxion/memory/
  shared/
    rules.md                  [user]   promises and limits that apply everywhere
    lessons.md                [agent]  lessons promoted out of a single world
  worlds/
    survival-01/              (folder name = Minecraft save folder name)
      world.md                [code]   identity, day, last played, active goals
      places.md               [code]   named places, coordinates, last confirmed
      containers.md           [code]   what is in which chest, and when it was opened
      lessons.md              [agent]  cause → effect rules learned in this world
      base.md                 [agent]  what we are building at home, and the plan
      open-questions.md       [agent]  things to ask the user, or to test in game
      notes/
        home-base.md          [agent]  optional prose about one place
      journal/
        2026-10-26.md         [agent]  daily log, plus one code-owned digest block
      history.md              [code]   retired facts, append-only
      facts.log               [code]   every observation the writer kept, JSON lines
  packs/
    2026-10-27T06-41-bring-iron.md     [code] last 50 compiled packs, for debugging
```

Deliberate choices:

- **Markdown, not SQLite.** The user must be able to read and edit it. Markdown is also the format
  the agent is best at writing. Main alternative: a SQLite fact store with generated markdown
  views — better queries and atomic writes, worse for "the user opens it in an editor and fixes a
  wrong coordinate". Revisit at the §10.3 ladder.
- **No index file at the start.** A few hundred lines are cheap to parse every time. An index is a
  second copy of the truth that goes out of sync. Add one only when parsing is measurably slow.
- **`packs/` exists.** The user asked to understand the system. The cheapest way to understand it
  is to open the exact text the model saw, three minutes ago, for a decision that went wrong. This
  is also the debugging story for session 14.

---

## 5. Example file contents

Illustrative, not from a real run.

### `world.md` [code]

```markdown
<!-- owner: code · generated from facts.log + live state · do not hand-edit -->
# survival-01

mode        survival, normal, no cheats
day         14 · 06:41 · morning · overworld
last played 2026-10-26 22:10 · 41 h total
spawn       118, 70, -70
position    128, 68, -64  (home-base, standing)
goals       [user] bring iron  ·  [self] finish the wheat farm (day 2 of 3)
```

### `places.md` [code]

Short fields only, so a table is safe.

```markdown
<!-- owner: code · do not hand-edit · last confirmed column is updated by the fact writer -->
# Places

| id           | name         | dim | x    | y  | z    | first seen  | last confirmed |
|--------------|--------------|-----|------|----|------|-------------|----------------|
| home-base    | home base    | ow  | 128  | 68 | -64  | d1 06:12    | d14 06:41      |
| mine-shaft-1 | iron shaft   | ow  | 96   | 32 | -140 | d9 14:02    | d11 18:20      |
| village-1    | plains vill. | ow  | -412 | 71 | 233  | d11 09:44   | d11 10:05      |
| portal-1     | nether portal| ow  | 140  | 70 | -58  | d12 20:30   | d12 20:31      |
| nether-base  | n. base      | ne  | 17   | 65 | -7   | d12 20:40   | d12 21:02      |
```

**Rule: a place gets a name only when it earns one.** A resource worth returning to, a landmark on
a route, or a base. The trajectory of every block walked is a separate, code-owned map (a Baritone
style chunk cache `[V docs/research/systems-review.md §3.8]`), not the notebook. Naming every cave
is how notebooks rot.

Why a table here and not in `containers.md`: every field is short, so alignment survives. A table
with one long field becomes unreadable and the parser breaks on any added column.

### `containers.md` [code]

```markdown
<!-- owner: code · one line per container · full contents only from an open window -->

chest-1 · home-base · pos 130,68,-64 · ow · opened d14 06:41 · block present yes (6 s ago)
  items: 5 iron_ingot · 32 cobblestone · 1 bucket · 12 torch
  hash: 9f2c1a7e

chest-2 · home-base · pos 131,68,-64 · ow · opened d12 21:10 · block present yes (6 s ago)
  items: UNKNOWN — never re-opened since d12 21:10
  hash: 4b81d0c3

barrel-1 · mine-shaft-1 · pos 97,31,-139 · ow · opened d11 18:20 · block present unknown
  items: 12 raw_iron · 3 torch
```

Note the line "block present yes / unknown". A container's *position* can be confirmed from a block
read when the chunk is loaded. Its *contents* cannot: Minecraft only sends a window's contents to
the client that opened it `[U]`, which is why mineflayer has a `syncWindow` no-op item click to force
the full window state back `[V docs/research/systems-review.md §3.1]`. So "what is in the chest" is
never checkable from a distance. That is a fact about the game, not about our design, and it shapes
the plans (see scenario A).

### `lessons.md` [agent]

```markdown
<!-- owner: agent · prose is yours · evidence ids are checked by code -->
# Lessons — survival-01

## Mining
- Iron ore needs a stone pickaxe or better. evidence: ev-0042 `[ok]`
- At mine-shaft-1 the sand at y=44 collapses when you dig under it. Dig from the side.
  evidence: ev-0917 `[ok]`
- Digging straight down below y=0 near x=90 hits lava. evidence: ev-0921 `[ok]`

## Home
- The furnace runs out of coal; coal sits in chest-1 but the user takes it.
  evidence: ev-1030 `[ok]`

## Unproven
- Villagers may trade iron for emeralds. evidence: none `[unverified]`
```

The `[ok]` / `[unverified]` marks are **written by code**, not by the agent. The agent writes an
evidence id; code checks that the id exists in `facts.log`. If it does not, the line is rendered
`[unverified]` in every prompt. This is ACE's lesson in a simpler form: contexts rot when they are
rewritten from memory, so updates must be incremental and traceable
`[V https://arxiv.org/abs/2510.04618]`.

### `base.md` [agent]

```markdown
<!-- owner: agent -->
# Home base — 128, 68, -64

Standing: 9x9 oak floor, 2 furnaces, crafting table, 2 chests, 1 bed, wheat farm 7x7 (unfinished).

Plan
1. finish the wheat farm (needs 14 more water-adjacent dirt) — do before the next night
2. move storage into a proper room; the user moved a chest once already (see journal d14)
3. a door on the north side; mobs walk in at night

Open: where to put the second bed if the user wants to sleep here.
```

### `rules.md` [user]

```markdown
<!-- owner: user · the agent reads this and never rewrites it -->
# Rules

- Do not break the oak floor in the base.  area: home-base
- Do not go below y=-20 without a water bucket.  always
- Items in chest-2 are mine; ask before taking.  container: chest-2
- Tell me in chat when you take damage or lose a tool.  always
```

Rules with `area:` / `container:` are parsed into deterministic guards. Free text is pasted into
the prompt and nothing more. This follows the session-1 direction: the user writes rules with an
LLM if they want, and code enforces them deterministically `[V docs/decisions.md]`.

### `journal/2026-10-26.md` [agent] + one code block

```markdown
<!-- owner: agent -->
# Day 14 — 2026-10-26

06:05 woke, ate bread, checked chest-1: 5 iron ingots still there.
06:20 went to mine-shaft-1. Sand collapsed at y=44, lost 3 hearts, came back.
06:40 user said "bring me iron". Told them I have 5 and asked how many.
07:10 user took 3 from chest-2 — I did not know it had any. Lesson: ask before promising.

<!-- code:begin digest · owner: code · regenerated, never edited by the agent -->
events: sleep 05:58 · 41 blocks placed, 12 broken (home-base only) · 1 fall 3 dmg
items:  −2 bread · +14 cobblestone · +2 raw_iron
pauses: 09:05–10:10 user control (65 min, unobserved)
<!-- code:end -->
```

### `history.md` [code]

```markdown
<!-- owner: code · append-only · facts are never deleted, only retired -->
d14 06:44  chest-2  pos 131,68,-64 → invalid: block is air (observed d14 06:44) → see chest-3
d14 06:45  chest-3  created, pos 134,68,-62, supersedes chest-2, items differ by −3 iron_ingot +64 dirt
d15 07:02  death-drop @ 88,-58,-147 → invalid: 5 min despawn window passed (see ev-1188)
```

Never deleting is a design choice, not sentimentality. It is the only way to answer "why did it
think that?" and the only way to fix a notebook that has drifted. Zep invalidates graph edges by
setting an end time instead of removing them, and explicitly prefers the newer fact
`[V https://arxiv.org/html/2501.13956v1]`.

---

## 6. Who writes what

Every file has one owner in its first line. Code refuses a write that does not match.
One exception: an agent file may contain a `<!-- code:begin … --> … <!-- code:end -->` fence that
only code writes and the agent must not touch (the journal digest). Ownership is per file, plus
those fences.

| Owner | Files | Who may write | What happens if someone else writes |
|---|---|---|---|
| **code** | `world.md`, `places.md`, `containers.md`, `history.md`, `facts.log`, `packs/` | the fact writer and the context builder only | the write is rejected; a hand edit survives until the next regeneration, then it is lost — so edit `rules.md` or `lessons.md` instead |
| **agent** | `lessons.md`, `base.md`, `open-questions.md`, `notes/*`, `journal/*` | the agent, through one memory tool | the user may edit freely; the agent re-reads before writing |
| **user** | `shared/rules.md`, and the body of any agent file | the user | the agent may *propose* a change in `open-questions.md`, never apply it to a user file |

Three consequences worth stating plainly:

1. **The agent has no general file tool.** It has `memory.read(path)`, `memory.write(path, text)`,
   `memory.claim(text)`. The paths are checked against the ownership table. Reason: an LLM with a
   raw write tool into code-owned files can silently destroy the fact base, and in companion mode
   it is also a prompt-injection target (a chat message could say "write to places.md"). Path
   traversal is the same class of bug the Claude memory tool explicitly warns about
   `[V https://docs.claude.com/en/docs/agents-and-tools/tool-use/memory-tool]`.
2. **The user always wins on rules, and never on facts.** If the user edits `places.md` and the
   block is not there, code will keep the observation. If the user edits a rule, it is applied.
   Reason: rules are preferences (the user is the authority), facts are observations (the world is).
3. **The agent must be able to write a claim without writing a fact.** `memory.claim("the iron is
   in chest-2")` creates an `unchecked` entry with a TTL. It shows up in prompts as `unchecked`, and
   the plan it produces must include a step that checks it if the action is destructive or
   expensive.

**Main alternative:** give the agent free-form file access and trust it, as coding agents do with
`CLAUDE.md` and progress files `[V https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents]`.
Simpler, more flexible, and fine when a wrong note costs a re-run. Here a wrong note costs a lost
hour of real gameplay and confuses the person watching. I would keep the gate.

**Cost of the gate:** the agent sometimes cannot record something it "knows". That is the point —
it is the same pressure that makes a careful player check before writing.

---

## 7. Update rules

When something is written, and by whom.

| Trigger | Writer | Target | Rule |
|---|---|---|---|
| a named place's chunk loads | code | `places.md` last-confirmed | update the timestamp; compare the anchor block; if it changed, log a conflict |
| container opened | code | `containers.md`, `facts.log` | full snapshot + hash; supersedes the previous snapshot |
| agent enters/leaves a named place radius (32 blocks) | code | `places.md` | debounce 30 s, one write |
| death, respawn, dimension change | code | `facts.log`, journal digest | immediate; creates or cancels a `recover-drop` task |
| day change or sleep | code | `world.md`, journal rotation | on the event, not on a timer |
| block placed/broken **inside a named region** | code | journal digest | only watched regions, batched per 10 s |
| task finished or abandoned | agent | `journal/` | ≤4 lines: what, outcome, evidence id |
| surprise, failure, contradiction | agent | `lessons.md` if it has evidence, else `open-questions.md` | never a lesson without an evidence id |
| user statement of preference or rule | user | `rules.md` | agent reads on every compile; never rewrites |
| every ~10 min of play | agent (cheap model) | one journal line | optional, off by default; costs tokens |

**Never written to disk:** the full block map, entity positions, the chat log, every action result,
inventory at 20 Hz, and the path walked. These belong to the live model or the chunk cache. Writing
them would turn the notebook into telemetry: huge, unreadable, and always slightly behind.

**Write on meaning, not on schedule.** A timestamped dump every minute produces a file nobody reads.
The triggers above are all "something a person would mention tomorrow".

---

## 8. Staleness: showing it, and refreshing it

### 8.1 The four marks

Every fact reaching a prompt carries exactly one mark. Four words, no percentages.

| Mark | Meaning | Comes from |
|---|---|---|
| `fresh` | confirmed by code within the current session, and the thing was observable | observation |
| `stale Nd` | confirmed by code, but N days (or N hours) ago, and not observable right now | stored observation |
| `unchecked` | written by the agent or the user, never confirmed by code | claim |
| `disputed` | two sources disagree; the observed value is shown, the other is named | conflict |

Do not show a numeric confidence. LLMs are bad at turning 0.73 into behaviour, and so are people.
Show the reason instead: `stale 3d (last opened d11 18:20)`. That is actionable: it says "go and open it".

### 8.2 Why the mark, not the age, decides behaviour

The rule the prompt states, in one line:

> `fresh` may be used. `stale` may be used for planning and must be checked before any destructive
> or expensive action. `unchecked` is a hypothesis, never a premise. `disputed` means stop and look.

"Expensive" is a policy, not a guess: crafting something irreversible, travelling more than ~100
blocks `[U: this distance is arbitrary and needs a measurement]`, giving items to the user, or
breaking a block in a protected area.

### 8.3 Refresh-on-touch

The core function. Roughly 40 lines of code; this is the whole "stale to current instantly" feature.

```python
def resolve(fact):
    if fact.kind == "block" and live.is_loaded(fact.pos):
        now = live.block_at(fact.pos)
        if now != fact.value:
            supersede(fact, now, reason="block_changed")   # append to history, update the file
            return observed(now)

    if fact.kind == "container":
        if live.window_open_is(fact.id) or live.opened_within(fact.id, seconds=60):
            return observed(live.contents(fact.id))         # trustworthy right now
        return stored(fact, mark="unchecked" if fact.never_confirmed else "stale")

    if fact.kind == "inventory":
        return observed(live.inventory())                   # never stored at all

    # not loaded, not openable: keep the value, mark the age
    return stored(fact, mark="stale", since=fact.confirmed_at, distance=live.distance_to(fact.pos))
```

Two honest limits, stated here so nobody is surprised later:

1. **You cannot refresh what you cannot observe.** A chest three kilometres away stays `stale` until
   someone walks there. "Instantly current" means "instantly current for everything observable now".
   Anything else is a lie the system would tell itself.
2. **Loading a chunk is not the same as seeing it.** The chunk gives blocks. It does not give
   contents of containers, and it does not give what happened while nobody was there.

### 8.4 What makes a fact stale without anyone touching it

- a pause window (the user played, the agent did not) — everything inside the area the user was in
- a death (inventory gone; drop exists for a few minutes `[U: 5 min in vanilla]`)
- a dimension change (the other dimension keeps running in singleplayer `[U]`)
- time passing for anything that ticks on its own: crops, furnaces, mob spawns, fire, item despawn

The last one is the awkward one. A furnace lit at 06:00 has finished by 06:10, and the notebook
should not pretend otherwise. Proposal: any task that involves a ticking block writes an
*expectation* (`furnace at home: 8 iron smelting, ready ~d14 06:08`) instead of a state. Expectations
are checked on the next visit, and a missed expectation is a small lesson, not a bug.

---

## 9. Conflicts

### 9.1 Trust order

```
observation (code, now)  >  stored observation (code, then)  >  user statement  >  agent claim
```

User statement beats agent claim because the user has eyes and intent. Observation beats user
statement because the user can be wrong about a coordinate, or be describing what used to be there.

### 9.2 The five cases

| Case | Resolution | What the prompt shows |
|---|---|---|
| code observed, then code observed something else | newer wins; older goes to `history.md` with `until` | the new value, `fresh` |
| code observed X, notebook says Y | code wins; the claim becomes `disputed` and is kept | `X (observed 4 s ago) · notebook said Y (d12) — notebook entry retired` |
| two agent claims | newer wins; older marked superseded, both kept | the newer one, `unchecked` |
| user says X, notebook says Y | user wins for rules and intentions; observation wins for the physical world | both, with who said what |
| nobody can check (not loaded, not openable) | nothing is resolved; both are kept | `stale`, plus the alternative as `also claimed: …` |

### 9.3 Showing the loser is not politeness

Keeping the losing claim in the prompt has a concrete effect: the model stops repeating the mistake,
and the user watching the screen can see *why* it believed something. It also gives us a measurable
number — the contradiction rate per session — which is the main health metric of this whole design
(§10.2).

### 9.4 Worked example

Day 12, `containers.md`: `chest-2 · 131,68,-64 · opened d12 21:10 · 3 iron_ingot, 9 dirt`.
Day 14, the agent walks home, the chunk loads, code reads the block: **air**.

1. `supersede(chest-2, value=air, reason=block_changed)` → history line, `containers.md` row flips to
   `block present no (d14 06:44)`.
2. The fact writer scans a 16-block cube for chest-like blocks: finds one at `134,68,-62`.
3. It does **not** merge them. A new id `chest-3` is created with `likely_same_as: chest-2` and the
   old row keeps a pointer.
4. The pack says: `chest-2 was at 131,68,-64; that block is now air (4 s ago). A chest-like block is
   at 134,68,-62. Contents of both: unknown until opened.`
5. The agent opens it. Code compares item multisets: `−3 iron_ingot, +64 dirt`. The link becomes
   `supersedes: chest-2`, with the difference recorded.
6. Lesson candidate, written only when the user confirms or the pattern repeats: "the user
   reorganises storage; if a container is missing, scan 16 blocks before asking".

Step 3 is the part people skip. Automatic merging is tempting and produces the worst bug in a memory
system: two different things silently become one thing.

---

## 10. How big it can grow before it hurts

### 10.1 Rough sizes (my estimates, `[U]`)

| File | Typical | Tokens if pasted whole | Grows by |
|---|---|---|---|
| `world.md` | 8 lines | ~120 | never |
| `places.md` | 30 rows | ~900 | ~30 rows per 40 h of play |
| `containers.md` | 25 containers | ~800 | ~10 per 40 h |
| `lessons.md` | 60 lessons | ~1,600 | ~5 per session |
| `base.md` | 25 lines | ~350 | rewritten, not appended |
| `journal/` (tail 2 days) | 2 files | ~400 | 1 per day |
| `rules.md` | 6 lines | ~120 | rarely |
| live self block | — | ~180 | — |

A routine task pack is ~1,100 tokens. A planning pack with everything is ~4,500. Both fit any
modern window; Claude models ship 200K or 1M `[V https://platform.claude.com/docs/en/build-with-claude/context-windows]`.

### 10.2 Why "it fits" is the wrong test

Three separate ceilings, and the token count is the least important:

1. **Attention.** Accuracy drops when relevant information sits in the middle of a long input, even
   for models built for long context `[V https://arxiv.org/abs/2307.03172]`. A 4,500-token pack is
   fine today. A 20,000-token pack is not fine, even though it "fits".
2. **Contradiction density.** Twenty entries are usually consistent. Two hundred drift. Every
   contradiction costs a wrong action and a confused user. This is the real limit.
3. **Cost per hour.** Illustrative: 1,100 input tokens at an assumed $3 per million input tokens
   `[U: price assumption, not verified]` is ~$0.003 per call. Two calls a minute for an hour is
   ~$0.40/h of pure context. This is why the pack is budgeted, not why it is small. Provider prompt
   caching makes a stable prefix much cheaper `[U]`, which is a reason to keep the pack order fixed.

### 10.3 The ladder: at each size, do this

| When | Do | Because |
|---|---|---|
| always | budget the pack: 1.2K routine, 6K planning, hard cap 8K | attention and cost |
| > 8 places | show only places matching the task plus the 2 nearest; put the rest behind an index line | relevance, not size |
| > 30 places | one-line index (`home-base 128,68,-64 · iron · base`) + full rows on demand | reading 30 rows to use 2 |
| > 60 lessons | tag each with 2–5 keywords; select 8 by keyword + all "pinned" (≤10) | lessons are mostly irrelevant per task |
| > 7 journal days | keep 2 hot, digest the rest into `journal/2026-10.md` | the log is for the human |
| > 25 containers | only containers in the current region, plus anything the task names | same as places |
| > 40 h of play | first review of the whole notebook by a person; delete places that were never revisited | drift |
| > 500 facts | the ladder has failed; go to section 11 | retrieval is now the problem |

### 10.4 The measurement that decides when to move

Do not guess. Once the agent plays for an hour without crashing, run this weekly:

- 20 questions with known answers ("where is chest-2?", "how many iron ingots did we have at
  d14 06:41?", "what was the lesson about sand?"). Feed the pack, ask, count correct answers.
- Vary only pack size: 1.2K, 3K, 6K, 12K, 24K. Plot accuracy against size.
- The size where accuracy drops by more than ~10% is the budget for that model. Re-measure when the
  model changes.

This is also the honest answer to "how big can it be before it hurts": *unknown until measured*, and
measurable in an afternoon.

---

## 11. Embeddings: if and when

Short answer: **not in the first version, and probably never for facts.**

Why not now:

1. **The corpus is tiny.** A few hundred short entries. Exact lookup returns everything relevant in
   microseconds. Voyager needed embeddings for thousands of skill descriptions
   `[V https://arxiv.org/html/2305.16291v2]`; we have 30 places.
2. **Similarity is not freshness.** Two chests are semantically almost identical and physically
   different. The failure mode of vector search here is returning the *other* chest with high
   confidence. That is worse than returning nothing.
3. **Embeddings add a second copy of the truth.** The vector must be recomputed when a line changes.
   Now the index can drift from the file, and you need a re-index job, a model id per vector, and a
   migration path — for a corpus that fits in one screen.
4. **Local Qwen3-Embedding is free and small** `[V docs/research/embeddings.md]`, so this is not
   about money. It is about one more moving part in a system that must be explainable.

The cheaper substitute, and my recommendation: **an index written by the agent.** Once a week (or
every 10 journal entries) the agent writes `journal/index.md`: five lines per month naming the
themes and the day files. Then the context builder greps by keyword first, and only if that fails,
reads the index. That is semantic search performed slowly, by hand, with an audit trail.

Add embeddings when **all** of these are true:

- the *notes* corpus (journal + lessons) exceeds ~300 entries;
- the keyword+index path misses queries the user actually asks ("where did I see that big cave?");
- you have run the section 10.4 measurement and the loss is from selection, not from size.

If added, then:

| Corpus | Embed? | Retrieve by |
|---|---|---|
| facts (places, containers) | never | exact id/name/position, distance |
| lessons | later, maybe | keyword tags first, embeddings second |
| journal | yes, when big | recency for the tail, embeddings for older |
| game knowledge (wiki, recipes) | yes | this is the classic RAG use `[V docs/research/embeddings.md]` |
| skills | separate subsystem (session 11) | name + reliability, embeddings last `[V docs/research/systems-review.md §4]` |

Zep's and Mem0's results support graph/structured memory over flat vector RAG for conversational
state `[V https://arxiv.org/abs/2501.13956]` `[V https://arxiv.org/abs/2504.19413]`, though both are
self-reported on their own benchmarks. Treat them as direction, not proof.

---

## 12. Is RAG suitable? A direct answer

RAG (retrieval-augmented generation) means: keep a corpus, fetch the few chunks that look relevant
to the query, put them in the prompt, generate. It was designed for static text
`[U: Lewis et al. 2020 is the usual citation; I did not read it this session]`.

It is a good fit when all of these hold: the corpus is large, it changes slowly, each chunk is
self-contained, and "relevant" is the hard part.

Our world facts fail the second and third conditions. They change constantly, and a chunk's meaning
depends on when it was true. A retrieved chunk saying "chest-2 has 5 iron" is not *relevant* or
*irrelevant*; it is *expired*. Similarity has no word for expired.

Where RAG is the right tool, and where it is not:

| Data | Changes | Size | Store | Retrieve by | RAG? |
|---|---|---|---|---|---|
| position, health, inventory | 20/s | tiny | live model | always, whole | no |
| loaded blocks, entities | 20/s | huge | live model / chunk cache | spatial query | no |
| named places | hours | ~30 | `places.md` | exact name, distance | no |
| containers | hours to days | ~25 | `containers.md` | place, item, id | no |
| lessons | days | ~60 | `lessons.md` | tags, task match | no (yet) |
| journal | daily | grows | `journal/` | last 2 + keyword | later |
| recipes, wiki, mob data | static | large | data reports / later a corpus | exact item id, then semantic | **yes** |
| skills | weekly | ~50 | code repo | name, reliability | separate session |
| the user's past instructions | rare | tiny | `rules.md` | always, whole | no |

So: **structured lookup for state, RAG for knowledge.** Two corpora, two mechanisms, never one
mechanism pretending to do both. Mixing them is what produces the classic bug where the agent
confidently fetches a fact that was true on Tuesday.

---

## 13. Building the prompt: the context pack

This is the only path from files to an LLM. One function, deterministic, logged.

```python
def build_pack(question, budget=1200):
    ents  = parse_entities(question)          # "iron" → item:iron_ingot, block:iron_ore

    pack  = []
    pack += live_self()                        # position, dim, health, food, hotbar, inventory
    pack += task_state()                       # current task, last 3 outcomes, time left
    pack += rules_all()                        # user rules, always, whole
    pack += places_for(ents, nearest=2, limit=4)
    pack += containers_in(those_places)        # resolved via resolve() → marked
    pack += lessons_for(ents, pinned=True, limit=8)
    pack += journal_tail(days=2)
    pack += unknowns(ents)                     # explicit "we do not know this"
    return trim(pack, budget)
```

Properties that matter:

- **Deterministic.** Same world + same question → byte-identical, apart from timestamps. That makes
  caching possible and makes a bad decision reproducible.
- **Budgeted.** `trim` drops lowest-priority sections, never individual lines mid-sentence, and
  records what it dropped in the pack header. A silently truncated pack is a debugging nightmare.
- **Logged.** Every pack is written to `packs/`, with the fact ids it used. When the agent does
  something stupid, the question "what did it know?" has an exact answer.
- **Honest about absence.** `unknowns` is not decoration. A model that is told "no iron ore in
  loaded chunks" plans differently from one that simply has no iron facts.

---

## 14. A full example pack: "the user said: bring me iron"

Compiled at day 14, 06:41, from five files plus the live model. No LLM call was used to build it.
Numbers are illustrative.

```text
=== MEMORY PACK · survival-01 · d14 06:41 · budget 1200 · sections dropped: none ===

WHO / WHERE
You are Praxion. Overworld, day 14, 06:41. You are at home-base (128, 68, -64), standing.
Health 20/20, food 17/20. Nothing hostile within 16 blocks.

CARRYING  (live, exact)
hotbar: 1 stone_pickaxe (worn) · 2 bread x5 · 3 torch x12 · 4 iron_sword
pack: 14 cobblestone · 6 oak_log · 1 bucket (empty) · 2 raw_iron · 3 stick
no iron_ingot

TASK
user, 2 min ago, in chat: "bring me iron"
state: not started. no iron ingots carried.

RULES  (user)
- Do not break the oak floor in the base. (area: home-base)
- Do not go below y=-20 without a water bucket.
- Items in chest-2 are mine; ask before taking.
- Tell me in chat when you take damage or lose a tool.

PLACES  (4 shown of 5; nether-base hidden: other dimension, not relevant)
- home-base   128,68,-64   ow   fresh 12 s    bed, 2 furnaces, crafting table, 2 chests
- mine-shaft-1 96,32,-140  ow   stale 3 d     entrance under the big oak; iron seen y=28–34
- village-1  -412,71,233   ow   stale 3 d     plains village, untraded
- portal-1   140,70,-58    ow   stale 2 d     nether portal

CONTAINERS  (2 in range)
- chest-1  home-base 130,68,-64  fresh 12 s   block present: yes
  5 iron_ingot · 32 cobblestone · 1 bucket · 12 torch
- chest-2  home-base 131,68,-64  contents UNCHECKED (last opened d12 21:10)
  block present: yes (12 s ago). The user said it is theirs.

LESSONS  (6 of 41, matched on "iron")
- Iron ore needs a stone pickaxe or better.  [ok]
- At mine-shaft-1 the sand at y=44 collapses. Dig from the side.  [ok]
- Digging straight down below y=0 near x=90 hits lava.  [ok]
- The furnace runs out of coal; coal sits in chest-1.  [ok]
- Smelting 1 raw_iron needs 1 coal and ~10 s.  [ok]
- Villagers may trade iron for emeralds.  [unverified]

RECENT
d14 06:05 woke, ate bread, checked chest-1: 5 iron ingots still there.
d14 06:20 mine-shaft-1 attempt abandoned: sand fall, −3 hearts.

UNKNOWN
- how many ingots the user wants
- whether chest-2 contains iron (contents unknown since d12; cannot be read from a distance)
- no iron ore in currently loaded chunks; nearest known ore is 3 days old

=== end pack · 1,090 tokens · 6 ms · facts used: 23 · claims shown as unchecked: 1 ===
```

What a correct next step looks like: check the task is "hand over iron". chest-1 is fresh and holds
5 ingots. The cheapest correct action is to walk 4 blocks, take 5 ingots, and hand them over — and
to ask how many, since "bring me iron" has no number.

The trap this pack is built to prevent: an LLM that reads `chest-2 … UNCHECKED` and answers "I have
8 ingots, 3 of them are in chest-2" — inventing contents. The mark makes that invention visible; the
rule in section 8.2 forbids using `unchecked` as a premise.

Note what is **not** in the pack: 35 lessons about farming and combat, the nether base, the village
trade table, the full 5-place list, and every block in the base. Selection is by task entity, not by
similarity. That is the practical difference between this and RAG.

---

## 15. Three walk-throughs

### A. A chest moved while the agent was away

Day 12, 21:10. Last time anyone opened `chest-2` at `131,68,-64`: 3 iron, 9 dirt, hash `4b81`. The
agent stops. The user plays for a while and reorganises the base: the chest moves to `134,68,-62`,
and the user takes the 3 iron.

Day 14, 06:41. The agent starts again, far from home, at `mine-shaft-1`.

1. **Before going home**, the pack shows `chest-2 · stale, last opened d12 21:10 · block present:
   unknown (chunk not loaded)`. Nothing can be refreshed; the agent is 200 blocks away. This is
   correct and the pack says so.
2. The task "store 14 cobblestone" plans a trip home. Walking in, the chunk loads.
3. `resolve()` reads the block at `131,68,-64`: **air**. Conflict. Code calls
   `supersede(chest-2, air, reason=block_changed)`, appends to `history.md`, and flips the row.
4. The fact writer scans a 16-block cube: one chest-like block at `134,68,-62`. Creates `chest-3`
   with `likely_same_as: chest-2` and *no merge*.
5. The pack now says: `chest-2 was at 131,68,-64; block is now air (4 s ago). A chest is at
   134,68,-62. Contents unknown until opened.`
6. The agent walks 3 blocks, opens it. Code snapshots: `9 dirt, 1 bucket`. Diff against the old
   snapshot: `−3 iron_ingot, +1 bucket`. The link becomes `supersedes: chest-2`, difference recorded.
7. Nothing is deleted. `history.md` has both lines. If the user later asks "where did my iron go?",
   the answer is in the file, with a time.

**Where this fails.** If the agent never goes home, it never learns. There is no background refresh;
the notebook is not a monitoring system. That is a deliberate boundary: it keeps the agent honest
about what it does not know, and it avoids inventing a sense it does not have.

### B. Death recovery

Day 15, 07:02. The agent is at `88,-58,-147`, digging in a shaft, and falls into lava.

1. **The live model, not the notebook, notices.** Health reaches 0. Code writes a `death` event with
   the exact position, the dimension, the inventory at death, and the tick. The agent itself does not
   report this and is not asked to.
2. A `recover-drop` task is created with a **hard deadline**: vanilla items despawn after about
   5 minutes `[U: 6,000 ticks; must be verified in game before relying on it]`. That is a fact the
   game owns, not a fact the notebook can know.
3. The pack for the very next decision reads:

```text
YOU DIED 40 s ago at 88,-58,-147 (overworld, y=-58, lava).
Respawned at 118,70,-70. Distance 214 blocks.
Your items are at the death point until about d15 07:07.
Route: no known path faster than ~4 min from here; mine-shaft-1 is a dead end.
The diamond pickaxe was in the pack at death.
```

4. The model must now choose something unpleasant and honest: run and probably lose everything, or
   accept the loss. A memory system earns its keep by making that decision *informed*, not by making
   it pleasant.
5. After the deadline, code marks `recover-drop` failed and **retires the belief** "items are at
   `88,-58,-147`" with the reason `deadline_passed`. It stays in `history.md`.
6. Only now does a lesson get written, and only with the death event id as evidence:
   `- Do not dig straight down below y=-58 near x=88; lava directly under. Carry a water bucket.
   evidence: ev-1188 [ok]`
7. The inventory in every later pack is live, so the lost pickaxe cannot leak into planning. The
   only place it can leak is the *prose*: `base.md` may still say "diamond pickaxe in chest-1". Code
   cannot check prose. Section 16 is about exactly this.

**Where this fails.** If the agent dies and the user keeps playing for an hour, the drop is gone and
the notebook knows it only as "unobserved window". The lesson is still valid; the recovery attempt
would have been pointless. Nothing in the design can fix that — the world does not keep receipts.

### C. Hand-over after an hour alone

09:00, the user hands over. 09:05, they touch the mouse and take control back. 10:10, they hand over
again. During that hour the agent observed nothing at all.

1. **On pause**, code writes a checkpoint: what the task was, the plan, the expected end state, the
   position. Cheap and immediate. The live model keeps running only as far as the proxy still gives
   data — and it does not, because the agent is no longer driving `[U: the proxy still sees the
   stream; whether it is *allowed* to keep observing is a policy question, not a technical one]`.
2. **On resume**, `world.md` and the journal get a `pause` row: `09:05–10:10, 65 min, unobserved`.
   This single row is the most important thing the notebook records, because it explains every
   difference that follows.
3. **Wake-up pass**, in this order, before any LLM call:
   - read `rules.md`, `world.md`, the last journal entry, the open task (files only);
   - take a live snapshot: position, dimension, health, inventory, loaded chunks;
   - diff the inventory against the checkpoint: `−5 iron_ingot, +1 diamond_pickaxe, +64 dirt`;
   - write the diff into the journal's code block (not prose);
   - mark every `unchecked` fact inside the area the user was in as `unverified after pause`;
   - re-check every container that is loaded *and* was part of the current task — block presence
     only, since contents need opening.
4. The pack becomes one small block, ~250 tokens:

```text
WHILE YOU WERE AWAY (paused 09:05–10:10, 65 min, you observed nothing)
you were: hauling cobblestone to home-base, step 2 of 3
now: at home-base (126, 69, -63), health 20/20
changed: −5 iron_ingot · +1 diamond_pickaxe · +64 dirt (dirt now in your pack)
unclear: chest-2 contents (you last saw them d12); the base layout may have changed
plan check: step 2 still valid? the chest you were walking to is 4 blocks away.
```

5. **One** decision call: continue, replan, or ask. For a bounded question like this, Jev at 130–300 ms
   `[V docs/research/README.md]` could answer; the big model is only needed if the plan broke.
6. Then one short line of chat to the user: "Back. You took 5 iron and left a diamond pickaxe; I'll
   finish the cobblestone run." That line is the product. It is what makes an hour of absence feel
   continuous.

**Where this fails.** If the user also edited the notebook by hand, code must re-read and reconcile.
If the pause was three days, anything that ticks on its own (crops, furnaces, mobs, fire) has moved
on, and the notebook can only say "unobserved for 3 days". A plan that assumes a farm is still
harvestable after three days is a plan that will be wrong.

---

## 16. Weaknesses, honestly

1. **Prose cannot be checked.** Code can verify `chest-2 is at 131,68,-64`. It cannot verify "the
   base is cosy" or "the user prefers stone walls". The unchecked part of the notebook is the part
   that drifts, and it will drift.
2. **The verification is bounded by perception.** A chest contents fact is unverifiable from a
   distance (section 5). So the system's confidence is highest exactly where it is least needed —
   things the agent is looking at — and lowest where memory matters most.
3. **Writing is a judgement call.** If the agent writes too little, the next session starts blind. If
   it writes too much, the notebook becomes telemetry and nobody reads it. There is no formula for
   this; there is a review, and a size budget.
4. **The agent can poison its own notes.** A wrong lesson, written with a valid evidence id, will be
   repeated with confidence. The evidence id only proves an event happened, not that the lesson is
   the right conclusion from it. Mitigation: lessons carry an outcome counter (`held 4/5`) and are
   demoted by the same reliability logic Craftonomous uses for skills
   `[V docs/research/systems-review.md §3.9]`.
5. **Files are a poor database.** No transactions, no concurrent writers, no schema. Two processes
   (agent plus a second body) writing one file will lose data. The fix is "one writer per file", which
   is a real constraint on companion mode.
6. **The user's edits and the code's writes collide.** Ownership rules reduce it; they do not remove
   it. A hand edit to a code-owned file is silently lost at the next regeneration, and that will
   annoy someone once.
7. **Merging is the dangerous operation.** Deciding that `chest-3` is the old `chest-2` is a guess.
   We refuse to do it automatically (section 9.4), which means the notebook can accumulate ghost
   entries. Ghosts are safer than wrong merges, but they are still noise.
8. **Naming is a social problem, not a technical one.** "village", "the village near the river",
   "village-1" will all appear, because two writers name things. Needs aliases and a rule that the
   user's name wins.
9. **It does not scale to many worlds or many agents.** One folder per save is fine for a demo. Ten
   worlds and two bodies is a different design.
10. **No fuzzy recall.** "Where was that big cave?" fails. Keyword plus an index will cover most of
    it; past that, embeddings, and then the freshness problem comes back for the journal too.
11. **Stale prose is invisible to code.** `base.md` saying "wheat farm unfinished" three days after
    it was finished will mislead planning, and no validator will catch it. Only a periodic
    re-read by a model, with the world in front of it, catches that — and that is a cost.
12. **This design assumes the proxy.** In companion mode with a separate account, the live model is
    the agent's own body and everything above still works. In player-bound mode, "the world while
    the user plays" is observable only by watching the user, which is a consent question, not an
    engineering one.

---

## 17. Questions I will not decide for you

Each with the recommendation I would give, and the alternative I would lose to.

1. **Where does the notebook live?** Recommendation: outside the repo, in the app's data folder
   (`%APPDATA%/praxion/memory`), because it is runtime data that changes every minute and would
   pollute git. Alternative: inside the repo, which makes it a demo asset and reviewable but turns
   every play session into a commit. Compromise: a `memory/example/` snapshot in the repo, the live
   folder outside.
2. **One memory tool with allowed paths, or a general file tool?** Recommendation: restricted tool
   (section 6). Alternative: free file access, simpler and more flexible, harder to keep honest.
3. **Language of the notes.** The repo rule is "write every file in English" `[V AGENTS.md]`;
   `rules.md` is yours and may be Turkish. Recommendation: agent notes in English, `rules.md` in
   whatever you write, and the agent must handle both. Alternative: everything Turkish, which is
   more natural to read but costs more tokens and mixes with English item ids.
4. **Journal granularity:** per day, per session, or per task. Recommendation: per day, with a
   per-task 4-line entry inside it. Alternative: per session, which is cleaner for long autonomous
   runs and worse for a human catching up.
5. **May the agent auto-promote a lesson to `shared/lessons.md`?** Recommendation: propose, never
   promote. A lesson from one world is often false in another. Alternative: auto-promote after three
   confirmations, which is more "alive" and harder to audit.
6. **Death policy.** Always try to recover, or weigh the cost? This is a gameplay/personality
   question, not a memory one, but memory has to encode the answer.
7. **Do we record what the user does while they hold the controls?** Recommendation: record only
   what affects the agent's plan (containers moved, blocks changed in named regions, items taken),
   never chat with other people, and make it switchable. This is the one part of this design with an
   ethical edge, and it should be a written rule, not a default.
8. **Do we keep the raw `facts.log` forever?** It grows faster than the notebook. Recommendation:
   keep 30 days hot, archive the rest compressed; the notebook is what matters.
9. **Mnema.** Session 12 asks how this relates to Mnema `[V docs/discussion-plan.md]`. I do not know
   its storage model well enough to say whether this notebook should live on top of it. Unknown;
   needs a look before the session.

---

## Sources

Repo files read this session:
`AGENTS.md` · `docs/decisions.md` · `docs/research/README.md` · `docs/research/systems-review.md` ·
`docs/research/embeddings.md` · `docs/discussion-plan.md` · `backlog.md` · `notes.md`.

Web, primary (`[V]`):

- Claude memory tool (client-side `/memories`, six file commands, size caps, path-traversal warning,
  multisession progress/log/feature-list pattern) —
  https://docs.claude.com/en/docs/agents-and-tools/tool-use/memory-tool
- Effective harnesses for long-running agents (initializer session, `feature_list.json`, progress
  notes, git log as recovery) — https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents
- Effective context engineering for AI agents (structured note-taking, just-in-time retrieval,
  compaction) — https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- Context windows, 200K / 1M — https://platform.claude.com/docs/en/build-with-claude/context-windows
- Generative Agents (memory stream; recency decay 0.995 per game hour; LLM 1–10 importance;
  relevance) — https://arxiv.org/abs/2304.03442 and https://arxiv.org/html/2304.03442v2
- MemGPT (virtual context, tiers, self-editing memory) — https://arxiv.org/abs/2310.08560
- Zep / Graphiti (bi-temporal, four timestamps, invalidate instead of delete, prefer newer) —
  https://arxiv.org/abs/2501.13956 and https://arxiv.org/html/2501.13956v1
- Mem0 (structured memory vs full context; claimed 91% lower p95 latency, >90% fewer tokens) —
  https://arxiv.org/abs/2504.19413
- Lost in the Middle (accuracy falls for information in the middle of long inputs) —
  https://arxiv.org/abs/2307.03172
- ACE, Agentic Context Engineering (brevity bias, context collapse, incremental delta updates) —
  https://arxiv.org/abs/2510.04618
- Voyager (skill library as a vector DB keyed by the embedding of the program description) —
  https://arxiv.org/abs/2305.16291 and https://arxiv.org/html/2305.16291v2

Secondary (`[S]`): Cline Memory Bank (fixed file set: `projectbrief`, `productContext`,
`activeContext`, `systemPatterns`, `techContext`, `progress`) —
https://docs.cline.bot/prompting/cline-memory-bank

Not obtained: `minecraft.wiki` and `minecraft.fandom.com` both returned HTTP 403 for every page
(inventory sizes, stack sizes, chunk loading, item despawn time). Every Minecraft mechanic in this
document is therefore `[U]` and must be verified against the protocol and the data reports in
session 2/3 before anything depends on it.

---

## 18. Comparison with the first proposal (`docs/research/memory-world-state.md`)

I wrote sections 0–17 before reading it, then read it once.
That file is a survey plus a skeleton; it is not wrong where it disagrees with me, and where it is
right I have said so.

### 18.1 Where the two designs agree

Independent agreement is the most useful thing here, because two designs reached it from different
starting points:

| Agreed | This document | First proposal |
|---|---|---|
| Most of this is not RAG | §12 | §2, §4 table |
| Facts need a key, not a similarity | §11, §12 | §2 bullet 1 |
| Invalidate, never overwrite | §9.4, §5 `history.md` | §5 |
| Never show a remembered fact without age and source | §8.1 | §5 |
| Two time axes per fact | §1, §8.1 | §4 (four timestamps, from Zep) |
| Read-through refresh before the fact reaches a model | §8.3 | §4 "read-through refresh" |
| The agent's own state is never memory | §2 | §4 freshness table, first row |
| An LLM must not write facts | §6 | §4 "who may write" |
| Container contents are only known from opening | §5 | §4 freshness table, `[U]` |

So the argument is not about the mechanism. It is about **how much machinery**, and **what the
thing is made of**.

### 18.2 Where they differ, and who I think is right

**1. Files versus SQLite.** Their §4 proposes one SQLite file per world, with FTS5 for BM25 and
R*Tree for spatial queries; the fact store is a projection of an append-only event journal.
Mine uses markdown files as the projection, with `facts.log` as the journal.

- Their design wins on: atomic writes, concurrent readers, real queries, replay from the journal,
  and a fixed schema (`{kind, key, value, observed_tick, source, superseded_by, valid_to}`). That
  schema is better than my loose "one line per fact".
- Mine wins on: the user can open the file and fix a coordinate; the agent writes prose that a
  person can audit; nothing needs a schema migration; `git diff` on a notebook is readable.
- The deciding question is not technical: **do you want to read the memory, or query it?** For a
  course project whose stated goal is that you master every part, reading it is worth a lot. Once
  a second process writes (two bodies, or a background janitor), files lose and SQLite wins.

My recommendation: files first, *with their record schema written down in the file header*, so the
move to SQLite later is mechanical. Main alternative: their design as is — and honestly, if the
notebook ever holds thousands of lessons, theirs is the one that survives.

**2. Their "push invalidation" is better than my "refresh on touch", and I would take it.** They
propose using block-change packets: the proxy already sees a block update, so a fact anchored at
that coordinate is contradicted the moment it happens, with no polling and no waiting for a read
`[memory-world-state.md §4]`. In my design the agent only finds out when it next looks at the place.
Their version is strictly better for loaded chunks, costs almost nothing, and closes the window
where the notebook is known-wrong but still says `fresh`. Caveat: it requires the proxy to keep
decoding while the agent itself is not acting, which is a session-2 question, not a memory question.

**3. Lesson retrieval: their scoring versus my hard filters.** They start from the Generative
Agents formula (recency × importance × relevance) with per-kind decay, plus BM25 and embeddings
`[§4]`. I propose tags, a pinned set, and a hard cap of ~8, with no scoring.

- Theirs is more principled and more "alive": an old but important lesson can still surface.
- Mine is cheaper and debuggable: you can predict exactly which lessons appear, which matters when
  the user is trying to learn the system.
- The honest problem with their version: three weights and a decay constant per kind are six or
  seven numbers that nobody can set correctly without an evaluation, and they say so themselves
  ("do not tune decay constants before there is an evaluation"). Until that evaluation exists, a
  scoring function is a random number generator with good manners.
- My problem: "the cave where the village was" never comes back, and lessons that do not share a
  keyword with the current task are invisible.

Recommendation: start with tags + pin. Add scoring only after the recall tests in §10.4 and their
§6 exist, and only for lessons — never for places or containers.

**4. Their event journal is bigger than mine.** They make the journal the source of truth for
everything: observations, skill results, chat, deaths `[§4]`. I write only *kept* observations to
`facts.log` and let the live model handle the rest.

- Theirs gives exact replay and a natural home for skill outcomes (which session 5 and 11 will want).
- Mine avoids a log that grows by megabytes per hour and turns into telemetry. A 20 Hz block stream
  written to disk is 72,000 events per hour `[U: arithmetic, 20 × 3600]`.
- The middle: journal the *decisions and outcomes* (task started, task ended, death, container
  opened, block changes inside named regions), not the raw stream. That is close to my §7 table,
  with their replay property kept for the events that matter.

I would take their journal for decisions and outcomes. I would not take it for the raw stream.

**5. Mobs.** They keep mobs with a short TTL `[§4]`. I keep entities live-only. For a singleplayer
demo, live-only is simpler and cannot go stale in a way that matters — a mob seen 40 seconds ago is
not evidence of anything. Their version would be useful for "a creeper was around the corner", which
is a reflex-layer concern (session 7), not memory.

**6. Their evaluation is better than mine for correctness; mine is better for capacity.** Their §6
scripts a world: store a chest, move it, destroy a block, restart the process, then check answers
and labels. That is a real test and it should exist. My §10.4 measures accuracy against pack size,
which theirs does not. The two are complementary and I would run both. Neither exists yet, so
"does it work" is currently unanswerable for both designs.

### 18.3 Why the first proposal was hard to read

Not because it is wrong. Because of its shape:

1. **It is a map of the territory, not a route.** Eight systems, five tables, several mechanisms, and
   no single moment where you see what the model actually reads for one real request. Everything is
   described at the level of "there is a resolver" without showing the resolver's output.
2. **The abstractions arrive before the examples.** "Belief store", "projection", "read-through
   refresh", "fact record" — each is correct, and each needs one concrete line of what it looks like
   on disk before it means anything.
3. **A load-bearing assumption is stated quietly.** "The lesson set is likely thousands of entries"
   is what justifies SQLite, FTS5, BM25 and eventually vectors. Nothing in the design bounds lesson
   growth. If instead the target is "60 lessons, pruned", three of those components disappear.
4. **No update rules.** It says who *may* write, not *when* anything is written. That is the part a
   person needs to picture the system running.
5. **The consequences of the constraints are missing.** "Container contents need an open window" is
   in a table with `[U]`. It is the single most important constraint in the whole topic: it means
   every plan that depends on a chest carries a visit, and it means "instantly current" has a
   physical limit. In the first proposal it is a row; here it is §5 and scenario A.

### 18.4 Proposal: one design, both halves

Not a decision — a concrete thing to discuss in session 3.

| Part | Take from | Why |
|---|---|---|
| Fact record schema | first proposal | it is better than my ad-hoc lines; write it in the file header |
| Push invalidation from block-change packets | first proposal | closes the known-wrong window for free |
| Journal of decisions and outcomes | first proposal | replay, and session 5/11 will need it |
| Markdown notebook, one folder per save | this document | readable, editable, demo-able |
| Ownership table + restricted memory tool | this document | keeps the agent from poisoning the fact base |
| Context pack: deterministic, budgeted, logged | this document | one place where checking happens; debuggable |
| Staleness marks (4 words, not percentages) | this document | behaviour changes on `unchecked`; a number does not |
| Tags + pinned lessons, no scoring yet | this document | no unmeasurable constants |
| Scripted memory test (§6 of the first proposal) | first proposal | proves correctness |
| Pack-size accuracy test (§10.4 here) | this document | proves capacity |
| SQLite | neither, yet | add when a second writer or >500 facts appears |

If I had to pick one sentence to describe the merged thing: **an append-only history of what was
observed and decided, projected into a small notebook a person can read, with one gate that checks
and stamps every line before a model sees it.**

---

## What you should be able to say back

If this design is right for you, you can answer these without the document:

1. Why is the inventory not stored in a file? *(It is observable; a copy can only be more wrong.)*
2. What does `unchecked` forbid? *(Using a claim as a premise for a destructive or expensive action.)*
3. Why does the agent not read the notebook directly? *(Because the pack is where checking and
   marking happen; direct reads carry no provenance.)*
4. What happens when a fact and the world disagree? *(World wins, the loser is kept with a time and
   a reason.)*
5. What is the first signal that the notebook has outgrown itself? *(The 20-question accuracy test
   drops as the pack grows — not the token count.)*
6. Why no embeddings yet? *(Exact keys and freshness; the corpus is small; similarity returns the
   wrong chest, confidently.)*

