<!-- Reviewer: Beam Free via OpenCode opencode v2.1.2. Lens: beam-competitors. Raw, unverified output. -->

### Findings

- **ID**: COMP-1
- **Severity**: major
- **Where**: `docs/architecture.md` §2, §5 (Action and verification); `docs/decisions.md` 2026-10-21/2026-10-23
- **Claim**: The only agents that have beaten the game (Mine AI MCP, rmalde, Gemini Argon) used privileged protocol-level bodies with server-side state hooks; Praxion's player-bound "hands on keyboard" route has no endgame precedent, and its receipt model assumes the proxy can attribute each injected input to a server acknowledgement, which `SendInput` alone does not provide (no request id).
- **Evidence**: Survey "Landscape" shows all three 2026 completions privileged (`https://github.com/aibengineering/mine-ai-mcp`); systems-review §3.1 notes mineflayer sends `sequence` but never handles `block_changed_ack`; survey idea 3 proposes proxy-side sequence pairing. Praxion risk #1 only lists "may not reach the game at endgame timing", not attribution.
- **Recommendation**: Make input-to-packet attribution (client sequence numbers + timing correlation) an explicit E2.7 experiment and a body invariant; define the fallback (client-mod body) trigger if attribution fails; state that look/move receipts are `unconfirmed` unless sequence acks are paired.
- **Confidence**: high

- **ID**: COMP-2
- **Severity**: major
- **Where**: `docs/architecture.md` §6, §8
- **Claim**: The design specifies skill admission and storage but no runtime skill selection/retrieval strategy, while Voyager's description-embedding top-5 retrieval (no success rate or preconditions) is the only precedent and is known to be weak.
- **Evidence**: systems-review §3.4 Voyager retrieval by description embedding only; §4 table shows no system ranks by success rate; Praxion §8 only says "index in RAM" without a skill index.
- **Recommendation**: Add a skill registry with preconditions, success rate (Wilson lower bound), and context-keyed stats; retrieval by precondition match + reliability, not embedding alone. Specify in §6.
- **Confidence**: high

- **ID**: COMP-3
- **Severity**: major
- **Where**: `docs/architecture.md` §6; `docs/research/systems-review.md` §4
- **Claim**: No system in prior art, including Praxion's proposed design, lets a skill declare its expected effect separately from its code for independent framework verification; Praxion derives guards from primitive effects but never checks the skill's actual world delta against a declared postcondition.
- **Evidence**: systems-review §4 final paragraph: "No system lets a skill declare its expected effect (a postcondition or world delta) separately from its code... This is an open gap for Praxion's verification layer."
- **Recommendation**: Extend the skill schema with an optional declared postcondition (inventory delta, block state) checked by receipts in the practice room; admission requires postcondition verification. Add to §6.
- **Confidence**: high

- **ID**: COMP-4
- **Severity**: major
- **Where**: `docs/architecture.md` §6, §10 (curriculum); `docs/decisions.md` session 10
- **Claim**: Praxion adopts a free Voyager-style curriculum but only says unverifiable tasks are never counted as success; it does not specify how the curriculum avoids proposing goals whose completion cannot be computed within the perception policy.
- **Evidence**: systems-review §1.1: Voyager's curriculum rule 8 forbids placing/building/planting/trading because its critic can only check inventory; survey challenge 6 notes the free-curriculum risk.
- **Recommendation**: Add a curriculum constraint: every proposed goal must carry a verifiable completion condition computable from Praxion's perception policy; the brain validates before sending. Specify the validator in §10.
- **Confidence**: high

- **ID**: COMP-5
- **Severity**: major
- **Where**: `docs/architecture.md` §3, §5 (brain loop)
- **Claim**: souta-lab's measured failure was 1,215 Jev calls/hour all for one unchanged situation; Praxion's event list ("monitor fired", "~60 s heartbeat") lacks a closed epoch-cause set, state-hash binding, and a hysteresis margin for replacing a committed action.
- **Evidence**: survey "Key mechanisms" §4 and idea 5; souta-lab `https://github.com/souta-lab/minecraft-jev-agent`.
- **Recommendation**: Adopt souta-lab's epoch causes (candidate set changed, lease ended, health/food/air band crossed, hostile in range, no progress) with a margin `P(new)-P(current) > threshold`; bind answers to a state hash and drop stale ones. Specify in §3/§5.
- **Confidence**: high

