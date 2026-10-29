<!-- Reviewer: Iceman Contributor Free via OpenCode opencode v2.1.2. Lens: iceman-memory. Raw, unverified output. -->

### Findings

- **ID**: MEM-001
  - **Severity**: blocker
  - **Where**: `docs/architecture.md` §8, `docs/sessions/12-memory-rag.md` §Decided, `docs/sessions/03-perception-world-model.md` §Decided
  - **Claim**: The design splits "live world state" to the Rust body (correct for reflexes/5ms budget) but also states "world memory" facts live in per-world files read through the body with an index in RAM, whilst `docs/architecture.md` §4 lists `Memory and briefing` under the brain (Python) as `[P]`. The boundary between "what the body remembers" (chunk cache/provenance ledger) and "what the brain remembers" (places/containers/events/lessons) is undefined. 
  - **Evidence**: `docs/decisions.md` "The live world state lives in the Rust body"; `docs/sessions/03-perception-world-model.md` §Minor picks "Remembered blocks of unloaded chunks are kept as a compact chunk cache in the body (Baritone style)"; `docs/architecture.md` §4: `Memory and briefing | Python (files read through the body) [P]` and §8: "World state is live and held by the body. Remembered facts carry age and source..." This is contradictory about ownership (body cache vs brain files). See also `docs/research/memory-review/explainer.md` §3.2 (journal/projection split) and `docs/research/memory-review/critic-simplicity.md` §3.1 (hard boundary argument).
  - **Recommendation**: Decide a single ownership boundary now (e.g. body owns unloaded-chunk block cache + the hidden-data/provenance ledger; brain owns places/containers/events/lessons as JSONL + index). Explicitly state which component may write which stores, how the brain reads body memory (only via the body port/queries), and make this a `[D]` not `[P]`. Without this, the "body survives silent brain" requirement cannot be enforced.
  - **Confidence**: high

- **ID**: MEM-002
  - **Severity**: blocker
  - **Where**: `docs/sessions/12-memory-rag.md` §Decided/Minor, `docs/research/memory-world-state.md` §4, `docs/research/memory-review/explainer.md` §6.1
  - **Claim**: The design relies on "container contents arrive only when opened" to justify "remembered with age until next open", but this is explicitly marked unverified (`[U]`) and the "read-through refresh" rule says "refresh if loaded" which directly contradicts it. 
  - **Evidence**: `docs/sessions/12-memory-rag.md` §Left open: "Whether container contents arrive only when opened: verify with a packet capture during 2.7 E2."; `docs/research/memory-world-state.md` §4 table: "Container contents | Remembered (the server probably sends them only when opened [U])"; `docs/research/memory-review/explainer.md` §6.1: "It treats 'chunk loaded' as 'observable', but container contents need an action (open the window)... And the whole chest story depends on \"contents arrive only when opened\" [U]". `docs/sessions/02-body-connection.md` notes `Set Container Content` packet behavior but does not settle this for all states.
  - **Recommendation**: Do not promise "instant refresh" for containers. Make the read rule explicit: `LIVE` only when the container window is actually open and contents observed; else `SEEN` with age and "must open to verify". Treat opening as an action (costly, may fail) and never let the memory/resolver trigger it autonomously. Block shipping the briefing for chest queries until E2 packet capture confirms the exact behavior.
  - **Confidence**: high

- **ID**: MEM-003
  - **Severity**: major
  - **Where**: `docs/sessions/12-memory-rag.md` §Left open, `docs/research/memory-world-state.md` §4, `docs/research/memory-review/explainer.md` §6.4
  - **Claim**: Object identity is missing entirely. The schema relies on `superseded_by`/newest-wins but cannot distinguish "moved chest" from "broke and placed elsewhere" or other reuses. This will produce incorrect answers (and the chest example in the research papers over this over).
  - **Evidence**: `docs/sessions/12-memory-rag.md` §Left open: "Object identity (a moved chest versus break and place), TTL numbers per fact kind: first code and measurement."; `docs/research/memory-review/explainer.md` §6.4: "The schema links facts (`superseded_by`), not objects. A moved chest, a renamed item, a rebuilt wall: the code sees break + place and cannot know it is \"the same\" thing. The story above papers over exactly this." `docs/research/memory-world-state.md` §4 suggests linking but no identity key.
  - **Recommendation**: Add an object identity layer before using remembered containers/places for "bring me X" commands. Options: track by anchor block + inventory signature + proximity over time, or require re-verification on approach and never assume identity across break+place. At minimum, block automatic identity inference and force `UNKNOWN` or re-observation when break+place occurs at different positions.
  - **Confidence**: high

