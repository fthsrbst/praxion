# Session 1: Product definition and scope

Started 2026-10-21. Sub-topics are closed one at a time.

| # | Sub-topic | State |
|---|---|---|
| 1.1 | Edition, version, vanilla or modded | Decided |
| 1.2 | How game-agnostic the core is | Open |
| 1.3 | Where it plays; cheat policy | Open |
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