- **ID**: COMP-6
- **Severity**: major
- **Where**: `docs/architecture.md` §5 (Perception)
- **Claim**: Craftonomous's fair-play profile includes a 16-block hearing channel; Praxion's perception is vision-only (line of sight), so creeper hisses and skeleton sounds cannot trigger reflexes until visual contact, likely too late.
- **Evidence**: systems-review §3.9 Craftonomous profiles (`fair-play`: 64-block sight, 16-block hearing); architecture §5 lists only line of sight.
- **Recommendation**: Add hearing as a perception channel with range and provenance, or explicitly justify vision-only and widen reflex trigger distances accordingly. §5.
- **Confidence**: medium

- **ID**: COMP-7
- **Severity**: major
- **Where**: `docs/architecture.md` §8 (Memory and knowledge)
- **Claim**: Baritone's 2-bit chunk cache and altoclef's container cache enable long-distance planning through unloaded chunks and "where was that chest" queries; Praxion's first memory (JSONL places/containers with an in-RAM index) is unlikely to scale to stronghold/chest recall.
- **Evidence**: systems-review §3.8 Baritone cached world; §3.7 altoclef trackers/container cache; architecture §8 stages SQLite/vectors behind a trigger but no spatial cache stage.
- **Recommendation**: Add a chunk-level spatial cache (at least special-block positions and opened containers) as an explicit memory stage triggered by long-horizon goals. §8.
- **Confidence**: medium

- **ID**: COMP-8
- **Severity**: major
- **Where**: `docs/architecture.md` §4 (Planner), §6
- **Claim**: mineflayer-pathfinder uses 40 ms/tick time-sliced A* with partial results and typed replanning triggers; Baritone uses segmented planning with expectation-based timeouts; Praxion's "A* over known blocks, segmented" specifies no budget, partial-result handling, or replanning triggers, and "may dig and bridge at higher cost" mutates the world without receipt accounting.
- **Evidence**: systems-review §3.3 pathfinder (40 ms/tick, 5 s total, `success|partial|timeout|noPath`, replanning triggers); §3.8 Baritone segmented planning; survey idea 3 receipts.
- **Recommendation**: Specify time budget, partial-result handling, replanning triggers (block change near path, chunk load, goal moved, dig error, 3.5 s no progress), and require planner mutations to pass through action-verification receipts. §4/§6.
- **Confidence**: high

- **ID**: COMP-9
- **Severity**: minor
- **Where**: `docs/architecture.md` §4 (Movement style); `docs/decisions.md` session 13
- **Claim**: rmalde's `camera-control.mjs` is a concrete per-axis velocity-limited P-controller with published parameters and an Enderman guard; Praxion cites "human-like camera, speed, timing" with no concrete controller or calibration dataset.
- **Evidence**: survey "Key mechanisms" §6 camera dynamics; systems-review §3.3 `human.js` (48 recorded players). rmalde code has no license: ideas only.
- **Recommendation**: Adopt rmalde's controller or mineflayer-pathfinder `human.js` as the starting implementation and calibration baseline; record initial parameters as defaults. §4/§13.
- **Confidence**: medium

- **ID**: COMP-10
- **Severity**: major
- **Where**: `docs/architecture.md` §6, §8
- **Claim**: Praxion's admission (3 runs vs a no-skill baseline) and lesson acceptance (3 proxy-verified traces) lack Craftonomous's Wilson-bound reliability, per-context stats, and separation of failure causes (skill broken vs world unsuitable vs precondition unmet), risking wrong promotion/retirement as in minecraft-agent-swarm.
- **Evidence**: systems-review §3.9 Craftonomous (Wilson lower bound, per-context stats, retirement as flag); §3.10 agent-swarm (`build_nether_portal` retired at 1/11, `wood_run` at 0/9 for "no tree in sight").
- **Recommendation**: Adopt Craftonomous typed failure kinds + Wilson bound + per-context stats + retirement as flag; separate failure causes in receipts before any reliability gating. §6/§8.
- **Confidence**: high

