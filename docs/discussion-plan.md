# Discussion plan

Agreed 2026-10-21. Every topic gets its own session and is discussed in depth, not skimmed.
Two goals: the user masters each topic and its technical structure, and the agent understands
exactly what the user means. Decisions are made per session and recorded in [decisions.md](decisions.md).

## Session format

1. **Technical background:** how the topic works and how existing systems solve it, from source
   code where useful ([research/systems-review.md](research/systems-review.md)).
2. **Options:** what each costs and what it enables.
3. **Questions:** start from the list below; add new ones as they come up.
4. **Decision:** what was chosen and why; what was rejected and why; what stays open.
5. **Consequences** for later sessions.

Notes: one file per session, `docs/sessions/NN-topic.md`. Each session opens with a summary of
earlier decisions.

## Order and rationale

Dependency order: first what we build and which body it plays through, then how that body sees
the world and verifies its actions, then skills and the brain. "What is a skill?" was first in an
earlier proposal; the review showed that the two play modes and the perception policy shape the
skill definition, so it moved to session 5.

Proposed order: 1, 2, 3, 4, 5, 8, 7, 9, 6, 11, 10, 12, 13, 14, 15.
Session 8 (Jev and LLMs) comes before 7 (reflexes) because Jev's role may reshape the
fast-decision layer. Session 13 (natural feel) is its own session but is also a criterion in every session.

## Topics

### 1. Product definition and scope
- Java or Bedrock; versions; vanilla or modded?
- Which worlds: your own world, your own server, other servers? (Most public servers ban bots.)
- Cheat policy: none at all, or a visible test switch?
- When playing alone, what is the ultimate goal: survive and progress, explore, build, the Ender
  Dragon, or self-generated goals?
- What does the first demo look like?
- How is success measured (command success rate, time, a criterion for "natural feel")?

### 2. Body and connection layer
Background: client-server model, protocol states, client-authoritative movement, version
differences. Three routes: protocol client (what mineflayer does), in-client mod (Baritone and
altoclef on Fabric), screen plus keyboard/mouse. Mineflayer is studied in detail here.
- Companion mode: separate account, LAN, offline-mode server?
- "I give it my screen": a mod controlling the player from inside, or screen in, keyboard/mouse out?
- How does the user take control back and hand it over again?
- How does one brain plug into two bodies (body port)?
- Write the protocol layer ourselves or adapt an MIT/BSD library? Which language?
- Tick timing on Windows.

### 3. Perception and world model
Background: chunk and entity data, occlusion, provenance tags and profiles (Craftonomous),
world caches (Baritone 2-bit chunks, altoclef container cache).
- Should the agent know only what a player could see?
- How long is remembered information valid, and how does it age?
- In player-bound mode, are screenshots part of perception?
- Where and how is world memory stored?

### 4. Action layer and verification
Background: how dig, place and inventory actions work in the protocol; optimistic prediction,
server confirmation, sequence acks (1.19+), inventory `stateId` and forced resync; evidence
levels; typed failure kinds.
- Which evidence level does each action result carry?
- What is the failure taxonomy?
- How are tasks the inventory cannot show (building, planting) verified?

### 5. Skill definition and execution model
Background: script model (Voyager, mindcraft) next to the reactive task tree (altoclef);
Craftonomous's skill contract; altoclef's `isFinished` and `isEqual`.
- What goes in the skill contract (expected effect, side-effect declaration, time estimate,
  interruptibility)?
- Reactive, script or both?
- How do skills call each other and declare dependencies?
- What form does LLM-written skill code take?

### 6. Movement and pathfinding
Background: A*, cost models (Baritone: expected ticks), time-sliced search, segmented plans,
forward physics simulation, stuck detection, blacklists.
- When may a path change the world (dig through, bridge)?
- How are stuck loops and endless retries prevented?
- Is human-like movement part of pathfinding or a separate layer?

### 7. Fast-decision layer: reflexes and priority arbitration
Background: mindcraft modes (fixed order), altoclef chains (dynamic priority), Baritone processes
(commands, temporary vs permanent, safe-to-cancel). Reflexes, player commands and the agent's own
plan meet in one arbiter.
- Which decisions take milliseconds, which seconds, which minutes?
- How does interrupted work continue?
- Where does Jev sit in this layer?

### 8. Jev and the LLM brain
Background: [research/decision-models.md](research/decision-models.md): Jev's API, question types,
calibration, latency and weaknesses; LLM roles (planner, chat, coder, critic); latency budget and
cost; turning game state into text.
- Which judgements go to Jev, which to an LLM, which to code?
- Which hosted LLMs in which roles? Hourly cost limit?
- How is state prepared given Jev's weakness with numbers?
- What happens when Jev is confident and wrong?

### 9. Player commands and task management
Input: Odyssey's tech-tree solver, altoclef's `ResourceTask`.
- Command channel: in-game chat, voice, a separate interface?
- Ambiguous commands ("collect ore"): ask or guess?
- Conflicts between a command, the agent's plan and danger.
- Reporting: when and how?
- Which commands must work from day one?

### 10. Autonomy: goal generation and long-horizon planning
Background: Voyager's curriculum and its limit, task DAGs (VillagerAgent), backward
decomposition (BAR), tech-tree graphs.
- What does the agent want to do on its own, and how does it decide?
- How does it behave toward the user as a companion?
- How do long projects (a base, the Nether) continue across sessions?

### 11. Skill learning and LLM-written code
Background: Voyager's loop, agent-swarm's candidate/approval pipeline, sandboxes (SES, separate
process, seccomp, quotas), static pre-checks, Wilson reliability, retirement lessons, storage and retrieval.
- What evidence admits a new skill into the library? Can it enter without the user's approval?
- Which API does generated code target, with what permissions?
- Versioning and rollback.
- Is learning by watching the user still a goal?

### 12. Memory and knowledge (RAG)
- What is remembered across sessions: the world, the user, events, its own experience?
- Background game knowledge through RAG: which sources, how retrieved, how kept current?
- Source licensing (Minecraft Wiki is CC BY-NC-SA) versus first-party data reports.
- How does it relate to Mnema?

### 13. Natural player feel
Background: `human.js` calibration (recorded players), look quantization, reaction delay,
personality, chat.
- Priority for naturalness: movement, decisions or conversation?
- May it make mistakes, may it die?
- Personality, name, language, voice?
- Are superhuman abilities (speed, never tiring) allowed?

### 14. Testing, evaluation and observability
Background: offline tests against a fake world (Craftonomous), a live test server, benchmark
reliability, run reports.
- How do we prove that something works?
- How do we watch what the agent is thinking?

### 15. Infrastructure and operations
- Where does it run (Windows PC, Pi, cloud)? Language and repository layout?
- Monitoring and limiting hosted-model cost.
- Keys and security.
