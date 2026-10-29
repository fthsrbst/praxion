# Should Praxion's memory be searched semantically? Is RAG the right tool?

Research note, 2026-10-27. Feeds session 3 (perception and world model) and session 12
(memory and knowledge). **Nothing here is a decision.** Every item is a proposal with its
reasoning and the main alternative.

Tags: **[V]** I read it in a primary source this session (URL or file path given). **[S]** a
secondary source. **[U]** my own knowledge, not verified here. "Vendor-run" means the people
who sell the system produced the number. A short ID such as `[V: 2410.10813]` is an arXiv ID; the
Sources section at the end gives its URL.

How thorough this is: for the 2026 arXiv papers I read the **abstract** (and, for LongMemEval,
Zep and LoCoMo, parts of the full text). I did not read those papers end to end. Treat their
numbers as strong hints, not settled facts.

---

## 1. Short answer

1. **Most of Praxion's memory is not a search problem.** Position, inventory counts, chest
   contents and "is this block still there" are exact values with a time stamp. You fetch them
   by key. Similarity search is the wrong tool for them. [V: arxiv.org/abs/2610.25373,
   arxiv.org/abs/2610.10265]
2. **Semantic search is a small, real tool.** It fits text that has no key: lessons, episode
   summaries, fuzzy user references ("the cave with the village"), wiki prose.
3. **RAG is a good pattern for static documents and a bad architecture for memory.** RAG
   ("retrieval-augmented generation" = fetch the top-k most similar text chunks and paste them
   into the prompt) has no write rule, no notion of truth, and no way to retire an old fact.
   Memory needs all three. [U]
4. **The hard part the user named — "pull stale data up to current instantly" — is not solved by
   better retrieval.** It is solved by freshness metadata, a read-through check against live
   perception, and push invalidation. Retrieval is the easy half.
5. **Proposal:** three tiers behind one read API — live state, a typed belief store, a text
   store. Start with exact lookups plus BM25 (keyword ranking). Add embeddings only for the
   classes where a test shows they beat keyword search. The main alternative is one big
   embedding index for everything; it is simpler to build and I expect it to be wrong more
   often, but it is cheap enough to test.

---

## 2. Words, in plain language

| Term | What it means here |
|---|---|
| **Embedding** | A list of numbers (a vector) produced by a model for a piece of text. Texts with similar meaning get similar vectors. |
| **Cosine similarity** | A number between -1 and 1 saying how close two vectors point. Used to rank "most similar text". |
| **Vector database / index** | A store that finds the nearest vectors fast. Examples: `sqlite-vec`, Chroma, FAISS. |
| **Semantic search** | Ranking stored text by embedding similarity instead of by shared words. |
| **RAG** | Fetch top-k chunks by similarity, put them in the LLM prompt, let it answer. A read pattern, not a storage design. |
| **BM25** | A classic keyword ranking formula. It rewards rare words and term frequency, and normalises for document length. SQLite's FTS5 has it built in. [V: sqlite.org/fts5.html] |
| **Metadata filter** | A hard condition applied before or beside ranking: `WHERE kind='chest' AND world='w1'`, or a time range, or a bounding box. |
| **Hybrid retrieval** | Run keyword and embedding search, then merge the two ranked lists. |
| **Reranker** | A slower model that re-scores the top ~50 candidates. More accurate per item, far more expensive per query. |
| **Freshness / TTL** | How old a fact is, and how long it may be trusted. TTL = "time to live". |
| **Read-through refresh** | On every read: if the fact is observable right now, re-read it from the world and update it. |
| **Invalidation** | Marking a stored fact as no longer true, keeping the history. Never silently overwriting. |
| **Event journal** | An append-only list of what happened, in order. The source of truth. |
| **Projection** | A table computed from the journal by plain code (e.g. "current chest contents"). Rebuildable at any time. |

---

## 3. The user's requirement, split into jobs

The sentence "memory that changes in real time, knows the past, pulls stale data to current, and
gives the LLM the right context" contains six different jobs. Only one of them (D) is really a
ranking problem; the rest are lookups, bookkeeping and budgeting.

