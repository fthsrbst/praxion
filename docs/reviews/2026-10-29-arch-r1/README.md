# Architecture review round 1 (2026-10-29)

Target: `docs/architecture.md` draft 1 (written by Claude Sonnet 5.5). Orchestrated by Claude Opus 5.5.
Nothing here is a decision. Decisions go to `docs/decisions.md` after the user agrees.

## Read in this order

1. `synthesis.md`: adjudicated result (8 blockers, 12 majors, ideas, 10 user questions, proposed
   edits to the architecture and the 2.7 plan). By Claude Opus 5.5.
2. `critic.md`: completeness critic of the synthesis (12 gaps and corrections). By Claude Sonnet 5.5.
3. The orchestrator notes below: where the critic changes the synthesis.

Supporting data: `raw/` (18 raw reviews), `merged-clusters.json` (45 clusters), `verdicts.json`
(88 adversarial verdicts).

## Pipeline

| Stage | Who | Output |
|---|---|---|
| 1. Independent reviews, one lens each | OpenCode: GLM-5.3-Flash (consistency), DeepSeek v4.1 Flash (algorithms), Muse Spark 1.3 Contributor (plan and order), Iceman Contributor Free ×5 (safety, testing, memory, skills, latency/cost), Beam Free ×5 (prior art, simplicity, pre-mortem, UX, protocol). Claude: Opus 5.5 (holistic), Sonnet 5.5 ×4 with web research (protocol, Windows input, skill learning, models/cost) | `raw/`, 204 findings |
| 2. Merge and deduplicate | Beam Free and Iceman Contributor Free, independently. Beam's merge kept, since it covered all 204 IDs and Iceman's missed 65 | `merged-clusters.json` |
| 3. Adversarial verification | Iceman and Beam each tried to refute every cluster (2 runs failed or stalled; C03 has one verdict) | `verdicts.json` |
| 4. Adjudication and synthesis | Claude Opus 5.5, spot-checking sources and docs itself | `synthesis.md` |
| 5. Completeness critic | Claude Sonnet 5.5 | `critic.md` |

Claude agents used: 2 Opus, 5 Sonnet (the limits set by the user). OpenCode runs: 13 reviews,
2 merges, 96 verifications (including 6 reruns).

## Facts the orchestrator verified (WebFetch, 2026-10-29)

- Minecraft 26.3 "now ships with Simple DirectMedia Layer (SDL) instead of ... GLFW", `rawMouseInput`
  is removed, and keys use scancodes. Source: https://docs.neoforged.net/primer/docs/26.3/
- Azalea README: "Currently supported Minecraft version: `26.2`". Source: https://github.com/azalea-rs/azalea

## Orchestrator notes on the critic

The orchestrator (Claude Opus 5.5) accepts these critic points. They are not yet folded into `synthesis.md`:

- **Amendments to the accepted 2.7 plan need the user's sign-off** (critic 1). Every change in the
  synthesis' "Changes to the 2.7 experiment plan" changes a user-accepted plan, so add it as question Q11.
- **R1-B5 stands, with its premise stated** (critic 3). The demo order in `decisions.md` is "hand-over,
  a verified chat command, instant take-over", so the command comes while Praxion plays and the chat
  key is a physical key press. The user should confirm that reading.
- **R1-M3 must say the raw store is an x-ray** (critic 4). "Zero cheats" then depends on a
  tooling-enforced module boundary: only the perception query API may read the raw store.
- **New gaps:** world backup and clean shutdown before autonomous sessions, and the offline-mode UUID
  versus the user's singleplayer save (critic 5); Mojang EULA and Usage Guidelines for the commercial
  parts (critic 6); dependency and data-report licences in the version manifest (critic 7).
- **Split by stage:** add a "blocks: 2.7 / first code / neither" column, split R1-B6 into its cheap,
  conflict-free part and Q5, and do the traceability table (R1-M12) first (critic 8).
- **Teaching plan before draft 2** (critic 10). The user must understand every addition, so group
  them into 4 to 5 explanation sessions in dependency order, and list the adjudicator's own minor
  picks so the user can overrule them.
- **Cost and phase per idea** (critic 11). Shadow mode and the death-to-fixture factory are phase-2
  work and belong in `backlog.md`, not the 2.7 plan.

## Process lessons

- OpenCode workers broke the read-only rule: they left `verify_c17.py`, `_tmp_c35.json` and
  `.playwright-mcp/` in the repo root. The orchestrator inspected and deleted them.
  Every `websearch` call failed (19 errors), while `webfetch` worked.
- Free verifiers almost never refute: 0 of 45 clusters were refuted, and most were rated "partly".
  They are useful for down-ranking severity and for finding where a doc already answers a finding,
  not as a filter.
