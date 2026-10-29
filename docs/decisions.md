# Decisions

Agreed decisions, newest first. Each entry: date, decision, reason, what it rules out.
Proposals and ideas stay in `notes.md` until they are decided here.

## 2026-10-29

- **Languages: Python plus a Rust core.** Python for the brain (LLM and Jev calls, skills); Rust
  for the wire and input path. Rust chosen over C++ (Azalea decoder; user confirmed). Decoder
  library Azalea, to be verified in experiment 2.7. Exact boundary decided in 2.5.
  Session 2.6, [sessions/02-body-connection.md](sessions/02-body-connection.md).
- **Hands: Windows `SendInput`, scan codes, key bindings read from `options.txt`.** No driver-level
  injection, so Praxion can tell its own input from the user's. Session 2.3.
- **Mouse has two modes.** In the world: relative moves, calibrated in a closed loop against the
  rotation the proxy sees. In menus: absolute cursor positioning, with wrong clicks caught by the
  slot number in the container-click packet. Session 2.3.
- **Focus loss: stop and tell the user.** Release all keys, stop safely, never take focus back by
  force. Praxion's own windows never take focus. Session 2.3.
- **Take-over ignores tiny mouse jitter.** Any key or mouse-button press, wheel turn or clear
  mouse movement takes control back; very small drifts are ignored (threshold tuned in
  experiment 2.7), so the user may rest a hand on the mouse. Session 2.4.
- **After a take-over, the interrupted task waits.** Praxion remembers it, does not play until the
  user says "carry on", then re-reads the world and re-plans if it changed. It never resumes from
  its own memory of the state. Session 2.4.
- **"Praxion is active" indicator: a small separate window outside the game area**, never
  stealing focus, nothing drawn over the game. Answers the open point from session 1. Session 2.4.
- **Brain and body are separate programs** (Python brain, Rust body) joined by a local connection:
  the body survives a brain crash, the brain can restart without dropping the game connection,
  and tests and replays use the same interface with a fake world. Session 2.5.
- **The brain may run on another computer.** Default is this PC only (loopback); network access
  needs an explicit key. Session 2.5.
- **If the brain goes silent, the body stops safely and tells the user:** finishes the current
  step to a safe point, stands still with basic survival reactions on. Which reactions: session 7.
  Session 2.5.
- **Seeing = line of sight in every direction within a distance limit** (about 64 blocks,
  tunable); a wall hides what is behind it. Camera direction is ignored. Session 3.
- **Hidden data: what the F3 screen shows is allowed, everything else is denied** (a mob's exact
  health, the dragon phase counter). Every privileged read goes to a ledger. Session 3.
- **The live world state lives in the Rust body**; the brain asks short queries and receives
  events, never raw chunks. Reason: the body must survive a silent brain and reflexes need the
  state within 5 ms. Session 3.
- **Remembered facts carry age and source, are re-checked when in line of sight, and Praxion tells
  the user when it is unsure.** Session 3, [sessions/03-perception-world-model.md](sessions/03-perception-world-model.md).
- **An action succeeds only with the server's word** (level 2 for world changes, level 3 when an
  effect is expected). No confirmation gives `unconfirmed`, then re-read the truth, then retry.
  Session 4.
- **Unverifiable tasks (not computable within the perception policy) are marked `unverifiable`,
  never counted as successes.** Session 4.
- **Safe take-over exception:** each action declares a cancellation class; in a dangerous moment
  (bridge-edge sneak, mid-air jump) its needed keys stay held until the user touches them or the
  moment passes. Refines the 2.4 rule "release all keys". Session 4,
  [sessions/04-action-verification.md](sessions/04-action-verification.md).
- **Skills are teleo-reactive rule lists (`condition -> action`) that the Rust body interprets**, so
  the body keeps playing while the brain thinks. Delicate micro behaviour stays in hand-written
  Rust primitives. Session 5.
- **LLM-written skills only combine existing primitives and skills.** Narrows the 2026-10-21
  decision "LLMs may write code, for skills": they write rule lists, not primitives. Session 5.
- **The compiler derives the guards** from primitive effects (generated from game data); the LLM
  writes steps and a goal. Session 5, [sessions/05-skills.md](sessions/05-skills.md).
- **Jev is the middle tier and is wired in by measurement:** only on options the code lists,
  deadline above about 1 s, compared with rules and the LLM at each decision point. Reflexes and
  anything under 1 s never depend on Jev or any hosted call. Session 8.
- **The LLM is called by events (closed list of causes plus a ~60 s heartbeat), not by a clock.**
  Session 8.
- **Cost brake: track and warn above a user-set threshold; never cut automatically.** Every call
  class has a deadline and a fallback (stop safely). Session 8.
