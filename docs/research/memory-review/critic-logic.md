# Review: "Memory and world state" — logic and correctness

Reviewer role: hostile but fair senior reviewer. I did not write
[memory-world-state.md](../memory-world-state.md). Reviewed 2026-10-27.

**Web access: yes.** I fetched the four cited papers and the two SQLite sources and checked the
numbers myself. What I could and could not check is listed in §1.

**Nothing here is a decision.** Every item is a finding plus a proposed fix or a question.
The author and the user decide.

---

## 1. Evidence check: every `[V]` claim I could test

I re-fetched each primary source. Where a number matched, I say "exact". Where the source says
something different from the proposal, I quote it.

| Claim in the proposal | Source I read | Verdict |
|---|---|---|
| Generative Agents: recency decays by 0.995 per **game hour since last access**; importance 1–10 from an LLM; relevance by cosine; all three min-max scaled, all weights 1 | https://arxiv.org/html/2304.03442v2 §4.1 | **Exact.** Paper says "since the memory was last *retrieved*", not "accessed". Same meaning. |
| Reflection runs when summed importance of recent events passes 150; reflections cite their sources | same, §4.2 | **Exact.** "including pointers to the memory objects that were cited". |
| Optimus-1: 851 objects, under 1 MB, queried by topological sort | https://arxiv.org/html/2408.03615v2 §2.1.2, §E.1 | **Exact.** "an HDKG containing 851 objects (nodes) requires less than 1 MB". |
| Optimus-1: diamond 3.25% → 9.59%, iron 36.3% → 53.3%; removing the graph costs ~20%, the pool ~12% | same, Table 3 and §3.3 | **Exact** (36.32 → 53.33). Author-reported, 18-task ablation. |
| MrSteve: entry is what-where-when; forgetting drops the oldest frame of the largest cluster | https://arxiv.org/html/2411.06736v5 §3.1 | **Exact.** `ℓ = (x, y, yaw)`; "the oldest frame is removed from the largest place cluster". |
| MrSteve: retrieval is "top-k event clusters **by text match**, then frames inside them" | same §3.1 | **Wrong.** Retrieval is cosine similarity between a text task embedding and **video** embeddings (MineCLIP), then frames inside clusters. It is cross-modal similarity, not text match. |
| MrSteve: map coverage 84.4% vs 50.8%, revisits 0.38 vs 2.68 | same, Table 1, §4.1 | **Numbers exact, evidence misused.** Table 1 is an *exploration-policy* comparison (count-based high-level + VPT-Nav vs four other policies). It is not a test of Place Event Memory and says nothing about the record shape or about retrieval cost. The paper's memory comparison is in §4.3, and the proposal does not cite it. |
| Zep: four timestamps per fact edge | https://arxiv.org/html/2501.13956v1 §2.2.3 | **Exact.** `t_valid`, `t_invalid`, `t'_created`, `t'_expired`. |
| Zep: a contradicting fact sets the old fact's invalid time; nothing is deleted; hybrid cosine + BM25 + graph walk | same | **Exact.** |
| Zep: "the authors admit DMR is weak: full-conversation 94.4% vs Zep 94.8%" | same, Table 1 and §4.3 | **Half true.** They call LongMemEval "more comprehensive and challenging" than DMR. But the proposal leaves out that same paper's *stronger* result: LongMemEval **71.2% vs 60.2%** for a full-context baseline, at **2.58 s vs 28.9 s** latency (Table 2). See M6. |
| sqlite-vec is pre-v1, warns of breaking changes, Windows supported, Apache-2.0/MIT | https://github.com/asg017/sqlite-vec README + GitHub API | **Mostly exact.** "pre-v1, so expect breaking changes" is verbatim. License is **Apache-2.0**, not "Apache-2.0/MIT" (API reports one licence). Latest release `v0.1.9` (2026-03-31), last push 2026-05-18. |
| FTS5 gives BM25, prefix and phrase queries | https://sqlite.org/fts5.html | **Exact.** `bm25()` is a built-in auxiliary function. |
| "Vector-DB-as-memory critique [S]: similarity ranks by **wording**, so an old fact can outrank the current one" | https://dev.to/mudiazuwa/your-vector-database-is-not-a-memory-system-24l2 | **Wrong summary.** The article says a search returns *both* the old and new statement and the model then blends them ("loves React and Svelte"). Its thesis is "memory is state management, not search". It never mentions wording. See M7. |