| Job | Example question | Nature of the job | Retrieval involved? |
|---|---|---|---|
| A. Know "now" | "Do I have 3 iron?" | Read live state | No |
| B. Know the world | "Is there a door at 120,64,-30?" | Read live chunks, else a typed fact | Exact key / range |
| C. Know the past | "Why did I die last night?" | Query the journal by time | Filter + maybe keyword |
| D. Know what it learned | "What did I learn about creepers?" | Rank text lessons | Keyword and/or embedding |
| E. Keep facts current | "The chest moved; do I still think it is there?" | Versioning + refresh + invalidation | No (metadata) |
| F. Feed the model | "What should Jev see before this decision?" | Assemble a small, labelled briefing | All of the above, budgeted |

The user's "changes in real time" is job E, and E is a data-model problem. "Gives the LLM the
correct context" is job F, and F is a budgeting problem. Treating the whole thing as one search
index collapses six jobs into one and loses the information each one needs.

---

## 4. Evidence: how memory systems actually score

| Work | What it measured | Result that matters here | Caveat |
|---|---|---|---|
| **LongMemEval** (ICLR 2025) [V: arxiv.org/abs/2410.10813] | 500 questions over long chat histories; five abilities: extraction, multi-session reasoning, temporal reasoning, knowledge update, abstention | Commercial assistants store *facts* and lose badly: GPT-4o offline reading of the full history scored 0.918; ChatGPT's memory feature scored 0.577; Coze 0.330. | Chat assistants, not game agents |
| LongMemEval, design ablations [V: same, §5.2–5.4] | Indexing and query choices | Splitting into rounds beat whole sessions. Summaries and extracted facts used alone as index keys **did not help**; concatenating them with the raw text did (+9.4% recall@k, +5.4% accuracy). Adding a time filter lifted temporal-reasoning recall by 6.8–11.3%. | Author-run, one benchmark |
| LoCoMo [V: arxiv.org/abs/2402.17753] | 300-turn dialogues, ~9k tokens, up to 35 sessions | Long-context and RAG both help (22–66% improvement) but still trail humans by 56%, and temporal reasoning by 73%. | Synthetic dialogues |
| **Zep / Graphiti** (vendor-run) [V: arxiv.org/abs/2501.13956] | LongMemEval | 71.2% vs 60.2% for full-context, with 1.6k vs 115k prompt tokens and 2.6 s vs 28.9 s latency. Its DMR score differs from the baseline by 1.4 points on a benchmark the authors themselves call weak. | Written by Zep's employees |
| **Mem0** (vendor-run) [V: arxiv.org/abs/2504.19413] | LoCoMo | Claims 26% relative gain over OpenAI's memory, 91% lower p95 latency, >90% token saving vs full context. | Vendor paper, LLM-as-judge |
| **Raw-RAG** (2026) [V: arxiv.org/abs/2610.30755] | Four memory benchmarks incl. LoCoMo, LongMemEval | A naive baseline — dense retrieval + rerank over the **raw** history, no memory construction — ranked first in 6 of 7 benchmark/model settings, beating the best agentic memory by 0.13–5.53 points. | Abstracts only; conversation tasks |
| **MemDerive** (2026) [V: arxiv.org/abs/2610.25373] | 1,503 queries over 4,692 events; derived state with gold derivation graphs | "Every memory system falls behind simple BM25 retrieval" on derived state. The same answer model reaches 96.0% when handed the supporting events. Failures are mostly *not gathered*, and systems rarely propagate implicit changes. | Fictional histories, English |
| **Stale, Misattributed, or Late** (2026) [V: arxiv.org/abs/2610.10265] | Personal-fact memory, measured *before* generation | Without update resolution, **70.3% of prompts expose a superseded value**. Once the active store is correct, "participant-aware BM25 is equivalent to the reference ranker within a 0.02 margin". Prompt prefill, not ranking, dominates latency. | Controlled benchmark |
| **Harness the Memory** (2026) [V: arxiv.org/abs/2608.15008] | Dense, sparse, text, graph, hierarchical and parametric substrates across 4 suites | "No single substrate consistently dominates." Excessive retrieval *hurts* sequential decision-making by pulling attention away from action-critical context. | Abstract only |

Three readings of this table:

- **Simple retrieval over raw records is embarrassingly competitive.** Two 2026 papers put a
  plain BM25 or plain dense baseline at or above purpose-built memory systems.
  [V: 2610.25373, 2610.30755]
