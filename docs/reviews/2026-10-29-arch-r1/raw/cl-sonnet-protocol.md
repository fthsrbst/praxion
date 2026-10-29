<!-- Reviewer: Claude Sonnet 5.5 (Claude Code workflow subagent). Lens: sonnet-protocol. Raw, unverified output. -->

### Findings

**PROTO-1 | blocker | `sessions/02` §Research findings "Mouse input path (GLFW)", §2.3, E5-E11; `architecture.md` §11 risk 1**
- **Claim:** The hands design is built on GLFW, but Minecraft 26.3 no longer uses GLFW. It moved to SDL3 in 26.3-snapshot-4. The doc's "GLFW commit inside it: unknown" is a version-baseline error, not an unknown. Everything the doc "verified" in `glfw/src/win32_window.c` (WM_INPUT handling, scan codes from `lParam`, WM_KILLFOCUS) describes code 26.3 does not run.
- **Evidence:**
  - GamingOnLinux: Minecraft "now uses SDL3 instead of GLFW for window management, input, and platform integration"; keyboard now uses SDL scancodes.
  - NeoForge 26.3 primer: LWJGL ships SDL, "most GLFW backend logic was removed or rewritten". The `rawMouseInput` option was removed from `Options`, and `updateRawMouseInput` was removed from `Window` and `InputConstants`. `MouseHandler#onMove` now takes relative x/y.
  - Nodecraft says the same, and adds that the SDL3 switch fixes a long list of input and fullscreen issues.
  - Fullscreen: Borderless Fullscreen became the default in 26.3 (GamingOnLinux). E11 should treat borderless as the main case.
  - SDL main-branch `WIN_HandleRawMouseInput` shows a different raw-input path. `hDevice` becomes the mouse ID, `WIN_SwapButtons` special-cases `hDevice == NULL`, and `GetMouseMessageSource` filters touch and pen. I read SDL main, not the SDL build bundled with 26.3, so its effect on injected events is **unverified**.
- **Recommendation:** Redo the input analysis against SDL3 and the 26.3 `MouseHandler` before E5-E11. Drop "raw input on/off" from E5, since the option no longer exists. Check how `options.txt` stores key bindings now (SDL scancodes). This is the place where "client-mod fallback" is most likely to be needed, so measure it early.
- **Confidence:** high that SDL3 is in 26.3 (3 independent sources). Medium on the downstream effects.

**PROTO-2 | major | `architecture.md` §4 (Decoder), §11 risk 2; `sessions/02` §2.2**
- **Claim:** The architecture says the decoder is "Azalea crates", but Azalea has no published release for 26.3.
  - crates.io has `0.16.0+mc26.1` (2026-03-28) as its newest version.
  - `main` README says "Currently supported Minecraft version: `26.2`".
  - 26.3 exists only as an unmerged `26.3` branch: `PROTOCOL_VERSION = 777`, `VERSION_NAME = "26.3"`. It is diverged from main by 11 commits ahead and 18 behind.
  - The branch's recent commits are bug fixes from 2026-09-24/25 ("fix ClientboundLevelChunkWithLight", "fix move entity packets", "fix out of sync particles"). So it is actively fixed, not stable.
  - Mojang's 26.4-snapshot-6 is already out (2026-10-27), so the cadence is a new release roughly every quarter.
- **Recommendation:** Treat Azalea as a pinned git dependency, with a rev for each Minecraft version, not as a library you can version-bump. State a version-pin policy (client, server jar and Azalea rev pinned together). Budget a decoder port per Minecraft release. Contradiction: `AGENTS.md` principle 2 prefers "mature library", while the Azalea README warns that "many parts are unfinished".
- **Confidence:** high.

**PROTO-3 | major | `architecture.md` §4/§11 risk 11; `sessions/02` §2.2 (Rust)**
- **Claim:** Azalea requires nightly Rust. `rust-toolchain.toml` on main is `channel = "nightly"`. The commits "Migrate from `generic_const_exprs` to `generic_const_args`" (2026-09-08) and "Rename nightly features, again" (2026-09-29) show the unstable features still shifting. The azalea-protocol README (via search snippet) says nightly is needed because specialization is unstable. The body (hands, hooks, watchdog) would be a Windows product on nightly.
- **Recommendation:** Pin the exact nightly date in the repo. Decide explicitly whether you accept a nightly-only body. Consider a minimal own decoder for the packets you need (a "Plan B" the doc already names), and build E14 around that comparison.
- **Confidence:** high.