Not checked this session: the second dev.to post (the proposal itself says "snippets only"), the
Optimus-1 and MrSteve appendices, the Optimus-1 date/details outside the numbers above.

**Verdict on evidence discipline: the numbers are honest.** Every figure I could test is real and
correctly transcribed, and the two weak spots are marked `[S]` and "author-reported" by the author
himself. That is better than most research notes. The errors are in *what the numbers are used to
prove*, not in the numbers.

---

## 2. The core judgement

The centre of the proposal is right and I would keep it: exact, keyed, typed facts for the world;
embeddings only for fuzzy text; a journal under both; every fact carries its age and its source.

The problem is that the proposal stops one level above the hard part. It names "read-through
refresh" and "the resolver" but never says what a refresh *costs*, who is allowed to see the data,
which clock orders events, or what the LLM actually receives. Those four gaps are where the design
will break in play, and they are not details — they are the design.

The document is also internally inconsistent in three places (§4 table vs §7 question 1, §2 "two
time stamps" vs the four-field record, §5 "do not tune constants" vs the TTLs already asserted in
§4), which matters because the user has to be able to explain it.

---

## 3. Findings, ranked

Severity: **H** = will produce wrong behaviour or block a decision, **M** = a real gap that costs
rework, **L** = clarity or hygiene.

### H1. The freshness table is omniscient, and that contradicts a decision that is already made

The table (§4) says "Blocks in loaded chunks → Authority: Live". That grants the memory layer
sight of every block in every loaded chunk, including blocks behind stone.

But `docs/decisions.md` (2026-10-21) says: "The agent's action and perception APIs expose only what
a player can do and see; test setup goes through a separate, logged harness." [V,
`docs/decisions.md` line 40] And session 2 records: "The client receives every chunk within view
distance, walls or not. The proxy knows more than the player sees; the visibility policy is
session 3." [V, `docs/sessions/02-body-connection.md` lines 31–32]

So the *policy* is open, but the *API constraint* is already decided. The table silently assumes
the opposite. Worse, §7 question 1 then asks "should memory obey the fair-play policy?" — the
document asks the question and pre-answers it in the same page.

Why this is not cosmetic: under a fair-play policy the memory write path needs a visibility test
per observation, and a visibility test is a raycast. That changes the cost of every snapshot in
the design, and it changes what `Observed<T>` means (a block you passed 40 blocks away from is
"unknown", not "air").

**Fix.** Rewrite the table's Authority column as "observable now / observed when in view / never",
and state explicitly which of the three is filtered by the perception policy. Add one line to §4:
"Memory stores only what the perception policy allowed at observation time; the policy itself is
session 3."

**Question for the author:** which is it — chunk-loaded or player-visible? The document has to
pick one before the schema is worth discussing.

### H2. "Read-through refresh" hides a three-way cost split, and it silently turns facts into actions

A read is not a read. Three cases, with very different prices:

1. **Free.** A block inside the current view: the client already holds it. Refresh costs a hash
   lookup.
2. **Cheap but visible.** Turn the camera, walk 3 blocks. Costs time and changes what a player
   would see; must be rate-limited.
3. **Costly and physical.** "What is in chest X" **cannot** be refreshed by reading. The client
   only receives container contents when the container is opened [U — the proposal says the same,
   tagged `[U]`; verify in session 2]. Opening takes walking, aiming, a right-click, and a second
   or two. It is an *action*, not a perception read, and it is one the planner has to choose.

The proposal's rule — "on every read, try to refresh; if you cannot, return the fact with its age"
— collapses all three into one. The consequence is that a remembered chest is *always* returned
stale, forever, because nothing in the design ever decides to go and look.

**Fix.** Make the resolver return one of three things instead of "value + age":

- `fresh(value)` — observed now, inside the policy.
- `stale(value, age, source, verify_goal)` — where `verify_goal` is a concrete, costed action the
  planner may schedule ("open the chest at 214/63/−88, ≈40 s").
- `unknown(reason)` — never observed, out of policy, or observed before the current epoch.

