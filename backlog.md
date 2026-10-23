# Backlog

Unfinished work left by agents and subagents. If you stop with something incomplete,
it goes here before the session ends. Remove an item (or move it to "Done") once it is finished and verified.

**Depth rule:** keep each entry short here. When an item needs more than ~15 lines
(investigation log, sub-tasks, findings), move the detail to `backlog/<slug>.md`
and leave a one-line link. Split further (`backlog/<slug>/<part>.md`) the same way if needed.

**Entry format**

```md
### <title>
- Source: <agent/model>, <YYYY-MM-DD>
- State: <what is done>
- Remaining: <what is left, concretely>
- Pointers: <files, branches, logs, links>
```

## Open

### Choose a license
- Source: Claude Opus 5.5, 2026-10-21
- State: The repo is public with no license (all rights reserved). Options compared in chat:
  MIT, Apache-2.0, AGPL-3.0, FSL/BSL. Leaning open-core (Apache-2.0 core, commercial parts closed).
- Remaining: Decide together with the business model, before the first code is committed.
- Pointers: `README.md` → License

### Finish the source-level systems review
- Source: Claude Opus 5.5, 2026-10-21
- State: Ten systems read; findings in `docs/research/systems-review.md`.
- Remaining: Craftonomous `src/planning/` and `docs/ARCHITECTURE.md`; VillagerAgent, Plan4MC,
  XENON, ADAM, MineLand, hermescraft, plancraft; mineflayer `blocks.js`/`entities.js`;
  protocol version differences. Do each before the session that needs it.
- Pointers: `docs/research/systems-review.md` §7

### Open points carried from session 1
- Source: Claude Opus 5.5, 2026-10-23
- State: Session 1 closed; these were raised and not settled.
- Remaining:
  - May a "Praxion is active" indicator exist outside the game window (separate window, phone)? Ask the user.
  - Verify that vanilla "Open to LAN" requires authenticated accounts (minecraft.wiki).
  - Session 2 experiments: synthetic mouse input reaching Minecraft's raw-input path on Windows;
    proxy decoding of an offline-mode connection; mouse gain formula against the 26.x source.
- Pointers: `docs/sessions/01-product-scope.md`

## Done