- **ID**: MEM-004
  - **Severity**: major
  - **Where**: `docs/architecture.md` §8, `docs/sessions/12-memory-rag.md` §Minor picks, `docs/sessions/03-perception-world-model.md` §Decided/Minor
  - **Claim**: The briefing contract is underspecified despite being required. Decisions say "every remembered fact carries age and source" and "unlabelled remembered facts are a test failure", but there is no ordering rule, no drop policy when over token cap, no enforcement that provenance tags are present for brain-consumed facts, and no hard block on leaking privileged/hidden data into the briefing.
  - **Evidence**: `docs/decisions.md` "Remembered facts carry age and source, are re-checked when in line of sight, and Praxion tells the user when it is unsure."; `docs/sessions/12-memory-rag.md` §Minor picks: "Briefing contract (first form): a hard token cap; every remembered fact carries its age and source; unlabelled remembered facts are a test failure; nothing is invented to fill space."; `docs/architecture.md` §8: "world state is live and held by the body. Remembered facts carry age and provenance..." but §4 puts `Memory and briefing` in brain as `[P]`. `docs/sessions/03-perception-world-model.md` §Minor picks defines provenance tags (`sight`, `hearing`, `memory` with age, `inference`, `testimony`, `privileged`) but no enforcement mechanism.
  - **Recommendation**: Make the briefing a pure renderer with a schema (e.g. `facts[]` with `{key, value, age_ticks, age_wall, source, provenance, confidence_or_label}`), a hard token cap, a deterministic drop order (oldest/least relevant first), and a validation step that fails the build/test if any remembered fact lacks age+provenance. Also explicitly strip or block `privileged` facts from brain briefings (and ensure they never reach Jev).
  - **Confidence**: high

- **ID**: MEM-005
  - **Severity**: blocker
  - **Where**: `docs/architecture.md` §8 risk 8, `docs/sessions/12-memory-rag.md` §Decided, `docs/decisions.md` §2026-10-29
  - **Claim**: Wiki RAG is adopted by user choice but the licence (CC BY-NC-SA) conflicts with an open-core Apache-2.0 model and the index must never be distributed. The design has no concrete mitigation beyond "build locally, never committed/distributed" and "retrieval layer source-agnostic" which are insufficient for an open-core product.
  - **Evidence**: `docs/decisions.md` "Game knowledge: the Minecraft Wiki text is indexed and a RAG is built (the user's choice over \"exact data and the LLM's knowledge\"). The text is CC BY-NC-SA: the index stays local and undistributed, and the licence conflict with an open-core model stays open."; `docs/architecture.md` §8: "and the index is never distributed because the wiki licence is CC BY-NC-SA [D]; the conflict with an open-core model is [O]."; `docs/sessions/12-memory-rag.md` §Decided and §Minor picks list local build/script but no legal/technical guardrails.
  - **Recommendation**: Treat this as a hard blocker until resolved. Options: use only Mojang data generator reports + first-party notes (the recommended path), replace corpus with original content, use a permissively licensed alternative, or isolate wiki RAG behind a clearly separated, non-open-core component. Do not ship the index in releases, add build-time checks to prevent committing it, and document the exact terms before first build. The current plan of "never distributed" is necessary but not sufficient.
  - **Confidence**: high

- **ID**: MEM-006
  - **Severity**: major
  - **Where**: `docs/sessions/12-memory-rag.md` §Decided, `docs/sessions/03-perception-world-model.md`, `docs/architecture.md` §8
  - **Claim**: Lessons are proposed by LLM and accepted by code after "3 proxy-verified successful traces", but "proxy-verified" is not defined against the receipt/evidence system, there's no refutation mechanism, and lessons go to LLM only but the briefing must not leak them to Jev (stated) with no enforcement. Also cross-world lessons vs per-world facts need clearer separation.
  - **Evidence**: `docs/sessions/12-memory-rag.md` §Decided: "Lessons come when a trigger fires (skills exist and a mistake is seen twice). They are general across worlds... The LLM proposes a lesson tagged `inferred`; code accepts it only after at least 3 proxy-verified successful traces. Lessons go to the LLM only, never into Jev's state, because state text steers Jev."; `docs/architecture.md` §8 same; `docs/sessions/04-action-verification.md`/receipts exist but no mapping to "proxy-verified traces" specified.
  - **Recommendation**: Define "proxy-verified" precisely (e.g. 3 independent successful uses where receipts show server-confirmed effects, no take-over during them, and referee verifies). Add a refutation path (a counter-trace immediately marks lesson as `suspect`/disabled), store lessons in a separate store from world facts, and add a hard filter in the briefing builder to exclude lessons when targeting Jev. Also consider the simpler approach from `docs/research/memory-review/critic-simplicity.md` §3.4: postpone lessons until after demo (skills first).
  - **Confidence**: high

