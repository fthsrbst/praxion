# Session 2: Body and connection layer

Opened 2026-10-23. Discussion closed 2026-10-29; experiments (2.7) not yet run.

Direction from session 1: the user's unmodified client connects to a private local vanilla server
(`online-mode=false`) through a read-only Praxion proxy (perception); Praxion acts through OS
keyboard and mouse input (action); physical input takes control back. Client mod is the fallback.

| # | Sub-topic | State |
|---|---|---|
| 2.1 | How the connection works (background) | Presented |
| 2.2 | Proxy: placement, decoding, own code or library, version pack | Decided (D1, D2); library Azalea, verified in 2.7 |
| 2.3 | Hands: Windows input, calibration, focus, GUI clicks | Decided (H1-H3); experiments in 2.7 |
| 2.4 | Hand-over and take-over; "Praxion is active" indicator | Decided; experiments in 2.7 |
| 2.5 | Body port: the interface between brain and body | Decided; message catalog in sessions 3-4 |
| 2.6 | Language, runtime, Windows tick timing | Languages decided (Python + Rust); timing and process boundary open |
| 2.7 | Risk experiments: what we measure, pass criteria | Decided; experiments not yet run |

## 2.1 How the connection works

- There is always a server. Singleplayer runs an integrated server inside the client; here it is
  a separate local process.
- The server is authoritative for the world (blocks, inventory, health, mobs). The client is
  authoritative for movement: it runs physics and reports position and rotation; the server may
  correct it with a teleport that the client confirms. Every camera move Praxion makes is
  therefore reported back through the proxy.
- TCP, length-prefixed packets with a VarInt id. States: Handshaking → Status | Login →
  Configuration → Play; Play can return to Configuration.
- Online mode authenticates through Mojang and encrypts the stream; a proxy sees only ciphertext.
  Offline mode does neither, so the proxy decodes everything.
- The client receives every chunk within view distance, walls or not. The proxy knows more than
  the player sees; the visibility policy is session 3.

## Research findings (2026-10-23)

Collected by a research subagent; items marked (checked) were re-verified by the main agent.
Confidence: verified (source code) · documented (official docs/wiki) · inferred · unknown.

### Version baseline
- Newest stable release: **26.3** (2026-09-15); snapshot 26.4-snapshot-5. (checked, Mojang
  version manifest.) Session 1 named 26.2; that is now one release behind.
- 26.3 bundles LWJGL 3.4.3 and needs Java 25. It uses SDL, not GLFW (corrected 2026-10-29, review round 1).
- Protocol version 777 = 26.3 (documented: minecraft.wiki; verified: minecraft-data, MCProtocolLib, Velocity).

### Mouse input path (GLFW, Windows): superseded
> **Superseded 2026-10-29.** Minecraft 26.3 ships SDL (`org.lwjgl:lwjgl-sdl:3.4.3`) and no GLFW
> library (Mojang version manifest; NeoForge 26.3 primer). The GLFW analysis below describes code
> 26.3 does not run; it is kept as history. Round 0 redoes it against SDL. See "2.7 Round 0".

- GLFW registers raw mouse input without `RIDEV_INPUTSINK`: WM_INPUT arrives only while the window
  is in the foreground. The WM_INPUT handler does no filtering by device, extra info or injected
  flag; relative events use `lLastX/lLastY` directly; absolute events are converted to a delta
  against the last cursor position. (checked, glfw master `src/win32_window.c`.)
- SendInput relative moves reach WM_INPUT in practice (community reports; not documented by Microsoft).
  Injected events carry `hDevice = 0` (observed by Chromium remoting; inferred). GLFW ignores hDevice.
- Raw input off: delta = WM_MOUSEMOVE position minus last position, cursor recentered with
  `SetCursorPos` (verified). Relative SendInput is subject to pointer speed and "Enhance pointer
  precision" (documented). Whether the raw path bypasses acceleration: test on Windows.
- MC-230466 (1.17): PyDirectInput moved the camera wrongly with Raw Input on, fine with it off.
  Hypothesis: absolute moves hit GLFW's absolute branch (inferred). Use relative `MOUSEEVENTF_MOVE` only.
- RDP blocks raw relative input; experiments must run at the physical machine, not over RDP.

### Sensitivity to rotation (verified, `MouseHandler.turnPlayer`, decompiled 26.2 mirror; NeoForge 26.3 patch identical)
```
ss = mouseSensitivity * 0.6 + 0.2      // options.txt, range [0, 1]
sens = ss^3 * 8.0
dYaw = dx * sens * 0.15 ; dPitch = dy * sens * 0.15   // pitch clamped to ±90
spyglass (first person): ss^3 instead of sens (8x slower)
cinematic camera: smoothed (SmoothDouble); invertMouseX/Y negate
```
- Default 0.5 gives 0.15° per count. Deltas accumulate only while the window is active and are
  applied once per render frame.