- **Most measured failures are not ranking failures.** They are missing intermediate state,
  un-propagated change, and a stale value that was still in the store. [V: 2610.25373,
  2610.10265]
- **Relevance is not freshness.** Similarity has no time axis, so the newest and the oldest
  version of a fact can score almost the same. [U]

### 4.1 What the best-known designs actually do

- **MemGPT / Letta** [V: arxiv.org/abs/2310.08560]: treats the context window like RAM and paged
  memory like disk; the model itself decides what to move between tiers, using interrupts.
  Lesson: tiers are the right shape. Weakness for us: the model manages its own memory, which is
  slow and can be wrong.
- **MemoryBank** [V: arxiv.org/abs/2305.10250]: forgets and reinforces memories by elapsed time and
  significance, following the Ebbinghaus forgetting curve. Lesson: age should matter. Weakness:
  one global curve, tuned by intuition, with no notion of a fact becoming false.
- **A-MEM** [V: arxiv.org/abs/2502.12110]: Zettelkasten-style notes with keywords, tags and links
  that evolve as new notes arrive. Lesson: linking related notes helps recall. Weakness: an LLM
  call per write to build the links.
- **Mem0** [V: arxiv.org/abs/2504.19413]: extract salient facts, consolidate them against existing
  memory, optionally add a graph layer. Lesson: consolidation matters. Weakness: an LLM per
  ingest, and every merge is a chance to join two things that are not the same — the failure mode
  measured in [V: arxiv.org/abs/2610.10265].
- **Zep / Graphiti** [V: arxiv.org/abs/2501.13956]: bi-temporal facts with explicit invalidation,
  hybrid search, reranking. Lesson: this is the closest design to our problem. Weakness: LLM
  extraction per episode, and a graph store to operate.
- **Generative Agents** [V: docs/research/memory-world-state.md]: one memory stream; score =
  recency (0.995 per game hour) + importance + relevance. Lesson: a three-term score is enough to
  start.
- What none of them do: treat "the world changed while I was not looking" as a first-class case.
  Zep's invalidation is the nearest; the 2026 PACMI paper makes cascading invalidation its whole
  subject [V: arxiv.org/abs/2610.05732]. For Praxion this case is the centre, not an edge case.

---

## 5. Where embeddings fail, with a Praxion example each

### 5.1 Numbers and coordinates

Embedding models are weak on magnitudes. A study of scalar knowledge found language models hold
"a significant amount" of magnitude information "but are short of the capability required for
general common-sense reasoning", and that **canonicalising numbers** measurably changed results.
[V: arxiv.org/abs/2010.05345]

Praxion example: "which chest has more iron?" and "nearest iron ore within 32 blocks". These are
a comparison and a range query. A cosine score cannot answer either. An embedding might return
"the chest with 30 iron" when you asked about 3.

Consequence: coordinates and counts never go through similarity. They go through `WHERE`, `>`,
`<`, and a spatial index. SQLite's R*Tree does exactly rectangle and range lookup. [V:
sqlite.org/rtree.html]

### 5.2 Negation

BERT-class models show "clear insensitivity to the contextual impacts of negation". [V:
arxiv.org/abs/1907.13528]

Praxion example: "don't break the door" and "break the door" can look almost the same to an
embedding model. [V: arxiv.org/abs/1907.13528] A user rule stored as text and retrieved by meaning
can flip its own meaning. Consequence: rules and commands are stored as structured records
(`action=forbid`, `target=door`, `scope=base`), with the original sentence kept as a comment.

### 5.3 Recency and change

This is the decisive one for the user's requirement.

- Without update resolution, 70.3% of prompts expose a superseded value. [V:
  arxiv.org/abs/2610.10265]
- Adding *temporal labels* to otherwise identical facts raised key-entity hit rate by 17.1–56.0
  percentage points, and cut evidence size by 49–57% versus session-aware RAG. [V:
  arxiv.org/abs/2610.21900]
- LongMemEval's time filter improved temporal recall by 6.8–11.3%. [V:
  arxiv.org/abs/2410.10813]
- Zep's whole design is bi-temporal: when a fact was true, and when the system learned/expired
  it. Contradiction sets an `invalid` time; nothing is deleted. [V: arxiv.org/abs/2501.13956]

