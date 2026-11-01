# Notes

Dump space for the user's ideas and for agents' notes, observations and proposals.
Newest first. Prefix each entry with a date and author. Nothing here is a decision
until it is recorded as one. Promote decisions out of this file when they are made.

---

## 2026-11-01 - OpenCode DeepSeek V4.1 Flash - s8b live: four driver/timeout bugs behind the skill failures

- `praxion-live` in skill mode finished the whole skill on the first primitive outcome, twice: the
  `runner.outcome()` precheck and the post-tick `result.outcome` check. The interpreter never advanced.
  Both finishes are now gated to `LiveMode::Primitive`; the outcome goes to the bridge.
- The interpreter's flight deadline came from the rules registry and was shorter than the primitives'
  own waits: place 20 vs a 40-tick receipt, craft 20 vs 140 take, eat 10 vs 32+ ticks, attack 20 vs 114
  in the live table, equip 5. The interpreter retried and dropped actions. Defaults now cover the
  internal bounds (mine 180, place 80, craft 300, eat 180, attack 420, equip 80, observe 100), and a
  timeout cancels the runner (`Runner::cancel`) instead of letting it run beside its retry.
- `move_to` lost a chosen block goal when a replan happened out of sight (pool: `no_target`) and a cell
  two blocks below the marker counted as "near" (303 stairs stopped under the plateau). The goal is
  cached and the radius is 1.5.
- The planner could not climb out of a flush 2-deep pool: its climb branch required water beside the
  body instead of the land top one block up (`planner/src/moves.rs`). Fixed with a unit test.
- Receipts did not understand tag items (`craft(#planks)` never confirmed) and a place whose block
  update arrived before registration expired `missing_block`. Both fixed with tests; the 10,000-action
  fault injection still reports 0 false confirmed.
- Free-spawn skills still fail: `mine(#logs)` searches 5 blocks and no skill explores; the interpreter
  retries the failed child forever (900 ticks/45 s, 29 no_block, outcome null). In the backlog.

---

## 2026-11-01 · Claude Opus 5.5 · Free models on Demo-0: good reviewers, weak long coders

- Beam Free: a strong source survey (Azalea headless, every claim with file:line) and a good
  independent tester (9 new forwarder tests, 4 real findings, a live status ping). But on the harness
  (a long coding job) it timed out after 60 min with no report.
- Iceman Contributor Free: the server survey left easy checks "unverified"; the headless login attempt
  timed out after 80 min with code that does not compile (pulled azalea-auth and bevy). Reviews only.
- DeepSeek v4.1 Flash finished every coding job it got (manifest, move, forwarder, decoder, harness,
  Azalea patch). Default: DeepSeek writes, Beam tests and reviews, Iceman takes small reviews.
- DeepSeek hand-writes what a mature crate already does when a brief does not forbid it: a 590-line zlib
  inflater and a ZIP reader in two days. Every coding brief now says: use mature crates (flate2, zip,
  serde_json); never hand-write a parser or codec.
- An independent test that "flaked once" failed 8 of 15 in a loop. Always loop a reported flake.
  Root cause: on Windows a socket handle duplicated with `TcpStream::try_clone` can stay "not
  connected" (WSAENOTCONN) for `shutdown` while reads and writes work, so the half-close FIN is never
  sent and the connection hangs. The forwarder now shares one handle per socket through `Arc` and tears
  the pair down if a half-close fails. Watch for `try_clone` + `shutdown` in any other socket code.

---

## 2026-11-01 · Claude Opus 5.5 · Never let a worker edit a script that running workers execute

Three parallel `oc-run.sh` runs died with exit 2 ("syntax error near `)'") when a fourth worker edited
`oc-run.sh`: bash reads a script lazily, so a running instance picks up the half-changed file. The
workers themselves finished (the service keeps going); only the wrapper crashed. Edit shared tooling
only when no run uses it, or copy the script per run.

---

## 2026-10-31 · Claude Opus 5.5 · Lessons from the first Windows 2.7 runs

- Free workers claim "all fixed" when they are not: a hands fix left a ring buffer, sliced waits and the
  watchdog pipe defined but never called (`cargo build` dead-code warnings gave it away). Ask for a
  finding -> file:line -> "on the real path" table, and require zero warnings.