**PROTO-4 | major | `sessions/02` §Protocol facts, §2.2 D2, E2**
- **Claim:** A passive decoder cannot decode chunks the way Azalea's own bot does, because of Known Packs. The real vanilla client answers Select Known Packs with `minecraft:core`, so the server omits Registry Data entries. Azalea's bot client replies with an empty list (`known_packs: vec![]` in `select_known_packs`), which forces the server to send full data. The `RegistryHolder` stores entries as `Option<NbtCompound>`. In the passive case, `dimension_type` (min_y, height) and similar entries will be absent, and chunk decoding needs them. The doc notes "needs local vanilla registry data" but D2 names only `--reports`. Reports give protocol ids and block states, not `dimension_type` contents. Those are in the `--server` data output (the wiki.vg and Gamepedia tutorials say `--reports` and `--server` are separate).
  - E2 as written ("Azalea decodes a 26.3 offline-mode session") could be satisfied by running an Azalea bot as the client. That would pass while the real passive case still fails.
- **Recommendation:** Specify in E2 that the traffic is a stock client, captured and replayed. Add a decoder step that seeds the registry holder from the server data generator output (`--server`/`--all`), with the Known Packs list read from the client's packet. Add a pass criterion "dimension heights match `dimension_type` of the jar".
- **Confidence:** medium-high (code read; the passive case not run).

