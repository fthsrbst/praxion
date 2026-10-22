# Session 1: Product definition and scope

Started 2026-10-21. Sub-topics are closed one at a time.

| # | Sub-topic | State |
|---|---|---|
| 1.1 | Edition, version, vanilla or modded | Decided |
| 1.2 | How game-agnostic the core is | Decided: Minecraft only |
| 1.3 | Where it plays; cheat policy; difficulty | Decided (body route still open, session 2) |
| 1.4 | Ultimate goal when playing alone | Open |
| 1.5 | First demo | Open |
| 1.6 | How success is measured | Open |

## 1.1 Edition, version, vanilla or modded

### Background
- **Java and Bedrock are different games on the wire.** Java: TCP protocol, well documented by the
  community, and nearly all bot tooling (mineflayer, Baritone, altoclef, Fabric). Bedrock: UDP
  (RakNet), a different protocol, little tooling, no client mods on consoles.
- **Player-bound mode needs an in-client mod to be robust**, which only Java allows. On Bedrock
  only screen-in, keyboard/mouse-out remains.
- **Every release changes the protocol** (packet ids, fields, block and item registries) and
  breaks bots: mineflayer's most-discussed issues are per-version support threads. Client mods are
  per-version too.
- **Versioning:** 1.21.11 (December 2025) was the last `1.x` release; versions are now year-based.
  Newest full release found: 26.2 (2026-06-16); 26.3 was in snapshots, release unconfirmed.
- **Java is unobfuscated from 26.1.** Fabric's 26.1.2 docs: no obfuscation mappings needed, Mojang
  names and parameter names included. This makes reading and modding the client much easier.

### Decision
- **Java Edition.** Developed, tested and run on **Windows**.
- **The demo targets the newest stable release** at the time coding starts. **Broad version
  support comes later.**
- **Vanilla first.** Block, item and recipe knowledge is read from data, not hard-coded, so
  modded play stays possible later without being built now.

Rejected: Bedrock (no client mods, little tooling, blocks player-bound mode); supporting a version
range from day one (mineflayer shows the cost: version branches spread through every plugin).

### Technical consequences (proposals; to confirm in session 2)
- **Version-specific code lives in one layer.** Because broad support is a stated goal, the demo
  must not let version details leak upward. The wire codec and the game data (block states,
  items, recipes, packet ids) form a per-version "version pack"; everything above sees only
  Praxion's own model. Broad support later means adding packs, not editing the agent.
  Mineflayer's counter-example: `supportFeature(...)` checks scattered across 40+ plugins.
- **Where version data comes from.** Two sources, to compare in session 2:
  - The official server's data generator
    (`java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports`) writes
    `blocks.json` (every block state with its protocol id) and a packets report (packet ids per
    protocol state). First-party, generated per version.
  - PrismarineJS `minecraft-data`: community JSON for protocol, blocks, items, recipes across
    versions. Using it as data is a separate question from the "mineflayer is not a dependency" decision.
- **Since 1.20.2 the server sends some registries at login** (configuration phase), but block
  states and items are built into the client and must come from the version pack.
- **The two bodies meet the version boundary at different levels.** The protocol client
  (companion) handles versions at the wire level; the in-client mod (player-bound) handles them at
  the Minecraft code level and never parses packets. Both translate into the same Praxion model.
- **Windows tick timing.** Mineflayer warns that its 50 ms physics loop is inaccurate on Windows
  (15.6 ms default timer resolution). The runtime and language choice must give a stable tick
  on Windows (session 2 / 15).
- **Test server.** A local vanilla dedicated server on Windows, `online-mode=false` for test bots.
  The account question for companion mode in the user's real world is sub-topic 1.3 and session 2.

### Open
- Which server software runs the user's world: singleplayer (integrated server, "Open to LAN"),
  vanilla dedicated, or Paper/Spigot? Paper changes timings and adds anti-cheat behaviour. → 1.3

### Sources
- Fabric, Migrating Mappings 26.1.2: https://docs.fabricmc.net/26.1.2/develop/porting/mappings/
- ADTmag, Mojang to remove obfuscation (2025-11-04): https://adtmag.com/articles/2025/11/04/mojang-studios-to-remove-code-obfuscation-from-minecraft-java-edition.aspx
- wiki.vg, Data Generators: https://wiki.vg/Data_Generators
- 2026 numbering change: https://www.gameserverkings.com/knowledge-base/minecraft/updating-your-server-version/

## 1.2 How game-agnostic the core is

### Background
- README and AGENTS.md said the core "must stay game-agnostic"; `docs/research/` lists a
  game-agnostic skill layer as an open gap in the field.
- What could be generic: skill contract and registry, reliability stats, verification framework,
  priority arbiter, Jev/LLM layer, code sandbox, memory. What cannot: body, perception, action
  primitives, voxel pathfinding, game knowledge, the skills themselves.
- Risk: designing a generic world interface without a second game means guessing its needs
  (AGENTS.md principle 3: no "for later" abstractions).