The lesson is not "use a graph". It is: **the reader must be told which version is current.**
Similarity cannot tell you that. A timestamp can.

### 5.4 Exact identifiers, rare tokens, conjunctions

- BM25 is a robust zero-shot baseline; dense retrievers are efficient but "often underperform"
  out of domain, while reranking and late interaction are the most accurate and the most
  expensive. [V: arxiv.org/abs/2104.08663, BEIR, 18 datasets]
- Single-vector embeddings have a provable expressiveness limit: some ranking problems need a
  representation of exponential size if you keep one vector per text, while multi-vector
  representations stay polynomial. [V: arxiv.org/abs/2608.21494, abstract]

Praxion example: "the chest at 118,63,-27 with the `minecraft:iron_ingot` stack". Both halves
are exact identifiers. A filter finds it in about a millisecond on a table of thousands of rows
[U]; an embedding may return the *other* iron chest, because that is what "similar" means.

### 5.5 Retrieval can make decisions worse

Excessive retrieval harms sequential decision-making by shifting attention away from the
action-critical context. [V: arxiv.org/abs/2608.15008] And relevant information placed in the
middle of a long context is used worse than the same information at the start or end. [V:
arxiv.org/abs/2307.03172]

Praxion example: the agent is about to place a block while a creeper approaches. Eight "related
memories" about caving crowd out the two lines that matter: creeper 3 blocks away, 4 s fuse.

### 5.6 Cost of building memory

- Zep's ingest runs LLM extraction per episode; that is the price of its quality. [V:
  arxiv.org/abs/2501.13956]
- On short records, "every memory pipeline costs at least twice full re-reading"; memory only
  pays off after 2–14 uses once records reach ~10k tokens. [V: arxiv.org/abs/2610.03902]
- A small 2026 study found Mem0 beat full context only with one weaker model, and its build cost
  alone exceeded a whole full-context rollout. [V: arxiv.org/abs/2610.29823] One conversation,
  one run per cell — weak evidence, but it points the right way.
- Writing embeddings has its own cost: one model call per new text memory, and a one-off
  re-embed of the whole corpus when the model changes. Journal rows and typed facts need neither.
  [U]

Praxion relevance: the agent can often *re-read* the world (the chunk is loaded, the chest is
openable). When re-reading is cheap and exact, storing a derived copy adds cost and a new way to
be wrong.

---

## 6. Where embeddings earn their place

| Case | Why similarity helps | Evidence |
|---|---|---|
| Lessons in the agent's own words | Two lessons mean the same thing with different words; no shared key | BM25 is the strong baseline and dense is not reliably better out of domain [V: 2104.08663]; the paraphrase gain is unmeasured for our text [missing] |
| Fuzzy user reference | "the cave with the village" has no canonical key; the agent must match it to a stored place description | No game-specific measurement found [missing]; rationale only [U] |
| Episode recall by gist | "that time I fell in lava" | Embeddings are the usual tool for this [U]; LongMemEval shows raw-text retrieval is strong but not that embeddings beat BM25 [V: 2410.10813] |
| Skill candidate ranking | Thousands of skills, short descriptions | Voyager retrieves top-5 by description embedding — and ignores preconditions and success rate, which is a known weakness [V: docs/research/systems-review.md] |
| Wiki prose | Document-shaped, static, paraphrased questions | RAG's home ground [U]; licensing is a separate problem (Minecraft Wiki is CC BY-NC-SA) [V: docs/discussion-plan.md] |

---

## 7. Hybrid retrieval: what is actually proven

1. **Keyword first is not a compromise; it is the measured baseline.** BM25 is robust across 18
   datasets; dense models are not reliably better out of domain. [V: arxiv.org/abs/2104.08663]
2. **Fusion is not free.** Reciprocal Rank Fusion (RRF) is sensitive to its parameters;
   a tuned convex combination of the two scores beat RRF in-domain and out-of-domain, and needed
   only a small tuning set. [V: arxiv.org/abs/2210.11934] Do not assume RRF "just works".
3. **Hard filters beat clever ranking.** LongMemEval's biggest wins came from narrowing the
   search space (time range, finer value granularity), not from a better similarity function.
   [V: arxiv.org/abs/2410.10813]