- **ID**: MEM-007
  - **Severity**: major
  - **Where**: `docs/architecture.md` §8, `docs/sessions/03-perception-world-model.md` §Minor picks, `docs/research/memory-review/critic-simplicity.md` §3.9, `docs/sessions/02-body-connection.md`
  - **Claim**: Chunk cache in body (Baritone-style 2-bit) is proposed but push invalidation only works for loaded chunks and "Unload Chunk" is the real transition point. The design doesn't budget for chunk snapshotting on unload, and journaling per block is impossible as noted in reviews.
  - **Evidence**: `docs/sessions/03-perception-world-model.md` §Minor picks: "Remembered blocks of unloaded chunks are kept as a compact chunk cache in the body (Baritone style, about 2 bits per block). A chunk snapshot, never a per-block journal: one chunk has 98,304 blocks."; `docs/research/memory-review/critic-simplicity.md` §3.9: "The proxy sees block-change packets only for chunks the client has loaded [U]... The case where the client *stops* being informed has its own packet: \"Unload Chunk\"... hook 'Unload Chunk' and snapshot the few facts worth keeping (named-place anchors, containers, ores). That is one handler instead of a general invalidation system."; `docs/research/memory-review/explainer.md` §6.3 same.
  - **Recommendation**: Treat chunk memory as a separate, measured feature (stage E) with a concrete trigger (travel distance exceeds view distance regularly). Only snapshot on `Unload Chunk` for a small allowlist (named places, containers, ores) not the whole chunk; use Baritone's 2-bit scheme or equivalent. Don't block demo (stages A-D) on this.
  - **Confidence**: high

- **ID**: MEM-008
  - **Severity**: major
  - **Where**: `docs/sessions/03-perception-world-model.md` §Decided, `docs/architecture.md` §5, `docs/research/memory-review/critic-simplicity.md` §4
  - **Claim**: "Out of sight a query answers `unknown`, not 'absent'" is correct, but the "re-checked when in line of sight" rule is not enforceable for containers/hidden state and the design still over-promises "stale to fresh". The physics limit (no packets for unloaded chunks, contents need window open) must be made explicit in all consumer code.
  - **Evidence**: `docs/sessions/03-perception-world-model.md` §Decided: "Remembered facts carry their age and how they were learned... Before a remembered fact is used, it is re-checked when it is in line of sight, and Praxion tells the user when it is unsure."; `docs/research/memory-review/critic-simplicity.md` §4: "Inside loaded chunks: the client is being kept current by the server, so 'now' is exact and free... Outside loaded chunks: the client receives nothing about those chunks... 'no system can know the current state'. The information does not exist on this machine."; `docs/decisions.md` fair-play/perception rules.
  - **Recommendation**: Encode this as a hard invariant: `read(key)` returns `LIVE` only if observable now for free; `SEEN(age, source)` otherwise; `UNKNOWN` if never seen and not observable. Never call actions to "refresh" memory. Make the consumer (brain/skills) explicitly ask "should I go look?" rather than the resolver doing it. Add tests that would fail if this is violated.
  - **Confidence**: high

- **ID**: MEM-009
  - **Severity**: major
  - **Where**: `docs/sessions/12-memory-rag.md` §Minor picks, `docs/architecture.md` §8, `docs/sessions/08-jev-llm-brain.md`
  - **Claim**: Briefing must be token-capped and every remembered fact must have age+source, but there's no ordering (e.g. goal-relevant first), no "unlabelled facts = test failure" enforcement point, no rule for what gets dropped, and no separation between facts for Jev vs LLM. Also Jev is latency-bound and hates long irrelevant state.
  - **Evidence**: `docs/sessions/12-memory-rag.md` §Minor picks lists requirements but no algorithm; `docs/sessions/08-jev-llm-brain.md` §Minor picks: "State for Jev: the code computes every number and passes short text with buckets... Jev's weakness with counting is avoided."; `docs/research/memory-review/critic-simplicity.md` §3.7: "Why a hard cap: Jev is a single-pass decision model at ~130–300 ms and its quality drops with long irrelevant state... also Jev is weak at counting and arithmetic, so the briefing must not ask it to add up inventory".
  - **Recommendation**: Define briefing as a function with deterministic ordering (current goal + immediate needs + relevant places/containers + recent events), hard token cap, drop order, and a validation that fails CI if any remembered fact lacks labels. Create separate briefing builders: `brief_for_jev()` (minimal, buckets, no prose lessons) and `brief_for_llm()` (more context). Enforce "lessons never go to Jev" in code.
  - **Confidence**: high