### Protocol facts
- Offline mode: no encryption request is sent (verified, `ServerLoginPacketListenerImpl`).
- Compression is independent of online mode; default `network-compression-threshold=256`.
  Setting `-1` on loopback removes zlib from the proxy's path (proposal).
- Configuration state: Select Known Packs, Registry Data, Update Tags, Feature Flags. With known
  packs, Registry Data may omit entries, so a passive decoder needs local vanilla registry data (documented).
- No official Mojang protocol spec. Reference: minecraft.wiki protocol pages; ground truth: the
  jar (unobfuscated since 26.1) and the data generator `--reports`.

### Protocol libraries and proxies (GitHub, 2026-10-23)
| Project | Language / license | Newest supported | Activity |
|---|---|---|---|
| MCProtocolLib (GeyserMC) | Java / MIT | 26.3 (merged 2026-10-14) | active |
| Azalea | Rust / MIT | 26.2 on main; `26.3` branch | push 2026-10-16 |
| node-minecraft-protocol | JS / BSD-3 | 26.1; 26.3 PR open | push 2026-10-20 |
| minecraft-data | JSON / license unstated in README | 26.1; 26.2, 26.3 PRs open | push 2026-10-20 |
| ZenithProxy | Java / AGPL-3.0 | `26.3.0` branch (MCProtocolLib + ViaVersion) | push 2026-10-21 |
| Velocity | Java / GPL-3.0 | 26.3, handshake/login and few play packets only | active |
| Valence | Rust / MIT | 1.20.1 | not viable |
| go-mc | Go / MIT | 1.21 | stale |

### Windows timing (documented)
- `timeBeginPeriod` is per-process since Windows 10 2004; on Windows 11 not guaranteed for an
  occluded or minimized window-owning process.
- `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` since Windows 10 1803. Realistic jitter ~0.5–2 ms,
  sub-0.1 ms with a final QueryPerformanceCounter spin (inferred). A 50 ms tick is easy.
- Camera is sampled per render frame; movement keys are read as held state per 50 ms client tick;
  attack/use presses are counted. Hold movement keys for at least one tick (inferred from source).

### Low-level hooks (documented, Microsoft Learn LowLevelKeyboardProc / LowLevelMouseProc)
- The installing thread must run a message loop. `LowLevelHooksTimeout` max 1000 ms; on timeout
  the hook is silently removed. Run hooks on a dedicated thread that returns immediately.
- `LLKHF_INJECTED` / `LLMHF_INJECTED` mark SendInput events. Driver-level injectors (Interception,
  virtual HID, some vendor macro tools) look physical (inferred).

### Focus
- `pauseOnLostFocus` still exists (default true). On a dedicated server the world keeps running,
  but the client opens the pause menu after 500 ms unfocused, releasing the mouse grab
  (verified, decompiled 26.2). Mouse deltas are dropped while unfocused.
- SendInput goes to the focused window only, and UIPI blocks it toward higher-integrity processes (documented).
- Conflict to resolve in 2.3: setting `pauseOnLostFocus:false` would change a user setting,
  which session 1 ruled out.

### Open to LAN
- Requires authenticated joiners: `IntegratedServer.initServer()` calls
  `setUsesAuthentication(true)` unconditionally (verified, decompiled 26.2). The wiki does not say so.
  Confirms the private dedicated server route.

### Still to test on Windows
- `hDevice` of SendInput events; whether raw-input deltas bypass pointer acceleration;
  the exact GLFW commit in LWJGL 3.4.3.

### Sources
- Mojang version manifest: https://piston-meta.mojang.com/mc/game/version_manifest_v2.json
- GLFW: https://github.com/glfw/glfw/blob/master/src/win32_window.c
- MouseHandler (26.2 mirror): https://github.com/Renekovski/26.2-mcp/blob/main/src/net/minecraft/client/MouseHandler.java
- NeoForge 26.3 patch: https://github.com/neoforged/NeoForge/blob/26.3.x/patches/net/minecraft/client/MouseHandler.java.patch
- Protocol: https://minecraft.wiki/w/Java_Edition_protocol/Packets
- MC-230466: https://bugs-legacy.mojang.com/browse/MC-230466
- SendInput / MOUSEINPUT: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput
- timeBeginPeriod: https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod
- Waitable timers: https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw
- Low-level hooks: https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc
- IntegratedServer (26.2 mirror): https://github.com/Renekovski/26.2-mcp/blob/main/src/net/minecraft/client/server/IntegratedServer.java

## 2.2 and 2.6: proxy and languages (2026-10-29)

### Decided 2026-10-29 (D1, D2: user confirmed)
- **D1: separate proxy process.** It forwards bytes untouched and decodes a copy. The forwarding
  path never encodes packets, so "read-only" holds structurally. A decoder that loses sync marks
  perception invalid and Praxion stops safely; the game is unaffected. Rejected: terminate both
  connections and re-encode (ZenithProxy style): a codec bug could corrupt the game.