- Worker self-tests can test a copy of the code (the first proxy self-test did), and a passing self-test
  missed a Windows-only bug (accepted sockets inherit the listener's non-blocking mode). Only the live run
  against the real client caught it. Always validate spikes against the real game.
- Periodic experiment timing can phase-lock to the game tick (450 ms gaps = 9 ticks) and hide variance.
  Use gaps that are not multiples of 50 ms.
- Workers also rewrote `.gitignore` and recreated a deleted crate-local config; check `git status` and
  ignored-file rules after every batch.

---

## 2026-10-29 · Claude Opus 5.5 · Lessons from the first multi-model review round

- OpenCode workers on free models ignore "read-only" in the brief: they wrote scratch files into
  the repo root and one used Playwright MCP. Inspect `git status` after every batch. Proposal:
  a read-only OpenCode agent (`.opencode/agents/reviewer.md` with edit denied) for review work.
- OpenCode `websearch` failed in every run (19 errors); `webfetch` worked. Web research belongs to
  the Claude subagents until this is fixed.
- Free verifiers (Iceman, Beam) refuted 0 of 45 clusters and rated most as "partly". They are good
  for severity down-ranking and for finding where the docs already answer a finding, but they are
  no filter. Keep the Opus adjudicator.
- Beam Free merged 204 findings with full ID coverage; Iceman missed 65. Prefer Beam for long-context merges.

---

## 2026-10-29 · User, answered by Claude Sonnet 5.5 · Reinforcement learning for some skills (idea, not a decision)

The user asked whether to train an RL model for some jobs. Not part of the first demo (session 8).
Candidates, in order of fit: (1) combat micro, bridging, water-bucket clutch: narrow, 20 Hz,
could replace hand-written Rust primitives, which stay swappable; (2) imitation of the user's own
motor style for naturalness (survey/01 idea 1; imitation, not RL); (3) a high-level decision
policy: not realistic for this project. Obstacles: Minecraft is slow to train on, "zero cheats"
limits shortcuts, RL policies tend to look robotic, and the user must understand every part.
Revisit after baseline measurements, for the primitive where the hand-written version falls short.
Unchecked: tick acceleration with `/tick rate`, existing RL environments (MineRL, MineDojo,
Craftium), VPT and PLAICraft data licenses.

---

## 2026-10-21 · User (recorded by Claude Opus 5.5) · Goals for Praxion

In the user's words, translated:
1. **Technical plan:** Jev with LLMs behind it. A better proposal is welcome.
2. **Feel:** the agent must feel like a natural player.
3. **Aim:** an agent system that thinks for itself and plays on its own, in two ways:
   - **Companion:** joins the user's world as a separate player.
   - **Player-bound:** the user hands over their own screen, i.e. their own character, and the agent plays it.
4. **Commands:** the player can give direct commands ("collect ore" and the like) and the agent must accomplish them.

## 2026-10-21 · Claude Opus 5.5 · Implications of the goals (proposals, not decisions)

- **Two modes mean two bodies.** Companion mode works through a separate protocol client (what
  mineflayer does). Player-bound mode needs control from inside the user's real client: a client
  mod (as Baritone and altoclef do on Fabric) or screen in, keyboard/mouse out. Mineflayer cannot
  serve the second mode at all. A body port, so one brain plugs into either body, belongs in the
  architecture from day one.
- **"Natural player" decides perception.** Reading blocks through walls breaks the feel. This
  points to a fair-play perception policy with provenance tags (Craftonomous). For movement,
  mineflayer-pathfinder's `human.js` calibration and look quantization become product requirements.
  No operator commands follows from the same goal.
- **"Must accomplish commands" means reliability and verification.** Frequent commands (collect,
  fetch, mine, craft) need deterministic, verified skills rather than LLM improvisation; Odyssey's
  tech-tree solver and altoclef's `ResourceTask` are direct precedents. How to read an ambiguous
  command is its own decision.
- **Playing alone also needs goal generation, long-term memory and self-assessment.** Voyager's
  curriculum does this but only proposes goals its inventory-based critic can check.
- **Commands, the agent's own plan and reflexes must meet in one arbiter.** In altoclef the user
  task sits at fixed priority 50, below survival reflexes.
- **Jev fits the middle layer, not the tick loop.** A hosted round trip of 70–500 ms cannot serve
  50 ms reflexes (lava, falling, low health); those stay in local code. Jev suits second-scale
  tactical judgements: is this mob a threat, flee or fight, is a chat message a command and which
  skill does it map to, does this need the big LLM.

## 2026-10-21 · Claude Opus 5.5 · Repo bootstrap

- `AGENTS.md` is the single source of truth; `CLAUDE.md` only imports it (`@AGENTS.md`).
  Reason: OpenCode v2 reads only `AGENTS.md` and Claude Code reads `CLAUDE.md`, so an import keeps
  them in sync by construction. A copy-and-sync scheme would drift.
- OpenCode v2.1.2 is installed. The free `minimax-m3.1-flash-free` works through both `opencode/` and
  `opencode-go/`. It answered a small bug-spotting task correctly in 5 s at cost 0.
- The global `opencode-cli` skill (`~/.agents/skills/opencode-cli`) was written for OpenCode v1 and
  Yele's old model ladder, so it is out of date for v2. For example, v2 has a shared background service
  and permission rules are a list of `{action, resource, effect}`.
  The project skill `opencode-worker` replaces it here. The global skill should get a v2 update later.
- `opencode.json` deny rules hold under `--auto`. One gap: shell commands that use `~` paths
  skip the external-directory check.
- Proposal: have the first review round right after the architecture draft, using a model other than the drafter's.