- **Smoothness is measured as a starvation gap** (goal active, no declared wait, no packets), not
  as the longest gap without input. Amends measure 3 of the 2026-10-23 success measures.
  Session 8, [sessions/08-jev-llm-brain.md](sessions/08-jev-llm-brain.md).
- **Reinforcement learning is a noted idea (see `notes.md`), not part of the first demo.**
  Primitives stay swappable. Session 8.
- **Arbiter tiers: survival reflex, then the user's command, then maintenance, then own
  projects, with a soft and a hard threshold per need.** A need waits for the command's next safe
  point until its hard threshold promotes it. Same-tier switches need a clear gain. Session 7.
- **Reflex settings are a fixed default (about 10 typed fields) the user can edit; the LLM does
  not tune them in the first version.** Session 7.
- **First reflex set: the first night, extended at every ladder rung**, tested with a coverage
  matrix judged by the test harness, not by Praxion. Session 7,
  [sessions/07-reflexes-arbiter.md](sessions/07-reflexes-arbiter.md).
- **Commands come from in-game chat with a prefix and from the separate interface.** Voice is
  left for later. Session 9.
- **Ambiguous commands: low risk, guess, start and ask without blocking; high risk, ask first.**
  Session 9.
- **Reports go to the separate interface first; in-game chat only short acknowledgements at safe
  moments; every claim cites a receipt.** Session 9.
- **A new command suspends the old one and the old one resumes afterwards; stop and cancel always
  work.** Session 9, [sessions/09-commands.md](sessions/09-commands.md).
- **A path may change the world anywhere (dig, bridge, tower) but must choose a sensible route**
  (world-changing moves cost more). Hard limits stay: ruleset-protected blocks, no digging straight
  down or into unknown blocks. Session 6.
- **Stuck handling is bounded:** retry counters, loop detection, blacklist with reset conditions,
  typed `unreachable`, then an event for the LLM. Never `/tp` or any operator command. Session 6.
- **Natural movement is a separate layer above the planner** (tuned in session 13). Session 6,
  [sessions/06-movement.md](sessions/06-movement.md).
- **A new skill goes through staged admission: static checks, then a practice room (a world copy
  on a test server, 3 runs against a no-skill baseline), then probation, then active.** Never first
  tried in the user's world. Session 11.
- **A skill learned in Praxion's own play and passing the gate becomes active without asking; a
  skill taught through chat needs the user's approval.** Session 11.
- **The user's own play is recorded locally now; learning from it comes later.** Session 11,
  [sessions/11-skill-learning.md](sessions/11-skill-learning.md).
- **Praxion chooses its own goals with a free, Voyager-style curriculum;** every goal carries a
  completion condition, and the ladder stays the yardstick. The user chose this over "the ladder
  as the spine". Session 10.
- **After a hand-over with no command: resume the suspended task, else choose a goal and say so.**
  Session 10.
- **Big risky steps (Nether, End, dragon) are announced with a short window (about 30 s) to say
  stop.** Session 10.
- **Long projects live in a project ledger on disk; progress is recomputed from the world after a
  restart.** Session 10, [sessions/10-autonomy.md](sessions/10-autonomy.md).
- **The first memory is the smallest one, built in stages:** a JSON Lines file per world plus an
  in-RAM index, places and containers, an event log with a read-only viewer, a briefing with a
  token cap. SQLite, vectors, lessons and the chunk cache wait for a measured trigger.
  Session 12.
- **Lessons come when a trigger fires, are general across worlds, and are proposed by the LLM and
  accepted by code after 3 proxy-verified successful traces.** Session 12.
- **Game knowledge: the Minecraft Wiki text is indexed and a RAG is built** (the user's choice
  over "exact data and the LLM's knowledge"). The text is CC BY-NC-SA: the index stays local and
  undistributed, and the licence conflict with an open-core model stays open. Session 12.
- **The game memory is separate from Mnema and the hub.** Session 12,
  [sessions/12-memory-rag.md](sessions/12-memory-rag.md).
- **Movement style is a general human style, not fitted to the user.** The recording of the
  user's play serves as the evaluation reference; personalising stays a later option. Session 13.
- **Human limits without fatigue:** reaction floor about 150 ms, human aim statistics, no faked
  breaks; lethal-danger reflexes may be faster. Session 13.
- **Praxion answers in the language the user writes in; short, calm, informative tone; name and tag
  `Praxion`, configurable.** Session 13,
  [sessions/13-natural-feel.md](sessions/13-natural-feel.md).
- **Tests run at three levels (fake world, protocol test body, real body), most at the bottom.**
  Session 14.
- **Evidence standard: many fresh seeds, confidence intervals, analysis written before the
  runs; a hand-picked best run is labelled as such.** Session 14.
- **Observability: a live window in the separate interface plus a replayable event log; every claim
  cites a receipt id.** Session 14.
