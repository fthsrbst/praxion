<!-- Reviewer: Beam Free via OpenCode opencode v2.1.2. Lens: beam-protocol. Raw, unverified output. -->

### Findings

- **ID**: BC-1
- **Severity**: blocker
- **Where**: `docs/architecture.md` §2, §4 (Forwarder/Decoder rows), §5; `docs/sessions/02-body-connection.md` §2.2
- **Claim**: Azalea and MCProtocolLib are *client* frameworks; Praxion needs a *passive, bidirectional* decoder (server→client for world state, client→server for receipts such as container-click slot indices). A client framework will not give both directions and may internally send packets, breaking the "forwarder never encodes" rule.
- **Evidence**: Azalea README describes "a collection of Rust crates for making Minecraft bots, clients, and tools" (https://github.com/azalea-rs/azalea); it exposes `Client` with `mine`/`block_interact`, i.e. an active client. The design's receipt ladder in `sessions/04-action-verification.md` requires attributing client→server packets (`Click Container` carries the slot index). No passive mode is described anywhere.
- **Recommendation**: Decide the decoder strategy explicitly before E2: either a neutral passive codec (parse both directions without sending) or a custom minimal decoder for the packet subset Praxion actually consumes. Do not assume Azalea can be dropped in as a read-only parser.
- **Confidence**: high

- **ID**: BC-2
- **Severity**: major
- **Where**: `docs/sessions/02-body-connection.md` §2.2; `docs/architecture.md` §11 risk 2
- **Claim**: Azalea 26.3 support lives on a stale branch (Cargo.toml `0.16.0+mc26.3`, branch last updated 2026-09-25) while `main` (last commit 2026-10-16) still advertises 26.2; Azalea is a hobby project maintained primarily by one person and explicitly does not support multiple versions at once.
- **Evidence**: GitHub branch page shows `26.3` updated 2026-09-25 vs `main` 2026-10-16; README says "Currently supported Minecraft version: 26.2" and "maintained primarily by one person as a hobby project" (https://github.com/azalea-rs/azalea). Azalea non-goals include supporting multiple versions (README "Non-goals").
- **Recommendation**: If Azalea is used, pin and vendor the exact `26.3` commit, add a documented re-pin procedure, and treat the MCProtocolLib fallback as a separate-process Java decoder (with IPC cost) rather than a drop-in. Reconsider building a minimal codec.
- **Confidence**: high

- **ID**: BC-3
- **Severity**: major
- **Where**: `docs/architecture.md` §4 (Decoder row), §8; `docs/decisions.md` (2026-10-29 "game data from Mojang data generator")
- **Claim**: Mojang data reports provide packet IDs and registry IDs, not field layouts, so "decode the copy using Mojang data reports" overstates what they do. A codec still has to be written or borrowed.
- **Evidence**: Data Generators page: the packets report stores "both a resource location and numeric id for each packet used in each state"; "A report that contains all packets used by both the server and client" with only `protocol_id` (https://minecraft.wiki/w/Minecraft_Wiki:Projects/wiki.vg_merge/Data_Generators). No field schemas.
- **Recommendation**: Rewrite the decoder line to say reports supply IDs/registries for validation; field decoding comes from the chosen codec (Azalea or custom). Add a build step that regenerates reports per version.
- **Confidence**: high

- **ID**: BC-4
- **Severity**: major
- **Where**: `docs/architecture.md` §5 (Perception); `docs/sessions/02-body-connection.md` (research "Configuration state" note)
- **Claim**: The decoder must track the full state machine (Handshaking→Login→Configuration→Play, and Play→Configuration via `Start Configuration`) and handle `Known Packs`: when client and server agree on known packs, `Registry Data` may omit entries, so a passive decoder without local vanilla registries cannot reconstruct state.
- **Evidence**: Session 2.2 research: "Configuration state: Select Known Packs, Registry Data, Update Tags, Feature Flags. With known packs, Registry Data may omit entries, so a passive decoder needs local vanilla registry data (documented)." Also Play can return to Configuration (`Start Configuration` packet, wiki packets list).
- **Recommendation**: Either force full Registry Data on the Praxion server (disable known packs) or bundle local vanilla registries from reports and apply deltas; specify state tracking and re-decode on Configuration re-entry in the decoder contract.
- **Confidence**: high

- **ID**: BC-5
- **Severity**: major
- **Where**: `docs/architecture.md` §2 (process boundary [O]); `docs/decisions.md` (proxy decision)
- **Claim**: If the decoder shares a process with the forwarder, a decoder panic/desync can kill the game connection, contradicting "the game is unaffected"; Azalea's tokio/Bevy runtime can also disturb the 50 ms tick budget.
- **Evidence**: Architecture says "A decoder that loses sync marks perception invalid and Praxion stops safely; the game is unaffected" and leaves "whether the forwarder is its own OS process or an isolated thread inside the body" open. Azalea workspace pulls bevy_ecs/bevy_app/tokio (Cargo.toml above).
- **Recommendation**: Make the decoder a separate process (or at minimum an isolated thread with `catch_unwind` and its own runtime) that signals "perception invalid" over a channel; the forwarder keeps forwarding regardless.
- **Confidence**: medium-high

- **ID**: BC-6
- **Severity**: major
- **Where**: `docs/decisions.md` (1.5 "private local server counts as singleplayer"); `docs/sessions/01-product-scope.md` §1.3; `docs/sessions/02-body-connection.md` (Open to LAN, focus notes)
- **Claim**: A dedicated server is not transparently "singleplayer": Open to LAN requires authentication (so the LAN route is unavailable without a mod), the client still pauses ~500 ms after losing focus (`pauseOnLostFocus` default true, and changing it is ruled out), the server keeps ticking while the client is paused (desync risk), and the world must be copied from a singleplayer save.
- **Evidence**: Session 2.2: "Open to LAN: Requires authenticated joiners: `IntegratedServer.initServer()` calls `setUsesAuthentication(true)` unconditionally (verified, decompiled 26.2)." Also "pauseOnLostFocus still exists (default true)... the client opens the pause menu after 500 ms unfocused, releasing the mouse grab (verified, decompiled 26.2)."
- **Recommendation**: Document the dedicated-server differences as a first-class constraint; decide whether Praxion manages the server instance (start/stop/copy save/set properties) and add pause-state handling to the action layer. Revisit the "avoid mods" preference if the pause behavior proves unworkable.
- **Confidence**: high

- **ID**: BC-7
- **Severity**: major
- **Where**: `docs/sessions/02-body-connection.md` §2.3 (H1/H2, experiments); `docs/architecture.md` §11 risk 1
- **Claim**: The design commits to relative `MOUSEEVENTF_MOVE` before the experiments verify it: SendInput reaching `WM_INPUT` is community-reported (not documented), raw-input bypass of pointer acceleration is unverified, the exact GLFW commit in LWJGL 3.4.3 is unknown, and MC-230466 shows absolute moves misbehave. The pass criteria exist but the fallback if they fail is underspecified.
- **Evidence**: Session 2.2: "SendInput relative moves reach WM_INPUT in practice (community reports; not documented by Microsoft)." "Raw input off: delta = WM_MOUSEMOVE... subject to pointer speed and 'Enhance pointer precision'." "MC-230466 (1.17): PyDirectInput moved the camera wrongly with Raw Input on." "RDP blocks raw relative input; experiments must run at the physical machine."
- **Evidence (URL)**: https://bugs-legacy.mojang.com/browse/MC-230466
- **Recommendation**: Keep E5–E9 as hard gates; specify in advance what happens on failure (e.g. require Raw Input on, or fall back to a client mod) instead of "stop and ask" only. Add an acceleration-compensating calibration mode for the Raw-Input-off case.
- **Confidence**: medium-high

- **ID**: BC-8
- **Severity**: major
- **Where**: `docs/sessions/02-body-connection.md` §2.4 (minor picks); `docs/architecture.md` §4 (Take-over, Hands)
- **Claim**: Take-over relies on `LLKHF_INJECTED`/`LLMHF_INJECTED`, which driver-level injectors and some macro tools do not set (they look physical); low-level hooks are silently removed if the installing thread blocks beyond 1000 ms; and the watchdog cannot use `GetAsyncKeyState` (it merges injected and physical state), so it must run its own hook/shared-state protocol.
- **Evidence**: Session 2.2: "The installing thread must run a message loop. `LowLevelHooksTimeout` max 1000 ms; on timeout the hook is silently removed." "Driver-level injectors (Interception, virtual HID, some vendor macro tools) look physical (inferred)." "LLKHF_INJECTED / LLMHF_INJECTED mark SendInput events."
- **Recommendation**: Specify the watchdog protocol (shared memory + its own hook, or lease-expiry release) and its failure modes; test hook survival under load; document that driver-level macro tools can defeat take-over.
- **Confidence**: medium

- **ID**: BC-9
- **Severity**: minor
- **Where**: `docs/sessions/02-body-connection.md` §2.3 (H3); `docs/sessions/01-product-scope.md` §1.3
- **Claim**: Focus loss handling is specified for Praxion's own windows but not for the game entering its pause menu: once paused, SendInput may be interpreted in menus and the client stops reporting movement, which the body may misread as "standing still".
- **Evidence**: Session 2.2: "the client opens the pause menu after 500 ms unfocused, releasing the mouse grab... Mouse deltas are dropped while unfocused." Architecture §5 only says "on focus loss it releases its keys and tells the user."
- **Recommendation**: Add a pause heuristic (no client→server movement packets for N ms while active ⇒ treat as paused ⇒ safe stop) and a test (E11) that checks menu-state input is never sent.
- **Confidence**: medium

- **ID**: BC-10
- **Severity**: minor
- **Where**: `docs/sessions/02-body-connection.md` (research "Protocol facts"); `docs/architecture.md` §2
- **Claim**: Compression removal (`network-compression-threshold=-1`) is proposed but not made a hard requirement; if left on, the forwarder must decompress to frame packets and the decoder must handle `Set Compression` during login, adding latency and failure modes.
- **Evidence**: Session 2.2: "Compression is independent of online mode; default `network-compression-threshold=256`. Setting `-1` on loopback removes zlib from the proxy's path (proposal)."
- **Recommendation**: Make threshold `-1` a checked precondition of the Praxion server instance (verify at startup; refuse to run otherwise) and document the server.properties management.
- **Confidence**: high

- **ID**: BC-11
- **Severity**: minor
- **Where**: `docs/sessions/02-body-connection.md` (research "Protocol facts", "Open to LAN"); `docs/decisions.md`
- **Claim**: Offline mode gives no encryption (good for decoding) but also no authentication; the dedicated server must be bound to 127.0.0.1 or any LAN host can join as the user. Also the server must accept the EULA and have its world populated from a singleplayer save.
- **Evidence**: Session 2.2: "Offline mode: no encryption request is sent (verified, `ServerLoginPacketListenerImpl`)." No bind-address requirement is stated anywhere in the docs.
- **Recommendation**: Add server-management requirements (bind 127.0.0.1, online-mode=false, EULA, save copy/import) and either manage the server process from Praxion or document manual setup with a checklist.
- **Confidence**: high

- **ID**: BC-12
- **Severity**: minor
- **Where**: `docs/architecture.md` §2, §5; `docs/sessions/02-body-connection.md` §2.7 (E1, E3)
- **Claim**: "Bytes untouched" still requires correct packet framing by the forwarder to hand a copy to the decoder; with compression on this requires decompression, and chunk bursts must not delay forwarding (E3). The forwarder's framing/backpressure contract is unspecified.
- **Evidence**: Architecture: "forward bytes untouched between client and server; hand a copy to the decoder" and "chunk bursts reported apart and must not delay forwarding" (E3).
- **Recommendation**: Specify the forwarder's framing (length-prefixed, with compression handling or the -1 precondition), error handling on bad lengths, and a test that a corrupt decoder cannot stall forwarding.
- **Confidence**: medium

### Ideas (brainstorm)

- Build a **minimal passive codec** for only the packets Praxion consumes (movement reports, block changes, inventory, chat, chunks) using Mojang reports for IDs and the unobfuscated 26.3 jar for layouts; drops the Bevy/tokio weight and the single-maintainer dependency.
- Make **Raw Input on** a checked precondition of the Praxion server/client setup, since MC-230466 shows absolute moves misbehave and acceleration compensation is slow.
- Add a **pause-state heuristic** (no client→server movement for N ms while active ⇒ safe stop) so the body never sends input into a paused game menu.
- Have Praxion **manage the dedicated server as a child process** (EULA, properties, save copy, bind 127.0.0.1) instead of assuming a manual setup that the user can get wrong.
- Run the decoder in its **own process with a defined "perception invalid" signal**, so a decoder crash cannot take down the game connection.
- Add **watchdog lease-expiry release** as the primary failsafe (already proposed) and test it as E12, rather than relying on a second hook that can also be removed.
- Pre-specify the **fallback on E5–E9 failure** (require Raw Input on, or switch to a client mod) so a missed experiment does not stall the project.

### Questions for the user

1. Is the dedicated-server route acceptable given its pause/desync and save-copy implications, or are you open to a Fabric client mod despite the "avoid mods" preference?
2. Should Praxion manage the dedicated server process itself, or is manual setup with a checklist acceptable?
3. Is pinning Azalea's stale `26.3` branch acceptable, or should we invest in a minimal passive codec from the start?
4. Is `network-compression-threshold=-1` on loopback acceptable (more traffic, simpler and faster decoding)?
5. If SendInput experiments fail their thresholds, which fallback do you prefer: require Raw Input on, or switch to a client mod?

FINAL_REPORT: The biggest risk is that the design assumes Azalea can act as a passive, bidirectional protocol decoder, but it is an active client framework whose 26.3 support sits on a stale branch maintained by one person, and Mojang reports only provide packet IDs rather than field layouts; combined with the unresolved dedicated-server pause/desync behavior and unverified SendInput/raw-input assumptions, the "eyes on the network" route is feasible but only if the decoder strategy, server management, and experiment fallbacks are specified before any code is written.
