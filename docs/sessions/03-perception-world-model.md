# Session 3: Perception and world model

Opened and decided 2026-10-29. Experiments from session 2.7 have not run yet; the decisions below
do not depend on them except where marked.

Inputs: `docs/research/memory-world-state.md`, `docs/research/memory-review/` (critiques and the
explainer), survey/01 (hidden metadata, perception profiles), systems-review (Craftonomous
profiles and provenance). Scope split: this session covers the live world model and how remembered
facts age. Lessons, RAG and cross-world memory belong to session 12.

## The problem
The proxy sees everything the server sends the client: every block of every loaded chunk
(including buried ore), and entities behind walls. Decision 1.3 says the agent knows only what a
player can see and do. This session fixes what that means.

## Decided (user, 2026-10-29)
- **Seeing: line of sight in every direction, within a distance limit.** A ray from the eye
  decides whether a block or entity is known; a wall hides what is behind it. Camera direction does
  not matter. Default distance about 64 blocks, tunable. Rejected: screen-only (cone of view) as
  the default, and "all loaded chunks except buried blocks". A stricter profile may be added
  later for an ablation on the same seeds (survey/01).
- **Data that arrives but is not displayed:** whatever the F3 screen shows is allowed (own
  coordinates, facing, biome, light level); everything else is denied, for example a mob's exact
  health or the dragon phase counter (the phase must be inferred from what is visible). Every
  privileged read is written to a ledger.
- **The live world state lives in the body (Rust), next to the decoder.** The body must survive
  a silent brain (2.5) and reflexes need the state within the 5 ms budget. The brain asks short
  queries ("nearest oak", "danger nearby?") and receives events about important changes, never
  raw chunks.
- **Remembered facts carry their age and how they were learned.** Before a remembered fact is
  used, it is re-checked when it is in line of sight; Praxion tells the user when it is unsure
  ("I last saw the chest 40 minutes ago").

## Minor picks by Claude (user may overrule)
- **Unknown is a value, not "absent".** Out of sight or range, a query answers `unknown`.
- **Provenance tags** on every fact: `sight`, `hearing`, `memory` (with age), `inference`,
  `testimony` (the user said so), `privileged`. Anything derived from `privileged` stays
  `privileged` (Craftonomous idea; MIT, an idea rather than copied code).
- **Hearing counts as a sense**: sound packets carry a position and are used within about 16
  blocks. Value tunable.
- **No screenshots in the decision loop.** The world comes from the protocol, which is exact.
  Screenshots are for debugging, recordings and the naturalness blind test.
- **Memory viewer:** the user can read what Praxion remembers (read-only first, editing later),
  following the "master every part" goal.
- **Remembered blocks of unloaded chunks** are kept as a compact chunk cache in the body
  (Baritone style, about 2 bits per block). A chunk snapshot, never a per-block journal: one
  chunk has 98,304 blocks.
- **Push invalidation** (a block-change packet contradicts a stored fact) works only for chunks
  the client has loaded; for the rest, a fact stays stale until re-observed.
- **Perception profiles are configurable** (the default is the decided one) so an ablation can
  measure what each withheld fact costs.

## Left open, with owner
- Object identity (a moved chest versus break plus place), TTL values per fact kind, the briefing
  contract for the LLMs (token budget, ordering, what is dropped): session 8 and session 12.
- Lessons, cross-world memory, may an LLM write lessons, journal and projection design,
  per-world storage (a SQLite file per world is the current candidate): session 12.
- Whether container contents arrive only when opened: verify with a packet capture during 2.7 E2.
- Handling of transparent blocks (glass, water, leaves) in the line-of-sight walk: first code.
