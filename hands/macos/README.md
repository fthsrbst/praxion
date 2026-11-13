# praxion-hands (macOS)

The input helper of Praxion's real-client body: one process that owns every OS
input call. The body writes one line per client tick to the helper's stdin and
reads one reply line per line from its stdout. Design:
`docs/proposals/2026-11-12-s3-hands.md` (accepted). Built from the measured
spikes in `spikes/macos-hands/`.

`praxion_hands.swift` is one file in two halves: a pure half (line parser,
`HandsMachine`, argument parser) that makes every decision and returns a list of
posts and lines, and an OS half that performs exactly that list. `--self-test`
exercises the pure half without permissions, a window server or a game.

## Build

```sh
mkdir -p hands/macos/build
xcrun swiftc -O -warnings-as-errors hands/macos/praxion_hands.swift \
  -o hands/macos/build/praxion-hands
```

`build/` is ignored by git. No other files are written, ever: there are no logs.

## Modes

Exactly one mode per run.

```sh
build/praxion-hands --self-test          # headless tests, no permission needed
build/praxion-hands --preflight          # print the permission booleans, never prompts
build/praxion-hands --serve --pid PID [--activate]
build/praxion-hands --release-all --pid PID
```

- `--self-test` prints `self_test_checks=N self_test_failures=M` and
  `result=ok|failed`; exit 0 only when `M` is 0.
- `--preflight` prints `post_event_access=` and `listen_event_access=` and
  `result=ok|fail`; exit 0 when both are true, 2 otherwise. It never calls
  `CGRequest*`, so it does not change TCC state.
- `--serve` is the live mode below.
- `--release-all` posts one key-up for every key the helper can press (W, S, A,
  D, Space, 1-9) and one button-up for both mouse buttons, then exits 0. Use it
  after a helper died: a stuck key or button is worse than a stray release.

## `--serve`

Start-up: post and listen access must be granted, the PID must not be the
helper itself and must look like a Minecraft/JVM process, `--activate` asks
macOS to bring the target forward, and the target must be frontmost within
15 s. Then the listen-only session tap is created and `READY` is printed. Any
failure prints `REFUSED 0 <reason>` and exits 2 without posting anything;
the reasons are `missing_permissions`, `target_is_self`, `target_unavailable`,
`target_not_minecraft`, `target_not_frontmost`, `event_source_unavailable` and
`event_tap_unavailable`.

After `READY`, every reply line is flushed immediately.

### Lines on stdin

ASCII, `\n` terminated, fields separated by exactly one space.

| Line | Meaning |
|---|---|
| `ARM` | enter the armed state; only from a not-armed state |
| `DISARM` | release everything, leave the armed state, reply `OK 0` |
| `F <tick> <generation> <keys> <dx> <dy> <buttons> <slot>` | one client tick |

`F` fields:

- `tick`, `generation`: unsigned decimals.
- `keys`: bit mask. bit 0 forward (W, 13), 1 back (S, 1), 2 left (A, 0),
  3 right (D, 2), 4 jump (Space, 49). Only bits 0-4 are accepted.
- `dx`, `dy`: signed decimal pixels in `-600...600`. Non-zero posts exactly one
  mouse-move event. Its type is `leftMouseDragged` while the helper holds the
  left button after this frame's releases, `rightMouseDragged` while it holds
  only the right, otherwise `mouseMoved`.
- `buttons`: bit mask, bit 0 left, bit 1 right. Only bits 0-1 are accepted.
- `slot`: `-` or `1`...`9`; a digit posts one key-down and one key-up of the
  number key (ANSI codes 18,19,20,21,23,22,26,28,25).

Unknown bits, an empty field (doubled, leading or trailing space), an unknown
command and any other deviation are malformed.

### Lines on stdout

| Line | Meaning |
|---|---|
| `READY` | the tap is in place and stdin is being read |
| `OK <tick>` | the frame was applied |
| `REFUSED 0 malformed` | the line was not understood and was ignored |
| `REFUSED <tick> stale` | generation below the highest seen; not applied |
| `REFUSED <tick> not_armed` | no armed state; send `ARM` first |
| `TAKEOVER <reason>` | the armed state ended; `foreign_input`, `focus_lost`, `tap_disabled` |
| `IDLE` | no frame for 250 ms while something was held; all released, still armed |
| `BYE <reason>` | process ending: `eof`, `sigint`, `sigterm` |

The generation is a watermark: it moves on every well-formed frame, even while
not armed, so a frame from before a takeover can never be applied after the next
`ARM`. A frame is checked for the armed state first and for the generation
second, so a not-armed frame is always answered `not_armed`.

### Order inside one frame

1. releases: keys in bit order, then buttons (left, right);
2. one mouse move, if `dx` or `dy` is non-zero;
3. presses: keys in bit order, then buttons (left, right);
4. the slot tap, if any;
5. `OK <tick>`.

Key-down and key-up are posted only for bits that changed; the same holds for
buttons.

## Armed-state safety

- Before every frame: the target must be frontmost. A frame that would press a
  button also needs the topmost normal-layer window under the cursor to belong
  to the target. Otherwise everything is released, `TAKEOVER focus_lost` is
  printed and the armed state ends.
- The listen-only session tap watches key down, flags changed, left/right/other
  mouse down, mouse moved, the three dragged types and the scroll wheel. Any
  event without the helper's tag (`eventSourceUserData = 0x5052_4148`) while
  armed releases everything and prints `TAKEOVER foreign_input`. Tagged events
  are the helper's own and are ignored.
- A tap disabled by timeout or by user input releases everything and prints
  `TAKEOVER tap_disabled`, whether the tick or a frame notices it first. The
  helper does not re-enable a disabled tap: it stays fail-closed, so a further
  `ARM` is answered with another `TAKEOVER tap_disabled` instead of holding keys
  without protection.
- No frame for 250 ms while something is held releases everything and prints
  `IDLE`; the armed state stays.
- End of stdin, `SIGINT` and `SIGTERM` release everything, print `BYE <reason>`
  and exit 0. `SIGPIPE` is ignored, so a closed stdout (the body died) cannot
  kill the helper before the release path ran.
- "Release everything" posts an up for exactly what the helper still holds and
  forgets it, so it is idempotent and safe to run twice.
- After a takeover, frames are refused (`not_armed`) until the body sends `ARM`.

All of this is decided by the pure state machine and covered by `--self-test`,
including the invariant that no terminal or takeover transition leaves a key or
button held.

## Posting paths (measured, do not change blindly)

- Keyboard with `event.postToPid(pid)`. A global keyboard post does not reach
  the client's key state.
- Mouse buttons and mouse moves with `event.post(tap: .cgSessionEventTap)` at
  the current cursor position. A PID-targeted mouse button does not work.
- Every posted event is tagged and has empty flags, so the tap can tell the
  helper's own input from the operator's.

## Permissions and environment

The terminal (or whatever launches the helper) needs both **Accessibility**
(post) and **Input Monitoring** (listen). `--preflight` only reports them.

Not handled: a second monitor, non-default key bindings, raw mouse input and
GUI scale other than the operator's. The helper never inspects `options.txt`;
the body refuses to arm when a needed binding is not the default.
