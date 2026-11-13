# Proposal: Praxion plays the operator's own player on macOS (first person)

Status: **proposed**, nothing here is decided. Author: Claude Fable 5.2, 2026-11-12.
Builds on decisions 2026-11-12 ("same-player control at server start, with explicit handoff").

## 1. Goal

The operator joins with the vanilla 26.3 client and presses "Give Praxion control". Praxion then
plays that player through keyboard and mouse input, so the operator watches in first person on
their own screen. Any physical input gives control back at once. The separate headless bot stays.

## 2. What already exists

| Piece | State | Evidence |
|---|---|---|
| The seam: everything above the actuator emits one `ControlFrame` per tick (keys held, look, clicks, hotbar slot, menu action) | decided, in code | `body/crates/headless/src/frame.rs`, architecture §5.2 |
| The world mirror is fed from the forwarder tee, not from the bot's own socket | in code | `headless/src/worldfeed.rs` |
| The tee decodes the real client's traffic: position and inventory match the server | measured offline | `logs/mac-dashboard-proxy-walk/` (453 packets, 0 failures) |
| Keyboard reaches the client; a physical key press ends a hold | measured live | `spikes/macos-hands/main.swift`, backlog |
| Mouse button and camera reach the client when posted at the HID or session tap | measured live, one click and one move | `logs/mac-mouse-probe/result.txt` |

So the missing parts are two: a host that runs the decision loop **without logging in**, and an
actuator that turns a `ControlFrame` into Quartz input.

```
 vanilla client ──► forwarder ──► server
        ▲               │ tee
        │ Quartz        ▼
   hands (new)     world mirror ─► perception ─► rules/interpreter ─► primitives
        ▲                                                              │
        └──────────────────────── ControlFrame ◄───────────────────────┘
```

## 3. Proposed pieces

1. **Real-client host** (`praxion-body --real-client`). No login, no own physics. The player's
   pose (position, rotation, on-ground) is read from the client's own movement packets on the tee.
   The tick clock is the client's: one decision per observed client tick. *Open:* which packet
   marks a client tick in 26.3 (to be read from an existing capture before any code).
2. **Quartz hands** (a third actuator next to the headless one and the planned Windows SendInput):
   - `held` keys: key-down/key-up on change only.
   - `look`: closed loop. Wanted yaw/pitch minus the rotation the client last reported, turned
     into a mouse delta with a measured gain (first number: 45 degrees per 300 px at sensitivity
     0.5), spread over the tick.
   - `clicks`: mouse button held for as long as the frames keep asking.
   - `select_slot`: number keys. `menu`: clicks at slot positions on screen (the hardest part;
     built last). `chat`: not typed; narration stays on the brain side.
   - Every event carries the tag; posts go to the session tap and only while the client is
     frontmost and the cursor is inside its window.
3. **Takeover.** A listen-only tap sees every untagged key, button and mouse movement. The first
   one releases all keys and buttons, bumps the frame generation (stale frames are dropped, as
   architecture §5.2 already says) and hands control to the operator. Re-arming is the explicit
   console action. Focus loss and a dead tap do the same.
4. **Watchdog.** A small second process releases every key and button if the hands process dies.
5. **Console.** The approved mode switch and the "Give Praxion control" button.

## 4. Build order (each step verified live before the next)

| Step | What | Passes when |
|---|---|---|
| S1 | Finish the mouse spike: held button on a survival block, right click, pitch, gain repeatability, takeover on mouse movement | block mined and placed on the server; the angle per pixel repeats within 2 %; a physical mouse move releases within 50 ms |
| S2 | Shadow mode: the host runs with no hands while the operator plays | perception facts and the console inventory follow the operator live for 10 minutes, 0 desync |
| S3 | Hands for `held`, `look`, `clicks`: run `move_to` and `mine` | a log is cut on the real client; receipts confirmed; takeover works mid-primitive |
| S4 | Menus: craft through the inventory and the crafting table | `get_wooden_pickaxe` finishes on the real client |
| S5 | Console mode and control button, headless mode untouched | the split-console checks from decisions 2026-11-11 still pass |

S1 is spike code and is allowed today. S2 onwards is Demo-1 product code: `AGENTS.md` forbids it
until the Windows experiments E8-E16 pass, so the user must lift or amend that rule for macOS first.

## 5. Risks and open questions

- Primitives read body state the headless actuator computes itself (velocity, on-ground,
  prediction sequence numbers for receipts). In real-client mode these come from packets; some
  may be missing or late. S2 measures this.
- Menu clicks depend on GUI scale and window size (the macOS twin of E10).
- A body that starts after the client joined gets a tee gap (backlog "Body restart in the middle
  of a game connection"); for now the host must start before the client joins.
- Global input: a wrong focus check sends keys to another application. The frontmost and
  cursor-in-window checks are preconditions of every post, not a one-time check.
- The first client launch sometimes hangs at `SDL_GL_SwapWindow` (backlog).
- Windows parity (SendInput) is unchanged by this proposal and still gated.