- **D2: game data from the Mojang data generator** (`--reports`: blocks, packets, registries).
  `minecraft-data` stays the backup (26.2/26.3 PRs open, license unstated in its README).

### Decided: languages (user, 2026-10-29; Rust pick confirmed)
- **Python and a native language.** The user chose a mix. Claude picked **Rust** over C++ (user
  confirmed): Azalea gives a Rust packet decoder, chunk, block and inventory crates
  under MIT; no maintained 26.x decoder exists in C++ or Python. Checked 2026-10-29: Python
  `mcproto` covers handshake, login and status only, so Python has no Play-state decoder.
- **Proposed split rule (to confirm in 2.5):** anything on the wire or the input path, or with a
  deadline under ~10 ms, is Rust; brain, skills, LLM and Jev calls are Python. The exact
  boundary (PyO3 module or local socket) is the body port, 2.5.
- **Library:** Azalea protocol/world/block/inventory crates (not its bot framework), verified in
  2.7 on 26.3 (its `26.3` branch was last pushed 2026-10-16). Fallback: MCProtocolLib (Java,
  master has 26.3) or an own decoder for the needed packets only.
- Body process is Windows-only (input injection); proxy and decoder are portable, so they can be
  developed on macOS against recorded packet captures.

## 2.3 Hands (2026-10-29)

Verified in GLFW `win32_window.c` (master): keys are read from the scan code in `lParam` (virtual
key only as a fallback); `WM_INPUT` is processed only while the cursor is disabled (in the
world); menus use the normal cursor; `WM_KILLFOCUS` releases the cursor at once.

### Decided (user, 2026-10-29)
- **H1:** `SendInput` with scan codes; bindings read from `options.txt`, never written. Rejected:
  driver-level injectors (no benefit; they hide the injected flag that take-over needs). Held
  movement keys last at least one 50 ms tick.
- **H2:** two mouse modes. World: relative `MOUSEEVENTF_MOVE`, gain estimated from `options.txt`
  (default 0.15 deg per count) and corrected from the rotation the proxy sees. Menus: absolute
  positioning from GUI scale and window size. Praxion runs per-monitor-v2 DPI aware.
- **H3:** on focus loss release all keys, stop safely and tell the user; never force focus back.
  Praxion windows use `WS_EX_NOACTIVATE`. If the game runs elevated, `SendInput` is blocked
  (UIPI): Praxion checks the integrity level at start and refuses with a clear message.
- Mineflayer is the reference model: `controlState`, rate-limited and quantized look, `human.js`.

### Experiments for 2.7
1. Relative `SendInput` reaches `WM_INPUT`. 2. Raw input bypasses pointer acceleration.
3. Input-to-packet latency, a one-tick key hold, aiming within 1 degree in 150 ms.
4. Where the cursor lands after a menu closes. 5. Focus loss in fullscreen (GLFW may iconify).

## 2.4 Hand-over and take-over (2026-10-29)

### Decided (user, 2026-10-29)
- **Take-over sensitivity:** key or mouse-button press, wheel, or clear mouse movement takes
  control back; tiny jitter is ignored. Threshold set in 2.7 from measured idle noise.
- **Interrupted work waits** for "carry on" (survey/02 idea 3, warm resume): the program is
  frozen, remaining work is recomputed from proxy-verified state, never from Praxion's own memory.
- **Indicator:** a separate small window outside the game area (second monitor if present),
  `WS_EX_NOACTIVATE`; shows who plays, current task, last command.

### Minor picks by Claude (user may overrule)
- A low-level hook on its own thread keeps a table of physically held keys, using the
  `LLKHF_INJECTED` / `LLMHF_INJECTED` flags. `GetAsyncKeyState` cannot be used because it merges
  injected and physical state. On the first physical event the hook sets a flag; the input
  thread checks it before every send. Target: take-over under 50 ms.
- On take-over Praxion releases only keys it holds and the user does not physically hold. The
  user's own event still reaches the game untouched.
- Hand-over starts only when no physical key or button is held (wait up to ~2 s, then tell the user).
- Every held key has a short lease that the input thread renews. A tiny watchdog process
  releases all injected keys if the main process dies, because Windows does not.
- The global hand-over hotkey is registered with `RegisterHotKey` (the game should not see it;
  inferred from the docs, to test in 2.7). The key combination is configurable.

### Experiments for 2.7
1. Hook-to-flag latency and total take-over latency. 2. Idle and resting-hand mouse noise, to set
the jitter threshold. 3. The hotkey never reaches the game. 4. Watchdog releases keys after a
forced kill of the main process. 5. A physically held key is not released by Praxion.

## 2.5 Body port (2026-10-29)

### Shape
Three message kinds: **observations** (body to brain: deltas and events), **commands** (brain to
body, each with an id and an owner token), **receipts** (body to brain: pending, then settled with
the evidence the body itself observed). Model: Mine AI MCP's submit/pending/settled protocol
(survey/01 mechanism 1) and Cairn's `ok / failed / preempted` results with owner tokens
(survey/03 mechanism 6). A receipt never rests on the brain's own claim. The same port serves the
real body, the in-memory fake world and recorded replays (decision 1.2: agent layers do not
import body layers).

