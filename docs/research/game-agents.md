# Skill-writing game agents: state of the art

Research date: 2026-10-21. `[V]` checked against a primary source, `[U]` not verified.
Numbers are the authors' own unless stated.

## Minecraft agents that build skill libraries

| Work | Core idea | Skills | Result | Code |
|---|---|---|---|---|
| Voyager (2023) [V] | Curriculum + executable JS skill library on Mineflayer + self-verification | Embedding-indexed flat list | 3.3x unique items vs baselines; diamond in 1/3 runs in a 2026 replication | MIT, unmaintained since 2024-04 |
| ODYSSEY (IJCAI 2025) [V] | Voyager + 40 primitive / 183 compositional skills + Minecraft-tuned LLaMA-3 | JS/JSON, MiniLM retrieval | | MIT; wiki data CC BY-NC-SA |
| Programmatic Skill Networks (2026-01) [V] | Skills call each other as a graph; REFLECT assigns failure blame; maturity-gated updates; refactors rolled back on regression | Program graph with per-skill score V(s) | Diamond 51±9 iterations, 3/3 runs (Voyager: 102, 1/3), on gpt-5-mini | Not released |
| MineEvolve (NeurIPS 2026) [V] | Monitor → Inducer → Curator → Adaptor; learns skills from successes and remedies from failures, no weight updates | Validated, merged, retrieved | Gains on MCU long-horizon tasks | Thin repo, no license |
| GITM (2023) [V] | Text-based hierarchical planner with decomposition memory | Structured actions | +47.5% ObtainDiamond, CPU only | No license, stale |
| JARVIS-1 (2023) [V] | Multimodal memory planner over trained controllers | | 12.5% diamond pickaxe | No license |
| Mindcraft / MineCollab (NeurIPS 2025) [V] | LLM + 47 Mineflayer tools; `!newAction` writes code (off by default) | Ad-hoc code | Multi-agent collaboration drops >15% when plans must be communicated | MIT, very active, MC ≤ 1.21.11 |

Also: Echo (experience transfer, 2026), MineNPC-Task (LLM writes Mineflayer JS, reviewer + capped repair), Project Sid (1000+ agents, preliminary).

## General game agents beyond Minecraft

| Agent | Interface | Learns new skills how | Notes |
|---|---|---|---|
| SIMA 2 (DeepMind, 2025-12) [V] | Screen + virtual keyboard/mouse | Gemini proposes tasks and rewards, self-improvement loop | Closed preview |
| Lumine (ByteDance, 2025-11) [V] | Pixels 5 Hz in, keyboard/mouse 30 Hz out, 7B VLM | Imitation only | Finished Genshin's 5-hour storyline; first action ~114 ms after heavy inference engineering |
| Game-TARS (ByteDance, 2025-10) [V] | Native keyboard/mouse | Large-scale training | Sparse thinking beat thinking every step (63% vs 45%) |
| NitroGen (NVIDIA, 2026-01) [V] | 256px frame → gamepad | Imitation on 40k hours, 1000+ games | Open weights but **non-commercial**; last frame only, a "System 1" |
| Cradle (BAAI, 2024) [V] | Screenshots → keyboard/mouse | Generates and curates code skills | Closest non-Minecraft analogue to Voyager; MIT, stale |

## Minecraft interfaces and benchmarks

- **Mineflayer** [V]: MIT, very active (4.39.0, 2026-09), supports MC 1.8 → 26.1. Protocol-level client, no mod needed.
- **MineStudio** [V]: MIT, active; pixel/RL stack on MineRL (JDK 8, Xvfb).
- **MineRL** (stale), **Malmo** (archived), **MineDojo** (stale), **Craftium** (Luanti, Minecraft-like, not Minecraft).
- **Benchmarks** [V]: MCU (3,452 tasks, VLM judge), MineAnyBuild (spatial planning), MineCollab (multi-agent), MineNPC-Task, MineExplorer (hidden prerequisites), TickingCollabBench (real-time constraints), MineCEraft (construction).

## Recurring failure modes

- **Latency:** LLM response time breaks agents in dynamic settings. Working systems separate a fast acting loop from slow, interruptible planning and reason only at key moments.
- **Verification:** LLM self-checks are weak. Stronger work uses execution success, rule validators, typed environment feedback and rollback. Long dependency chains remain the hard part.
- **Memory:** short memory is a common limit (SIMA 2, NitroGen).
- **Safety:** LLM-written code is untrusted. Mindcraft's in-process sandbox is still injectable; one survey finds 26% of community skills contain vulnerabilities.
- **Evidence quality:** most skill-library results are 3 runs on one model.

## Gaps a new project could own

1. A game-agnostic skill layer. All skill-writing work is Minecraft-specific.
2. A reusable verification harness: pre/postconditions, regression tests per skill, rollback.
3. A fast reflex/decision layer under a code-writing planner. Nobody has demonstrated both together with learning.
4. Test-time skill learning for pixel-based games.
5. Reproducible, cost-reported evaluation with many seeds and small models.
6. A safe runtime for LLM-written skills (OS-level isolation + capability limits).

## Sources

Voyager https://arxiv.org/abs/2305.16291 · ODYSSEY https://github.com/zju-vipa/Odyssey ·
PSN https://arxiv.org/abs/2601.03509 · MineEvolve https://arxiv.org/abs/2603.13131 ·
Echo https://arxiv.org/abs/2604.05533 · GITM https://arxiv.org/abs/2305.17144 ·
JARVIS-1 https://arxiv.org/abs/2311.05997 · Mindcraft https://github.com/mindcraft-bots/mindcraft ·
MineCollab https://arxiv.org/abs/2504.17950 · Project Sid https://arxiv.org/abs/2411.00114 ·
MineNPC-Task https://arxiv.org/abs/2601.05215 · SIMA 2 https://arxiv.org/abs/2512.04797 ·
Lumine https://arxiv.org/abs/2511.08892 · Game-TARS https://arxiv.org/abs/2510.23691 ·
NitroGen https://github.com/MineDojo/NitroGen · Cradle https://arxiv.org/abs/2403.03186 ·
Mineflayer https://github.com/PrismarineJS/mineflayer · MineStudio https://github.com/CraftJarvis/MineStudio ·
MCU https://arxiv.org/abs/2310.08367 · MineAnyBuild https://arxiv.org/abs/2505.20148 ·
MineExplorer https://arxiv.org/abs/2605.30931 · TickingCollabBench https://arxiv.org/abs/2606.15684 ·
Parallelized planning-acting https://arxiv.org/abs/2503.03505 · Skill security survey https://arxiv.org/abs/2602.12430