4. **Rerankers are the accuracy ceiling and the cost floor.** Reranking/late interaction are the
   best zero-shot performers "at high computational costs". [V: arxiv.org/abs/2104.08663]
5. **Keep the raw text.** Compressed keys alone did not help; raw-plus-expansion did. Facts
   extracted by an LLM lose information that the reader later needs. [V:
   arxiv.org/abs/2410.10813, arxiv.org/abs/2610.30755]

---

## 8. Decision table

Recommendation per memory kind. "Exact" = key or range lookup in SQL. "BM25" = keyword ranking.
"Dense" = embedding similarity. "Hybrid" = filter → BM25 + dense → fuse → optional rerank.

| # | Kind of memory | Example question | Recommended mechanism | Evidence | Confidence |
|---|---|---|---|---|---|
| 1 | Own position, health, hunger, inventory counts | "Do I have 3 iron?" | **Exact, live only.** Never stored as memory | Live perception is exact by design [V: docs/decisions.md]; a stored copy goes stale without update resolution [V: 2610.10265] | High |
| 2 | Blocks in loaded chunks | "Is there a door at 120,64,-30?" | **Exact, live chunk cache** | Loaded-chunk data is exact and cheap to read [V: docs/decisions.md]; no ranking involved [U] | High |
| 3 | Blocks in unloaded chunks, builds | "Where is my base wall?" | **Exact + spatial index + age** (R*Tree or a grid) | R*Tree does range queries [V: sqlite.org/rtree.html]; derived state needs rules and dependencies [V: 2610.25373] | High |
| 4 | Container contents | "What is in the chest by the base?" | **Exact key**, one row per observed change, with `observed_at` | Container data arrives only when opened [U]; stale exposure is the top failure [V: 2610.10265] | Medium |
| 5 | Resource spots (ore, trees) | "Where was that iron vein?" | **Exact + spatial**; mark depleted on mining; re-verify on approach | Container/resource caches work; reset conditions are required [V: docs/research/memory-world-state.md] | High |
| 6 | Named places | "Go home" | **Exact name → coordinate**, verify the anchor block on arrival | Place memory with what/where/when works [V: docs/research/memory-world-state.md] | High |
| 7 | Episodes, deaths, conversations | "Why did I die last night?" | **Time-range + kind filter over the journal**; keyword as tie-break | Time filters lifted recall 6.8–11.3% [V: 2410.10813]; raw records beat derived ones [V: 2610.30755] | High |
| 8 | Lessons ("creepers blow up doors") | "What did I learn about caving?" | **Filter, then BM25, then optional dense, optional rerank**; score = relevance × recency × importance | Three-term score from Generative Agents [V: docs/research/memory-world-state.md]; BM25 strong baseline [V: 2104.08663]; no substrate dominates [V: 2608.15008] | Medium |
| 9 | Fuzzy user references | "the cave with the village" | **Dense over place/episode descriptions**, then confirm by position or by asking | No game measurement [missing]; embeddings are the only cheap way to match free text [U] | Low |
| 10 | User commands, preferences, rules | "Always keep 4 torches" | **Exact, verbatim, versioned**; latest active + full history | Knowledge-update is a named LongMemEval ability [V: 2410.10813]; merge errors keep stale values [V: 2610.10265] | High |
| 11 | Recipes, block/item data | "What makes a piston?" | **Structured tables/graph from game data**, queried by code | Graph memory of 851 objects under 1 MB was effective [V: docs/research/memory-world-state.md]; "knowledge comes from data" [V: docs/decisions.md] | High |
| 12 | Skills | "Which skill fetches wood?" | **Registry + structured preconditions + reliability**, dense only for candidate ranking | Filters-then-embedding-then-rerank [V: docs/research/embeddings.md]; Voyager's embedding-top-5 ignores preconditions [V: docs/research/systems-review.md] | Medium |
| 13 | Wiki prose / unusual game facts | "How do I cure a zombie villager?" | **Classic RAG** over a wiki index | RAG fits static document text [U]; licence CC BY-NC-SA [V: docs/discussion-plan.md] | Medium |