### Decided (user, 2026-10-29)
- Separate programs, local connection.
- Brain may run on another computer; default loopback, remote needs an explicit key.
- Brain silent: safe stop and notify (body finishes the step to a safe point, stands still, keeps
  basic survival reactions on).

### Minor picks by Claude (user may overrule)
- Silence is declared after a heartbeat gap, default about 3 s, tuned in 2.7.
- Take-over and reflexes preempt without a handshake; a preempted command settles as `preempted`,
  not as a failure.
- Message format and transport are chosen when the first code is written; they are an
  implementation detail behind this contract.

### Left open on purpose
- Who holds the world model (body or brain): session 3. The port carries both an event stream and
  queries, so either answer fits.
- The command and failure catalog, and evidence levels per action: session 4.
- Which reflexes stay on: session 7.

## 2.7 Risk experiments (2026-10-29)

### Decided (user, 2026-10-29)
- **Thresholds accepted as listed.** A threshold that is missed stops the work and goes to the
  user; Claude never switches route (for example to a client mod) on its own. Rows marked
  "measure only" are recorded, not judged.
- **Hands experiments run at the physical Windows PC** (Remote Desktop blocks raw relative input).
  Round 1 runs on macOS first.
- **Experiment code lives in `spikes/`, untracked, until the license is chosen.** Reports and
  measurements are committed as documents. `spikes/` is in `.gitignore` (minor pick by Claude).

### Amended plan (user, 2026-10-29, review round 1 Q11)
The table below is amended as follows; the stop rule is unchanged, and a miss report carries a
fallback shortlist for the user.
- **Round 0 (desk, new):** version manifest; read the 26.3 SDL input path and key-binding format;
  record a stock-client capture corpus (Status ping, Login, Configuration with Known Packs, one
  Play→Configuration re-entry, 10+ min of Play). Server checklist for all rounds.
- **E1, E3:** macOS judges correctness only; latency is judged on Windows under co-load (game,
  test server, recorder) with p99 and p99.9; decode and world-model update measured apart; chunk
  bursts reported apart; the forwarder-tee topology of decision Q3 is what is measured.
- **E2:** decode the stock-client corpus at the pinned Azalea commit; pass = zero decode errors and
  per-packet agreement with an oracle.
- **E5:** the raw-input axis is dropped (the option no longer exists); add a pointer speed /
  enhanced-precision matrix and check that an injected relative move turns the camera under SDL.
- **E7, E8:** close the loop on serverbound per-tick packets. **E9:** add input-to-packet attribution.
- **E10:** compare `SetCursorPos` with absolute `SendInput`. **E11:** borderless first.
- **E12:** add the nonce check, hook-liveness probe, watchdog on the process handle with mouse
  buttons, danger-held keys released last, the command gesture.
- **E15 (new, Windows, round 4):** integrated tick, packet → decode → world model → reflex → hands →
  server ack, under a chunk burst and a combat fixture; end-to-end p99 of a scripted reflex within 50 ms.
- **E16 (new, after round 4):** protocol-body fidelity: the same scripted movement and dig sequence on
  the real and the headless body, traces compared.

### Experiments (as accepted before the amendment)
| Round | # | Measures | Pass |
|---|---|---|---|
| 1 Eyes (also on macOS) | E1 | Latency added by the forwarding proxy | p99 < 1 ms; 10 min of play without disconnect or desync |
| | E2 | Azalea decodes a 26.3 offline-mode session (fallback MCProtocolLib) | 10 min, zero decode errors; sampled blocks match server commands |
| | E3 | Packet arrival to state update | p99 < 5 ms steady state; chunk bursts reported apart and must not delay forwarding |
| | E4 | How often the client reports its pose | measure only |
| 2 Hands (Windows, physical) | E5 | Relative SendInput turns the camera; degrees per count | within 2% of the formula, raw input on and off measured separately |
| | E6 | Aim within 1 degree in 150 ms | at least 95 of 100 |
| | E7 | One-tick key hold | at least 95 of 100 give exactly one tick (proxy-verified) |
| | E8 | Tick-aligned click, then the water-bucket save | click at least 95%; save at least 90% of 50 falls |
| | E9 | Input-to-packet latency | measure only |
| 3 Menus and focus | E10 | Menu click lands on the intended slot (GUI scale 1-4, two window sizes) | at least 99 of 100 |
| | E11 | Focus loss in fullscreen, borderless, windowed | keys released within 100 ms; no stuck key |
| 4 Take-over and timing | E12 | Physical event to last injected event; idle mouse noise; hotkey hidden from the game; watchdog | p99 < 50 ms; user-held keys never released; hotkey never reaches the game; watchdog releases keys within 1 s of a kill |
| | E13 | Jitter of the 50 ms loop with the game running | p99 < 2 ms |
| | E14 | Written comparison of the two decoder libraries | report |

