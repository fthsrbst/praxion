# Praxion architecture (draft 1)

Status: draft for review, 2026-10-29. Written by Claude Sonnet 5.5 from `decisions.md` and
`sessions/02` to `sessions/15`. The writer does not review this draft (AGENTS.md); a review round by a
model other than the writer is the next step.

Tags: **[D]** decided with the user (source in `decisions.md`), **[P]** proposed by the writer while
consolidating (not yet agreed), **[O]** open. Anything untagged is explanation.

## 1. What Praxion is

An autonomous Minecraft agent (Java Edition 26.3, singleplayer, Windows) that plays the user's own
character through the real client, writes its own skills while it plays, verifies them against the
world and reuses them [D]. First demo: a fresh survival world; the user hands over control, gives
a verified chat command, takes control back instantly, hands over again, and Praxion survives the
first night alone. The capability target is the whole game up to the Ender Dragon [D].

Out of scope for now [D]: other games, multiplayer, Bedrock, mods, local models, cheats of any kind.

## 2. System overview

```
        ┌───────────────────────── Windows PC (dedicated to the game) ───────────────────────────┐
        │                                                                                        │
        │   Minecraft client ──TCP──► Forwarder ──TCP──► Vanilla server (offline mode, loopback) │
        │   (user's, unmodified)      (bytes untouched)                                          │
        │        ▲   │ raw input             │ copy of the byte stream                           │
        │        │   │ (SendInput)           ▼                                                   │
        │        │   │                  ┌─────────────────────────── BODY (Rust) ───────────┐    │
        │        │   └──────────────────│ decoder ─► world model (live state, line of sight)  │    │
        │        └── hands ◄────────────│ reflexes ► arbiter ► rule-list interpreter          │    │
        │            take-over hooks    │ planner (A*) ► movement style ► hands               │    │
        │            watchdog           │ receipts, ruleset enforcement, event log            │    │
        │                               └──────────────────────┬──────────────────────────────┘    │
        │                                        body port (local connection)                      │
        │                               ┌──────────────────────┴──────────────────────────────┐    │
        │   Interface window ◄──────────│ BRAIN (Python)                                       │    │
        │   (indicator, reports,        │ command parsing, curriculum, Jev + LLM clients,      │    │
        │    command box, live view)    │ skill writer + admission, memory briefing, RAG       │    │
        │                               └──────────────────────┬──────────────────────────────┘    │
        └──────────────────────────────────────────────────────┼───────────────────────────────────┘
                                                               ▼
                                            hosted Jev and LLMs (the only network dependence)
```

Process boundaries [D unless tagged]: the forwarder never encodes packets and decodes only a copy
for the body; the body and the brain are separate programs joined by the body port; a watchdog
process releases injected keys if the body dies; the interface window never takes focus. Whether
the forwarder is its own OS process or an isolated thread inside the body is **[O]**: the decision
says the forwarding path is isolated; experiments E1 to E3 must show that the extra hop keeps
decoding within the 5 ms budget, with the isolated thread as the fallback.

## 3. Time scales and who decides

| Time | Layer | Work | Runs in |
|---|---|---|---|
| under 50 ms | Reflexes | lava, falls, fire, creeper, low health; take-over | body, pure functions, no network [D] |
| 50 ms to 1 s | Arbiter, rule-list interpreter, planner | choose the owner of the body, run skills, move | body [D] |
| above 1 s | Jev | bounded choices that the code lists | brain, hosted [D] |
| seconds to minutes | LLM | plans, chat, goals, skill writing | brain, hosted [D] |
| minutes to hours | Projects, ladder | long goals, the project ledger | brain + disk [D] |

Rules [D]: nothing with a deadline under about 1 s may depend on Jev or any hosted call; the LLM is
woken by events, not a clock; every call class has a deadline and a fallback; a silent brain makes
the body stop safely.

## 4. Components

