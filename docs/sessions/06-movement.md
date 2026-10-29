# Session 6: Movement and pathfinding

Decided 2026-10-29. Inputs: systems-review (mineflayer-pathfinder, Baritone, prismarine-physics,
altoclef blacklists), the Azalea pathfinder (checked 2026-10-29: `astar`, `moves`, `parkour`,
`costs`, `simulation`, `mining`; MIT; tied to Bevy ECS and the Azalea client), sessions 2.3, 3, 4, 7.

## Three parts
1. **Planning:** A* over a block graph. Each move (walk, jump, parkour, descend, climb, swim,
   dig, place) has a cost in expected ticks. Planning is time-sliced and segmented: the next
   segment is planned while the current one runs.
2. **Execution:** the plan becomes held keys and camera movement through the hands (2.3). The
   client runs its own physics, so Praxion only presses keys; the planner predicts with forward
   physics and compares with the position the proxy reports every tick.
3. **Watching:** a changed block, a moved goal, a lost position or no progress triggers a replan.

## Decided (user, 2026-10-29)
- **A path may change the world, anywhere, but it must pick a sensible route.** Moves that dig,
  place or build towers are always allowed and cost more than moves that do not, so an
  unmodified route wins unless it is far worse. The user chose this over "only when the skill
  allows" (which would have blocked hills and rivers less often but made side-effect
  declarations a gate) and over "never".
- **Stuck handling is bounded:** a retry counter and a progress check per target and per node, and
  detection of replans that repeat over the same cells. After N tries the target or node goes to
  a blacklist (reset when Praxion gets closer or has a better tool), the failure is typed
  `unreachable`, and an event wakes the LLM (session 8), which proposes another target or route.
  No `/tp` or any operator command, ever. Rejected: ask the user at every stuck and retry forever.
- **Natural movement is a separate layer above the planner.** The planner finds the efficient
  route; a movement-style layer (camera, speed, sprint timing, small deviations) makes it look
  human and is tuned in session 13 (mineflayer's `human.js`, calibrated on 48 players, is the
  model). Rejected: human-like costs inside the planner, and no naturalness.

## Minor picks by Claude (user may overrule)
- **Hard limits that stay even though paths may change the world:** never a block or region the
  user's ruleset protects (checked in code before any input, as in session 7); never dig straight
  down; never dig into unknown blocks (lava). Every block a path changes is written into the
  receipt and the ledger.
- **The planner uses only blocks the world model knows** (session 3). Unknown cells are a frontier:
  plan to the frontier, look, plan again (segmented plans).
- **Policy object** in the style of mineflayer's `Movements`: allowed moves, maximum drop,
  avoided blocks, exclusion areas, cost multipliers. Hazards (lava, fire, cactus, deep water, falls
  above the maximum drop, hostile mobs) carry costs; reflexes (session 7) stay separate.
- **Replan triggers:** block change near the path, new chunk next to a visited one, goal moved,
  dig or place error, cost of the next move rose by more than a limit, position error above a
  limit, no progress for a time (3.5 s in mineflayer; tuned).
- **Reuse:** try to lift the A*, moves and cost code of Azalea (MIT) behind our own state types; if
  it is bound to the ECS too tightly, write our own following the same cost model. Baritone is
  LGPL: ideas only (expected-tick costs, segmented plans, cached 2-bit chunks, refusing
  interruption through `isSafeToCancel`). Mineflayer's `prismarine-physics` (MIT) or
  `azalea-physics` for the forward prediction.
- **Bridge-edge sneaking and mid-air jumps** are `unsafe_until` actions (session 4).

## Left open, with owner
- Cost numbers, maximum drop, progress limits and N tries: measurement during implementation.
- Whether the Azalea planner can be separated: a small spike with the first code.
- Style parameters of the movement layer: session 13.