- **ID**: COMP-11
- **Severity**: minor
- **Where**: `docs/architecture.md` §7; `docs/decisions.md` session 2.3
- **Claim**: dsh-mc-client's invariant—send no input unless the foreground window is Minecraft and no unexpected screen is open—is a body-level safety rule; Praxion's focus rule releases keys on loss but does not state the send-side guard.
- **Evidence**: survey "What Praxion can take" §3; dsh-mc-client `[C]` (`https://github.com/DRheEheAMGary/dsh-mc-client`).
- **Recommendation**: Add a body invariant: no `SendInput` unless the foreground window is the game and no modal screen is open; verify it in the watchdog. §7.
- **Confidence**: high

- **ID**: COMP-12
- **Severity**: minor
- **Where**: `docs/architecture.md` §1, §4, §11
- **Claim**: The ecosystem's latest verified real-client control is 26.1.2 (dsh-mc-client) and privileged agents target 1.21.x; Praxion targets "26.3" with an Azalea decoder that is explicitly "to be verified", compounding version risk.
- **Evidence**: survey landscape (dsh-mc-client 26.1.2; Mine AI MCP tested 1.21.4); architecture §11 risk 2.
- **Recommendation**: Pin a verified stable target version for the first demo and treat 26.3 as an aspiration; keep version-specific code in one layer (already decided). §1/§11.
- **Confidence**: medium (unverified what "26.3" denotes)

### Ideas (brainstorm)

1. Adopt Mine AI MCP's async submit/pending/settled protocol plus receipts as the body-port contract — proven full-game completion with ~92% wall time spent waiting on the world, not the model.
2. Use Odyssey's tech-tree solver tables (regenerated from `minecraft-data`) for "obtain X" goals instead of LLM planning — deterministic and zero LLM cost.
3. Personal motor model learned from the user's own play via a Raw Input sink (`RIDEV_INPUTSINK`) — strongest naturalness idea in the survey; makes hand-over look like the same person.
4. Make the reflex policy LLM-tunable through typed fields with revision and lifetime (Mine AI MCP style), hard-capped by the ruleset — adaptation without code changes.
5. Candidate-skill sandbox with process isolation, quotas and seccomp (agent-swarm) even for rule lists — defense in depth for later primitive generation.
6. Persistent chunk cache (Baritone-style, 2 bits/block plus special-block index) — enables stronghold/chest recall across unloaded chunks.
7. Use Craftonomous's perception-provenance profiles as the reference for "fair-play" — already matches Praxion's line-of-sight policy and includes DDA occlusion.
8. Decision-epoch gate with hysteresis (souta-lab) — prevents Jev flip-flopping and stacked latency.

### Questions for the user

1. Should the first demo pin a verified stable version (e.g., 1.21.x) instead of 26.3, to de-risk Azalea and OS-input experiments?
2. Is the player-bound OS-input route mandatory for v1, or should a protocol-body (Mineflayer-style) companion mode ship first as a stepping stone?
3. Should skill success be judged by declared postconditions (world delta) in addition to practice-room admission?
4. Should the reflex policy be LLM-tunable (Mine AI MCP style) in v1, or fixed defaults only?
5. Should auditory perception be in scope for first-night reflexes?

FINAL_REPORT: The most serious competitive gaps are that Praxion's player-bound OS-input route has no endgame precedent and its receipt model assumes input-to-acknowledgement attribution that SendInput cannot provide (COMP-1); that the free Voyager-style curriculum can propose unverifiable goals without a perception-policy validator (COMP-4); and that skill admission and lesson acceptance lack Craftonomous's Wilson-bound reliability, per-context stats and failure-cause separation, risking the same wrong-retirement failure seen in minecraft-agent-swarm (COMP-10). Secondary but material gaps are the missing decision-epoch discipline that souta-lab shows is required to stop Jev flip-flopping (COMP-5), the absence of any skill retrieval/ranking strategy (COMP-2), and no independent postcondition verification for skills (COMP-3). The design is ahead of prior art on perception provenance, teleo-reactive recovery, and staged admission, but behind on spatial memory, auditory perception, and concrete motor-control calibration.