That is also the honest answer to the user's "pull stale data to current instantly": it is instant
when the client already holds the data and impossible otherwise. Say so in the document.

### H3. No epoch model, so `observed_tick` is not a total order and "while I was away" does not exist

The record has `observed_tick` and `observed_wall`. A tick counter belongs to a process. Restart
the process and it starts again at 0. So after a crash:

- session 1 records tick 50,000, "chest has 5 diamonds";
- restart, session 2 records tick 10, "chest has 2 diamonds";
- any rule of the form "higher tick wins" now lets the **old** fact win.

The same missing concept covers the user's own question ("what if the chest is moved while the
agent is away?"). While Praxion is closed, the world still runs if the user plays without it. When
the agent comes back, every fact is from "before", and the document has no way to say *how long*
before, or to mark a global gap. It can only say "40 minutes ago" per fact, one at a time.

**Fix.** Two extra fields, both cheap:

- `epoch` — a monotonically increasing session id, written once per process start. The fact key's
  ordering becomes `(epoch, tick)`. This alone fixes the crash-ordering bug.
- `world_watermark` — one row per epoch: "at epoch N start, the last trusted moment was
  `(epoch_prev, tick, wall)`". The briefing then opens with one line: "3 h 12 min of game time
  passed since I last saw this world; everything below is from before that, unless marked fresh."

That single line is worth more to the LLM than a per-fact age, and it is the answer to (b) that
the proposal is missing.

### H4. Silent divergence has no detector

The proposal says "the proxy sees block-change packets, so a fact anchored at a changed coordinate
is contradicted the moment it happens, with no polling." That is true only for chunks the client
currently holds, and only if every packet was decoded correctly.

Failure case the document does not cover: one unknown packet id (after a version bump), one
dropped TCP segment, one decode bug on a compression boundary, one proxy restart while the game
keeps running. Because the proxy is read-only and forwards bytes untouched [V, `backlog.md`
"Session 2.2 decisions pending"], play continues normally. Nothing throws. The live state quietly
drifts, and every fact written afterwards inherits the error — forever, because the journal is the
source of truth and nobody marks it as suspect.

**Fix.** Perception integrity as a first-class concept:

- per-chunk `last_full_refresh` stamp; a chunk older than N is "suspect" and its blocks read as
  `unknown`, not as air;