| Component | Language | Responsibility | Decisions |
|---|---|---|---|
| Forwarder | Rust | forward bytes untouched between client and server; hand a copy to the decoder | 2.2 |
| Decoder | Rust (Azalea crates, to be verified) | decode the copy using Mojang data reports; loss of sync marks perception invalid | 2.2 |
| World model | Rust | live state under the line-of-sight rule; provenance on every fact; hidden-data ledger | 3 |
| Reflexes | Rust | `evaluate(world) -> action`, fixed typed settings, first-night set growing per rung | 7 |
| Arbiter | Rust | tiers: survival, command, maintenance, projects; soft and hard thresholds; owner tokens | 7 |
| Rule-list interpreter | Rust | runs teleo-reactive skills each tick; resume equals re-evaluate | 5 |
| Primitives | Rust | hand-written low-level behaviours with a cancellation class; swappable | 5, 4 |
| Planner | Rust | A* over known blocks, segmented; may dig and bridge at a higher cost; bounded stuck handling | 6 |
| Movement style | Rust | human-like camera, speed, timing above the planner | 6, 13 |
| Hands | Rust | `SendInput`, scan codes, two mouse modes, closed-loop calibration, focus rules | 2.3 |
| Take-over | Rust | low-level hooks tell physical from injected input; keys held in dangerous moments | 2.4, 4 |
| Receipts | Rust | evidence ladder; success needs the server's word | 4 |
| Ruleset enforcement | Rust | protected blocks, items, regions refused before any input | 1.4, 6, 7 |
| Event log | Rust | append-only, replayable, every claim cites a receipt id | 14 |
| Commands | Python | prefix chat or interface text, mapped by Jev to a closed schema, validated by code | 9 |
| Curriculum | Python | LLM proposes goals with a completion condition; project ledger on disk | 10 |
| Jev and LLM clients | Python | role-based models, stamped directives, cost tracking and warning | 8 |
| Skill writer and admission | Python + harness | the LLM writes rule lists; compiler derives guards; staged admission | 5, 11 |
| Memory and briefing | Python (files read through the body) **[P]** | JSON Lines per world, index in RAM, a briefing with a token cap | 12 |
| Wiki RAG | Python | local index of Minecraft Wiki text for the LLM's knowledge questions | 12 |
| Interface window | not decided **[O]** | indicator, reports, command box, live view, memory viewer | 2.4, 9, 14 |
| Test harness | Python + Rust | logged setup channel, referee, fixtures, practice room on a test server | 14, 11 |

## 5. Data flows

**Perception.** Client and server talk through the forwarder. The decoder reads the copy and the
world model keeps only what a player could see: line of sight in every direction within about 64
blocks, plus what the F3 screen shows [D]. Everything else is denied and written to the ledger. Out
of sight a query answers `unknown`, not "absent" **[P]**.

**Action and verification.** The arbiter picks the owner; the interpreter picks a rule; the planner
and movement style produce keys and mouse moves; hands inject them. The proxy then sees what the
client sends and what the server answers. A receipt reports the evidence level reached: sent,
client agreed, server confirmed, effect seen. Without the server's word the result is
`unconfirmed`, Praxion re-reads the truth and only then retries [D]. A task whose success cannot be
computed within the perception policy is `unverifiable` and never counted as success [D].

**Commands.** A prefixed chat message (read as a packet, no screen reading) or text from the
interface becomes a closed command through Jev and code validation [D]. A new command suspends the
old one; "stop" and "cancel" always work [D]. Low risk and ambiguous: guess, start, ask without
blocking; high risk: ask first [D].

**The brain loop.** An event (program finished, monitor fired, skill failed, chat, new threat,
take-over ended, ruleset refusal, or a heartbeat of about 60 s) wakes the brain. It reads a
briefing (facts with age and source, within a token cap), picks or writes a rule list, and sends
it to the body as a directive stamped with generation, tick and model. The body adopts it only if
the generation is unchanged and the guards hold on the actual state [D, stamped directives as [P]].

**Hand-over and take-over.** Hand-over by chat command, global hotkey or the interface; it starts
when no physical key is held **[P]**. Any physical key or clear mouse move takes control back in
under 50 ms; the interrupted task waits for "carry on" and is recomputed from the world [D].

## 6. Skills and learning

A skill is a teleo-reactive rule list `condition -> action`, interpreted by the body [D]. Actions
are hand-written primitives or other skills. The LLM writes steps and a goal; the compiler derives
the conditions from the primitives' effects, generated from game data [D]. The LLM may only
combine existing primitives and skills [D].

Admission [D, details as [P] in session 11]: static checks, then a practice room (a copy of the world
on a test server, three runs against a no-skill baseline), then probation on the test server, then
active. A skill learned in Praxion's own play enters without asking; a skill taught through chat
needs the user's approval. Versions live in git with automatic rollback; the skill writer is a
low-privilege process that can write only to a candidates directory.