- **Fast tests on every change, level 2 and 3 tests nightly or on request.** Session 14,
  [sessions/14-testing.md](sessions/14-testing.md).
- **The brain runs on the same Windows PC by default.** Session 15.
- **One repository; the layers are enforced by tooling.** Session 15.
- **API keys live in the operating system credential store; only the brain holds keys.**
  Session 15.
- **Licence: Apache-2.0 for the open core, commercial parts closed.** `LICENSE` added; which parts
  are commercial is still open. Session 15,
  [sessions/15-infrastructure.md](sessions/15-infrastructure.md).
- **Experiments (2.7): thresholds accepted; Windows tests at the physical PC; spike code in
  untracked `spikes/` until the license is chosen.** A missed threshold stops the work and goes to
  the user; no automatic route change. Session 2.7.
- **Mineflayer is the reference model for the body.** Not a dependency (see 2026-10-21). We take
  its ideas: the `controlState` held-keys table, rate-limited and quantized look, `human.js`
  calibration (for session 13).
- **Proxy: a separate process that forwards bytes untouched and decodes a copy.** The forwarding
  path never encodes packets, so read-only holds structurally. A decoder that loses sync marks
  perception invalid and Praxion stops safely; the game is unaffected. Rejected: terminate and
  re-encode both connections (a codec bug could corrupt the game). Session 2.2.
- **Game data comes from the Mojang data generator reports** (blocks, packets, registries).
  `minecraft-data` is the backup only. Session 2.2.

## 2026-10-23

- **Success measures:** milestone ladder to the Ender Dragon over many fresh seeds; a
  proxy-verified command suite; smoothness (never freeze while an LLM thinks, instant take-over);
  naturalness (human movement statistics and a blind test); hosted cost per hour, no cap yet.
  Targets are set after baseline measurements. Course project; schedule does not constrain
  design. Session 1.6.
- **First demo:** fresh survival world; the opening scenario is hand-over, a verified chat
  command, instant take-over, and an autonomous first night. Capability target is the whole game
  up to the Ender Dragon. Hand-over by chat command, global hotkey or a separate interface.
  Must be flawless; audience: the community and the user's professor. Session 1.5.
- **Goals when alone:** the user's projects first, then Praxion's own ideas. Goals come from
  in-game chat, a separate interface, or Praxion's own inference in autonomous mode. Death is
  acceptable. Rules (protected areas, items, limits) may be written with an LLM; proposal: code
  enforces them deterministically after the user confirms. Plays toward finishing the game; an
  LLM may decide to continue past it. Session 1.4.
- **Body direction: eyes on the network, hands on the keyboard.** A read-only local proxy
  between the user's unmodified client and a private local server gives exact perception;
  OS-level keyboard and mouse input drives the real game window. A client mod is the fallback.
  Details in session 2. Session 1.5.
- **Praxion never changes the user's game settings.** It reads them and calibrates itself.
- **The PC is dedicated to the game while Praxion plays.**

## 2026-10-22

- **First demo: Praxion plays the user's own character** (player-bound), watched on the user's
  own screen with no visible change when control passes. Touching mouse or keyboard, or closing
  Praxion, takes control back. A private local server counts as singleplayer. Implementation
  effort is not a criterion; quality is. Body route still open. Session 1.5.

## 2026-10-21

- **Survival, Normal difficulty, zero cheats.** The agent's action and perception APIs expose only
  what a player can do and see; test setup goes through a separate, logged harness. Preference:
  avoid mods; the body route is decided in session 2. Session 1.3.
- **Minecraft only; the second-game goal is dropped.** Layering stays for Minecraft's own needs
  (two bodies, version packs, offline fake world): agent layers do not import body or
  version-pack layers, enforced by tooling. Session 1.2.
- **Singleplayer only, for now.** Multiplayer servers are out of scope until decided otherwise.
  Session 1.3.
- **Java Edition, on Windows.** The demo targets the newest stable release; broad version
  support comes later. Vanilla first; block, item and recipe knowledge comes from data, not
  code. Version-specific code must stay in one layer so later versions are added, not patched
  in. Rejected: Bedrock (no client mods, blocks player-bound mode). Session 1.1,
  [sessions/01-product-scope.md](sessions/01-product-scope.md).
- **The decision layer uses TypeSafe Jev, with LLMs behind it.** How Jev and the LLMs split the
  work is open and is discussed in session 8 ([discussion-plan.md](discussion-plan.md)).
  The user is open to a better proposal.
- **Models are hosted, not local.** Reason: decision quality.
- **LLMs may write code**, for skills and similar.
- **Mineflayer is not a dependency.** Praxion is a separate project; ideas, and code where
  licenses allow, may be taken from existing systems
  ([research/systems-review.md](research/systems-review.md)).
- **Working method:** every topic is discussed in depth, one session per topic, with notes.
  The user must master each topic and its technical structure.