**PROTO-5 | major | `sessions/02` §2.7 (E1, E3, E13), "Round 1 also on macOS"**
- **Claim:** Latency thresholds measured on macOS say nothing about Windows, where the real system runs. Windows scheduling and timers (`timeBeginPeriod` per-process since Windows 10 2004, and the doc's own note about occluded windows) behave differently. A p99 below 1 ms for an extra user-space hop is plausible on a quiet machine, but the PC will also be running a game (rendering plus JVM), the server JVM and the Praxion processes. E1 as a pass/fail gate may fail from scheduler preemption, not from design.
- **Recommendation:** Treat macOS round 1 as functional (decode correctness) only. Rerun E1/E3 on the Windows PC under game load, and report p99.9 too. Use `TCP_NODELAY` on both legs, a bounded queue to the decoder that drops decode work but never blocks forwarding, and a small high-priority forwarder thread. State what the 1 ms is protecting: actual gameplay needs are likely looser.
- **Confidence:** medium.

**PROTO-6 | major | `architecture.md` §2/§5 (loss of sync handling); `sessions/02` D1**
- **Claim:** "A decoder that loses sync marks perception invalid" is under-specified when frames are dropped or the decoder lags. The decoder is stateful across the stream: connection state (Login, Configuration, Play, and Play back to Configuration), the compression threshold from Set Compression, and registry data from the configuration phase. If any frame is dropped to protect the forwarding path, later packets are mis-decoded. Azalea's `Connection` is built to own the socket and to switch state itself (`set_compression_threshold`). A passive consumer has to drive the state machine from the packets it observes. I did not find ready-made passive helpers in the files I read: `read_raw_packet_from_buffer` and `deserialize_packet` are what exist, plus the frame splitter.
  - Positive: the `declare_state_packets` macro does return `LeftoverData` for under-read packets, so silent partial parsing is detected. I first suspected otherwise from `read.rs` alone, but the check lives in `azalea-protocol-macros` (lines 257-264, 308-311).
- **Recommendation:** Put a light frame-level state tracker (state, compression, Known Packs) next to the forwarder, separate from the heavy decode, and define the recovery: re-sync only at a state boundary or reconnection. Add a "loss of sync" fault-injection test (drop a frame, expect perception invalid and no crash).
- **Confidence:** medium.

**PROTO-7 | major | `architecture.md` §1 "singleplayer", `sessions/02` §2.1, `sessions/01`**
- **Claim:** An external local server is not singleplayer from the user's side.
  - It drops these singleplayer behaviors: the world appears in Multiplayer, not the world list; Esc does not pause the world (the doc covers this only for focus loss); no Open to LAN or cheats toggle; the server must be started and stopped by Praxion tooling; two JVMs run.
  - **`white-list` now defaults to `true` for new `server.properties` in 26.3** (Nodecraft), so the first connection will be refused unless the setup sets `white-list=false` or adds the user.
  - **Unverified, plausible risks:** the player gets the offline-mode UUID, so existing singleplayer inventory, advancements and skin will not carry over; with offline UUIDs the skin is probably the default one. `enforce-secure-profile` interacts with chat signing: the wiki says that when it is off chat is unsigned and "clients see a warning". I did not find authoritative text on `level.dat` player data migration (my search returned nothing on it).
- **Recommendation:** Add a session-1 amendment saying exactly what the user gives up, and ask the user. Provide a "new world only" flow, with a documented import of an existing save, and test it. Set `white-list=false`, `enforce-secure-profile=false` and `player-idle-timeout` deliberately in the generated `server.properties`.
- **Confidence:** high on whitelist and no-pause, low on UUID and skin effects.

**PROTO-8 | minor | `sessions/02` §Protocol facts (offline mode, compression)**
- **Claim:** The "no encryption in offline mode" fact is confirmed, but cite it correctly. The wiki Packets page says "as of 1.21, the vanilla server never uses encryption in offline mode". Compression is separate: default threshold is 256, `-1` disables compression, `0` compresses everything (server.properties wiki). Setting `-1` on loopback is therefore valid. Note Azalea's decompressor uses `flate2` with no explicit backend, so without `-1` a chunk burst pays for zlib decompression in the decode path.
- **Recommendation:** Set `network-compression-threshold=-1` in the generated server config and add a test with 256 as well (so you do not depend on this setting). The forwarder must still observe Set Compression, since a server could send it with other values.
- **Confidence:** high.

**PROTO-9 | major | `sessions/02` E3 (5 ms), `architecture.md` §2**
- **Claim:** No published decode-speed data supports the 5 ms budget. The only Azalea benchmark in `azalea-protocol/benches/read.rs` measures one packet (`ClientboundWaypoint`). The README claim of speed is about NBT only (the azalea-nbt crate says it is "the fastest NBT decoder"). Chunk data is kept as bytes in the packet, and a commit notes it cannot be parsed in azalea-protocol because it needs context from other packets, so chunk decoding cost lives in azalea-world and has no benchmark I could find. A decode of one chunk is unlikely to take 5 ms in Rust, but bursts (login sends hundreds of chunks) and the "line of sight" world model are unmeasured.
- **Recommendation:** Treat E3 as a measurement of two numbers: decode time per packet type, and world-model update time. Keep "must not delay forwarding" as the hard criterion. Decide the p99 budget from what the reflexes actually need.
- **Confidence:** medium.

**PROTO-10 | minor | `sessions/02` fallback MCProtocolLib; `architecture.md` §11 risk 2**
- **Claim:** The fallback is real but not drop-in. MCProtocolLib master has "26.3 release" merged 2026-10-14 (commit list), but the GitHub releases list I fetched ends at 1.21.4-1 (2025-01-18); the README gives only the opencollab Maven coordinates with a placeholder version. So the usable artifact is a snapshot or a source build (I could not confirm which, unverified). It is Java, so the body would need a JVM sidecar and a second language, which contradicts the Python-plus-Rust decision.
- **Recommendation:** Reword the fallback as "own minimal decoder for needed packets, using MCProtocolLib source and the jar as references", or keep MCProtocolLib as a test oracle only.
- **Confidence:** medium.

**PROTO-11 | minor | `sessions/02` D2, `architecture.md` §8**
- **Claim:** The data-generator reports give packet ids and block states, not field layouts. D2 "game data from the Mojang data generator" does not remove the need for a hand-written or Azalea-generated parser per packet. I could not open the wiki page (404); the evidence is wiki.vg and Gamepedia text (blocks.json with protocol ids, a packets report with all packets) and my background knowledge. Treat the "ids only, no fields" part as **unverified** until a run of the generator.
- **Recommendation:** Add a cheap check to E2: run `java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --all` for 26.3, and diff `packets.json` ids against Azalea's table.
- **Confidence:** medium.

**PROTO-12 | minor | `sessions/02` E2 "sampled blocks match server commands"**
- **Claim:** The check needs the server console or RCON, which the "zero cheats" rule treats as a harness channel. Make sure the harness channel is separate from the agent's APIs and logged, and that E2's block sampling does not use operator commands in the player's session.
- **Recommendation:** Use console or the Management Protocol (see Ideas), not in-game ops.
- **Confidence:** medium.

### Ideas

1. **Differential decode oracle.** Since 26.1 the jar is unobfuscated, so run the game's own packet codecs in a small Java harness to decode captured traffic and compare with Azalea per packet. It gives a pass criterion stronger than "zero errors".
2. **Capture corpus first.** Record real stock-client sessions (join, 10 min walk, dimension change, config re-entry) as pcap-like files before writing code. Every decoder, replay and CI test reuses them.
3. **Server Management Protocol for the harness.** JSON-RPC over WebSocket, `rpc.discover`, version 3.1.0 per Nodecraft (wiki page documents players, gamerules, save/stop, settings, notifications). It replaces RCON for the referee's server control; it has no block or inventory reads, so the referee still needs the proxy or console.
4. **Version-pin manifest.** One file pins client, server jar, Azalea rev, nightly date and generated data hash; CI refuses mismatches.
5. **Minimal own decoder for ~40 packets.** Chunk, block update, entity position/metadata, health, inventory, chat, teleport. Compare cost against porting Azalea each quarter.
6. **Stay on 26.3 deliberately.** Mojang servers keep old jars, and the Minecraft Launcher can pin versions, so skip 26.4 until a stable Azalea tag exists.
7. **Status-ping-aware forwarder.** The Multiplayer screen opens extra Status connections; test the forwarder with those and with reconnects.
8. **Passive "decoder canary" in-game.** Compare decoded player position and health with the F3 screen on a fixed route to detect silent drift without operator commands.

### Questions for the user

1. Are you willing to give up singleplayer behavior (Esc pauses the world, Open to LAN, world list, your current character's inventory and skin) for a dedicated-server setup? Do you want an import path for existing worlds?
2. Is a nightly-only Rust toolchain and a git-pinned Azalea branch acceptable for the product, or should the decoder be our own minimal code?
3. Should we stay on Minecraft 26.3 until the experiments pass, even though 26.4 snapshots are already out?
4. The client now uses SDL3, not GLFW: do you want the hands experiments (E5-E11) redone against SDL3 before anything else in round 2?
5. Is a macOS pass for E1-E3 meant only as a decode-correctness test, with latency thresholds judged on the Windows PC?

### Sources

- https://crates.io/api/v1/crates/azalea (newest 0.16.0+mc26.1, 2026-03-28)
- https://github.com/azalea-rs/azalea (README: supported 26.2; non-goal: multiple versions)
- https://raw.githubusercontent.com/azalea-rs/azalea/main/README.md
- https://raw.githubusercontent.com/azalea-rs/azalea/main/CHANGELOG.md
- https://raw.githubusercontent.com/azalea-rs/azalea/main/rust-toolchain.toml (nightly)
- https://api.github.com/repos/azalea-rs/azalea/branches?per_page=100
- https://api.github.com/repos/azalea-rs/azalea/commits?per_page=15
- https://api.github.com/repos/azalea-rs/azalea/commits?sha=26.3&per_page=8
- https://api.github.com/repos/azalea-rs/azalea/compare/main...26.3
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/src/packets/mod.rs (777, "26.3")
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/src/read.rs
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/Cargo.toml
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/azalea-protocol-macros/src/lib.rs (LeftoverData)
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/benches/read.rs
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-protocol/src/connect.rs
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-client/src/plugins/packet/config/mod.rs (empty known packs)
- https://raw.githubusercontent.com/azalea-rs/azalea/26.3/azalea-core/src/registry_holder/mod.rs
- https://piston-meta.mojang.com/mc/game/version_manifest_v2.json (release 26.3 on 2026-09-15; 26.4-snapshot-6 on 2026-10-27)
- https://minecraft.wiki/w/Java_Edition_protocol/Packets (777, offline never encrypts, compression)
- https://minecraft.wiki/w/Server.properties
- https://minecraft.wiki/w/Minecraft_Server_Management_Protocol
- https://nodecraft.com/blog/game-news-tips/minecraft-java-26-3-wilderness-bound-is-out-what-it-means-for-your-server (SDL3, `white-list` default true)
- https://www.gamingonlinux.com/2026/09/minecraft-java-edition-26-3-wilderness-bound-update-released/page=1/ (SDL3, borderless default)
- https://docs.neoforged.net/primer/docs/26.3/ (SDL, `rawMouseInput` removed, new transient-block packet)
- https://raw.githubusercontent.com/libsdl-org/SDL/main/src/video/windows/SDL_windowsevents.c (SDL main, not the bundled build)
- https://api.github.com/repos/GeyserMC/MCProtocolLib/commits?per_page=8
- https://api.github.com/repos/GeyserMC/MCProtocolLib/releases?per_page=5
- https://raw.githubusercontent.com/GeyserMC/MCProtocolLib/master/README.md
- https://wiki.vg/Data_Generators and https://minecraft.gamepedia.com/Tutorials/Running_the_Data_Generator (via search results, not fetched directly)