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

## 2026-11-09 chat spike (macOS)

The talk design left two facts unmeasured and both are load-bearing (draft risks 1 and 2, critique
findings 1 and 8). Measured here on the pinned 26.3 server (protocol 777,
`third_party/minecraft/server-26.3.jar`) with two headless clients on one machine. Code:
`spikes/chat/` — `chat-spike` logs in, sends one `ServerboundChat` and one `ServerboundChatCommand`
(`/me`), and prints one JSON line per packet; `run_chat_spike.py` owns the harness `TestServer`,
asserts the result and deletes its run directory.

Server: `TestServer` defaults (`harness/praxion_harness/server.py:150-166`) plus
`online-mode=false`, `enforce-secure-profile=false`, `max-players=4`, `view-distance=4`, fresh seed
911. Both play `Login` packets reported `online_mode=false` and `enforces_secure_chat=false`: the
flag the body used to see only in fixtures (`body/crates/world/src/tests.rs:76`) now has a measured
live value.

### 1. The offline server accepts unsigned chat

A sent, after both clients were in play:

```
ServerboundChat { message: "chat spike: unsigned line from A", timestamp: <unix ms>, salt: 0,
                  signature: None, last_seen_messages: { offset: 0, acknowledged: <empty>, checksum: 0 } }
```

It was accepted. The server logged `[Not Secure] <A> chat spike: unsigned line from A` (vanilla's
marker for an unsigned message) and broadcast it to both clients. The whole `say` path therefore
does not need signing, a session key, or a `chat_session_update` — the fallback to "/me only" or
"console only" is not needed.

`ServerboundChatCommand { command: "me waves at the spike" }` was accepted too (logged
`[Not Secure] * A waves at the spike`), which matters only for the next section.

### 2. A player's message arrives as `ClientboundPlayerChat`, and `sender` is the identity

The listener (B, standing in for the user's own client, connected straight to the server) received
A's line as `ClientboundPlayerChat`. Decoded fields, exactly as they arrived:

| Field | Value |
|---|---|
| `sender` | `b76d4ad3-8d01-3150-b333-68cfb4257774` — A's offline UUID |
| `body.content` | `chat spike: unsigned line from A` |
| `signature` | `None` (`Option<MessageSignature>`) |
| `unsigned_content` | `None` |
| `filter_mask` | `PassThrough` |
| `index` | `0` — per-sender counter |
| `global_index` | `0`, then `1` for the next broadcast — per-connection counter |
| `body.timestamp`, `body.salt` | as the server sent them (`salt = 0`; the timestamp did not equal the client's own field); neither is identity |
| `body.last_seen.entries` | empty |
| `chat_type.chat_type` | `Reference(ChatKind { id: 0 })` — registry index 0, `minecraft:chat` (`chat.type.text`, renders `<A> text`) |
| `chat_type.name` | `A` (the sender's display name as a component) |
| `chat_type.target_name` | `None` |

The sender receives its own line back on its own connection in the same packet with the same
fields — vanilla does this so the sending client can render its own message. Nothing in the packet
says "this is mine", so a client recognises itself by `sender == own UUID`.

B's line arrived at A the same way, with `sender = f44cd58d-7902-30e7-8bde-679d783ee631` (B's
offline UUID) and `index = 0`. So **the operator filter is `ClientboundPlayerChat.sender` compared
against a UUID set**; the message text is `body.content`.

The offline UUID is derived from the name, not looked up: `b76d4ad3-8d01-3150-b333-68cfb4257774` is
exactly `UUID.nameUUIDFromBytes("OfflinePlayer:A")` (MD5, version 3, RFC 4122 variant), and the same
holds for B. With `online-mode=false` the operator table can therefore be built from the configured
player name alone, with no tab-list round trip.

### 3. `/me` is the one disguised case

The `/me` line reached both clients as `ClientboundDisguisedChat`:

| Field | Value |
|---|---|
| `message` | `waves at the spike` (a component; the whole line renders as `* A waves at the spike`) |
| `chat_type.chat_type` | `Reference(ChatKind { id: 1 })` — vanilla's chat_type registry index 1, i.e. `minecraft:emote_command`, the entry whose translation key is `chat.type.emote` (`* %s %s`) |
| `chat_type.name` | `A` |
| `chat_type.target_name` | `None` |
| `sender` | **absent** — the packet has no UUID field at all |

The ids are registry indices, not names: a client that wants the key has to keep the chat_type
registry from the configuration `RegistryData` packet. The rendering settles it here — the server log
line was `* A waves at the spike`, which is `chat.type.emote`, not `chat.type.text`.

So a command line has no identity field: only `chat_type.name`, a display component, compared as
text. Player-typed chat took the player path here even though it was unsigned, while `/me` took the
system-message path; server and console output (`system_chat`) has no sender either, which is the
right default: neither is the user.

Answer to the question the design asked: **`ClientboundPlayerChat`, field `sender`, for anything the
user types as chat; `ClientboundDisguisedChat.chat_type.name` only for `/`-commands, which the
operator filter should ignore or match by name.** The `ServerboundChat` tee is not needed for the
inbound side at all, which also settles critique finding 1: the user's connection never passes the
forwarder (`harness/praxion_harness/autonomy.py:36`), and it does not have to.

### What this does not settle

- A real vanilla client in offline mode was not run here. Both clients sent unsigned chat; a signed
  client would arrive as the same `ClientboundPlayerChat` with `signature` present — the server
  chooses the packet from the message shape, not from the client — but that is reasoning, not a
  measurement.
- How the server normalises the case of a login name was not tested. Keying the operator table by
  UUID avoids the question.
- A user connected through the forwarder (the other topology) was not measured; it is no longer
  needed for chat.

### Reproduce

```sh
cd spikes/chat
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo build
python3 run_chat_spike.py            # starts the server, runs both clients, asserts, cleans up
```
