# Session 7: Fast-decision layer, reflexes and arbiter

Decided 2026-10-29. Inputs: survey/03 (leases, tiered arbiter, owner tokens, cancellation
classes), survey/01 (typed survival policy, reflex coverage matrix), systems-review (mindcraft
modes, altoclef chains, Baritone processes).

## Time scales
Under 50 ms: reflexes (pure code in the Rust body). 50 ms to 1 s: skill rules and the arbiter.
1 to 10 s: Jev (session 8). Seconds to minutes: LLM. Minutes to hours: projects (session 10).
Take-over bypasses the arbiter entirely (2.4).

## Decided (user, 2026-10-29)
- **Tiers with two thresholds.** Order: survival reflex, then the user's command, then
  maintenance (eat, sleep, tools), then own projects. Each need has a soft threshold (done at the
  command's next safe point) and a hard threshold (promoted to survival, immediate). A switch
  inside one tier happens only when the gain exceeds `k x loss_on_interrupt` and no `until` guard
  is active, so there is no flip-flopping. Rejected: command always first (starving during a
  30-minute command) and need always first (commands constantly cut).
- **Reflex settings are a fixed default that the user can edit.** About 10 typed fields (critical
  health, evade range, water-bucket save, and so on) in a configuration file; reflexes never wait
  for the network. LLM tuning of the policy is not in the first version; it can be added later
  within caps set by the user's ruleset. The demo must survive a model outage.
- **First reflex set is the first night, extended at every ladder rung.** Creeper, skeleton and
  arrows, zombie, spider, lava, drowning, falls (water bucket), fire, starvation, Enderman gaze.
  Nether and End reflexes (ghast, blaze, piglin, dragon) arrive before the rung that needs them.

## Minor picks by Claude (user may overrule)
- **A reflex is a pure function** `evaluate(world) -> action`, priority ordered, run in the Rust
  body on every tick, with no network and no LLM. Jev is never on this path (session 8).
- **Ownership:** one owner of the body at a time, with epoch-stamped owner tokens. A reflex's
  acquire always succeeds and aborts the running skill; a stale token's writes do nothing. Split
  leases (camera, locomotion, hands) are not built until path tracking needs them.
- **After a reflex ends,** the interrupted skill re-evaluates its rules and continues by itself
  (the teleo-reactive property). The interruption counts as `preempted`, not failure. If the
  reflex ran long or the world changed a lot, the LLM is woken by an event (session 8).
- **Ruleset enforcement:** protected blocks, items and regions are checked in code before any
  input is sent; it applies to reflexes too.
- **Toggle keys:** with `toggleSneak` or `toggleSprint` set in `options.txt`, hold and release
  differ; the input layer models toggle versus hold per key.
- **Coverage matrix:** each cell is (threat, distance, light, terrain) with a pass predicate over
  server facts and a latency budget. The logged test harness sets up each fixture; Praxion's own
  report never decides a verdict. Every death in a run becomes a new cell.
- **Demo gate:** 100% on reflex cells over 10 instances each; at least 18 of 20 fresh-seed first
  nights without death; arbiter tests: no A to B to A switch within 5 s, tier-1 preemption within
  one tick, no starvation death in 30-minute commands.

## Left open, with owner
- The numbers: `k`, the hunger and sleep thresholds, per-reflex distances; found by measurement.
- The reflex list per ladder rung beyond the first night: written as each rung is reached.
- Command handling and conflicts: session 9. Goal generation: session 10.
