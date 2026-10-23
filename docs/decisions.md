# Decisions

Agreed decisions, newest first. Each entry: date, decision, reason, what it rules out.
Proposals and ideas stay in `notes.md` until they are decided here.

## 2026-10-23

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
