# Session 14: Testing, evaluation and observability

Decided 2026-10-29. Inputs: tests defined in sessions 2.7 to 13 (see the inventory), survey/01
(refereed fixtures, Mine Labs), survey/02 (latency sweeps), survey/04 (skill CI, learning test),
systems-review (Craftonomous fake body and offline suite), decision 1.6.

## Test levels
| Level | What | Speed | Catches |
|---|---|---|---|
| 1 Fake world | in-memory fake body, no game (also runs on macOS) | seconds | logic, rule lists, arbiter, memory |
| 2 Protocol test body | headless client on a local test server | minutes | verification, receipts, the practice room (session 11) |
| 3 Real body | real client with real mouse and keyboard (Windows) | slow | input timing, focus, menus |

Rules already decided: test setup goes through a separate logged harness channel, never through
the agent's own API (decision 1.3); a referee reads server facts and Praxion's own report never
decides a verdict (session 7); tests run on a copy of the world, never in the user's world
(session 11).

## Decided (user, 2026-10-29)
- **Three levels, most tests at the bottom.** Each level catches what the one below cannot.
  Rejected: only the real client (slow, costly) and only the fake world (never tests real input).
- **Evidence standard: many seeds, confidence intervals, analysis written before the runs.** Each
  ladder rung reports reach rate, time and deaths over fresh seeds with intervals (the first-night
  demo gate: at least 18 of 20 fresh seeds without death). The demo video is one of the measured
  runs; a hand-picked best run is labelled as such (decision 1.6). Target numbers are set after the
  first baseline.
- **Observability: a live window plus a replayable log.** The separate interface (2.4) shows the
  active goal, rule or skill, the arbiter's decision, every Jev and LLM call (question, answer,
  probability, latency, cost) and the receipts with failure kinds. All of it goes to an event log
  on disk that can be replayed; every claim cites a receipt id. A short report ends each run.
- **When tests run:** the fast ones (levels 1, unit tests) on every change; levels 2 and 3, which
  cost tokens and time, run in a nightly batch or on request. Cost and tokens are recorded.

## Minor picks by Claude (user may overrule)
- **Run report fields:** seed, rung times, deaths with causes, command-suite results, cost per
  hour, longest starvation gap, take-over latency, Jev and LLM latency percentiles, skill gate
  statistics. One machine-readable file plus a one-page summary.
- **Seeds are recorded;** a run can be repeated. Fixture restore time decides whether fixtures run
  nightly (above 30 s) or on every change (session 11).
- **Tools:** `cargo test` for the Rust body, `pytest` for the Python brain; the fake body is the
  same port as the real one (session 2.5), so one suite runs against all levels where it can.
- **LLM reasoning:** hosted models show summarised reasoning only, so the log keeps the structured
  directive and its stamp (session 8), not a raw trace.
- **Privacy:** logs and recordings stay local.
- **Reviews:** a review round by a model other than the writer's, as AGENTS.md requires, after the
  architecture draft and before any large change; failure analysis of every death goes into the
  test matrix (session 7).

## Test inventory (defined in earlier sessions)
| Source | Test |
|---|---|
| 2.7 | experiments E1 to E14 with thresholds |
| 4 | fault injection (false `confirmed` must be 0); take-over at every phase of bridging, pillaring, eating, chest and bucket use |
| 5 | 20 obtain/ensure skills with undo-perturbations; composition templates on two models |
| 7 | reflex coverage matrix with refereed fixtures; arbiter tests (no A-B-A within 5 s) |
| 8 | latency sweeps; chaos test (cut providers, inject 5, 15, 30 s of LLM latency) |
| 11 | gate mutants and red team; rollback test; learning test at equal compute |
| 12 | memory test suite on a scripted world; retrieval test for the RAG |
| 13 | movement statistics against held-out human play; blind test with OBS recordings |
| 10 | curriculum arm in the learning test |

## Left open, with owner
- How faithfully the protocol test body stands in for the real body: measured after 2.7 and the
  first level-2 tests.
- Exact thresholds and seed counts: set after the first baseline.
- Where tests run (the dedicated Windows PC, a cloud machine), keys, cost limits: session 15.
