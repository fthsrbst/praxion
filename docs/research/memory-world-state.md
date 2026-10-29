# Memory and world state: how to remember without lying

Research for session 3 ("Perception and world model", open question: where and how is world
memory stored, and how does remembered information age). 2026-10-27. Nothing here is a decision.

Evidence tags: **[V]** read from the primary source in this session, **[S]** secondary or
blog-level source, **[R]** repo review already in `systems-review.md`, **[U]** my own knowledge,
not verified here, check before relying on it.

## 1. The question

The user wants memory that (a) keeps what the agent learned in the past (places visited, items
gained, inventories, lessons), (b) can pull stale information up to date at any moment, and (c)
gives the LLMs and Jev a correct context. Sub-question: is this semantic search / RAG, or something else?

## 2. Short answer

- **Most of this is not RAG.** Places, coordinates, container contents and counts are exact,
  keyed and time-sensitive. They need typed lookups with freshness metadata, not similarity search.
- **Embeddings still have a narrow job:** lessons, episode recall by meaning, fuzzy user references
  ("the cave where we found the village"), and wiki-style knowledge.
- **The core idea is two time stamps per fact plus a read-through check:** every remembered fact
  carries when it was observed and where it came from; before a fact reaches an LLM, a resolver
  tries to refresh it from live perception; if it cannot, the fact goes out labelled with its age.
- **An append-only event journal is the source of truth;** the fact store is a projection of it.
  That gives "knows the past" and "current now" from one mechanism and makes the memory replayable for debugging.

## 3. What existing systems do

| System | Memory design | What it teaches us | Caveat |
|---|---|---|---|
| Generative Agents (2023) [V] | One stream of natural-language memories. Retrieval score = recency (exponential decay 0.995 per game hour since last access) + importance (LLM-rated 1–10 at write time) + relevance (embedding cosine); each min-max scaled, weights all 1. Reflection runs when summed importance of recent events passes 150 and writes higher-level insights that cite their sources. | A simple three-term score works; reflections as cited, higher-level memories. | Text-only social sim; no notion of a fact becoming false. |
| Optimus-1 (Minecraft) [V] | Two stores. Knowledge graph: objects as nodes, edges to what they craft; 851 objects, under 1 MB; queried by topological sort. Experience pool: filtered frames + sub-goal + plan, saved for successes and failures. | Craft knowledge is a graph, not a vector index. Failures are worth storing: retrieving both success and failure cases lifted diamond success from 3.25% (no retrieval) to 9.59%, iron from 36.3% to 53.3%. Removing the graph cost about 20% on average, removing the pool about 12% (author-reported). | Frames embedded because the agent sees pixels. We read exact data from the protocol, so we do not need frame embeddings for the "what". [inference] |
| MrSteve (Minecraft) [V] | Place Event Memory: positions cluster into places, each place into event clusters; every entry is (what: video embedding, where: x/y/yaw, when: timestamp). Retrieval is hierarchical: top-k event clusters by text match, then frames inside them. Forgetting: drop the oldest frame of the largest cluster. | The what-where-when triple is the right record shape. Hierarchical retrieval is cheaper than scoring every entry. Map coverage 84.4% vs 50.8% for a baseline; revisits 0.38 vs 2.68. | One simulator with exact positions; noisy positions untested. |
| Zep / Graphiti [V] | Temporal knowledge graph: raw episodes (kept losslessly), extracted entities/facts, community summaries. Four timestamps per fact edge (when it was true, when it was ingested/expired). A new fact that contradicts an old one sets the old one's `invalid` time; nothing is deleted. Retrieval = cosine + BM25 + graph walk, reranked. | Invalidate, never overwrite. Keep two time axes. Hybrid retrieval beats one method. | An LLM extracts and detects contradictions, which is slow and costly; we do not need that because our observations are already structured. The authors admit their benchmark (DMR) is weak: full-conversation baseline scored 94.4% vs Zep 94.8%. |
| Craftonomous [R] | Every fact is `Observed<T>` with provenance (sight, hearing, memory-with-age, inference, testimony, privileged). Fair-play profile: 10-minute memory; out of range returns "unknown", not "absent". | Provenance and age travel with the fact; "unknown" is a value. | Memory horizon is a flat 10 minutes, not per fact type. |
| altoclef [R] | Container cache of chests opened before; `ResourceTask` checks dropped items, then known chests, then mining. Blacklist per target that resets when the bot gets closer or gets a better tool. | Remembered containers are a resource source; failure memory needs reset conditions. | Cache is in-process, lost on restart. |
| Vector-DB-as-memory critique [S] | Blog posts agree: similarity ranks by wording, so an old fact can outrank the current one; superseded facts both come back; "all facts about X" is a filter, not a similarity query; small hot state wants a key lookup. RAG stays right for document-shaped, static knowledge. | Supports the split in section 4. | Secondary sources, no game focus. |

## 4. Proposed shape (for discussion)

```
 network proxy ──► perception ──► LIVE STATE  (RAM; authoritative for "now")
                       │
                       ▼
                 EVENT JOURNAL (append-only; observations, skill results, chat, deaths)
                       │ projection (deterministic code)
        ┌──────────────┼───────────────────┐
        ▼              ▼                   ▼
   BELIEF STORE     LESSONS            STATIC GAME DATA
   facts + age +    distilled,         recipes, blocks, items
   provenance       embedded           (Mojang data, tables/graph)

 decision time:  question ─► RESOLVER ─► live | belief + age | lessons ─► BRIEFING (Jev / LLM)
```