Also in round 2: `hDevice` of injected events (informational), and the mouse gain formula against
the 26.x source.

### Order
Round 1 first: it is cheap and decides the decoder. Rounds 2 to 4 follow at the Windows PC. All
rounds run before session 3, because a failure changes the route (client-mod fallback).

Update from session 4: the rule "release all keys on take-over" has an exception for dangerous
moments (bridge-edge sneak, mid-air jump); see `docs/sessions/04-action-verification.md`.

## 2.7 Round 0 (desk, started 2026-10-29)

### Version manifest (draft; becomes `versions/` at Demo-0)
| Item | Pinned value | Source |
|---|---|---|
| Minecraft Java | 26.3, release 2026-09-15; version JSON sha1 `5df1795e16eea19d75b923d66ecca9ce183353c5` (re-published 2026-10-27) | piston-meta version manifest v2 |
| Protocol | 777 | Azalea `26.3` branch `azalea-protocol/src/packets/mod.rs`; minecraft.wiki |
| Java runtime | 25 (`java-runtime-epsilon`) | 26.3 version JSON |
| Client jar | sha1 `e877b6a07acd633fb3bb475002175cec036e7b87`, 41,483,720 bytes | 26.3 version JSON |
| Server jar | sha1 `33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c`, 62,294,556 bytes | 26.3 version JSON |
| LWJGL | 3.4.3, with `lwjgl-sdl` (no GLFW) | 26.3 version JSON libraries |
| Azalea | branch `26.3`, commit `8041a14706fbdb2b8c481d2e8960d7411e850796` (2026-09-25); 11 ahead and 18 behind `main` (`0d38e0d`, 2026-10-16); licence MIT | GitHub API |
| Rust toolchain | `nightly-2026-08-20`, no extra flags (user, 2026-10-30; rejected: `nightly-2026-09-25` plus `-Znext-solver=coherence`). Re-pin when Azalea moves to `main` | build test, see "Azalea build" |
| Data reports | tree hash `a6e12ba626ea9dcb25f2a3336522f787e82d9691bdb5817d90db070f0895a79f` (see below) | data generator, 2026-10-30 |

Newer at the time of writing: `26.4-snapshot-6`. Not used (decision Q4).

### SDL 3.4.14 Windows input path (read 2026-10-29)
LWJGL 3.4.3 bundles **SDL 3.4.14** (LWJGL 3.4.3 release notes, 2026-08-23). Read from SDL tag
`release-3.4.14`, `src/video/windows/` (`SDL_windowsevents.c`, `SDL_windowsrawinput.c`,
`SDL_windowsvideo.c`) and `include/SDL3/SDL_hints.h`:

- **Keyboard (default path):** `WM_KEYDOWN`/`WM_KEYUP`; the scancode comes from `lParam` bits 16-23
  plus `KF_EXTENDED` (`WindowsScanCodeToSDLScanCode`). `SendInput` with `KEYEVENTF_SCANCODE` fills
  exactly these bits, so hands H1 holds. Raw keyboard is off by default
  (`SDL_HINT_WINDOWS_RAW_KEYBOARD`, default 0); if a game turns it on, `WIN_HandleRawKeyboardInput`
  has no `hDevice` filter, so injected keys still pass.
- **Mouse in relative mode:** SDL enables raw mouse input (`WIN_SetRawMouseEnabled`) on a hidden
  message-only window in its own thread (`CreateWindowEx(... HWND_MESSAGE ...)`, `dwFlags = 0`, so
  no `RIDEV_INPUTSINK`: input arrives only while the game is in the foreground).
  `WIN_HandleRawMouseInput` sends relative motion for any `hDevice`, including `NULL` (injected
  events get mouse id 0), so injected relative moves should turn the camera (verify in E5).
- **Filter that matters for the nonce:** raw mouse events are dropped when `ulExtraInformation`
  carries the pen/touch signature (`(x & 0xFFFFFF00) == 0xFF515700`, `MI_WP_SIGNATURE`) or, when
  touch devices exist, when bit `0x80` is set. **Praxion's `dwExtraInfo` nonce must avoid the
  `0xFF5157xx` pattern and keep bit `0x80` clear.** The same check guards `WM_MOUSEMOVE` and buttons.
- **Absolute moves** (`MOUSE_MOVE_ABSOLUTE`) go through a separate RDP/tablet branch; keep using
  relative `MOUSEEVENTF_MOVE` only (H2 unchanged).
- **GameInput:** SDL can read keyboard and mouse through GameInput instead of raw input, but only if
  the game sets `SDL_HINT_WINDOWS_GAMEINPUT` (default off). Whether `SendInput` reaches GameInput is
  unknown. **Closed 2026-10-30:** Minecraft 26.3 does not set it (see "Jar reading on the Mac").
