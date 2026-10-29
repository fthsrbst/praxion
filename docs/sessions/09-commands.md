# Session 9: Player commands and task management

Decided 2026-10-29. Inputs: survey/01 idea 8 (one decision drives chat and action), Odyssey's
tech-tree solver and altoclef's `ResourceTask` (systems-review), decision-models.md (chat is an
injection surface), sessions 2.4, 5, 7 and 8.

## Facts that shape the design
- The proxy sees the user's typed chat as a serverbound packet, so reading a command needs no OCR.
- Praxion can only write to in-game chat by typing: the movement stops for about 1 to 3 s, and in
  player-bound mode the message appears as the user's own character speaking.

## Decided (user, 2026-10-29)
- **Command channels: in-game chat with a prefix, and the separate interface.** A prefixed message
  (for example `p: collect wood`; the prefix is configurable) is a command; other chat stays
  chat. The separate interface is the window of 2.4 with a text box. Voice is left out of the first
  version and can be added later.
- **Ambiguous commands.** Low risk: Praxion guesses, starts working and asks at the same time
  without blocking ("collecting iron, is that what you meant?"); if the user answers differently
  it switches. High risk: it asks first and waits. The user added the non-blocking question to the
  proposal.
- **Reports go to the separate interface first.** In-game chat only gets short acknowledgements,
  and only at safe moments. Every claim refers to a receipt, so what Praxion says matches what it
  does.
- **A new command suspends the old one and the old one resumes afterwards** (Praxion announces it;
  after the new command it re-reads the world and continues). "Stop" and "cancel" always work.
  Rejected: a queue (urgent commands wait) and cancelling the old command (half-done work lost).

## Minor picks by Claude (user may overrule)
- **Free text becomes a closed command.** Jev (session 8) maps text to a verb plus typed
  arguments from a fixed list; code validates every argument. Unknown verb: Praxion says it does
  not know the command (learning by chat is session 11). Only the user's own player is obeyed.
  Chat is an injection surface, so no free text ever reaches a planner as an instruction.
- **What counts as high risk** comes from the user's ruleset (protected items, regions) plus
  irreversible or expensive actions (spending diamonds, enchanted gear, boss fights).
- **"Get X" commands** use a recipe and tech-tree solver generated from game data (the Odyssey
  idea, regenerated from Mojang data), compiled into a rule-list skill (session 5) by the
  compiler that derives the guards.
- **Always-available commands:** stop, cancel, status ("what are you doing?"), carry on.
- **Day-one candidates:** collect an item or block; craft; smelt; go to a named place or to me;
  follow me; come here; sleep; eat; store items in a chest; defend me. The final command suite
  with proxy-verified success conditions is fixed in session 14.
- **Report events:** accepted, finished, failed with a typed reason (session 4), a question for
  the user, uncertainty about a remembered fact (session 3). No other chatter.
- **Praxion's in-game messages** carry a visible tag; the hook flags tell them apart from the
  user's typing. The name and tag are decided in session 13.

## Left open, with owner
- Voice channel: later. Personality, name, language of replies: session 13.
- Goal generation when no command is active: session 10. Teaching new commands by chat: session 11.
- The final command suite and its success conditions: session 14.