**Fact record (sketch):** `{world, kind, key, value, observed_tick, observed_wall, source,
confidence, superseded_by, valid_to}`. `source` reuses Craftonomous's provenance tags, which also
lets the fair-play policy keep privileged data out of memory.

**Freshness classes (initial thinking, to settle in session 3):**

| Kind | Authority | Staleness rule |
|---|---|---|
| Own inventory, health, position | Live only, never stored as memory | n/a |
| Blocks in loaded chunks | Live | Becomes memory when the chunk unloads |
| Blocks in unloaded chunks | Remembered | Shown with age; refreshed automatically when the chunk loads again |
| Container contents | Remembered (the server probably sends them only when opened [U]) | Until next open; always labelled with age |
| Mobs | Remembered for seconds to minutes | Short TTL |
| Named places (home, portal, mine, village) | Remembered long | Verified on approach; invalidated if the anchor block is gone |
| Player-built structures | Remembered | Verified on approach |
| Resource spots (ore, trees) | Remembered | Marked depleted on observed mining; re-verified on approach |

**Read-through refresh ("stale to fresh").** The resolver, on every read: (1) is this fact
observable live right now (chunk loaded, inside the perception policy)? Then refresh it and
record the supersession. (2) If not, return it with age and source. Plus **push invalidation:**
the proxy sees block-change packets, so a fact anchored at a changed coordinate is contradicted
the moment it happens, with no polling. [packet details: U, verify in session 2]

**Retrieval by question type:**

| Question | Mechanism |
|---|---|
| "Where is my bed / nearest known iron / what is in chest X?" | Typed query plus spatial index. No embeddings. |
| "What do I need for an iron pickaxe?" | Recipe graph from game data (Optimus-1 style). Not memory. |
| "What happened last night / why did I die?" | Journal query by time and kind. |
| "What did I learn about creepers / caves?" | Lessons: structured pre-filter (kind, area, tags), then BM25 + embedding, ranked by relevance, recency and importance. |
| "Go back to that cave with the village" (user, fuzzy) | Embedding match over place descriptions, then confirm with the user or the position. |
| Unusual game knowledge | Optional wiki index (this is the only classic RAG case). |

**Lesson scoring baseline:** start from the Generative Agents formula, but (a) decay per kind
instead of one global factor, and (b) importance assigned by code rules (death, lost item, boss
progress) instead of an LLM call per write. Both are proposals to test, not findings.

**Who may write:** code writes facts from verified observations only. An LLM may propose lessons
and reflections, tagged `inferred`, with a reliability record (Craftonomous's Wilson lower bound
is a ready pattern [R]). The writer never reviews its own work, so a lesson earns trust through
outcomes, not through the model that wrote it.

**Storage candidate:** one SQLite file per world. FTS5 gives BM25 ranking, prefix and phrase
queries [V]; the R*Tree module serves spatial lookups [U]. For vectors, `sqlite-vec` exists
(Apache-2.0/MIT, Windows supported) but is pre-v1 and warns of breaking changes [V]; the lesson
set is likely thousands of entries, so plain in-process cosine over the rows is enough to start
[inference]. Embeddings: Qwen3-Embedding local, per [embeddings.md](embeddings.md). Language
choice is not decided, so none of this assumes a runtime.

## 5. What not to do

- Do not put coordinates, counts or inventories into an embedding index and ask for "similar" rows.
- Do not let a model overwrite a fact. Supersede it, keep the history.
- Do not send an LLM a remembered fact without its age and source.
- Do not adopt Graphiti or Mem0 wholesale: they target conversation memory, run an LLM per ingest,
  and add a graph database to maintain. Take the invalidation idea, not the product.
- Do not tune decay constants before there is an evaluation.

## 6. How to know it works (proposal)

A memory test suite on a scripted world: the agent stores a chest and a base, the harness then
moves the chest, destroys a block, changes the time, restarts the process; questions with known
answers must come back correct or labelled stale. Metrics: wrong-fact rate in briefings, stale-fact
labelling rate, correction latency after the agent re-observes. Compare against a plain vector-store
baseline to measure what the structure buys.

## 7. Questions for the user (session 3)

1. Should memory obey the fair-play perception policy (only what a player could know), including
   how long a fact may be trusted?
2. Per-world memory only, or also lessons that carry across worlds?
3. May an LLM write lessons into memory at all, or only code?
4. Is it acceptable that the agent tells the user when it is unsure ("I last saw the chest 40 minutes ago")?
5. Should the user be able to read and edit the memory (a viewer)? Strong for the "master every part" goal.

## Sources

- Generative Agents: https://arxiv.org/abs/2304.03442 (HTML v2 read) [V]
- Optimus-1: https://arxiv.org/abs/2408.03615 (HTML read, first 100k characters) [V]
- MrSteve: https://arxiv.org/abs/2411.06736 (HTML v5 read, first 100k characters; appendices not read) [V]
- Zep / Graphiti: https://arxiv.org/abs/2501.13956 (HTML read) [V]
- sqlite-vec: https://github.com/asg017/sqlite-vec · FTS5: https://sqlite.org/fts5.html [V]
- Vector DB vs memory critiques: https://dev.to/mudiazuwa/your-vector-database-is-not-a-memory-system-24l2 ·
  https://dev.to/memorylake_ai/why-rag-alone-does-not-solve-agent-memory-2bil [S, snippets only, pages not read]
- Craftonomous, altoclef, Voyager: [systems-review.md](systems-review.md) [R]
- Not read: Optimus-1 and MrSteve appendices, MemGPT/Letta, Mem0 primary docs, MineExplorer, PEAM (2026). Candidates for a second pass.
