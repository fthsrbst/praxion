# Notes

Dump space for the user's ideas and for agents' notes, observations and proposals.
Newest first. Prefix each entry with a date and author. Nothing here is a decision
until it is recorded as one. Promote decisions out of this file when they are made.

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