- Pointer speed and "enhanced pointer precision": raw input reports device deltas before Windows
  ballistics for real mice; whether an injected relative move is scaled first is **unknown**; E5
  measures it (the pointer-speed matrix).

### Jar reading on the Mac (2026-10-30)
The user allowed downloads into untracked `spikes/round0/` (2026-10-30). Downloaded and verified:
Temurin JDK 25.0.5+8 (Adoptium sha256 match), the 26.3 version JSON, client and server jars (sha1
match the manifest above), Vineflower 1.12.0 decompiler (Maven Central sha1 match). The 26.3
version JSON has no `client_mappings`: the jar ships unobfuscated, so class names below are Mojang's.

**SDL hints Minecraft 26.3 sets** (every `SDLHints.SDL_SetHint` call site in the client jar; no other
hint setter is referenced):

| Class | Hint | Value |
|---|---|---|
| `RenderSystem` | `SDL_NO_SIGNAL_HANDLERS` | 1 |
| `RenderSystem` | `SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS` | 0 |
| `RenderSystem` | `SDL_QUIT_ON_LAST_WINDOW_CLOSE` | 0 |
| `RenderSystem` | `SDL_MOUSE_FOCUS_CLICKTHROUGH` | 1 |
| `RenderSystem` | `SDL_ENABLE_SCREEN_KEYBOARD` | 0 |
| `RenderSystem` | `SDL_IME_IMPLEMENTED_UI` | `composition` |
| `Window.setQuitShortcuts` | `SDL_WINDOWS_CLOSE_ON_ALT_F4` | 1 or 0 |
| `MacosUtil` (macOS only) | `SDL_VIDEO_MAC_FULLSCREEN_MENU_VISIBILITY`, `SDL_MAC_CTRL_CLICK_EMULATE_RIGHT_CLICK` | 1 or 0 |

- **No `SDL_WINDOWS_GAMEINPUT`, no `SDL_WINDOWS_RAW_KEYBOARD`, no relative-mode hints.** So on
  Windows SDL uses its defaults: keyboard through `WM_KEYDOWN` scancodes, mouse in relative mode
  through raw input. The GameInput risk is closed for the stock client. Caveat: SDL also reads hints
  from environment variables; the vanilla launcher's version JSON sets none (verified), but a
  third-party launcher could. The body should log the `SDL_*` environment of the game process.
- `SDL_MOUSE_FOCUS_CLICKTHROUGH=1`: a click on the unfocused window is delivered to the game as well
  as focusing it. Relevant to E11 and take-over: a focus-restoring click is also a game click.
- Mouse grab (`InputConstants.grabMouse`): `SDL_WarpMouseInWindow` to the centre, then
  `SDL_SetWindowRelativeMouseMode(true)`; release does the reverse. Also `SDL_SetWindowMouseGrab` in `Window`.
- `InputQuirks.RESTORE_KEY_STATE_AFTER_MOUSE_GRAB` is true off macOS: after a grab the game re-reads
  key state. Relevant to HoldGuard (keys held across a menu close); check in E11/E12.

**Mouse to rotation (26.3, `MouseHandler` + `Entity.turn`, decompiled):**
```
onMove (SDL_EVENT_MOUSE_MOTION): grabbed -> accumulate xrel, yrel (SDL relative deltas)
                                 not grabbed -> accumulate position differences
handleAccumulatedMovement (once per render frame): if grabbed and player exists -> turnPlayer
ss   = mouseSensitivity * 0.6 + 0.2        // options.txt, [0, 1], default 0.5
sens = ss^3 * 8.0
dYaw = (float) xo * 0.15 ; dPitch = (float) yo * 0.15 ; pitch clamped to ±90
xo = dx * sens (normal) | dx * ss^3 (spyglass, first person) | SmoothDouble (cinematic)
invertMouseX / invertMouseY negate xo / yo
```
- Same as the 26.2 formula above, except `invertMouseX` is new. Default 0.5 gives 0.15° per count.
- Deltas are dropped while the window is inactive (`isWindowActive`), as before.
- Whether SDL scales `xrel` for an injected move is still E5's question; with no
  `SDL_MOUSE_RELATIVE_*` hint set, SDL's defaults apply (system scaling off).

**Key bindings in `options.txt`:**
- Line format `key_<mapping name>:<key name>`, e.g. `key_key.forward:key.keyboard.w`. The file starts
  with `version:<data version>` and is upgraded through `DataFixTypes.OPTIONS`.
- Key names are `key.keyboard.<name>` or `key.mouse.<name>`; unnamed codes are `key.keyboard.<n>` /
  `key.mouse.<n>`. The 256 keyboard names map to **SDL scancodes**, which are USB HID usage IDs
  (`a`=4, `w`=26, `space`=44, `escape`=41, `t`=23, `slash`=56, `left.shift`=225, `left.control`=224).
  Names are positional, not layout letters: `key.keyboard.w` is the key at the US-W position on any layout.