- a forced full resync when the client moves into a fresh chunk or after any decode error
  (the analogue of mineflayer's `syncWindow` trick [R, `systems-review.md` §3.1]);
- **fail closed**: on a decode error, mark the epoch dirty, not just log it.

### H5. The user asked for inventory memory; the document deletes it

The user's own words: "we should record places the agent has been, knowledge it gained,
**inventory information** and various other information."

The table says: "Own inventory, health, position → Live only, never stored as memory." That may be
defensible for the *current* inventory. It is not defensible for the user's question, because it
leaves four real questions unanswerable: *"where did my diamonds go", "did I already smelt that
iron", "how many torches did I have before the creeper", "which chest did I put the bucket in"*.

Worse, the container memory then carries a job it cannot do: containers hold items, but the only
way to know an item is *yours* is the inventory stream. With inventory excluded, "where are my
diamonds" becomes "where is the last chest I saw that had diamonds", which is a guess.

**Fix.** Journal inventory as a **delta stream** (the wire already is one: the client predicts a
click and sends `changedSlots`, and the server answers with a full window state when the client is
stale [R, `systems-review.md` §3.1]) and project it into: current inventory (live, rebuildable)
plus item-flow records ("32 iron ore → 32 raw iron at furnace (x,y,z), epoch 7"). That is cheap, it
is exact, and it makes the container facts useful. Say explicitly that the live inventory is a
*cache of the projection*, not "not memory".

### H6. Memory becomes an x-ray machine over time, and the document never notices

Every resource spot is remembered and "re-verified on approach"; nothing is ever forgotten. So
after a few hours the agent holds a map of every ore, chest, spawner and cave it has ever seen,
and it can plan against that map from anywhere. No player has that. It is a stricter form of the
advantage that `decisions.md` refuses when it rejects cheats and demands a natural feel [V,
`docs/decisions.md` lines 8–16, 40].

Note that the proposal's own caveat about Craftonomous — "memory horizon is flat 10 minutes, not
per fact type" — cuts both ways: per-type horizons are better, but only if some horizon is short.
The table instead gives long life to *every* kind: named places "remembered long", structures
"verified on approach", resource spots forever.

**Fix.** Make retention an explicit, per-kind policy with a *forgetting* rule, and make the
accumulated map a named decision for the user, not a side effect: "May Praxion keep a permanent
ore map across sessions? Yes/No/only within a region." Add a rule of the shape "a resource spot
not re-observed for K game days is demoted to a hint, then dropped". This is also the cheapest
naturalness win available.

### H7. The briefing — the thing the user actually asked for — is one arrow

Question (c) is "give the LLMs the correct context". The document's answer is the arrow
`RESOLVER → BRIEFING (Jev/LLM)`. There is no contract: no size budget, no ordering, no dedup, no
policy for what wins when two facts disagree, no statement of what a briefing looks like for Jev
(which answers typed questions, never generates text [V, `research/decision-models.md` lines
14–29]) versus a frontier LLM (which reads prose).

This is the highest-leverage gap, because a perfect store with a bad briefing is a lying agent,
and the document's own success metric ("wrong-fact rate in briefings") cannot even be measured
until the briefing is defined.

**Fix.** Write the briefing as an appendix with a concrete example for each consumer:

- **Jev:** not a briefing at all — a fixed, small state block plus typed questions computed by
  code (distances, counts, "is the remembered chest within reach: yes/no"). Jev is weak at
  numbers and arithmetic [V, same file, line 22], so numbers must arrive pre-computed.
- **LLM:** a ranked, capped list where every line carries `value`, `age`, `source`, and a
  `verify` hint; hard cap on tokens; duplicates collapsed; contradictions shown *as* a
  contradiction, not silently resolved.

**Question:** who owns the briefing — memory or the planner? If memory ranks, memory is making
decisions. The proposal should say.

### H8. Push invalidation feeds the memory from blocks the player could not see

"**Push invalidation:** the proxy sees block-change packets, so a fact anchored at a changed
coordinate is contradicted the moment it happens, with no polling."

The client receives chunk data for every chunk within view distance, walls or not [V, session 2
file]; block updates for those chunks almost certainly follow the same rule [U — not verified this
session; check in 2.2]. So the agent is told, in real time, that a block changed inside a sealed
room it has never entered, or behind a hill it is facing away from. Under a fair-play policy that
is a leak, and it is a *worse* leak than an omniscient read, because a read can be filtered at the
moment of reading ("can I see this block now?") while a delta arrives with no record of what the
player could see. By the time the fact is written, the information needed to filter it has been
thrown away.

Two defensible answers, and the document must pick one:

- **(a) Chunk policy.** Accept that memory is chunk-loaded, not eye-line. Then say so, drop the
  fair-play framing for memory, and accept that the agent knows through walls. Simple, honest,
  and consistent with `decisions.md` only if the *action* API stays fair.
- **(b) Visibility stamps.** Every delta is stored with "observed while visible / while not
  visible". Costs a raycast per changed block, and a rule for deltas that arrive for unloaded
  geometry. Expensive but keeps the naturalness story intact.

**Fix.** Add the choice to §7 as a question for the user, phrased as a fair-play consequence and
not as an implementation detail, because it changes what the agent can know.

### M1. The fact record is under-specified in ways that cause concrete bugs

`{world, kind, key, value, observed_tick, observed_wall, source, confidence, superseded_by, valid_to}`

- **No dimension.** Minecraft coordinates repeat in the Overworld, Nether and End. A chest at
  `100/64/100` in the Overworld collides with one in the Nether. `world` may be intended to mean
  "save file", which does not help. **Fix:** the key is `(world, dimension, kind, key)`.
- **`confidence` is undefined.** A number between 0 and 1? Set by whom, calibrated how? An
  uncalibrated confidence is a lie with two decimal places. **Fix:** delete it. Age plus source
  plus epoch already carry the uncertainty, and both are checkable.
- **`superseded_by` and `valid_to` are the same fact twice.** Pick one; keep the other derivable.
- **`kind` is free text but used as a schema.** The table already implies K kinds with different
  authorities and different TTLs. If `kind` drives behaviour, it is an enum, and the document
  should list it.

### M2. One schema for facts and lessons is a category error

A fact is a claim about the world at a coordinate, refuted by a block change. A lesson is a
sentence about how to play ("don't dig straight down"), refuted by nothing in particular and
ranked by usefulness. They share no key, no write path, no invalidation rule, and no evaluation.

Forcing both through one record plus a free-form `kind` buys nothing and guarantees a pile of
special cases. **Fix:** three tables, one journal: `facts` (keyed, versioned), `episodes` (append,
time-ordered), `lessons` (text + tags + reliability + citations). The proposal's §4 diagram
already implies this split; the schema should follow the diagram.

### M3. Wilson lower bound is the wrong tool for lessons

The document says an LLM-written lesson "earns trust through outcomes" and points at
Craftonomous's Wilson lower bound `[R]`. Wilson is a lower bound on a *success probability* from
Bernoulli trials (systems-review: 5/5 ≈ 0.57, 38/40 ≈ 0.84 [V, `systems-review.md` §3.9]). A
lesson is not a Bernoulli process; there is no "trial" whose outcome tells you whether "creepers
can't break doors" is true.

What you *can* measure is a **prediction**: "this action will succeed", followed by success or
failure. That is a skill's question, not a lesson's.

**Fix.** Keep Wilson for skills (session 11). For lessons, either (a) record the source and let
the planner weigh them, or (b) attach each lesson to the predictions it licenses and score *those*.
Do not attach a success rate to a sentence.

### M4. The Generative Agents formula is ported with two known defects

The document says "start from the Generative Agents formula" and only changes decay and importance
assignment. Two problems, both in the original:

1. **Min-max scaling is per query.** Scores are normalized over the candidate set, so a score of
   0.8 means "top of this list", not "80% good". Two queries are not comparable, and one outlier
   flattens the rest. Fine for a social sim; awkward when you want a threshold ("below this,
   don't tell the LLM").
2. **Recency is measured since *last retrieval*.** A memory that is fetched often never ages. For
   a chat agent that is a feature. For a lesson store it is a trap: a wrong-but-popular lesson
   ("mine gold with a stone pickaxe", which wastes the ore) gets retrieved, used, and therefore
   stays "recent" forever, crowding out the correction. This is exactly the failure the document
   warns about elsewhere ("superseded facts both come back").

**Fix.** Store `observed_at` and `last_used_at` separately, decay on `observed_at` for anything
that can become false, and use "usage" only as a tie-breaker. Drop min-max in favour of a fixed
scale per term, so the threshold in H2/M3 is meaningful.

### M5. Conflict resolution is one case out of five

The document covers "live beats remembered" (refresh + supersession). It does not cover:

- remembered vs remembered (two places both called "home");
- lesson vs observation (a lesson says "this cave has no exit", an observation says otherwise);
- **user statement vs memory** ("no, I moved the chest to the kitchen"). Craftonomous's testimony
  tag is cited but its rule is not adopted. Testimony never becomes sight [R,
  `systems-review.md` §3.9] — so what does the briefing show when the user contradicts memory?
  The design has no rank for the user, and the user is the highest-value source in the system.

**Fix.** Write the precedence order once, as a table, and let it be argued about: live observation
> user testimony (this session) > fresh inference > remembered observation > old user testimony >
> lesson. State what happens when the top two disagree: show both, with a `verify` goal.

### M6. Zep is dismissed using its weakest number

The proposal uses DMR (94.4 vs 94.8, a 0.4-point win) to show Graphiti is not worth adopting. That
is the paper's weakest result. The same paper reports LongMemEval at **71.2% vs 60.2%** with
gpt-4o, and **2.58 s vs 28.9 s** latency [V, §4.3, Table 2]. If the argument is "we don't need it",
the honest reason is not "their benchmark is weak" — it is "their mechanism runs LLM prompts for
entity extraction, entity resolution, fact extraction and fact resolution on every ingested episode
[V, §6.1], and our observations are already structured, so we get invalidation for free". That
reason is in the document and is the right one. The unsupported one should go.

This matters because the user will read this file to understand RAG, and a one-sided number is
seed for a wrong mental model.

### M7. The `[S]` row misstates its source, and the misstatement is technically wrong

The row claims blogs say "similarity ranks by **wording**". The article I fetched says the opposite
kind of thing: a semantic search returns the old and the new statement together and the model
blends them into a false hybrid; memory is state management, not search [V, URL above]. Also,
"ranks by wording" describes **BM25** (a keyword score), not **dense embeddings** (vectors, cosine
similarity). The proposal's own stack uses both, so the row as written teaches the user the wrong
distinction.

**Fix.** Restate the row as: "Similarity has no notion of *now* and no notion of *superseded*; a
stale and a current fact are near-duplicates, so both are returned and the model averages them."
That claim is true, is in the source, and actually supports the split the document wants.

### M8. Per-world storage quietly answers the cross-world question

The document asks "per-world memory only, or also lessons that carry across worlds?" (§7 Q2) but
the storage choice already answers it: "one SQLite file per world". If lessons live in that file,
every fresh seed starts from zero — and `decisions.md` measures success as "a milestone ladder to
the Ender Dragon **over many fresh seeds**" [V, `docs/decisions.md` lines 8–12]. A design that
throws memory away per seed cannot show learning across the ladder, and "writes its own skills
while it plays" becomes "writes skills it then forgets".

**Fix.** Two files, not one: `world_<id>.sqlite` (facts, episodes: local, disposable) and
`knowledge.sqlite` (lessons, skill reliability, static game data pointers: global, kept). The
question in §7 then becomes a policy question ("may knowledge from world A leak into world B?"),
which is answerable, instead of an accidental consequence of a file name.

### M9. The test plan cannot fail

§6 is the right *kind* of test and the wrong *set*:

- it never tests the case that matters most (the chest is moved **while the agent is away and
  cannot see it**), only the visible case;
- no torn-write test (kill the process mid-write; does the projection match the journal?);
- no "two contradicting facts" test with a check on what the *briefing* shows;
- no "unknown vs absent" test (does the agent say "I don't know" or "there is no ore here"?);
- no pass thresholds, so "correct or labelled stale" cannot fail;
- no baseline for the claim that structure beats a vector store — the document promises a
  comparison but the metric is undefined.

**Fix.** One line per test with a number: e.g. "20 scripted moves; 0 wrong-fact briefings; 100% of
unseen-change briefings carry age; median briefing build ≤ 20 ms".

### M10. A local embedding model on the machine that runs the game is an unmeasured risk

"Embeddings: Qwen3-Embedding local, per `embeddings.md`". That file says Apple Silicon latency is
unmeasured and must be measured [V, `research/embeddings.md` line 31]. The game machine is Windows
and must stay smooth ("never freeze while an LLM thinks" [V, `docs/decisions.md` lines 8–12]). An
embedding job that competes for GPU/CPU with the client is a stutter source.

It is also in tension with a decided item: "Models are hosted, not local. Reason: decision
quality." [V, `docs/decisions.md` line 54]. Embeddings are not decision models, so this is not a
violation — but the document should say that out loud, and why the exception is safe, instead of
leaving the reader to notice the contradiction.

**Fix.** State when embedding happens (never on the hot path; batch after a session, or on a
worker thread), measure p95 on the real machine, and keep the `Embedder` interface from
`embeddings.md` so a hosted model can replace it.

### M11. The design depends on "verified observation", which is session 4's undecided topic

"Code writes facts from verified observations only" — but what counts as verified is the open
question of session 4 (evidence levels, sequence acks, `stateId` resync) [V,
`docs/discussion-plan.md` §4]. The memory design therefore has an unmarked dependency on a later
decision. That is allowed, but it must be written down, or the two sessions will disagree later.

**Fix.** Add a "Depends on" line to §4: perception policy (session 3), evidence levels (session 4),
proxy decode decisions (session 2.2). Then a reader can see why the record looks the way it does.

### L1. The tag legend is incomplete

`[inference]` appears in the Optimus-1 caveat but is not in the legend on line 6. Either define it
or use `[U]`. Small, but this document's whole credibility rests on tag discipline, and the user
is being taught to read the tags.

### L2. The document asserts TTLs and then says not to pick TTLs

§5: "Do not tune decay constants before there is an evaluation." §4: "Mobs: short TTL", "Named
places: long", "Until next open". Those are constants. Either mark them "placeholder, to be
measured" or remove them from the table and keep them in a policy file.

### L3. "Two time stamps per fact" is not what the record has

§2 promises two stamps; the record has `observed_tick`, `observed_wall`, plus `valid_to`, plus
`superseded_by` — four ways to say "when". Meanwhile the document praises Zep for exactly this
four-stamp idea. Say four, name each one's job (see H3), or drop two.

### L4. "40 minutes ago" is the wrong unit for a Minecraft agent

A Minecraft day is 20 real minutes [U]. So "I last saw the chest 40 minutes ago" means "two in-game
days ago" — the same words a player would use for "this morning". The user cares about natural
feel; a unit that misleads a human listener also misleads the LLM. Decide which clock the agent
*speaks* in (days and time of day), while keeping two clocks internally.

### L5. Nothing about the user as a memory subject

Session 12 lists "the world, **the user**, events, its own experience" [V,
`docs/discussion-plan.md` §12]. The proposal has places, containers, mobs, lessons — no user
preferences, no standing instructions, no "the user hates when I dig through the floor". Either
scope it out explicitly or add a `preference` kind with `testimony` provenance.

### L6. Memory is an injection surface, and the document does not say so

Place names come from... where? Lessons are LLM-written. Chat is player-written. All of it goes
into a prompt. In singleplayer the risk is low, but the rule is cheap to write: memory text is
data, never instructions, and it is quoted and tagged as such in the briefing. `decision-models.md`
already treats chat as an injection surface for Jev [V, `research/decision-models.md` line 63];
memory deserves the same sentence.

### L7. No volume or time budget anywhere

Two numbers that decide whether this design is buildable:

- A full view of loaded chunks is ~625 chunk columns [U, default render distance] at ~16 KiB per
  packed chunk [R, `systems-review.md` §3.8] ≈ 10 MB of packed blocks. If the journal snapshots a
  chunk on unload, 2,000 unloads cost ≈ 32 MB at 2 bits per block, or ≈ 256 MB at 1 byte per block
  [U, arithmetic]. Affordable, but it has to be a choice: delta-only storage costs almost nothing
  and cannot answer "what was here when I left".
- A naive entity journal at 20 Hz for 10 mobs is 720k events per hour [U, arithmetic].

Neither is measured or even mentioned. The choice between "snapshot on unload" and "delta only"
changes the whole storage story and should be an explicit design line, with an estimate.

---

## 4. The four failure cases, walked through

**A chest is moved while the agent is away.** No packet arrives (the chunk is unloaded, or Praxion
is closed). The fact stays valid, unmarked, and the age is per-fact, so nothing says "3 hours
passed". Fails via H3. H2's `verify_goal` would help only once the agent is near enough to open the
chest; nothing in the design brings it there, or warns the planner that the trip may be wasted.

**Two facts disagree.** If both are live, the live read wins and supersession is clean. If one is
remembered and one is a lesson, or one is the user's word, there is no rule (M5). If both are
remembered at the same key, the newer tick wins — except after a restart, where ticks are
meaningless (H3).

**The proxy misses a packet.** Play continues (read-only forwarding), nothing throws, live state
drifts, and every later fact inherits the error with full confidence. No detector exists (H4).
This is the worst failure class in the document: silent, permanent, and invisible in the test plan.

**After a crash.** Tick ordering breaks (H3); a torn journal entry may leave the projection ahead
of the journal with no checkpoint or rebuild rule; and the "source of truth" claim cannot be
checked because replay cost and rebuild policy are unstated (M9, L7).

---

## 5. What the proposal gets right

- **The central split is correct.** Keyed, typed facts for things that are true or false right now;
  embeddings only for language-shaped things. Independent support: the dev.to argument that memory
  is state management, not search [S, URL above], and Zep's hybrid retrieval beating one method
  [V].
- **"Supersede, never overwrite"** is right, matches Zep's edge invalidation [V], and is what makes
  "knows the past" and "correct now" coexist instead of fighting.
- **"Never send an LLM a fact without its age and source"** is the single best line in the
  document. If the user remembers one rule from this whole topic, this is the one.
- **"Unknown is a value"** (taken from Craftonomous [R]) is the fix for the most common sin in
  agent memory, which is treating absence of data as absence of the thing.
- **The journal-under-a-projection shape** is the right way to get replay and debugging, and it is
  cheap to build on SQLite.
- **The retrieval-by-question-type table** is the most useful page in the file. It answers the
  user's "is RAG suitable?" question in a form he can check line by line, and the answer — mostly
  no, with a narrow yes — is correct for this domain.
- **The numbers are honest.** All four papers' figures check out. That is rare enough to say.

---

## 6. Top five things that must change

1. **Decide perception before memory writes, and rewrite the freshness table in those terms.**
   The table currently assumes the omniscience that `decisions.md` rules out and that §7 Q1 then
   asks about. The same filter must apply to push-invalidated deltas, not only to reads.
   (H1, H8)
2. **Replace "read-through refresh" with three refresh classes and a `verify_goal` in the read
   result.** Container contents cannot be refreshed for free; the resolver must be able to hand
   the planner an action and its cost. This is the mechanism the user's "stale to current"
   requirement actually needs. (H2)
3. **Add epochs and watermarks, and a divergence detector.** `(epoch, tick)` ordering, one
   "time passed since I last saw this world" line per session, per-chunk freshness stamps, and
   fail-closed on decode errors. Without these the journal is not a source of truth, it is a
   rumour. (H3, H4)
4. **Define the briefing contract, and make inventory and item provenance first-class.** The user
   asked for correct context and for inventory. One arrow in a diagram is not an answer, and "never
   stored" contradicts the request. (H7, H5)
5. **Split the stores and write the retention policy.** Facts, episodes, lessons: three schemas.
   Global knowledge outside the per-world file so a fresh seed is not amnesia. Per-kind retention
   with real forgetting, and an explicit user decision on whether a permanent ore map is allowed.
   (M1, M2, M8, H6)

---

## 7. What is still unclear to me (honest unknowns)

- Whether the client receives block updates for chunks it holds but cannot see at the moment of the
  change. I believe yes (chunk data is sent wall-blind [V, session 2 file]), but I did not verify
  the packet stream this session. If yes, the fair-play leak in H8 is real, and it is a bigger
  problem than H1 because a delta cannot be filtered after the fact.
- Whether opening a chest is the *only* way to learn its contents in 26.3 (shulker boxes, hoppers,
  spectators, the "container preview" in some mods). Unverified.
- Whether a lesson can be evaluated at all without turning it into a prediction. I think not, but
  that is a design opinion, not a finding.
- Whether "blocks in unloaded chunks are remembered" survives contact with world height and
  vertical chunks (a player at y=−50 has loaded the same column but not the same section). Unknown,
  and the document's language ("chunk") is ambiguous about 16³ sections versus 16×16 columns.

---

## 8. Glossary (for the user)

- **Fact / belief store:** a table of statements about the world, each with a key ("chest at
  214/63/−88"), a value ("5 diamonds"), and a time.
- **Journal (event log):** an append-only list of things that happened ("block changed", "chest
  opened", "died"). Append-only means you never rewrite old lines.
- **Projection:** a readable table built by replaying the journal. If the table is wrong, you
  rebuild it from the journal instead of arguing with it.
- **Supersession:** marking an old fact as replaced instead of deleting it. You keep history, and
  "now" is the set of facts nothing has replaced.
- **Epoch:** a counter that increases once per process start. Needed because a tick counter resets.
- **Watermark:** a marker meaning "everything before this point is the past; I was not watching
  between here and now".
- **Read-through refresh:** when someone asks for a fact, check the live world first; if that is
  impossible, return the stored value plus its age.
- **BM25:** a keyword-ranking score. Good at names and rare words, blind to meaning.
- **Embedding / cosine:** a vector of numbers representing meaning; cosine similarity measures the
  angle between two vectors. Good at paraphrase, blind to truth and time.
- **Raycast (DDA):** stepping a ray through the voxel grid to find the first solid block. Used to
  decide whether a block is actually visible.
- **TTL:** time to live; how long a fact is allowed to be used before it must be re-checked.
