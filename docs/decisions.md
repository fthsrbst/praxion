# Decisions

Agreed decisions, newest first. Each entry: date, decision, reason, what it rules out.
Proposals and ideas stay in `notes.md` until they are decided here.

## 2026-10-21

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