Read the table by its shape: **rows 1–7 and 10–11 are 9 of 13 rows, and none of them wants an
embedding.** Only rows 8, 9, 12 and 13 are text-similarity problems, and only row 9 depends on it
completely.

---

## 9. Proposed shape, and the alternative

```
 live perception ──► LIVE STATE (RAM)  ── always current, never "retrieved"
        │
        ▼
 EVENT JOURNAL (append-only: observations, actions, chat, deaths, skill results)
        │  code projects it downward
        ├──► BELIEF STORE (typed rows: key, value, observed_at, source, valid_to)
        │      + spatial index · + freshness labels · + invalidation
        └──► TEXT STORE (lessons, episode summaries, rules)
               + FTS5/BM25 · + optional vectors · + hybrid ranking

 read path:  question ─► resolve (live? belief+age? text search?) ─► BRIEFING (Jev / LLM)
```

**Proposal (the user decides).**

1. **One journal, three projections.** The journal is the only source of truth; every other
   store can be rebuilt from it. This gives "knows the past", "current now" and replay for
   debugging from one mechanism.
2. **Facts are typed rows with two time stamps** — when it was true in the world, and when we
   learned it — plus source, confidence and `valid_to`. Supersede; never overwrite.
3. **Freshness is enforced on read, not on write.** The resolver tries live perception first,
   then a belief with its age, then text. Every value reaching a model carries `source` and
   `age`. "Unknown" is a legal answer.
4. **BM25 before embeddings.** Ship exact + keyword retrieval first. Put embeddings behind one
   `Embedder` interface, so they can be switched on, off or swapped without touching callers.
5. **A small typed query API for the models** (`get_fact`, `find_near`, `speak_of`, `search_notes`)
   plus a pre-assembled briefing for the fast path. Not "here are 10 chunks".

**Worked example (the "stale to current" requirement).** At 10:00 the agent opens a chest at
(118, 63, -27) and sees 12 iron ingots. One row is written: `key=chest(118,63,-27)`,
`value={iron:12}`, `observed_at=10:00`, `source=sight`. At 10:40 the agent is asked "where is the
iron?". The chest is 200 blocks away and its chunk is not loaded, so the resolver cannot re-read
it. It answers with the belief, labelled: "12 iron in the chest at 118,63,-27, last seen 40
minutes ago". At 10:41 the agent walks past that chunk. The chunk loads, the proxy shows the block
entity, and the row is superseded with `observed_at=10:41`. If instead the chest was broken while
the agent was away, the block-change packet (if the proxy sees it [U: verify in session 2]) marks
the old row `valid_to=10:31` at the moment it happens. No similarity search anywhere in this
example; the whole requirement is met by keys, timestamps and one refresh rule.

