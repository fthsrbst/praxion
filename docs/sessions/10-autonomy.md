# Session 10: Autonomy, goal generation and long-horizon planning

Decided 2026-10-29. Inputs: decision 1.4 (goals when alone, goal layers), survey/01 (milestone
planner, enumerated options), survey/02 (plan the next subgoal early), systems-review (Voyager
curriculum rule 8, Odyssey tech-tree solver, altoclef), sessions 4, 5, 7, 8, 9, 11.

## Goal layers (from 1.4)
Needs (eat, sleep, tools: session 7 tiers), projects (a base, a farm, a mine, a Nether trip) and
aspirations (the ladder: first night, iron, diamonds, Nether, stronghold and portal, Ender
Dragon). The user's projects come first, then Praxion's own ideas.

## Decided (user, 2026-10-29)
- **Praxion chooses its own goals with a free, Voyager-style curriculum.** The LLM proposes the
  next goal each time (session 8 events: a goal finished or failed). This is the user's choice over
  the recommended "ladder as the spine, code lists the options". Consequences kept in the minor
  picks below: every goal carries a completion condition, and the ladder still measures progress.
- **After a hand-over with no command,** Praxion first resumes a suspended task ("carry on", 2.4);
  if there is none it chooses its own goal and says so in the separate interface.
- **Big risky steps are announced, with a short window to say stop.** Entering the Nether or the
  End and fighting the dragon: Praxion announces it in the separate interface ("preparing for
  the Nether, going in about 30 s"); if the user says stop it stops, otherwise it goes. Rejected:
  asking every time (hurts autonomy and smoothness) and no announcement.
- **Long projects live in a project ledger on disk,** each as a goal, a tree of subgoals and
  progress evidence. After a restart Praxion recomputes what is done from the world, not from its
  own memory (the session 4 rule). Rejected: LLM memory and starting over each time.

## Minor picks by Claude (user may overrule)
- **A free curriculum still needs verifiable goals.** Each proposed goal must carry a completion
  condition that the world model can compute (session 4). A goal that cannot be verified may run
  but is marked `unverifiable` and excluded from success statistics. This is where Voyager's
  curriculum was limited: it proposed only goals the inventory could show.
- **The ladder is the yardstick, not the leash.** Rung progress is computed from the proxy
  whatever the LLM chooses, so the success measures of 1.6 hold; the curriculum prompt includes the
  rung status so the LLM is pulled toward progress, and a goal that keeps Praxion away from the
  next rung for a long time is logged.
- **The announcement window** is about 30 s and configurable. "Big risk" comes from the ruleset
  plus rung transitions (Nether, End, boss fights).
- **Ledger entries** hold the goal, subgoals, the receipts that prove each, the date and the
  world; progress is recomputed on load and conflicts are shown to the user.
- **Planning chains** ("get iron tools") use the recipe and tech-tree solver generated from game
  data (session 9) and compile into rule-list skills (session 5). An HTN layer above the rule
  lists is deferred until logs show thrashing or LLM calls per rung that do not fall.
- **Fair-play stronghold finding:** from the visible flight of thrown eyes of ender, with a
  Bayesian estimate over stronghold chunks; the dragon phase is inferred from what is visible
  (session 3).
- **Next subgoal is planned early** (session 8), so there is no dead time between goals.

## Left open, with owner
- The curriculum prompt and the form of goal proposals: first code and measurement.
- Behaviour toward other players: the first demo is singleplayer only (decision 1.3).
- Naturalness of what it chooses to do (building style, pauses, curiosity): session 13.
- Measuring whether the free curriculum beats the ladder spine at equal compute: sessions 14 and 15
  (an arm in the learning test of session 11).