## 7. Safety and trust boundaries

- **Zero cheats** [D]: the agent's APIs expose only what a player can do and see; test setup uses a
  separate logged harness channel; no operator commands, no `/tp`.
- **Ruleset** [D]: written with an LLM, confirmed by the user, enforced by code before any input.
- **Take-over** [D]: injected input is distinguishable from physical input; Praxion never steals
  focus; on focus loss it releases its keys and tells the user.
- **Hosted dependence** [D]: reflexes, take-over and ruleset enforcement never use the network.
- **Keys** [D]: in the OS credential store, held only by the brain, never logged; remote brain access
  needs its own key.
- **Chat is an injection surface** [D]: only the user's player is obeyed; free text never reaches a
  planner as an instruction.
- **Privacy** [D]: recordings of the user's play and all logs stay local.

## 8. Memory and knowledge

World state is live and held by the body. Remembered facts carry age and provenance, are re-checked
when in line of sight, and Praxion tells the user when it is unsure [D]. The first memory is one JSON
Lines file per world with an in-RAM index, places and containers, an event log with a read-only
viewer and a briefing with a token cap; SQLite, vectors, lessons and a chunk cache wait for a
measured trigger [D]. Lessons are general across worlds, proposed by the LLM and accepted by code
after three proxy-verified successful traces [D]. Game data (recipes, blocks, loot) comes from the
Mojang data generator; the Minecraft Wiki text feeds a local RAG for the LLM only, and the index
is never distributed because the wiki licence is CC BY-NC-SA [D]; the conflict with an open-core
model is **[O]**. The game memory is separate from the hub [D].

## 9. Testing and observability

Three levels, most tests at the bottom: fake world, protocol test body, real body [D]. A referee
reads server facts; Praxion's own report never decides a verdict [D]. Claims rest on many fresh
seeds with confidence intervals and an analysis written before the runs [D]. Fast tests run on
every change; levels 2 and 3 run nightly or on request [D]. A live window and a replayable event
log show what Praxion is thinking [D]. The test inventory is in `sessions/14-testing.md`.

## 10. Repository layout **[P]**

One repository with the layers enforced by tooling [D]. Directory names are proposed:

```
body/        Rust workspace: forwarder, decoder, world, reflexes, arbiter, interpreter,
             primitives, planner, movement, hands, takeover, receipts, ruleset, event log
brain/       Python package: commands, curriculum, models, skills, memory, rag
interface/   window (technology open)
harness/     test server control, referee, fixtures, fake world
skills/      hand-written core skills (reviewed), candidates/ (writer-only)
docs/        decisions, sessions, research, this file
spikes/      local experiment code (untracked)
```

Layering rule [D]: `brain/` may not import `body/` internals; both talk only through the body port.
The build or a lint rule fails otherwise.

## 11. Risks and open points

| # | Risk or open point | Where it is resolved |
|---|---|---|
| 1 | Synthetic mouse and keyboard input may not reach the game at endgame timing | experiments E5 to E9 (2.7) |
| 2 | Azalea may not decode 26.3, or its decoder may be too slow | E2, E3; fallback MCProtocolLib |
| 3 | The extra hop from forwarder to body may break the 5 ms budget | E1 to E3 |
| 4 | Whether a protocol test body stands in for the real body | first level-2 tests |
| 5 | Jev's value is unproven; it may not beat rules or the LLM | measurement per decision point (session 8) |
| 6 | The free curriculum may produce unverifiable goals and unrepeatable runs | learning test arm (session 11) |
| 7 | Effect declarations of primitives must be exact for the compiler to derive guards | tests in `sessions/05-skills.md` |
| 8 | Wiki RAG licence conflicts with an open-core model | business-model decision |
| 9 | Which parts are commercial and closed | business model |
| 10 | Format of the rule lists; where the guard compiler runs; who owns the memory file | first code |
| 11 | Whether the Azalea planner can be lifted out of its framework | spike (session 6) |

## 12. Not in this draft

Message formats and syntax (chosen at the first code), numeric thresholds (found by measurement),
Nether and End reflexes (written before their rungs), voice commands, reinforcement learning and
personalising the movement style (both later options).