- Mouse buttons use SDL numbering: `left`=1, `middle`=2, `right`=3, then 4–8 (GLFW used 0/1/2).
- Default bindings in `Options`: forward 26, left 4, back 22, right 7, jump 44, sneak 225 (toggle
  option), sprint 224, inventory 8, drop 20, chat 23, command 56, attack mouse 1, use mouse 3,
  pick mouse 2.
- Consequence for the actuator: read the user's bindings from `options.txt`, map SDL scancode →
  Windows set-1 scancode (inverse of SDL's `windows_scancode_table`), send `KEYEVENTF_SCANCODE`.

**Data reports:** `java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports`
writes 1,666 files under `generated/reports` in ~5 s. Two runs gave the same tree hash, so the
reports are deterministic.
- Tree hash (minor pick by Claude): sha256 of the sorted `shasum -a 256` listing of every file
  under `reports/`. Value: `a6e12ba626ea9dcb25f2a3336522f787e82d9691bdb5817d90db070f0895a79f`.
- `blocks.json` sha256 `7a0de8aa…05a4`: 1,286 blocks, 35,723 states.
- `packets.json` sha256 `2f048631…6886`: play 144 clientbound / 69 serverbound; configuration 21 / 10;
  login 6 / 5; status 2 / 2; handshake 1.
- `registries.json` sha256 `51f62d56…cdac6`.

### Azalea build (2026-10-30)
- The `26.3` branch does **not** build on the nightly near its commit date: `nightly-2026-09-15`,
  `nightly-2026-09-25` fail in `azalea-entity` (E0284 on `azalea-core::bitset::FixedBitSet`, from the
  unstable `generic_const_exprs`); `nightly-2026-10-29` panics the compiler. Upstream CI on the branch
  is red on every commit since 2026-07-05; `main` is green (2026-10-16).
- Cause: `main` fixed nightly breakage after the branch split: `153c90aa` (#378, 2026-08-26) adds
  `-Znext-solver=coherence` to `.cargo/config.toml`; `b65fa8cf` (#380) moves to `generic_const_args`.
- Two setups build `azalea-protocol` and `azalea-world` cleanly (macOS arm64, `--locked`, ~1 min):
  A. `nightly-2026-09-25` with `RUSTFLAGS="--cfg docsrs_dep -Znext-solver=coherence"` (main's own fix);
  B. `nightly-2026-08-20`, no flags.
- Tests on B: `azalea-buf` 11, `azalea-world` 15, `azalea-block` 11, `azalea-registry` 6,
  `azalea-chat` 31 pass, 0 fail (`azalea-buf` needs `--features azalea-buf/serde_json` when tested
  alone). `azalea-protocol`'s own tests do not compile: the `update_advancements` test still uses
  fields removed in `aa5d4cbe` (2026-09-24). `azalea-core` alone does not compile under `cargo test`
  (feature unification; builds fine as a dependency). Not chased further.
- Packet IDs: all 260 packets of every state and direction match `packets.json` in order (names
  differ only in `debug/x` vs `debug_x`). This checks IDs, not field layouts; E2 checks layouts.
- Risk for E2 and Demo-0: the branch is unfinished upstream work (red CI, stale tests). Expect to
  carry small local patches or to follow `main` once it moves to 26.3. Fallback stays MCProtocolLib.

### Still open in round 0
- Capture corpus: needs the client running with the user's Microsoft login, so the user must be at
  a machine. Any machine works: the client jar is identical on macOS and Windows. Needs a local
  26.3 server in offline mode (server jar and JDK are already in `spikes/round0/`).
- Windows-only checks move to round 2 (E5): `hDevice` of injected events, pointer-speed matrix.

## 2.7 Windows runs (2026-10-31, draft, in progress)

Machine: the user's Windows 11 laptop (RTX 3060 Laptop, 8 threads, on AC, power plan "Performance").
Server: vanilla 26.3 jar (sha1 match), offline mode, `127.0.0.1:25566`, RCON on loopback.
Client: vanilla 26.3 started by `spikes/round0/launch_client.py` straight from the version JSON (74
libraries and the client jar sha1-checked), no launcher tweaker. Code: `spikes/` (see `spikes/README.md`).

### Setup findings
- **TLauncher** starts the game wrapped in `org.tlauncher.tweaker.Tweaker` (LaunchWrapper), and the game
  process it starts could not be terminated from the user's session ("Access is denied"), consistent with
  an elevated process. Under H3 an elevated game blocks `SendInput` (UIPI). Its client jar on disk is
  byte-identical to vanilla. Decision (user, 2026-10-31): experiments launch the client directly.
- Toolchain: host `x86_64-pc-windows-gnu` (no MSVC on this PC; minor pick by Claude). Two fixes, in
  `spikes/.cargo/config.toml`: `llvm-dlltool` (a copy of `llvm-ar` from `llvm-tools`) and `rust-lld` as
  linker. GNU `ld` with LLVM import libraries links but the EXE dies with `STATUS_ACCESS_VIOLATION`.
- The 26.3 dedicated server ships with the whitelist on and `accepts-transfers=false`.
- No stock way to force Play→Configuration: `DebugConfigCommand` exists but is registered only in the
  development environment. The corpus has no re-entry; the decoder's synthetic test covers it.
- OpenCode: after `oc-run.sh` kills a timed-out client, the service **keeps running the session** (a
  decoder worker kept editing and running `cargo clean` for 18 minutes). Stop it with
  `opencode session export <id>` then `opencode session delete <id>`.

### Capture corpus (round 0)
| Capture | Content | Records | Notes |
|---|---|---|---|
| `corpus1` | 13 min play (injected walk/turn/jump, 3 far teleports = chunk bursts, 300 status pings, `/transfer`) | 40,343 | sha256 `3bb8e784…3222`; player died after a far teleport (fall); transfer refused (server setting) |
| `corpus2` | E1 run: 4,000 status pings, chunk bursts | 4001 connections | every connection has open and close records (fixed proxy) |
| `corpus3` | ground-truth blocks, E7 key holds, E6 aiming | 28,686 at snapshot 1 | live session, still capturing |

### Results so far
| # | Result | Verdict |
|---|---|---|
| E1 | Interleaved status pings, proxy vs direct, game running, chunk bursts, n=3000 pairs: added round trip p50 0.155 ms, p99 0.50 ms, p99.9 2.0 ms, max 4.1 ms (both directions) | **pass** (p99 < 1 ms) |
| E2 | `corpus1` (13 min): 291,523 packets, 4 errors, all `level_particles` (Azalea's `ClientboundLevelParticles` leaves 6 of 56 bytes unread). `corpus3` snapshot: 293,036 packets, 0 errors, 10/10 ground-truth blocks right (incl. a block removed after placement, block properties, a block inside a `fill`) | **miss** on `corpus1` (zero errors required); layouts otherwise agree |
| E3 | Offline replay of `corpus1` (processing time only, no queueing): steady-state decode p99 0.7 µs, p99.9 7.9 µs; chunk decode p99 133 µs; chunk apply p99 163 µs; block apply p99 28 µs | **pass** (p99 < 5 ms); arrival-to-state under live load is E15 |
| E4 | `client_tick_end` 20.00/s; `move_player_pos` 3.0/s mean, 50 ms gaps while moving, gaps > 1 s when still; `move_player_rot` gaps 50 ms | measure only |
| E7 | 50 ms `W` hold, tuned timer. Run 1 (gap 450 ms = 9 ticks, constant phase): 100/100 exactly one tick. Run 2 (gap 433 ms, phase swept 0.5–50 ms): 194/200 one tick, 4 two ticks, 2 never on the wire | **pass** (97% ≥ 95%), phase-dependent |
| E9 | Key-down to `player_input` on the wire, phase swept (n=200): p50 25.5 ms, p99 51.8 ms, min 2.2 ms (one client tick); run 1 was phase-locked at ~11 ms | measure only |
| E5 | 0.15°/count exact: 1000 counts → 150.000°, 1400 → 209.998°, 500 × 1 count → 75.001°, pitch 500 → 75.000°, with enhanced pointer precision ON, speed 10 | **pass**; pointer-speed matrix not run (system setting; needs the user) |
| E6 | 100/100 targets within 1° (error p50 0.00005°, max 0.0005°); 10 events at 10 ms spacing, interval p99 10.6 ms → ~95 ms per aim | **pass** |
| E13 | 50 ms loop, game running, 3 × 60 s each: default p99 1.3 / 5.0 / 24.4 ms; with `nothrottle,high,mmcss` p99 0.73 / 0.86 / 0.55 ms, max ≤ 1.21 ms | **pass only with the process settings** |
| other | The first relative mouse event after the game grabs the mouse (join, respawn, menu close) is dropped: seen 4 times | finding |

Not run yet: E8 (needs a mouse-button command and a live decode tap for the click timing), E10 (mouse
clicks, GUI scales), E11 (window modes; run with a separate `--gameDir` so the user's `options.txt` is
untouched), E12 (needs the user's physical key presses), E5 pointer-speed matrix (needs the user to change
a system setting), E14 (written comparison), E15, E16.

### Proposals for the user (not decided)
- E2: carry a local patch for `ClientboundLevelParticles` on the pinned Azalea commit (find the 26.3 layout
  in the server jar), then rerun E2 on a fresh 10-minute corpus.
- E13: the body process sets `nothrottle,high,mmcss` (process-local; no system settings) as a requirement.
- E7: hold keys for a number of client ticks counted from the proxy's `client_tick_end`, not for a fixed
  50 ms; a fixed hold misses or doubles about 3% of the time depending on phase.
- The first relative move after a grab is dropped: after a grab the body sends a 1-count primer, or
  checks the resulting rotation in the proxy stream.
