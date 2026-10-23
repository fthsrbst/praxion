# Session 2: Body and connection layer

Opened 2026-10-23. In progress.

Direction from session 1: the user's unmodified client connects to a private local vanilla server
(`online-mode=false`) through a read-only Praxion proxy (perception); Praxion acts through OS
keyboard and mouse input (action); physical input takes control back. Client mod is the fallback.

| # | Sub-topic | State |
|---|---|---|
| 2.1 | How the connection works (background) | Presented |
| 2.2 | Proxy: placement, decoding, own code or library, version pack | Open |
| 2.3 | Hands: Windows input, calibration, focus, GUI clicks | Open |
| 2.4 | Hand-over and take-over; "Praxion is active" indicator | Open |
| 2.5 | Body port: the interface between brain and body | Open |
| 2.6 | Language, runtime, Windows tick timing | Open |
| 2.7 | Risk experiments: what we measure, pass criteria | Open |

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
- 26.3 bundles LWJGL 3.4.3 and needs Java 25. GLFW commit inside it: unknown.
- Protocol version 777 = 26.3 (documented: minecraft.wiki; verified: minecraft-data, MCProtocolLib, Velocity).

### Mouse input path (GLFW, Windows)
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