**Main alternative:** one embedding index over everything written as text, top-k into the
prompt, no typed store. It is less code, and Raw-RAG shows it is stronger than expected on
conversation QA. [V: arxiv.org/abs/2610.30755] I expect it to fail here for a specific reason:
the questions Praxion asks most are exact and time-bound ("is the chest still there", "do I have
3 iron"), and those are the classes where similarity is provably the wrong operation. The
experiment in §11 is designed to falsify my expectation.

**Second alternative:** a temporal knowledge graph (Zep/Graphiti style). It handles invalidation
well, but its ingest needs an LLM per episode, and our observations are already structured — we
would pay to re-derive what the proxy already tells us exactly. [V: arxiv.org/abs/2501.13956]

Implementation note: tiers 2 and 3 can both live in **one SQLite file per world** — typed tables
and an R*Tree index for beliefs, FTS5 for keyword search. That is one dependency, no server.
[V: sqlite.org/fts5.html, sqlite.org/rtree.html]

### 9.1 How this differs from `memory-world-state.md`

The earlier note reached the same core conclusion (most memory is typed, embeddings are narrow,
two time stamps plus a read-through refresh). The differences are small but real:

| Earlier note | This note |
|---|---|
| "An append-only event journal is the source of truth" — one sentence | Same, plus the measured support: raw records beat derived stores in two 2026 benchmarks [V: 2610.30755, 2610.25373] |
| Lesson scoring from Generative Agents (recency + importance + relevance) | Same baseline, but BM25 is the default ranker and embeddings must beat it in the test of §11 |
| Retrieval table by question type | Same idea, but each row now carries its evidence and a confidence level |
| Not discussed | Cost and latency of the read path (§10), and the fact that prefill, not ranking, dominates [V: 2610.10265] |
| Not discussed | A concrete falsification experiment (§11) |

If the earlier note was hard to read, start with §2 (words) and §3 (the six jobs); §8 is the
compressed answer.

---

## 10. Cost and latency, in numbers we have

| Item | Number | Source |
|---|---|---|
| Jev decision budget | 130–300 ms per decision, ~$0.0003 | [V: docs/research/README.md] |
| Hosted text embedding (Gemini Embedding 2) | $0.20 per 1M tokens | [V: docs/research/embeddings.md] |
| Local embedding (Qwen3-Embedding 0.6B) | free, CPU/GPU time only; **latency on our Windows target unknown** | [V: docs/research/embeddings.md] + [U] |
| BM25 in SQLite FTS5 | no extra service; k1=1.2, b=0.75 hard-coded | [V: sqlite.org/fts5.html] |
| Prompt size, full history vs structured | 115k vs 1.6k tokens (72×) per question | [V: arxiv.org/abs/2501.13956] |
| Latency, same comparison | 28.9 s vs 2.6 s | [V: arxiv.org/abs/2501.13956] |
| Where the latency goes | prompt prefill, not ranking | [V: arxiv.org/abs/2610.10265] |

Derived point (arithmetic, [U]): at one decision every 10 s, 115k tokens per decision is ~41M
input tokens per hour; 1.6k is ~0.6M. Context assembly matters more for cost than the choice of
retrieval algorithm. This is why the briefing, not the index, is the design centre.

What we cannot say yet: the p95 latency of a local embedding query on the Windows machine, and
the price of the LLM we will actually use. Both are measurable in an afternoon; both are
session 8/15 material.

---

## 11. What evidence is missing, and the one experiment that would settle it

**Missing:**

1. **No public benchmark for game-world memory retrieval.** The closest is MineNPC-Task, which
   instruments memory reads and writes in Minecraft but does not compare retrieval methods. [V:
   arxiv.org/abs/2601.05215]
2. **No measurement of embeddings vs BM25 on short, first-person lesson text.** The retrieval
   benchmarks I read are web, biomedical and QA corpora; none use short game notes.
   [V: arxiv.org/abs/2104.08663]
3. **No number for how often a user's phrasing is a paraphrase** rather than a name the agent
   already stored. Unknown until we log real commands.
4. **No local latency figure** for embeddings on the target machine.
5. **No evidence that reranking helps at 1k–10k documents**, which is our scale. BEIR's rerankers
   were measured on large corpora. [V: arxiv.org/abs/2104.08663]

**The experiment (proposal, about one day):**

*Setup.* One scripted world, 2–3 hours of play with the journal on. Then 250 questions in six
classes: (1) own state, (2) place/coordinate, (3) container, (4) episode, (5) lesson, (6) fuzzy
reference. Every question gets a gold answer **and** the gold evidence rows. 20% of questions are
"changed" questions whose correct answer differs from the old stored one. 10% are unanswerable,
where "unknown" is the only correct answer.

*Configurations.* A: exact only. B: exact + BM25. C: exact + dense (Qwen3-Embedding 0.6B, local).
D: exact + BM25 + dense, convex-combination fusion tuned on 30 held-out questions (RRF is not
assumed to work; a tuned combination beat it in the one study I found) [V: arxiv.org/abs/2210.11934].
E: D + Qwen3-Reranker 0.6B on the top 20.

*Metrics.* Recall@5 for gold evidence. Answer accuracy with the real reader model (LLM judge,
50 answers hand-checked). **Wrong-fact rate**: answered with a superseded value. Abstention
correctness. p50/p95 latency on the target machine. Input tokens per briefing and cost per hour.

*Decision rule (a proposal; the user sets the real one).* Classes 1–3 (own state, place, container)
must be 100% correct with a 0% wrong-fact rate, or the design is broken regardless of retrieval
quality. Keep dense retrieval only if it beats BM25 by at least 5 points on classes 5–6 without
raising the wrong-fact rate. If p95 latency does not fit the Jev path, embeddings run only on the
slow LLM path.

*Why this settles it.* It measures the exact classes in the decision table, on our data, with the
failure mode that worries us (stale answers) as a first-class metric. If dense wins class 6 and
ties class 5, the answer is "embeddings for place references only". If it wins neither, the answer
is "BM25 and exact lookups", and we saved a dependency.

A cheaper first step (half a day): 60 questions, configs B vs C only, no reranker.

---

## 12. Questions for the user

1. Is a remembered fact allowed to be wrong-but-labelled, or must the agent say "unknown" more
   often? (This sets how aggressive read-through refresh must be.)
2. Do lessons carry across worlds, or is memory per-world? (Cross-world lessons are the one place
   a growing text corpus is guaranteed, which changes the retrieval calculus.)
3. May an LLM write text memory, or only code? (LLM-written lessons are exactly the corpus where
   embeddings might pay off.)
4. Should the user be able to browse and edit memory? A viewer makes the system explainable and
   doubles as the debug tool for the experiment above.
5. Is "the agent remembers only what a player could know" a rule? If yes, memory must obey the
   same provenance policy as perception.

---

## Sources

Primary, read this session:

- LongMemEval — https://arxiv.org/abs/2410.10813 (abstract + §5 full text)
- LoCoMo — https://arxiv.org/abs/2402.17753 (abstract + results text)
- Zep — https://arxiv.org/abs/2501.13956 (abstract, §2.2.3, §4.3.2, Table 2/3)
- Mem0 — https://arxiv.org/abs/2504.19413 (abstract only)
- MemGPT — https://arxiv.org/abs/2310.08560 (abstract only)
- MemoryBank — https://arxiv.org/abs/2305.10250 (abstract only)
- A-MEM — https://arxiv.org/abs/2502.12110 (abstract only)
- JARVIS-1 — https://arxiv.org/abs/2311.05997 (abstract only)
- BEIR — https://arxiv.org/abs/2104.08663 (abstract only)
- Fusion functions for hybrid retrieval — https://arxiv.org/abs/2210.11934 (abstract only)
- Lost in the Middle — https://arxiv.org/abs/2307.03172 (abstract only)
- What BERT is not (negation) — https://arxiv.org/abs/1907.13528 (abstract only)
- Do Language Embeddings Capture Scales? — https://arxiv.org/abs/2010.05345 (abstract only)
- Raw-RAG — https://arxiv.org/abs/2610.30755 (abstract only)
- MemoryTax — https://arxiv.org/abs/2610.29823 (abstract only)
- MemDerive — https://arxiv.org/abs/2610.25373 (abstract only)
- Stale, Misattributed, or Late — https://arxiv.org/abs/2610.10265 (abstract only)
- When Evidence Changes (memory repair vs re-reading) — https://arxiv.org/abs/2610.03902
- PACMI (cascading invalidation) — https://arxiv.org/abs/2610.05732
- TEM (temporal state tracking) — https://arxiv.org/abs/2610.21900
- Harness the Memory (substrates) — https://arxiv.org/abs/2608.15008
- Memory as Code — https://arxiv.org/abs/2610.27700
- PolyMemDB — https://arxiv.org/abs/2608.25577
- Retrieval Needs Multivectors (single-vector limits) — https://arxiv.org/abs/2608.21494
- Persistent Memory in Multi-Agent Inference (cost/null result) — https://arxiv.org/abs/2610.07782
- PEAM (parametric memory in Minecraft) — https://arxiv.org/abs/2605.27762
- MineNPC-Task — https://arxiv.org/abs/2601.05215
- XENON (experience-based knowledge correction) — https://arxiv.org/abs/2505.24157
- SQLite FTS5 (bm25) — https://sqlite.org/fts5.html
- SQLite R*Tree — https://sqlite.org/rtree.html

Repo files read this session: `AGENTS.md`, `docs/decisions.md`,
`docs/research/memory-world-state.md`, `docs/research/systems-review.md`,
`docs/research/embeddings.md`, `docs/research/README.md`, `docs/discussion-plan.md`.

Not read, worth a second pass: full texts of the 2026 memory papers; Graphiti's ingestion cost
documentation; the full DMR critique in Zep §4.2; Mem0's code; any measurement of embedding
behaviour on numeric literals and negation in *retrieval* (as opposed to language-model
benchmarks).
