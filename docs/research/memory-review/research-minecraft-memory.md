# How Minecraft and game agents actually implement memory (2024–2026)

Research for session 3. 2026-10-27. Nothing here is a decision. Every claim carries a tag.

## 0. How to read this, and what I could and could not check

**Evidence tags**

| Tag | Meaning |
|---|---|
| **[V]** | I read this in a primary source this session. The URL or file path is next to the claim. |
| **[R]** | I read it in a repo file this session (path given). That file's own evidence level is unchanged; it is cited, not re-verified. |
| **[U]** | My own knowledge. Not verified. Check before relying on it. |

**Tools.** arXiv abstract/HTML pages, the arXiv API, and the GitHub API/raw files worked. Two things did
not: the `websearch` tool (cancelled) and DuckDuckGo HTML (HTTP 202). So there is **no blog or vendor
source in this file**, and no paper I could only reach through a search engine. Everything below is a
paper, a paper's public code, or a repo file.

**Scope.** Table A has thirteen Minecraft systems; MineNPC-Task and WALL-E are added below it; table B has
five systems from outside Minecraft. Two of them I read at code level (mindcraft, MFM); the rest at paper
level. The previous proposal, [memory-world-state.md](../memory-world-state.md), already covers Generative
Agents, Optimus-1, MrSteve (main text), Zep/Graphiti, Craftonomous, altoclef and Baritone; this file does
not re-explain those, it adds what was missing and draws the comparison across all of them. Section 8
lists the delta.

---

## 1. Twelve words, explained once

Memory writing is full of words that mean different things to different people. These are the meanings
used in this file.

- **Fact** — a single statement about the world: "there is a chest at (120, 64, −340)". Either true or
  false right now.
- **Belief** — a fact the agent thinks is true, with the time it was last checked. Beliefs can be wrong.
- **Episode** — a record of one thing that happened: what was seen, what was done, what came out.
- **Journal (or log)** — an append-only list of episodes. Nothing is ever edited or deleted. New lines go
  at the end.
- **Projection** — a table built by reading the journal and throwing away detail. It can always be
  rebuilt from the journal, so it is never the source of truth.
