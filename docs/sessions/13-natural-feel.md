# Session 13: Natural player feel

Decided 2026-10-29. Inputs: survey/01 (personal motor model, camera dynamics), survey/02 (camera-only
fillers), systems-review (mineflayer-pathfinder `human.js`), decisions 1.6, sessions 6, 8, 9, 11.

## Three layers
Movement (camera, speed, overshoot, key overlap), decisions (pauses, curiosity, waiting) and
conversation (language, tone, name). Movement comes first: it is the most visible at hand-over and
the only layer with a measure already decided (1.6: movement statistics against recorded human play,
plus a blind test with viewers).

## Decided (user, 2026-10-29)
- **Movement style: a general human style.** The user chose this over "learn the user's own
  style". The movement-style layer (session 6) uses human defaults, not a model fitted to the
  user. Rejected: a robotic efficient style (it fails the naturalness measure). The recording of the
  user's play (session 11) continues but is used as the reference for evaluation, and personalising
  the style from it stays a later option.
- **Human limits, no fatigue.** Visual reaction delay has a floor in the human range (about
  150 ms), aim speed and accuracy follow human statistics, and Praxion does not tire or fake breaks
  (it sleeps only when the game needs it). Lethal-danger reflexes (lava, falling) may act faster
  than that floor, because safety wins there (session 7). Rejected: full human imitation with
  fatigue and AFK (hurts flawlessness) and unlimited speed (fails the blind test).
- **Language: the language the user writes in.** Turkish gets Turkish, English gets English.
- **Tone: short, calm, informative,** no jokes, little chatter. Name and tag `Praxion`, configurable.

## Minor picks by Claude (user may overrule)
- **Priority:** movement, then decisions (pauses, camera-only waiting; session 8), then conversation.
- **No scripted mistakes.** Imperfection comes only from the human-style movement model (overshoot,
  settle time, reaction delay). Death is allowed (decision 1.4) but never staged; every death is
  studied and becomes a test case (session 7).
- **Parameter sources:** `human.js` from mineflayer-pathfinder (MIT; calibrated on 48 recorded
  players: head-turn distributions, sprint delay, jump rate, pure-pursuit steering) for defaults. The
  camera controller of rmalde (velocity-limited P-controller) has no licence: ideas only.
- **Style never overrides correctness:** precision tasks (a water-bucket save, an edge) override the
  style where the action contract demands it.
- **Measures (from 1.6):** movement statistics tested against held-out human sessions (KS or
  energy-distance tests not significant; a detector's AUC at most about 0.6), and a blind test using
  OBS recordings of the real window in a paired forced-choice design (not ReplayMod).
- **Default language when the message language is unclear:** Turkish.

## Left open, with owner
- The numeric defaults of the style layer and whether the human defaults pass the statistics: first
  code and session 14 measurements.
- Whether to personalise the style from the user's recordings: after baseline measurements.
- A voice (speech) for Praxion: later, together with the voice command channel (session 9).