### Decision
- **Minecraft only. The second-game goal is dropped.** README and AGENTS.md updated.
- **Layering stays, for Minecraft's own reasons.** The user accepted the boundary proposal; with
  one game its purpose changes from "plug in other games" to: two bodies behind one brain,
  version packs, and an in-memory fake world for offline tests. The agent layers do not import
  the body or version-pack layers; this is enforced by tooling, not discipline.
  (Interpretation recorded so the user can overrule it.)

Rejected: a generic game-adapter interface up front.

## 1.3 Where it plays (partial)

### Decision
- **Singleplayer only, for now.** Multiplayer servers (own or public) are out of scope until
  decided otherwise. This also answers 1.1's open question about server software.

### Consequence: one mod could serve both modes (proposal for session 2)
- A singleplayer world runs an **integrated server inside the client process**. A Fabric mod in
  the user's client therefore reaches both the client side (the user's own player) and the server
  side (the world, its entities, its players).
- **Player-bound mode:** the mod drives the user's client player (as Baritone and altoclef do).
- **Companion mode:** the mod spawns a **server-side fake player** in the integrated server and
  drives it. Precedent: Carpet's `/player` (gnembon/fabric-carpet, MIT, active, last commit
  2026-10-18): `EntityPlayerMPFake extends ServerPlayer`, joined through
  `PlayerList.placeNewPlayer` with a `FakeClientConnection`; `EntityPlayerActionPack` (~640 lines)
  drives use, attack, jump, move and look. Carpet fake players count as real players for chunk
  loading, mob spawning and random ticks, and can walk, mine and place.
- What this removes for the demo: a network protocol client, the wire codec, and a second
  Minecraft account. Joining an "Open to LAN" world with a protocol client would most likely need
  a second licensed account, because LAN worlds authenticate joining players (unverified).
- What it costs:
  - **The server side is omniscient.** Every loaded block and entity is readable, so the fair-play
    perception policy must be enforced by Praxion itself (session 3).
  - **Server-side movement differs from client-side movement.** A fake player's physics run in the
    server tick, not through the client's prediction and correction. Effect on "natural feel"
    needs checking (sessions 2 and 13).
  - **Multiplayer later needs another body.** A protocol client (or a client mod on a second
    install) comes back when servers enter scope; the body port keeps that a contained addition.
  - **Version coupling moves into the mod.** Since 26.1 the game is unobfuscated, which makes
    porting the mod between versions easier, but it is still per-version work.
- Singleplayer pauses when the game loses focus or opens the menu. altoclef turns off
  `pauseOnLostFocus` while it runs; Praxion needs the same in both modes.

### Sources
- Carpet: https://github.com/gnembon/fabric-carpet ; fake players described at
  https://modrinth.com/project/9FuEJ7Wg/version/Na1AglK8

## 1.3 (continued): cheats, game mode, difficulty, mods

### Decision
- **Survival, zero cheats.** The agent's action API offers only what a player can do; its
  perception API only what a player can see. Test setup (world reset, test items) runs through a
  separate, logged harness channel the agent cannot reach.
- **Difficulty: Normal.** Hostile mobs, hunger and combat are in demo scope.
- **User preference: avoid mods** ("solve it without getting into mods"). Recorded as a
  preference; the body route is decided in session 2.

### Consequences of avoiding mods (to settle in session 2)
- **Companion mode needs a network client.** Without a mod, the only way into a singleplayer
  world is "Open to LAN". Vanilla LAN worlds very likely require every joining client to have a
  valid account: Fabric mods such as OfflineLAN and Quick-LAN exist precisely because vanilla
  exposes no offline toggle (unverified against the wiki). A second licensed account would follow.
- **Alternative: a private local server.** A vanilla dedicated server on the same PC with
  `online-mode=false`. The user's normal client joins it; Praxion joins without a second account.
  The world can be copied from an existing singleplayer save. To the user it plays like
  singleplayer, but it is a server process, so "singleplayer" needs re-confirming.
- **Either way Praxion needs its own protocol client** (what mineflayer does: codec, login,
  chunk parsing, physics). This is the main cost of the no-mod route.
- **Player-bound mode has no clean no-mod route.** Options:
  1. Screen capture plus synthetic keyboard and mouse (SIMA 2, Lumine, Cradle). Most general,
     slowest, and at odds with deterministic verified skills.
  2. Hand-over through a proxy: Praxion holds the user's account connection and plays; the user
     watches as a spectator and takes control by connecting. Precedent: ZenithProxy (bot mode when
     no player is connected, spectators watch the bot; 1.12.2 for 2b2t, 1.20 port in progress).
  3. On a private offline-mode server: Praxion logs in under the user's name when the user steps
     away; the user watches from a spectator account.
  4. A client mod after all.

### Sources
- OfflineLAN: https://modrinth.com/project/offlinelan · Quick-LAN: https://modrinth.com/mod/quick-lan
- ZenithProxy: https://gittrend.io/repo/rfresh2/ZenithProxy