- **Consolidation** — turning many episodes into a smaller, more general record ("5 successful wood
  runs" → "punch the trunk, then the block above it").
- **Embedding** — a list of numbers representing the meaning of a text, so that "similar meaning" becomes
  "small distance". A **vector index** (Chroma, `sqlite-vec`) searches by that distance.
- **RAG** — retrieval-augmented generation: fetch a few text chunks by similarity and paste them into the
  prompt. Good for documents. Bad at deciding which of two contradicting facts is current.
- **BM25 / FTS5** — keyword search with word-frequency scoring. Exact words only, no meaning. SQLite has
  it built in.
- **TTL** — time to live. A record is discarded after a fixed age.
- **Invalidation** — marking an old record as no longer true instead of deleting it. The history stays.
- **Chunk** — Minecraft's 16×16×384 column of blocks. The server only sends blocks for chunks near the
  player. Outside them the agent literally cannot see, so it must remember.

---

## 2. The short answer to "is this RAG?"

**No, not mostly — and the literature says the same thing by not doing it.**

Of the thirteen systems in table A, six put an embedding in the memory path: JARVIS-1, Optimus-1, MrSteve,
WISE, Voyager, and XENON (for item names only). All six use it for the same narrow job — *"which past
experience, place, or item name looks like this one"*. **Not one** uses similarity to answer "what is true
at coordinate X now". Places, counts, container contents and inventories are looked up by key, by position,
or by walking to the thing and looking again.

Meanwhile the two systems with the strongest memory results in 2026 (PRO-LONG and MFM) both dropped
similarity search entirely and use **a full append-only log plus programmatic search over it** — text
matching and code, not embeddings.

- PRO-LONG, ARC-AGI-3, tool ladder ablation, GPT-5.5: read-only access to the log **23.1%** → add
  grep/regex **27.2%** → add Python **38.3%** → add write/edit **41.2%**
  ([arXiv:2607.20064](https://arxiv.org/abs/2607.20064), Table 1) [V]
- MFM, Minecraft: replacing its keyword/structured index with "chronological text matching over a flat
  log" dropped the Dragon score from 16 to **8.7** milestones
  ([arXiv:2610.22686](https://arxiv.org/abs/2610.22686), §4.3) [V]

That is the central finding of this review, and it is also the answer to the user's question: **the
memory Praxion needs is closer to a well-indexed database plus a searchable log than to a RAG pipeline.**
Embeddings keep one small job — fuzzy recall of past experience and of what the *user* meant — and
section 7 prices that job.

---

## 3. What Minecraft agents store, and how (table A)

Rows are roughly ordered from least to most memory; the last one is there because it has no memory at
all. "Staleness handling" is the column to read first: it is empty almost everywhere.

| System (year, license) | What is stored | Record format | Staleness / change handling | Retrieval | Embeddings? | Measured benefit |
|---|---|---|---|---|---|---|
| **STEVE-1** (2023; code at [Shalev-Lifshitz/STEVE-1](https://github.com/Shalev-Lifshitz/STEVE-1), **no license file**) [V] | Nothing outside the network. Its only memory is the last **128 hidden states** of a Transformer-XL — "only a few seconds of gameplay" at 20 Hz | recurrent state inside the policy | none — it is a sliding window | implicit | no | — (this is the baseline everyone else beats) — [V] [arXiv:2411.06736](https://arxiv.org/abs/2411.06736) §1 |
| **DEPS** (2023, paper CC BY 4.0; code at [CraftJarvis/MC-Planner](https://github.com/CraftJarvis/MC-Planner), no license file at the repo root) [V] | Nothing persists. Per attempt it holds candidate plans and picks one by predicted completion time | plan objects in the prompt | none; a failed plan is simply re-planned | "Selector" = softmax over predicted horizon | no | MrSteve's appendix compares it head-to-head; 4 tasks, 27–67% [V] [arXiv:2411.06736](https://arxiv.org/abs/2411.06736) App. M |
| **Plan4MC** (2023, MIT) [V] | A **skill graph**: skills as nodes, "this skill needs that item" as edges. No episodes, no places | graph in code | none | graph walk + search | no | no memory ablation; the graph is the method — [arXiv:2303.16563](https://arxiv.org/abs/2303.16563) |
| **GITM** (2023, **no license**) [V] | For each **sub-goal**, a list of successful action sequences. After 5 successes they are LLM-summarised into one general plan, stored first in the list | Python dict `{subgoal: [plan, plan, …]}` | none. A plan that stopped working keeps its place at the head of the list | **dictionary lookup by sub-goal string, then take element 0.** No similarity at all | **none** | Diamond 35.0% → **67.5%**; iron pickaxe 57.5% → 95.0% (40 games each) [V] [arXiv:2305.17144](https://arxiv.org/abs/2305.17144) Tab. 3 |
| **JARVIS-1** (2023, **no license**) [V] | Successful **(task, observation) → plan** entries. Grows without limit; self-instruct fills it | key-value memory; keys multimodal (text task + visual state), values plans | none. A plan that is now wrong is retrieved like any other | two-stage: text-embedding filter, then **visual** CLIP similarity re-rank, top-k, one entry per sub-goal | yes (CLIP text + image) | Diamond pickaxe: abstract claims "5 times" the reliability of VPT, the figure caption gives **6% vs 2.5%** (2.4×); the text I read does not reconcile the two, so do not quote "5×" without finding the metric. Ablation: reasoning-before-retrieval helps, multimodal beats text-only, no numbers in text [V] [arXiv:2311.05997](https://arxiv.org/abs/2311.05997) §1, §3.3, §4.2.2 |
| **Optimus-1** (2024, **no license**) [V] | Two stores: a **knowledge graph** of 851 objects (node → what it helps craft, queried by topological sort), and an **experience pool** of successful *and failed* sub-goal cases | graph + per-sub-goal records carrying frames, plan, initial state, failure tag | none. "REPLAN" cases are stored as failures; nothing ages | graph: topological sort. Pool: MineCLIP similarity of sub-goal text to stored frames | yes (MineCLIP) | Diamond 3.25% → 9.59%, iron 36.3% → 53.3% from adding the pool [R] [memory-world-state.md](../memory-world-state.md) L33 |
| **MrSteve** (2024, **no license**) [V] | **Place Event Memory**: position → place clusters → event clusters. Each frame is `(embedding, (x,y,yaw), t)` | nested clusters; `what/when` = MineCLIP video embedding + timestamp, `where` = pose | **Only capacity.** When full, drop the oldest frame of the largest event cluster. No age, no re-check | top-k event clusters by task-vs-centre cosine, then per-frame scores above threshold | yes (MineCLIP) | Map coverage 84.4% vs 50.8%; revisits 0.38 vs 2.68 [R] [memory-world-state.md](../memory-world-state.md) L34 |
| **MrSteve**, memory pressure (same paper) [V] | Same, tested at 0.1K–12K frames capacity | — | "drop oldest in largest cluster" degrades gracefully; FIFO memory does not | — | — | At 12K steps: ~305 event clusters, ~40 frames each, ~1.2K similarity comparisons per query; a centre-only variant needs ~0.3K with a small loss [V] [arXiv:2411.06736](https://arxiv.org/abs/2411.06736) App. O, P |
| **WISE** (2026; code not found) [V] | MrSteve's place/event memory **plus** a VLM-built **Semantic Affordance Event Graph**: entity nodes with coordinates and timestamps, `CAN_OBTAIN` edges (cow → beef), `CO_OCCURS_WITH` edges | graph, built asynchronously off the control loop, batched VLM calls | **Nothing in the text I read**: no expiry, no invalidation, no re-verification; same capacity rule as MrSteve | two levels: cosine over frames, then graph traversal from the task word ("obtain beef" → beef node → incoming `CAN_OBTAIN` → cow instances → coordinates), combined with weight λ | yes (MineCLIP) + VLM labels | 77% vs **31%** for feature-only retrieval and vs 33% for its MrSteve baseline, on ABC-Sparse [V] [arXiv:2606.12852](https://arxiv.org/abs/2606.12852) §4.5.2 |
| **XENON** (2025–26; code at [sjlee-me/XENON](https://github.com/sjlee-me/XENON), pushed 2026-09-14, **no license file**) [V] | Two count tables: a dependency graph (item → required items) and **Failure-aware Action Memory**: `S(action,item)` and `F(action,item)` successes/failures | integer counters per pair, thresholded: invalid if `F ≥ S + x0` | **The only system that handles change under a name.** When the environment's true rules are swapped mid-run (and it is told *which* items changed, not how), it re-learns 7/7 changed items; every baseline scores **0.0** when actions change | counters + Sentence-BERT similarity between item names; failed actions are *pruned from the candidate list* before the LLM sees them | partly (Sentence-BERT, for item similarity only) | Without the learned graph, iron 0.10 → 0.36 success; adding action correction → 0.45; +reprompting → 0.46 [V] [arXiv:2505.24157](https://arxiv.org/abs/2505.24157) §4.2, Tab. 16–17 |
| **mindcraft** (2024–, MIT) [V] | Two unrelated things: named places `{name: [x,y,z]}`, and an LLM prose summary of the chat/turn history **truncated to 500 characters** | `memory_bank.js`: a plain dict, saved as JSON. `history.js`: rolling summary + full history dumped to a timestamped file | none. A place name never expires and is never re-checked; the summary is overwritten each time | dict lookup by name | no | none claimed — [src/agent/memory_bank.js](https://github.com/kolbytn/mindcraft/blob/HEAD/src/agent/memory_bank.js), [history.js](https://github.com/kolbytn/mindcraft/blob/HEAD/src/agent/history.js) |
| **Voyager** (2023, MIT) [R] | Skill library only: `{name, code, description}` per learned skill. No world facts, no places, no inventory history | JS files + `skills.json` + Chroma | A skill with an existing name **overwrites** the old one; older versions are kept on disk but unreachable | description embedding, top-5, query = task context + chat summary | yes | Removing the library makes progress "plateau" (qualitative, no number) [V] [arXiv:2305.16291](https://arxiv.org/abs/2305.16291) §3.4; storage detail [R] [systems-review.md](../systems-review.md) §3.4 |
| **PEAM** (2026; code at [Eurekaarrow/PEAM](https://github.com/Eurekaarrow/PEAM), **no license file**; paper CC BY 4.0) [V] | An **episodic store** of successful and failure→correction trajectory pairs, plus **LoRA adapters per skill category** that absorb chosen episodes into weights | trajectories; adapters; a "parameterization worthiness" score decides what is worth internalising | none for facts. Its "forgetting" axis is about *skills*, not about the world: 0% cross-category forgetting vs 32.4% (shared LoRA), 43.3% (EWC), 78.5% (full fine-tune) | routing by skill category; suitableness gate; fallback to the slow LLM | no real embedding (TF-IDF overlap of code snippets, for redundancy scoring only) | 69.7% vs Voyager 54.5% (23/33 vs 18/33, McNemar p=0.018); 3.2 s vs 5.5 s latency; 4.6K vs 31.2K tokens per task [V] [arXiv:2605.27762](https://arxiv.org/abs/2605.27762) Tab. 1–2 |
| **Optimus-3** (2025, MIT) [V] | No external memory. Memory moved into weights (System 1 / System 2 mixture of experts) | — | — | — | — | — [V] [arXiv:2506.10357](https://arxiv.org/abs/2506.10357) abstract |

Two more that matter for the "what is a fact worth storing" question, and are easy to miss:

- **MineNPC-Task** (Microsoft, 2026). Its landmark memory is `{"weapon storage": {coordinates: [x,y,z],
  radius: 6}}`. No timestamp, no version, no confidence [V]
  ([arXiv:2601.05215](https://arxiv.org/abs/2601.05215) App. D.2). The paper's own conclusion from 216
  subtasks with 8 expert players: participants "highlight… the need for stronger memory persistence
  across tasks" [V] (abstract). The benchmark that was *built to test memory* found memory lacking.
- **WALL-E** (2024). Not a memory store, a **rule library**: rules are induced from trajectories, then
  refined, translated to code, and **pruned by maximum coverage** when predictions and reality disagree.
  It compares the predicted `action_result` with the observed one and learns only from the mismatches
  [V] ([arXiv:2410.07484](https://arxiv.org/abs/2410.07484) §3.2). This is the cleanest published example
  of "the world model is corrected by evidence", and it is not used by anyone else.

---

## 4. What non-Minecraft agents do better (table B)

The two best memory designs of 2026 are not Minecraft papers. They are worth more to us than most of
table A, because they are honest about the trade-off.

| System (license) | What is stored | Record format | Staleness handling | Retrieval | Embeddings? | Measured benefit |
|---|---|---|---|---|---|---|
| **MFM** (2026, **MIT code**) [V] | Five kinds at once: **episodic** (observation image, structured state, action, outcome), **spatial** (a grid of cells walked), **semantic** (`notes.md`, `GUIDE.md`), **procedural** (`procedures/`, executable), **working** (`working.md`: the current plan) | one directory per decision: `episodes/ep_000042/{observation/image.png, state/state.json, action.json, outcome.json}`, one `index.jsonl` line per closed episode, `grid.jsonl`, and the model's own `mine/` notes. All harness-written and immutable once closed | **None by design, and this is the point**: the index is only an *address*. To trust a remembered place you load its frame and compare it with what you see now. Paper example: a note about "a mine entrance that has become blocked" [V]. Consolidation is *rebuilt from the index* at every task boundary "so it is never stale and needs no separate state" [V] | agent writes a query (`--near X Z`, `--tag stuck`, `--action go_to`, `--grep`, `--last 5`), gets rows with file paths, then **looks** at the frames | **none** | Dragon 16/24 milestones vs 12.7 best baseline; removes stored frames → 8.0/8.3; removes indexing → 8.7; removes consolidation → 8.3 [V] [arXiv:2610.22686](https://arxiv.org/abs/2610.22686) §4.3 |
| **MFM carry-over** (same) [V] | A new run in the *same world* inherits every earlier run's log, episodes and index, hard-linked, plus a table of which of the earlier run's scripts were called, refused and how they ended | `previous/life_<i>/`, `previous/index.jsonl`, `previous/scripts.md` | justified only because "the benchmark's world is deterministic for a given seed, so what one run verified… is true for the next run" [V] | the model greps them itself; nothing is injected | no | A `bridge_north` procedure written in a *previous life* was called 9 times in the next one and produced the bridge to the lava pool; `cut_step` was called 60 times; one inherited procedure was **disabled** after a death was traced to it [V] §5.1 |
| **PRO-LONG** (2026, **MIT**) [V] | One append-only `logs.txt`: per action, a header (action number, level, attempt, score), the model's summarised plan, the chosen action, and the resulting board. Nothing is discarded | plain text, designed to be grepped | none needed: the log *is* the history, and "fuller memory" scores higher — truncating to the last 25 actions is worse, and the gap widens with more attempts [V] | **regular expressions and Python over the file**; the agent writes scripts (one wrote `regress.py` to replay every action and check its own game model) | none | +15.7 to +21.0 points over the same agent without the log, at 4.2–5.8× fewer tokens than specialised harnesses [V] [arXiv:2607.20064](https://arxiv.org/abs/2607.20064) |
| **VISTA** (2026, **MIT**) [V] | Every past frame, **losslessly**, addressed by frame ID. Agent gets only the current frame automatically and can pull old ones, compare, or zoom | image files + IDs, model-directed inspection | the model looks again when it doubts itself; "exact pixel readout" lets it re-read numbers it mis-saw | by frame ID, plus tool calls to reorganise its visual input | no | Harness ladder, GPT-5.6: images 47.3 → +limits 51.7 → +conversation 65.8 → **+notes 70.1** → **+lossless visual memory 94.1** → +pixel readout 99.0 [V] [arXiv:2610.02200](https://arxiv.org/abs/2610.02200) §4.3 |
| **Generative Agents** (2023) [R] | One stream of natural-language memories + reflections | text rows with timestamp, importance, last-access | recency decay 0.995/game-hour; nothing is ever marked false | score = recency + importance + cosine | yes | — [R] [memory-world-state.md](../memory-world-state.md) L32 |
| **Zep / Graphiti** (2025) [R] | Bitemporal fact graph: raw episodes kept, extracted edges with four timestamps | graph DB | **contradiction sets the old edge's invalid time; nothing deleted** | cosine + BM25 + graph walk, reranked | yes | — [R] [memory-world-state.md](../memory-world-state.md) L35 |

---

## 5. The six patterns that repeat

Ranked by how much evidence is behind them, not by how elegant they are.

### 5.1 Keep the raw evidence; index it thinly; let the model go back and look

Four independent results, one shape. MFM: never summarise the episode, index it. VISTA: never compress
the frame, keep it by ID. PRO-LONG: never truncate the log. MrSteve: keep the frames, store a centre
embedding as a *shortcut* to them.

The numbers are large and they all break the same way when you remove the raw layer: MFM 16 → 8.0 by
dropping frames [V]; VISTA 70.1 → 94.1 by adding lossless frames [V]; PRO-LONG worse with a 25-action
window [V]. The opposite direction — "compress everything into a summary" — is what mindcraft does
(500 characters) and it is the weakest memory in table A [V].

**Why it works, in one sentence.** A summary decides *now* what will matter *later*, and that guess is
usually wrong; an index only decides *where to look*, which is a much cheaper guess.

**What it means for Praxion.** This is almost free for us. The proxy already sees every packet, so an
append-only journal costs disk, not design. The hard part is the index, not the storage.

### 5.2 Structured semantics beat similarity for "what do I need" questions

WISE's ablation is the cleanest measurement in this whole review: same frames, same controller, same
exploration — replace the affordance graph with visual similarity and success falls **77% → 31%**, i.e.
back to its MrSteve baseline of 33% [V]. GITM goes further: **no embeddings at all**, a dictionary keyed by
sub-goal, and diamond success more than doubles (35.0 → 67.5) [V].

Two different mechanisms, one conclusion: when the question has a *name* ("beef", "iron pickaxe",
"chest"), a key, a graph edge or a recipe beats a distance.

### 5.3 Failures are worth more than successes, and only some systems know it

- Optimus-1 stores REPLAN cases and attributes part of its gain to having both [V] (store design) /
  [R] (numbers).
- PEAM trains on **failure→correction pairs**; removing the contrastive term drops wrapper-format-valid
  code from 12/12 to 0/12 [V].
- XENON prunes actions that failed repeatedly, and only concludes "my recipe knowledge is wrong" when
  *every* action has failed [V]. That second inference is the interesting one: it separates "the tool is
  broken" from "the world is not as I believed".
- altoclef blacklists a target after N failures and **resets the count** when the bot gets closer or gets
  a better tool [R] — the reset condition is what makes a blacklist a memory instead of a grudge.

Voyager, JARVIS-1, MrSteve, WISE and mindcraft store successes only, and none of them stores *why*
something failed.

### 5.4 Consolidation is a compression step that keeps its receipts

GITM: after 5 successes, summarise into one plan and keep the summary at the head of the list [V].
MFM: fold episodes into `notes.md` / `GUIDE.md` / `procedures/`, but every note cites the episode IDs it
came from, and the episodes are never touched [V]. PEAM: fold a whole trajectory into an adapter, with a
score that penalises redundancy against what is already internalised [V].

The receipt is the difference between "the memory lied" and "the memory can be audited".

### 5.5 Freshness is handled on the read path, not by expiring data

Nobody in table A expires world facts. The systems that survive a changing world do it at read time:

- MFM re-looks at the frame before walking to a place [V].
- WALL-E compares its prediction with the observed result and edits the rule [V].
- XENON re-learns a dependency when actions keep failing [V].
- MFM's `EXPECT:` protocol makes this explicit — the model writes what it expects the log to show next
  turn, and the next briefing must begin with `OBSERVED vs EXPECTED:` [V] (this text is in the shipped
  code, `MCU/mcu_worldmodel/episodes.py`).

Expiring data on a timer is what altoclef/Craftonomous do (10-minute memory horizon) [R], and both are
coarse: one clock for every kind of fact.

### 5.6 Everything that matters happened *inside one run*

Every result above was measured in a single episode, a single process, or (MFM) with an explicit
determinism argument for carrying across runs [V]. Nothing measures a memory that has to survive a
restart, a save/load, or a week of real time. That is precisely the regime Praxion's demo is in
(hand-over, take-over, the user playing in between).

---

## 6. What is surprisingly absent

### 6.1 The headline: nobody handles a world that changed while the agent was away

This is the direct answer to the user's requirement, and it is a real gap.

Searching every system in table A for a record that says *when a fact was true* and *whether it is still true*:

| What I looked for | What I found |
|---|---|
| A stored fact with `observed_at` | MrSteve/WISE store a timestep per frame, but only for *when I was there*, never compared with now [V] |
| A fact marked invalid/stale | none in table A. Zep does it for conversation facts [R] |
| Expiry by kind of fact | none. Capacity eviction only (oldest in largest cluster) [V] |
| Re-checking a fact on arrival | MrSteve's Mode Selector decides *explore vs execute* from whether the resource is in memory [V] — the closest thing — but it does not verify the memory |
| Persisting block data across sessions | Baritone's 2-bit-per-block chunk cache in region files, plus an index of chests/furnaces/portals [R]. **No invalidation** [R] |
| Container contents remembered | altoclef's container cache, in-process, lost on restart [R] |

The strongest counter-examples are all about **rules**, not **world state**: XENON (recipe rules swapped
mid-run, 7/7 re-learned) [V] and WALL-E (rule library pruned by prediction error) [V]. Both need the
agent to keep failing first. Neither can answer "is the chest still at (120, 64, −340)?".

Why the gap exists is not mysterious. For a Minecraft paper, the world is a fresh seed every episode, so
there is nothing to be stale: the agent has never been here before. Praxion is the opposite case. It plays
one persistent world that **the user also plays**, so the only entity that changes the world while
Praxion is away is the user — and hand-over/take-over is the demo. The gap in the literature is the
centre of our problem.

### 6.2 Other gaps worth naming

1. **No system measures wrong-memory cost.** Every ablation removes memory and measures task success
   (MFM, WISE, GITM, PEAM). Not one measures *how often a briefing contained a false fact*, which is the
   failure mode that would embarrass Praxion in front of a professor.
2. **No cost model for memory.** MFM and PRO-LONG report tokens; nobody reports *storage* growth or
   retrieval latency at 10⁵ episodes. MrSteve reports query cost (1.2K vs 0.3K comparisons) [V] and is
   the exception.
3. **Transfer is not real.** MC-EvoBench (2026, controlled, 5 seeds, matched step budgets) tested whether
   experience transfers to related tasks in Minecraft and found **no stable transfer**: "related
   experience yields no stable transfer… its small wood-species gain hinges on one seed, and all
   intervals include zero" [V] ([arXiv:2610.13829](https://arxiv.org/abs/2610.13829)). Any claim that
   Praxion's lessons will transfer between worlds needs its own evidence.
4. **Retrieval ignores reliability.** Voyager's top-5 is similarity only; success rate, preconditions and
   cost play no part [R]. A skill that works 1 time in 10 is retrieved as readily as one that works 9 in
   10. Craftonomous's Wilson bound [R] and XENON's counters [V] are the fixes, and neither is used by a
   memory system proper.
5. **Written by the model, trusted by the model.** MFM lets the model write `notes.md` freely; nothing
   checks a note against the episodes it cites. PEAM's worthiness score includes "stability" [V] but that
   is about skills. No system in table A gives a memory a confidence that a human could audit.
6. **The LLM never sees the price of a memory.** No paper reports what a briefing costs in prompt tokens
   *per fact*, so nobody designs for a budget. PEAM's whole pitch is that this cost is real: 31.2K →
   4.6K tokens per task by moving skills out of the prompt [V].

---

## 7. One chest, five moments (a concrete walk-through)

The previous proposal was hard to follow because it described a store, not a story. This is the same idea
told as a timeline. It is a *proposal*, not a finding; the mechanisms named are from section 5.

Setup: the agent put a chest at (120, 64, −340) and filled it with 12 iron ingots and a diamond pickaxe.
Then the user took over and played for forty minutes.

**Moment 1 — the write (the agent is standing there).**
One episode line goes into the journal: what it saw (chunk loaded, block ID at that coordinate), what it
did (open, withdraw, deposit), what came out (window contents as sent by the server). Then one belief row:

```
kind=container  key=(120,64,-340)  value={iron_ingot:12, diamond_pickaxe:1}
observed_tick=184223  observed_wall=14:02:11  source=sight  confidence=exact
superseded_by=null
```

Nothing else. No embedding: nobody will ever ask "which chest is semantically similar to this one".

**Moment 2 — the agent walks away.**
The chunk unloads. The belief does not change, but its age starts growing. That is all "staleness" means
here: an unchanged row plus a clock.

**Moment 3 — the user plays and empties the chest.**
The agent sees nothing. It cannot: the chunk is not loaded, and the server sends no block updates for
chunks the client does not have. **No design can fix this.** (Which is why the answer to "pull stale to
current instantly" has to be "instantly *when observable*, otherwise labelled" — the honest version.)

**Moment 4 — the agent is asked "do we have a pickaxe?".**
The question reaches the *resolver*, not the store. Three outcomes, in this order:

1. **Live?** Is the chunk loaded right now? If yes: ask the proxy, get the truth, and if it differs from
   the belief, write a supersession (`old.superseded_by = new_id`) and hand the LLM the new value with
   `source=sight`. Cost: one map lookup.
2. **Not live, but cheap to check?** The pickaxe is not in the inventory and the agent is 40 blocks away —
   a policy decision, not a memory decision: is walking there worth it? (This is the one place a decision
   model earns its keep.)
3. **Not checkable.** Return the belief *with its age and source*: "12 iron ingot and a diamond pickaxe,
   as I saw them 41 minutes ago". The LLM and Jev both get the age. Neither gets a bare fact.

The difference between outcome 1 and outcome 3 is the whole design, and it is one function.

**Moment 5 — the agent returns and the chunk reloads.**
The server resends the chunk. The proxy sees the new block states and any block-change packets. The
resolver notices the belief's key is now live and refreshes it *without being asked*, writing the
supersession. If the agent opens the chest, the window contents arrive as packets and the container
belief is refreshed the same way. Correction latency here is "one chunk load", not "next time someone
asks" — this is the "stale to current" the user asked for, and it comes from where the agent is standing,
not from a smarter index.

**What the LLM sees, in each case.** Not the table above. Something like:

```
Have: 12 iron ingot, 1 diamond pickaxe  [seen 41 min ago, chest at 120/64/-340, not checked since]
Near: crafting table at 118/64/-338 (live), furnace at 121/64/-341 (live)
```

The compact line is a *view*, built by code from the belief table. The LLM never reads the table. Jev gets
the same facts as fields. That separation — store, resolver, view — is what makes the memory explainable
to a person, which is the user's stated goal.

**Where the embeddings are, in this story.** Nowhere. They appear in exactly one place: when the user says
"go back to the cave where we found the village", and the agent has to match that sentence against 200
place descriptions. That is a search over *descriptions of places*, not over facts.

---

## 8. What the repo already had, and what is new here

Already in `docs/research/memory-world-state.md` [R]: the RAG-vs-typed split, the two-timestamp +
read-through idea, append-only journal + projection, the freshness table by kind, Generative Agents,
Optimus-1, MrSteve main text, Zep, Craftonomous, altoclef, Baritone.

New in this file [V] unless marked:

- **MFM at code level.** The actual on-disk layout, the "index is an address, not a summary" rule, the
  `EXPECT:` protocol, cross-life carry-over, and the ablation numbers.
- **PRO-LONG.** The tool ladder (23.1 → 41.2) — the strongest quantitative case against embeddings-first.
- **VISTA.** The 70.1 → 94.1 step, i.e. what "keep the raw evidence" is worth when measured alone.
- **WISE.** 77% vs 31% for structured semantics vs similarity, and the fact that it has *no* expiry or
  invalidation.
- **XENON.** Counter-based reliability with reset, and the only dynamic-knowledge experiment in Minecraft.
- **WALL-E.** Rule library pruned by prediction error.
- **GITM.** The dictionary-with-no-embeddings design and its ablation.
- **PEAM.** Parameter consolidation, token cost per task, forgetting numbers.
- **MineNPC-Task.** The landmark schema, and the finding that players want stronger memory.
- **MC-EvoBench.** Negative result on experience transfer.
- **mindcraft's `memory_bank.js`.** 25 lines: a dict of `name → [x,y,z]`, a getter, save/load. No
  timestamp — the honest baseline.
- **Licensing reality** (GitHub API and raw `LICENSE` fetches, 2026-10-27). Four of the thirteen have a
  real code license and all four are MIT: Voyager, Plan4MC (`PKU-RL/Plan4MC`), mindcraft, Optimus-3. Eight
  have public code with **no license file** — MrSteve (`frechele/MrSteve`), GITM, JARVIS-1, Optimus-1
  (`iLearn-Lab/NeurIPS24-Optimus-1`), STEVE-1, XENON, PEAM, DEPS (`CraftJarvis/MC-Planner`) — which
  normally means "all rights reserved" [U]: reading is fine, copying is not. WISE's code I could not find.
  **Read them; copy almost none of them.**

---

## 9. Five ideas worth borrowing

Each is a proposal with its reasoning, its main alternative, and what would prove it works. None of them
is decided.

### Idea 1 — Journal first, text index second, embeddings last

**What.** One append-only journal file per world (JSONL or SQLite), one row per observation/action/result.
On top of it, three cheap read paths: (a) key lookup for exact facts, (b) SQLite FTS5 (BM25) for text,
(c) a `procedures/` and `notes/` directory the agent writes itself. Embeddings only for place
descriptions and lessons, added when there is a measured need.

**Why.** It is the only pattern with large measured wins in 2026, from three independent groups: PRO-LONG
+15.7–21.0 points and 4.2–5.8× cheaper [V]; MFM 16 vs 12.7 milestones and a 2× drop without the index
[V]; VISTA 70.1 → 94.1 [V]. All three are *simpler* than an embedding pipeline, not more complex.

**Main alternative.** A vector store as the primary index (Chroma/`sqlite-vec`) plus LLM summarisation of
old history. Rejected because: similarity ranks by wording, so a superseded fact can outrank the current
one [R]; summarisation loses exactly the detail that turns out to matter (VISTA, MFM [V]); and both cost
an LLM call per write.

**Cost and risk.** Disk. A journal of a 40-hour playthrough with a per-tick state line is large — unknown
how large until we measure, and the measurement is trivial: log a two-hour session and multiply. The
index has to stay thin or queries get slow; MFM's rule ("grepped, never read whole" [V]) is the
discipline.

**Test.** Replay a recorded 2-hour session through both designs (journal+FTS5 vs Chroma) and compare:
correct answers on 50 scripted questions, tokens sent to the LLM, latency at p50/p99, and whether a
superseded fact ever appears in a briefing.

### Idea 2 — Freshness belongs to the read path, as a resolver, not to the write path as a TTL

**What.** Every fact carries `observed_at`, `source` and `superseded_by`. A single resolver function sits
between the store and every reader (LLM, Jev, planner). On each read, in order: (1) observable live →
refresh and supersede; (2) observable at a cost → the *caller* decides whether to pay; (3) not observable
→ return with age and source. Plus a push path: block-change packets invalidate any belief anchored at
that coordinate immediately, no polling.

**Why.** It is the only design that answers the user's requirement without lying, and every piece of it
already exists in the literature *separately*: Zep's invalidate-never-delete [R], WALL-E's
predict-compare-correct [V], MFM's look-again-before-you-trust [V], XENON's "everything failed, so the
knowledge is wrong" [V]. What nobody has done is put them behind one read path for a game world.

**Main alternative.** A TTL per kind of fact (10 minutes for mobs, forever for named places), as
Craftonomous does [R]. Simpler, no resolver, but it is a fixed guess, it cannot tell "stale but probably
still true" from "stale and probably false", and it cannot use the one thing Praxion has (exact live
perception) to fix itself.

**Cost and risk.** The resolver is on the hot path, and a slow resolver is a frozen bot — the exact
failure the repo's decisions warn about. It must be pure, synchronous and allocation-light, with the
network/disk refresh moved to a worker. Also: "observable live" is a perception *policy* question
(fair-play), which is session 3's open question, not a memory question.

**Test.** The scripted world from the earlier proposal, plus the one thing nobody has measured: **the
wrong-fact rate in briefings** and **correction latency** (ticks from the chunk reloading to the belief
being right). Target: zero unlabelled stale facts, ever.

### Idea 3 — "Expect, then compare" as the self-correction loop

**What.** Every plan the agent commits ends with a machine-checkable expectation ("after this, inventory
delta = +1 log, position within 2 blocks of X"). Next turn, code — not the LLM — compares the expectation
with the journal and states the difference. Twice-failed expectations block the same target from being
proposed again.

**Why.** It is in shipped 2026 code as a prompt protocol [V] (`MCU/mcu_worldmodel/episodes.py`,
`MFM_EXPECT_NOTE`), and it is the cheapest known way to turn the journal from a passive log into a
correction signal. It also fixes a documented Praxion-relevant bug class: "the action returned" is not
"the world changed" [R] ([systems-review.md](../systems-review.md) §1.2).

**Main alternative.** Let the LLM read the log and notice the discrepancy itself. Works (PRO-LONG does
exactly this, with Python over the log [V]) but costs a full model call and is unverifiable — there is no
record of what was expected, so nobody can audit the miss.

**Cost and risk.** Writing a checkable expectation for every action is work, and a wrong expectation
becomes a false alarm that eats trust. Start only with actions whose effect is countable (inventory
delta, block changed, position reached).

**Test.** Inject 20 known divergences (a dig that the server refuses; a pickup that never lands). Every one
must be caught by code, with zero LLM calls.

### Idea 4 — A thin structured layer for "what do I need" and "what is near", so the LLM never guesses

**What.** Two typed graphs, both derived from data we already have, both queried without embeddings:
the **recipe/tech graph** (item → prerequisites, from Minecraft's own recipe data — Optimus-1's 851-node
graph [V], Odyssey's tables [R]) and a **place graph** for the local world (`chest ↔ base`, `cow →
beef`, `ore → pickaxe`, edges learned from what the agent actually saw — WISE's `CAN_OBTAIN` [V]).

**Why.** WISE: 77% vs 31% for the same memory without the graph [V]. GITM: no embeddings at all, diamond
35.0 → 67.5 [V]. The recipe graph is free — Mojang ships the data ([decisions.md](../../decisions.md)
already says game knowledge comes from data, not code).

**Main alternative.** Let the LLM answer from its own knowledge with a RAG'd wiki. Cheap to start, but it
is exactly the failure XENON documents: LLM priors about Minecraft are wrong often enough that the agent
must correct them from experience, and a wiki index cannot be corrected by experience [V].

**Cost and risk.** Two graphs are two things to keep consistent with the world. The place graph is the
riskier one: an edge like `cow → beef` is true forever, but `chest at X contains iron` is not — so the
place graph must store *types and relations*, never *contents*. That distinction is the whole discipline.

**Test.** 30 "what do I need for X" questions answered from the graph with zero LLM calls; then the same
30 through a RAG baseline, comparing accuracy and latency.

### Idea 5 — Reliability counters on remembered items, with reset conditions

**What.** Every place, resource spot, container and lesson carries two counters (uses, failures) and a
reset rule ("reset when the agent gets closer or gets a better tool" — altoclef's rule [R]; "invalid only
when failures exceed successes by x₀" — XENON's rule [V]; "rank by the Wilson lower bound" —
Craftonomous [R]). Retrieval ranks by *expected value*, not by similarity.

**Why.** It is the one thing every memory system in table A lacks (section 6.2, gap 4) and the one thing
that makes a memory veto-able by evidence. XENON measured what happens when you prune instead of just
retrieving: iron success 0.10 → 0.36 [V]. altoclef's reset condition is the difference between "this cave
had no iron" and "this cave is bad" [R].

**Main alternative.** One global "memory confidence" score, or trusting the model's own reflection
(Reflexion-style). Reflexion is in PEAM's baseline table and scores 27.3%, below plain MrSteve-style
memory at 33.3% [V] — textual self-reflection was the weakest memory mechanism measured there.

**Cost and risk.** Counters go stale themselves (a spider spawner that was cleared refills). They must be
tied to the same freshness rules as facts, or they become a second source of lies.

**Test.** Log every retrieval and its outcome for 200 decisions; check that ranking by counter beats
ranking by similarity on "did the retrieved thing help".

**A sixth idea, deliberately deferred.** PEAM folds chosen episodes into model weights (69.7% vs 54.5%,
4.6K vs 31.2K tokens [V]). It is the most interesting long-term direction and the least urgent: it needs
a fine-tuning pipeline, a training set Praxion does not have yet, and it moves memory *out* of the part
the user can read — which conflicts with "the user must understand every part". Skills and consolidation
belong to sessions 7/11, not to session 3.

---

## 10. What I would measure first (and what nobody measures)

Ordered by how much a wrong answer would cost us.

1. **Journal size and write rate.** Log a 2-hour session through the real proxy. Bytes per hour, rows per
   second, and whether a per-tick state line is affordable or must be event-only. Unknown today; no paper
   reports it.
2. **Chunk reload behaviour.** When a chunk unloads and reloads, does the server resend full chunk data,
   and does the client therefore learn the new truth for free? [U] — I believe yes, but this must be
   verified in session 2's packet work, because idea 2 depends on it entirely.
3. **Wrong-fact rate in briefings.** The metric nobody reports. Fabricate a scenario: the agent stores 20
   beliefs, the harness changes 10 of them, the agent is asked 20 questions. Count unlabelled false
   answers. Target: 0.
4. **Correction latency.** Ticks from "the change becomes observable" to "the belief is correct". Compare
   read-through vs TTL vs push-invalidation.
5. **Token cost per decision**, with and without memory. MFM and PRO-LONG show memory *reduces* cost
   (PRO-LONG 4.2–5.8× cheaper [V]); if ours increases it, the design is wrong.
6. **A RAG baseline, run honestly.** The earlier proposal suggests this; I agree, with one addition —
   give the RAG baseline the *same* facts, embedded, so the comparison isolates the retrieval mechanism
   rather than the content.

---

## 11. Open questions for the user

1. **Must memory obey fair-play?** If the agent may only know what a player could know, then a remembered
   chest is *always* uncertain, and the design above is forced. If memory may use proxy truth, freshness
   becomes trivial but memory stops being a player-like faculty. This is the biggest fork in the whole
   topic, and it is session 3's question, not mine.
2. **One world or many?** MFM carries memory across runs only because its seed is deterministic [V].
   Praxion's world is persistent, so carry-over is *more* justified — but lessons ("creepers explode near
   you") are true in every world. Which of the two are we building first?
3. **May the agent be unsure out loud?** "I last saw that chest 40 minutes ago" is the honest output of
   idea 2. It is also a behaviour a player rarely shows. Does naturalness allow it?
4. **How much of memory should the user be able to read and edit?** MFM's whole memory is text files a
   person can open [V]; PEAM's is adapter weights nobody can read [V]. Given "the user must master every
   part", this is not a detail — it argues for the filesystem shape.
5. **What is the budget?** Disk and tokens per hour. Without a number, ideas 1 and 3 cannot be judged.

---

## Sources

**Read at paper level this session [V]**

- MFM — Multimodal Filesystem Memory for Long-Horizon Embodied Agents, 2026-10:
  https://arxiv.org/abs/2610.22686 · code MIT: https://github.com/Hither1/MFM
- PRO-LONG — Programmatic Memory Enables Long-Horizon Reasoning, 2026-07:
  https://arxiv.org/abs/2607.20064 · code MIT: https://github.com/alexisfox7/PRO-LONG
- VISTA — A Visual Harness for Reasoning in an Interactive World, 2026-10:
  https://arxiv.org/abs/2610.02200 · code MIT: https://github.com/joshhhhhan/VISTA
- WISE — A Long-Horizon Agent in Minecraft with Why-Which Reasoning, 2026-06:
  https://arxiv.org/abs/2606.12852
- PEAM — Parametric Embodied Agent Memory, 2026-06: https://arxiv.org/abs/2605.27762 · code (no license
  file): https://github.com/Eurekaarrow/PEAM
- XENON — Experience-based Knowledge Correction for Robust Planning in Minecraft, v3 2026-02:
  https://arxiv.org/abs/2505.24157 · code (no license file): https://github.com/sjlee-me/XENON
- MrSteve — What-Where-When Memory, v5: https://arxiv.org/abs/2411.06736 (main text + Appendices M–P)
- WALL-E — World Alignment by Rule Learning: https://arxiv.org/abs/2410.07484
- JARVIS-1: https://arxiv.org/abs/2311.05997
- GITM: https://arxiv.org/abs/2305.17144
- Voyager: https://arxiv.org/abs/2305.16291
- Optimus-1: https://arxiv.org/abs/2408.03615 (Appendix E)
- Optimus-3: https://arxiv.org/abs/2506.10357
- MineNPC-Task: https://arxiv.org/abs/2601.05215
- MC-EvoBench: https://arxiv.org/abs/2610.13829
- MineExplorer (benchmark; frame-buffer and environment-step ablations, no memory store):
  https://arxiv.org/abs/2605.30931
- Plan4MC: https://arxiv.org/abs/2303.16563 · code MIT: https://github.com/PKU-RL/Plan4MC
- DEPS: https://arxiv.org/abs/2302.01560 · code (no license file):
  https://github.com/CraftJarvis/MC-Planner
- STEVE-1: I did not read its own paper; the memory description comes from MrSteve §1 [V].
  Code, no license file: https://github.com/Shalev-Lifshitz/STEVE-1

**Read at code level this session [V]**

- mindcraft `src/agent/memory_bank.js`, `src/agent/history.js`
  (https://github.com/kolbytn/mindcraft, MIT)
- MFM `mfm/memory/store.py`, `mfm/memory/consolidate.py`, `MCU/mcu_worldmodel/episodes.py`,
  `MCU/mcu_worldmodel/memory.py`, `MCU/mcu_worldmodel/previous.py` (MIT)

**Licenses (GitHub API and raw `LICENSE` files, 2026-10-27) [V]**

MIT: mindcraft, Voyager, Plan4MC (`PKU-RL/Plan4MC`), Optimus-3, MFM, PRO-LONG, VISTA.
No license file detected: MrSteve (`frechele/MrSteve`), GITM, JARVIS-1, Optimus-1
(`iLearn-Lab/NeurIPS24-Optimus-1`), STEVE-1 (`Shalev-Lifshitz/STEVE-1`), XENON (`sjlee-me/XENON`, pushed
2026-09-14), PEAM (`Eurekaarrow/PEAM`), DEPS (`CraftJarvis/MC-Planner`), MINDSTORES
(`a37research/mindstores-code`).
Not found: WISE. Papers whose arXiv licence as printed on the page applies: DEPS and PEAM (CC BY 4.0),
MFM, VISTA, PRO-LONG, JARVIS-1, GITM, XENON, MineNPC-Task.

**Repo files used [R]**

- [memory-world-state.md](../memory-world-state.md) — Generative Agents, Optimus-1 ablation numbers,
  MrSteve coverage numbers, Zep, Craftonomous, Baritone, altoclef
- [systems-review.md](../systems-review.md) — Voyager storage/retrieval, mindcraft, altoclef, Baritone
  cache, "done is rarely verified"
- [game-agents.md](../game-agents.md), [README.md](../README.md), [decisions.md](../../decisions.md)

**Not read, and why**

- Optimus-2: goal-observation-action conditioned policy, no memory component in the abstract [V].
- MINDSTORES (2501.19318): store is `(state, task, plan, outcome)` tuples as text embeddings [V]
  (abstract); I did not read its retrieval or ablation, so its row is absent from table B on purpose.
- EvolvingBrain (2610.20786): "Structured Abstractive Memory" + stratified skills [V] (abstract only).
- Echo / Experience Transfer (2604.05533), Project Sid, SIMA 2: memory is not their contribution, and the
  repo already lists them.
- Anything behind a search engine (blogs, vendor docs, Twitter threads): no working search tool this
  session.
