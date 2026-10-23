# Session 1: Product definition and scope

Started 2026-10-21. Sub-topics are closed one at a time.

| # | Sub-topic | State |
|---|---|---|
| 1.1 | Edition, version, vanilla or modded | Decided |
| 1.2 | How game-agnostic the core is | Decided: Minecraft only |
| 1.3 | Where it plays; cheat policy; difficulty | Decided (body route still open, session 2) |
| 1.4 | Ultimate goal when playing alone | Decided |
| 1.5 | First demo | Decided; milestone ladder proposed |
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

## 1.5 First demo (partial)

### Decision (user, 2026-10-22)
- **The first demo is player-bound: Praxion plays the user's own character.** Companion is not the first demo.
- **The user watches on their own screen.** Switching from the user playing to Praxion playing
  must not change anything about how the game window looks.
- **Taking control back:** touching the mouse or keyboard, or closing Praxion.
- **A private local server on the user's PC counts as singleplayer.**
- **Implementation effort is not a criterion**: AI agents write the code. Quality and
  reliability still are.

### Consequence for the body route
- Route B alone (Praxion logs in under the user's name, the user watches from another client)
  fails the "screen must not change" requirement.
- Route A alone (screen in, keyboard/mouse out) meets it but knows the world only from pixels.
- Proposal under discussion: **eyes on the network, hands on the keyboard** (see below).

### Proposal: eyes on the network, hands on the keyboard
- The world runs on a private local vanilla server (`online-mode=false`). The user's normal client
  connects to it **through a local, read-only proxy** run by Praxion.
- **Perception:** the proxy reads every packet in both directions. In offline mode the
  connection is not encrypted (encryption is negotiated only in online mode), so the proxy decodes
  the exact world state: chunks, entities, inventory, health, and the player's own position and
  rotation as the client reports them. It never alters or injects packets.
- **Action:** Praxion drives the real game window with operating-system keyboard and mouse input.
  The client behaves exactly as if the user played: same camera, same animations, same screen.
- **Closed loop:** every input is checked against what the client then reports (rotation and
  position packets) and what the server answers (block changes, inventory updates).
- **Take-over:** Windows low-level input hooks flag injected events (`LLKHF_INJECTED`,
  `LLMHF_INJECTED`), so any physical keyboard or mouse event can be told apart from Praxion's and
  can stop Praxion immediately.
- **Fair play comes free:** the proxy sees exactly what the user's client receives. Occlusion and
  sight range still need enforcing (session 3).
- **Risks to verify (session 2):** synthetic mouse input reaching Minecraft's raw-input path;
  dependence on sensitivity, FOV, GUI scale and window size; GUI clicks (inventory, crafting)
  need exact slot coordinates; the window must keep focus and the PC is unusable for anything
  else while Praxion plays; frame-rate and input timing jitter; future versions changing GUI layout.
- **Fallback if the risks prove too costly:** a client mod (route C).

### Decision (user, 2026-10-23)
- **Direction accepted: eyes on the network, hands on the keyboard.** Details and the risk
  experiments belong to session 2; a client mod stays the fallback.
- **Never change the user's game settings.** Praxion adapts to sensitivity, FOV, GUI scale,
  window size and key bindings as they are.
- **The PC being unusable for other work while Praxion plays is acceptable.**

### How Praxion adapts without touching settings (proposal)
1. **Read, never write, the client's own config.** `.minecraft/options.txt` holds mouse
   sensitivity, FOV, GUI scale, raw-input flag and every key binding (`key_key.forward`, ...).
   Praxion uses the user's own keys. Window client size comes from the Win32 API. A file watcher
   picks up changes the user makes in the options screen.
2. **Self-calibrate from the proxy.** Every mouse movement Praxion sends is followed by the
   rotation the client reports to the server, so the counts-to-degrees gain is measured, not
   assumed, and re-measured continuously. Vanilla's formula (sensitivity s: f = 0.6s + 0.2,
   degrees per count = 8 * f^3 * 0.15) is a starting estimate to verify against the 26.x source.
3. **Close every loop.** Turning: move, measure, correct until within tolerance; this also
   absorbs Windows pointer acceleration if raw input is off. Clicking a slot: the client's
   container-click packet carries the slot index, so a wrong click is detected at once.
   Slot positions start from GUI scale and window size (vanilla auto scale: the largest scale
   that fits width/320 and height/240, capped by the setting).
4. **FOV does not affect perception**, which comes from the network. It only matters for deciding
   what the player could see on screen (session 3).

## 1.4 Ultimate goal when playing alone

### Background
- No built-in end: altoclef fixes one goal (kill the Ender Dragon) and plays like a speedrun
  machine; Voyager chases novelty and never settles; benchmarks use single tasks.
- Human players layer needs (survive, food, shelter, tools), projects (base, farm, mine,
  storage) and aspirations (Nether, enchanting, the dragon).
- Player-bound play adds a permission question: it is the user's character and the user's world.

### Decision (user, 2026-10-23)
- **The user's projects come first, then Praxion's own ideas.**
- **Long-term goals arrive three ways:** in-game chat, a separate interface, or, in autonomous
  mode, inferred by Praxion itself.
- **Death is acceptable.** Praxion plays the game normally, risks included.
- **The ruleset (protected areas, items, limits) may be written with an LLM.**
- **Completion-oriented:** Praxion plays toward finishing the game, and an LLM may decide to keep
  playing beyond it.

### Technical consequences (proposal)
- **LLM writes the ruleset; code enforces it.** The user states rules in plain language; an LLM
  translates them into a structured ruleset (regions as boxes, protected blocks and items, risk
  limits), which is shown back for the user to confirm. Enforcement is deterministic in the
  action layer: an action that would break a protected block is refused before input is sent,
  never left to a model's judgement in the moment. Same principle as zero cheats: what is not
  allowed is not reachable.
- **Goal layers:** needs, projects, aspirations. The aspiration arc follows the game's
  progression to the Ender Dragon; that arc needs long-horizon planning (stronghold location by
  eyes of ender, portal, the fight) and is not first-demo scope.

## 1.5 First demo (continued)

### Proposed scenario (accepted as a base)
Fresh survival world, Normal. The user plays, hands control over (screen unchanged), gives a chat
command ("collect wood, make a crafting table and a stone pickaxe"), Praxion executes with
proxy-verified steps, the user touches the mouse mid-task and Praxion stops at once, the user
hands over again and says "carry on", Praxion survives the first night alone (food, shelter,
mobs) and moves toward iron tools by its own choice next morning.

### Decision (user, 2026-10-23)
- **Capability target is the whole game: Praxion must be able to reach and kill the Ender
  Dragon.** The scenario above is the opening, not the ceiling.
- **Background knowledge via RAG** is part of the design; discussed in a later session.
- **Hand-over can start three ways:** in-game chat command, a global hotkey the game never sees,
  and a button in a separate interface.
- **The demo starts in a fresh world.**
- **The demo must be flawless and smooth.** Audience: the community and the user's professor.

### Proposal: milestone ladder (to confirm in 1.6)
The full run is measured as a ladder; each rung is a checkpoint that can be verified from the
proxy's view of the world:
1. First night survived (tools, food, shelter).
2. Iron tools and armour.
3. Diamonds.
4. Nether: blaze rods, ender pearls.
5. Stronghold located, End portal activated.
6. Ender Dragon killed.

### Open
- Licensing of RAG sources: Minecraft Wiki text is CC BY-NC-SA (non-commercial), which conflicts
  with an open-core business model; game facts can instead come from the server's data reports.
  To settle in the knowledge session.
