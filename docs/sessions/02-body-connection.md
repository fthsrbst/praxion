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
| Rust toolchain | Azalea asks for an undated `nightly`; pin a dated nightly near the branch commit, e.g. `nightly-2026-09-25` (minor pick by Claude; confirmed when E2 builds) | `rust-toolchain.toml` |
| Data reports | hash recorded after the first data-generator run | — |

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
  unknown. **Open:** which SDL hints Minecraft 26.3 sets.
- Pointer speed and "enhanced pointer precision": raw input reports device deltas before Windows
  ballistics for real mice; whether an injected relative move is scaled first is **unknown**; E5
  measures it (the pointer-speed matrix).

### Still open in round 0 (moved to the Windows PC session; the user chose no downloads on the Mac)
- Which SDL hints Minecraft 26.3 sets (GameInput, raw keyboard, relative-mode warp): grep the client
  jar for `SDL_` hint strings, or decompile `com.mojang.blaze3d.platform` / `MouseHandler`.
- The 26.3 mouse sensitivity formula in `MouseHandler` (session 2.3 still cites the old formula).
- Key-binding format in `options.txt` under SDL scancodes.
- Data generator run for the data-report hash (needs JDK 25 and the server jar).
- Capture corpus: needs the game running.

