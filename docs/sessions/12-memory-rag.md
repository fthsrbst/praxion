# Session 12: Memory and knowledge (RAG)

Decided 2026-10-29. Inputs: `docs/research/memory-world-state.md`, `docs/research/memory-review/`
(explainer, the simplicity and logic critiques, the two fresh designs), `docs/research/embeddings.md`,
sessions 3, 4, 10 and 11.

## Kinds of memory
| Kind | Example | Where it lives |
|---|---|---|
| Live state | blocks and mobs around, health | Rust body (session 3), not memory |
| World memory | home, chest contents, what happened last night | per-world file, facts carry age and source (session 3) |
| Lessons | "creepers explode, keep away" | text, general across worlds |
| Game data | recipes, blocks, items | Mojang data, tables and a graph (session 2), not memory |
| Project ledger, recorded play | long projects, the user's play | sessions 10 and 11 |

RAG (search by meaning) does not fit exact keyed facts; it fits text. In-game stream data (world
state, events, chests, places) uses typed lookups and an event journal, not RAG. That is the
non-RAG system the user remembered from the memory research.

## Decided (user, 2026-10-29)
- **The first version has the smallest memory, built in stages.** One append-only JSON Lines file
  per world (the last line for a key wins, the past stays readable) plus an in-RAM index.
  Stages: A perception plus game data; B places and containers with the read rule
  (live, seen with an age, or unknown); C event log plus a read-only viewer; D one briefing
  function with a fixed token budget. SQLite, vectors, lesson scoring and the chunk-memory cache
  are added only when a measurement asks (triggers below). Rejected: the full design up front.
- **Lessons come when a trigger fires** (skills exist and a mistake is seen twice). They are
  general across worlds, while world facts stay per world. The LLM proposes a lesson tagged
  `inferred`; code accepts it only after at least 3 proxy-verified successful traces. Lessons
  go to the LLM only, never into Jev's state, because state text steers Jev.
- **Game knowledge: the Minecraft Wiki text is indexed and a RAG is built.** The user chose this
  against the recommendation (exact data plus the LLM's own knowledge, no RAG). The wiki text is
  CC BY-NC-SA, which needs attribution and non-commercial use, and conflicts with an open-core
  business model; this stays an open licence constraint (see the minor picks).
- **The game memory is separate from Mnema and the hub.** It lives in local files and works offline
  with no network delay; the hub stays the project memory of the development agents. Mnema is
  not described anywhere in the repo, so this is based on the assumption that it is the hub's
  project map and memory system; the user may correct it.

## Minor picks by Claude (user may overrule)
- **Wiki index is built locally and never committed or distributed:** a script builds it from a
  download the user runs, keeping page title, URL, revision and licence with every passage; the
  retrieval layer is source-agnostic, so the corpus can be replaced by our own notes if the
  business model needs it. Whether the wiki offers a dump and what its terms say: to be checked
  before building.
- **Embeddings:** Qwen3-Embedding (Apache-2.0, local, free) for text, with a reranker if needed;
  Gemini Embedding 2 stays an option for images (embeddings.md). The choice is confirmed by a
  retrieval test on real questions.
- **RAG serves the LLM for knowledge questions only** (strategy, mechanics the data tables do not
  hold), returns passages with source and date, and never feeds Jev or the reflexes. Consulting a
  wiki is what a human player may do, so it does not break the fair-play rules of session 3.
- **Triggers for stage E:** block memory of unloaded chunks when the demo's travel distance
  regularly exceeds view distance; lessons when skills exist and a mistake repeats; vector search
  for places or lessons when they no longer fit the briefing budget or retrieval fails in
  measurements; SQLite when the file passes a few hundred thousand lines or the viewer needs paging.
- **Briefing contract (first form):** a hard token cap; every remembered fact carries its age and
  source; unlabelled remembered facts are a test failure; nothing is invented to fill space.
- **Memory tests:** a scripted world; move a chest, break a block, change the time, restart the
  process; each question must come back correct or labelled stale. Compare with a plain vector
  store to see what the structure buys.

## Left open, with owner
- Object identity (a moved chest versus break and place), TTL numbers per fact kind: first code
  and measurement.
- Which questions need wiki RAG at all: measured on real runs; retrieval failures decide.
- Sharing lessons through the hub: later, by hand.