- **ID**: MEM-010
  - **Severity**: major
  - **Where**: `docs/sessions/12-memory-rag.md` §Decided, `docs/architecture.md` §8, `docs/sessions/03-perception-world-model.md`
  - **Claim**: Wiki RAG is "for the LLM only" and "never feeds Jev or the reflexes", which matches fair-play (consulting wiki is like human), but there's no hard enforcement mechanism and the retrieval layer could leak into other paths. Also "which questions need wiki RAG at all" is left open with no trigger.
  - **Evidence**: `docs/sessions/12-memory-rag.md` §Minor picks: "RAG serves the LLM for knowledge questions only (strategy, mechanics the data tables do not hold), returns passages with source and date, and never feeds Jev or the reflexes. Consulting a wiki is what a human player may do, so it does not break the fair-play rules of session 3."; `docs/architecture.md` §8 states same.
  - **Recommendation**: Treat RAG as a separate service only callable by the LLM client with explicit `purpose: knowledge_question`, add a filter that blocks any call from Jev/reflexes/arbiter, and log every RAG query with source/date. Also add a retrieval test to decide which questions actually need it (per left-open) to avoid pulling wiki into normal gameplay.
  - **Confidence**: high

### Ideas (brainstorm)

1. **Adopt the "smallest memory first" strictly (JSONL + in-RAM index + read rule)** - Why: Matches `docs/research/memory-review/critic-simplicity.md` §5 and `docs/sessions/12-memory-rag.md` stages A-D. Avoids SQLite/vectors/lessons until measured triggers fire; cheaper to reason about and easier for the user to "master every part".
2. **Make "unknown is a value" a first-class type in the API** - Why: `docs/sessions/03-perception-world-model.md` and `docs/research/memory-review/explainer.md` §1 both call this out. Enforce it in queries (return `Unknown` not `None`/omission) so consumers can't accidentally treat "not remembered" as "false".
3. **Enforce briefing as a pure, validated renderer with dual builders (Jev vs LLM)** - Why: Directly addresses MEM-004/MEM-009. Jev needs buckets and counts-computed, LLM needs more prose; separation prevents state-text steering of Jev (explicitly called out in `docs/sessions/12-memory-rag.md`).
4. **Treat container identity as "unverified until observed in window" by default** - Why: Addresses MEM-002/MEM-003. Forces re-observation on approach/change and prevents confident wrong answers from moved chests. This is conservative but correct for fair-play and reliability.
5. **Defer wiki RAG behind a strict gate + first-party data fallback** - Why: Addresses MEM-005. Try Mojang data generator + our own notes first; only enable wiki RAG if measurements show gaps, and isolate it completely from Jev/reflexes with legal review.
6. **Add a "memory test suite" as a hard gate before demo** - Why: `docs/sessions/12-memory-rag.md` §Minor picks calls for it. Script moves chest/break block/change time/restart and assert correct or labelled stale. This would catch most of the issues above (identity, refresh, age labels).
7. **Snapshot on Unload Chunk with allowlist, not full chunk cache** - Why: Cheaper and safer than full 2-bit chunk cache. Matches `docs/research/memory-review/critic-simplicity.md` §3.9 and avoids speculative storage until travel distance justifies it.

### Questions for the user

1. **Memory ownership:** Should the Rust body own the unloaded-chunk block cache and provenance ledger, while the Python brain owns places/containers/events/lessons as per-world JSONL files (with brain reading body only via the body port)? Or should everything go in one place? (Needed to resolve MEM-001)
2. **Container refresh:** Is it acceptable that "bring iron from chest X" may require walking to and opening the chest to be sure (and we label old memories as stale), rather than trusting a remembered count while the chunk is loaded? (Needed to resolve MEM-002)
3. **Wiki RAG vs open-core:** Given CC BY-NC-SA conflict with Apache-2.0 open-core, do you want to (a) drop wiki RAG entirely and rely on Mojang data + first-party notes, (b) keep it local-only and never distribute (and accept open-core boundary issues), or (c) replace with a permissively licensed corpus? (Needed to resolve MEM-005)
4. **Object identity:** For the demo, is "never assume identity across break+place" acceptable (force re-observation / UNKNOWN on moved containers), or do you want a best-effort identity heuristic implemented now? (Needed to resolve MEM-003)
5. **Lessons timing:** Should lessons be fully postponed until after the first demo passes, or built behind the "3 proxy-verified traces" gate now? (Needed to resolve MEM-006; `docs/research/memory-review/critic-simplicity.md` recommends postponing)

FINAL_REPORT The top 3 findings are MEM-001 (unclear ownership between body brain memory creating enforcement gaps for the "body survives silent brain" requirement), MEM-005 (wiki RAG CC BY-NC-SA vs open-core Apache-2.0 remains an unresolved blocker with no concrete mitigation), and MEM-002 (container contents refresh assumption unverified and contradictory to "chunk loaded = observable", risking confident wrong answers for core tasks like fetching items). All three need explicit decisions/mitigations before building the memory/briefing paths.
