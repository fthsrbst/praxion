# Praxion architecture (draft 2)

Status: draft 2 for review, 2026-10-29. Draft 1 was written by Claude Sonnet 5.5. Draft 2 was written
by Claude Opus 5.5 from `decisions.md`, `sessions/02` to `sessions/15` and review round 1
([reviews/2026-10-29-arch-r1/](reviews/2026-10-29-arch-r1/README.md)). The writer does not review
this draft (AGENTS.md). Draft 2 has been reviewed once by GLM-5.3-Flash, DeepSeek v4.1 Flash and Beam
([reviews/2026-10-29-arch-d2/](reviews/2026-10-29-arch-d2/)); their findings are folded in below.
On 2026-10-29 the user accepted every proposal in this draft, so its former [P] items are now [D].

Tags: **[D]** decided with the user (source in `decisions.md`), **[P]** proposed, not yet agreed,
**[O]** open. Anything untagged is explanation. Review-round IDs (R1-…) point to
[synthesis.md](reviews/2026-10-29-arch-r1/synthesis.md).

## 1. What Praxion is

An autonomous Minecraft agent that plays the user's own character through the real, unmodified
client, writes its own skills while it plays, verifies them against the world and reuses them [D].

- **Platform** [D]: Java Edition 26.3 (protocol 777), pinned with the server jar, the Azalea commit
  and the Rust toolchain in one version manifest until Demo-1 passes; upgrades need the user's approval.
  Windows, singleplayer through a private local server, Survival, Normal difficulty.
- **Rules of play** [D]: zero cheats; Praxion never changes the user's game settings; death is
  acceptable; the user's projects come before Praxion's own ideas; big risky steps (Nether, End,
  dragon) are announced about 30 s ahead so the user can say stop.
- **Feel** [D]: a general human movement style; reaction floor about 150 ms, faster only for
  lethal danger; human aim statistics, no fatigue or faked breaks; Praxion answers in the user's
  language, short and calm, as `Praxion` (configurable).
- **First demo** [D]: a fresh survival world; hand-over, a verified chat command given while
  Praxion plays, instant take-over, hand-over again, and an autonomous first night. Must be
  flawless. The capability target is the whole game up to the Ender Dragon.
- **Success measures** [D]: milestone ladder over many fresh seeds, a proxy-verified command suite,
  smoothness (starvation gap, instant take-over), naturalness (movement statistics and a blind
  test; session 13 picks KS / energy distance, AUC ≤ 0.6 and a paired forced choice [D]), hosted cost per hour. Targets are set after baseline measurements.
- **Out of scope for now** [D]: other games, multiplayer, Bedrock, mods, local decision models
  (a local embedding model for the wiki RAG is allowed, session 12), voice,
  reinforcement learning, personalised movement.

### 1.1 First slices **[D]**

Engineering principle 3 needs a gate for "first code". Proposed, in dependency order:

| Slice | Contents | Proves |
|---|---|---|
| Demo-0 | protocol test body (headless packet actuator), decoder, world model, hand-written rule-list core, reflexes, referee, fake world | the core survives a scripted first night on fresh seeds, with no SendInput and no LLM |
| Demo-1 | Demo-0 through the real client: SendInput actuator, movement style (human camera, speed and timing, session 13), take-over, command gesture, body-side control verbs, indicator | the first demo, without learning, that moves like a person (movement statistics and the blind test, §9) |
| Demo-2 | brain: Jev and LLM clients, curriculum, skill writer and admission, memory briefing | Praxion writes and admits its own skills |

**Order (2026-11-03) [D]:** Demo-0, then a thin Demo-2 slice on the protocol body (the LLM writes one
skill, admission checks it, the headless body plays it), then Demo-1, then the rest of Demo-2. The
brain talks to the body only through the body port (§5.4), so the slice does not depend on SendInput.
The slice's scope is designed with the user before code.

First product code is gated on the reviewed draft and the passed 2.7 experiments [D], and then
starts with Demo-0 only [D]. The gate is split (2026-11-01) [D]: Demo-0 depends on E1-E4 and E13,
which passed; Demo-1 code waits for E8, E10, E11, E12, E15 and E16.

## 2. System overview

```
┌──────────────────────────────── Windows PC (dedicated to the game) ─────────────────────────────────┐
│                                                                                                     │
│  Minecraft client ──TCP──► FORWARDER (own process) ──TCP──► Vanilla server (offline, 127.0.0.1)     │
│  (user's, unmodified)       bytes untouched, no decoder                                             │
│        ▲                         │ tee: copy of both directions (loopback, never blocks forwarding)  │
│        │ SendInput               ▼                                                                  │
│        │        ┌──────────────────────────── BODY (Rust) ───────────────────────────────┐          │
│        │        │ decoder ► raw store ► perception query API (line of sight, deny default) │          │
│        │        │ reflexes ► arbiter (intent stack) ► rule-list interpreter ► primitives   │          │
│        │        │ planner ► movement style ► ControlFrame ► actuator ──────────────────────┼──┐       │
│        │        │ receipts · ruleset · control-verb parser · event log (source of truth)   │  │       │
│        │        └───────┬──────────────────────────┬───────────────────────────────────────┘  │       │
│        └────────────────┼──────────────────────────┼──────────────────────────────────────────┘       │
│   take-over hooks       │ status feed              │ body port (length-prefixed JSON, TCP)            │
│   watchdog process      ▼                          ▼                                                  │
│                 Indicator + interface ◄──── BRAIN (Python)                                            │
│                 (never takes focus)         commands (Jev), curriculum, LLM clients, skill writer,    │
│                                             admission, memory projections and briefing, wiki RAG      │
└────────────────────────────────────────────────────┼───────────────────────────────────────────────┘
                                                     ▼
                                  hosted Jev and LLMs (the only network dependence)
```

Process boundaries [D]:

- **Forwarder:** its own minimal process. It forwards bytes untouched, has no decoder and no game
  logic, and tees a copy to the body. Hard requirement: no body or decoder crash may drop the user's
  game connection. If E1/E3 miss the latency budget, the decoder may move into the forwarder; the
  forwarder never moves into the body.
- **Body:** decoder, world model and everything with a deadline under 1 s.
- **Brain:** a separate program joined by the body port. It runs on the same PC by default and may
  run remotely with a key.
- **Watchdog:** its own process. It waits on the body's process handle and releases every held key
  and mouse button from a shared held-key table if the body dies [D, R1-M5].
- **Indicator and interface:** fed by the body for status, so they work with the brain down.
  Neither ever takes focus.

## 3. Time scales and who decides

| Time | Layer | Work | Runs in |
|---|---|---|---|
| under 50 ms | Reflexes, take-over | lava, falls, fire, creeper, low health; take-over | body, pure functions, no network [D] |
| 50 ms to 1 s | Arbiter, interpreter, planner | choose the owner of the body, run skills, move | body [D] |
| above 1 s | Jev | bounded choices that the code lists | brain, hosted [D] |
| seconds to minutes | LLM | plans, chat, goals, skill writing | brain, hosted [D] |
| minutes to hours | Projects | long goals in the project ledger | brain proposals, body event log [D] |

Rules [D]: nothing with a deadline under about 1 s depends on Jev or any hosted call; the LLM is
woken by events, not a clock; every call class has a deadline and a fallback; safety never depends
on the brain or the network.

Reaction floor [D]: the reflex layer may react within 50 ms, but only reflexes tagged
`lethal_fast` may act faster than the 150 ms human floor. Every reflex in the coverage matrix
carries `latency_class: human_floor | lethal_fast` [D].

**Call classes** [D, numbers by measurement]: each class (command mapping, wake decision, interim
pick, plan, skill writing, chat reply) gets a model role, a reasoning effort, a deadline and a
fallback. Model latencies come from session 8's measurements; reasoning models may take 10 to 100 s,
so the plan role must start early or use low effort.

**Brain outage** [D]: if the brain is silent for about 3 s (heartbeat gap, a 2.5 pick [D]), the body finishes the
current primitive, runs a hand-written shelter or retreat skill, then holds with reflexes on and
tells the user through the indicator. Control verbs keep working.

**Cost** [D]: the user is warned above a threshold. Each goal gets at most 3 repair attempts. A global
LLM call-rate breaker degrades to code-only after repeated failures. One high hard stop, set by the
user, pauses autonomous LLM work while reflexes and the core keep running.

## 4. Components

| Component | Lang | Responsibility | Source |
|---|---|---|---|
| Version manifest | data | pins client, server jar, protocol 777, Azalea commit, toolchain date, [D]; plus data-report hash and dependency licences [D]; the forwarder refuses a handshake with another protocol number [D] | Q4 |
| Forwarder | Rust | forwards bytes untouched; tees a copy to the body over loopback with a bounded queue that never blocks forwarding | 2.2, Q3 |
| Decoder | Rust | packet field layouts from the pinned Azalea commit; ids and registries from Mojang data reports; tracks Handshake, Login, Configuration (with Known Packs) and Play, including re-entry into Configuration; loss of sync marks perception invalid | 2.2, R1-B2 |
| Raw store | Rust | everything the decoder receives; readable only by perception (module boundary plus lint) | R1-M3 |
| Perception API | Rust | deny-by-default queries under the line-of-sight rule (§5.1); `unknown` is a typed value; denied attempts go to the ledger | 3, R1-M3 |
| Reflexes | Rust | `evaluate(world) -> action`, about 10 typed settings the user can edit, first-night set growing per rung | 7 |
| Arbiter | Rust | tiers survival, command, maintenance, projects; soft and hard thresholds; owns the intent stack; all quantities in expected ticks [D] | 7, HOL-8 |
| Rule-list interpreter | Rust | three-valued teleo-reactive execution (§6.1) | 5, R1-M2 |
| Primitives | Rust | hand-written behaviours, one contract record each (§6.2) | 5, R1-M7 |
| Planner | Rust | A* core and cost constants from Azalea; our own place, bridge and pillar moves; requests aims from movement style; contract in §5.5 | 6, R1-M10 |
| Movement style | Rust | human camera, speed and timing above the planner; the only producer of look output [D] | 6, 13 |
| Actuator | Rust | consumes a `ControlFrame` per tick; implementations: SendInput (real play), headless packet client (level 2, practice room), client mod (future fallback) | R1-B7 |
| Hands (SendInput) | Rust | scan codes, two mouse modes, closed-loop gain calibration, the wrong-click slot check, a per-run nonce in `dwExtraInfo` [D]; targets SDL3 in 26.3, not GLFW | 2.3, R1-B1 |
| Take-over | Rust | low-level hooks; physical vs injected input; hook-liveness probe [D]; command gesture; danger-held keys released last [D] | 2.4, 4, Q1 |
| Control-verb parser | Rust | stop, cancel, status, carry on, hand-over parsed locally before any brain call | Q5 |
| Receipts | Rust | per-action evidence contracts and causal attribution (§5.3) | 4, Q7 |
| Ruleset | Rust | body-owned read-only file with a hash check; refuses before any input; brain only proposes changes [D] | 1.4, R1-M11 |
| Event log | Rust | append-only, replayable, the single source of truth; every claim cites a receipt id | 14, R1-M4 |
| Commands | Python | Jev maps non-control commands to a closed schema; code validates | 9 |
| Curriculum | Python | free, Voyager-style goals with a computable completion condition, checked before a directive is sent | 10 |
| Obtain-X solver | Python | recipe and tech-tree solver from game data, compiled into a framework-generated rule list (not an LLM skill) [D, session 9/10 pick] | 9, 10 |
| Jev and LLM clients | Python | role-based models, stamped directives, cost tracking, call-class deadlines | 8 |
| Skill writer and admission | Python + harness | step programs compiled to rule lists; staged admission (§6.3); writer is a low-privilege process that writes only to a candidates directory [D, session 15 pick] | 5, 11 |
| Memory projections and briefing | Python | rebuilt from the event log: places, containers, ledger progress, skill statistics; briefing with a token cap | 12, R1-M4 |
| Wiki RAG | Python | local index of Minecraft Wiki text for the LLM's knowledge questions; behind a flag, off the Demo-1 path [D] | 12 |
| Indicator and interface | Python + web | local web page served by the brain on loopback: reports, command box, live view, memory viewer; the indicator is a separate small window fed by the body [D] | 2.4, 9, 14, Q2 |
| Watchdog | Rust | own process [D]; waits on the body's process handle and releases keys and buttons from the shared held-key table [D] | 2.4 |
| Test harness | Python + Rust | logged setup channel, referee, fixtures, practice room on a test server, server checklist, world backups | 14, 11 |

Language split [D, with a note]: Python brain, Rust body. The "~10 ms" split rule in session 2.6 is
a proposed rule of thumb, not a decision.

## 5. Data flows and contracts

### 5.1 Perception

The decoder fills the raw store with everything the server sends. Other modules see the world only
through the perception API [D]:

- **Line of sight** within about 64 blocks in every direction; camera direction is ignored; F3
  information is allowed, everything else is denied [D].
- **Query result** [D]: every query answers `Known(value, provenance, age) | Unknown(reason)`, with
  `reason` one of `out_of_sight`, `out_of_range`, `budget`, `denied`, `never_seen`.
- **Block visibility** [D]: bounded ray tests per candidate at query time, cached per (eye cell,
  target). A block change in the target's chunk, any eye move to another cell or a chunk unload
  invalidates the affected entries. Precomputing visibility for a 64-block sphere (about 1.1
  million cells, 4/3·π·64³) does not fit the budget.
- **Ray budget** [D]: a per-tick budget; reflexes get a reserved share no other caller can use.
  When the budget is spent a query returns `Unknown(budget)`, never a stale value.
- **Entity visibility** [D, decided before Demo-0]: one ray per tick per tracked entity, capped
  (the cap is measured), cached per (eye cell, entity id, entity cell) and invalidated when either
  moves; entities beyond the cap are `Unknown(budget)` and the overflow is counted.
- **Transparent materials** [O]: the table of see-through blocks is decided at first code.
- **Hearing** is a sense [D, session 3 pick]: sound packets within about 16 blocks give facts with provenance `hearing`.
- **Provenance** on every fact: `sight`, `hearing`, `memory` (with age), `inference`, `privileged`
  (ledgered) [D, session 3 pick].
- **`unknown`** is a value, not "absent" [D]. Out of sight, a query answers `unknown`.
- **Ledger** [D]: denied access attempts are logged, not denied facts, at a bounded rate (the first
  occurrence per (query, fact kind) per tick plus a counter).
- **Containers** are live only while their window is open; otherwise they are seen facts with an
  age. No identity is assumed across break plus place [D].

### 5.2 Action: frames, actuators and take-over

```
skills / reflexes / planner / movement style
                 │ ControlFrame { tick, generation, held: {forward, sneak}, look: Δ(+3°, −1°), clicks: [Attack] }
                 ▼
   SendInput actuator │ headless packet actuator │ (client mod, future)
```

- Everything above the actuator produces only `ControlFrame`s [D]. Cancellation classes stay on
  actions; the frame carries only held state [D].
- **Frame fields** [D]: `tick`, `generation`; `held: {control: down | up}` with each control's
  hold or toggle mode read from `options.txt`; `look: Delta(Δyaw, Δpitch) | Aim(yaw, pitch)`;
  `clicks: [Attack | Use]`; `menu: Option<{window, slot, button, mode}>` for slot actions. The
  SendInput actuator spreads a tick's look over the render frames it spans and reports the
  applied counts back for gain calibration.
- **Stale frames** [D]: the actuator drops any frame whose `generation` is not current. Take-over
  increments the generation before it releases keys, so no frame built in that tick is injected.
- **Danger-held keys** [D]: a second record beside the frame, `HoldGuard { tick, dangerous: {controls},
  until_condition, max_ticks }`, is written by the running primitive. The take-over path reads it to
  know which held controls must not be released. It carries no control intent.
- **One owner of look** [D]: movement style is the only producer of look output. The planner and
  primitives request an `Aim`; a path segment starts only once the aim tolerance is met.
- **Hands under SDL3** [D for the facts, O for the effects]: 26.3 uses SDL, not GLFW; the
  `rawMouseInput` option is gone and key bindings use scancodes. Whether injected relative moves
  turn the camera (`hDevice = 0`), and how pointer speed affects the gain, is measured in round 0 and E5.
- **Take-over** [D]: any physical key, button, wheel or clear mouse move takes control back within
  50 ms; tiny jitter is ignored; danger-held keys stay held until the user touches them or the
  moment passes. No tier can revoke them [D].
- **Command gesture** [D]: a physical press of the bound chat or command key releases all keys
  except danger-held ones and waits while chat is open. If the next chat the client sends is a
  prefixed command, Praxion takes it and keeps control; anything else is a take-over.
  Details [D]: chat closed with nothing sent resumes the task; the wait is bounded at 30 s, after
  which Praxion says so and holds; reflexes stay armed and the pause heuristic is suppressed while
  the chat screen is open; the intent stays `suspended` during the wait; a prefixed control verb is
  still parsed by the body (§5.6).
- **Focus** [D]: on focus loss Praxion releases its keys, stops and tells the user; it never takes
  focus. Clicking into the interface's command box counts as a take-over.
- **Pause heuristic** [D]: no client movement packets for N ms while active means a safe stop.
  Praxion never injects into the pause, options, social or death screens; inventory, chest,
  crafting-table and other container screens are normal play and use the absolute menu mode [D].
- **Ownership** [D]: every injected event carries a per-run nonce; an event without it is the user's.
  SDL 3.4.14 drops mouse events whose extra info looks like pen or touch, so the nonce avoids the
  `0xFF5157xx` pattern and keeps bit `0x80` clear (sessions/02, round 0).
  A periodic probe on an unbound key proves the hook is alive. If the hook is lost, Praxion releases
  all keys and stops.
- **Hand-over** [D]: by chat command, global hotkey or the interface. It waits until no physical
  key is held [D]. If no key is released for 5 s, the indicator names the held key and offers
  "hand over anyway", which releases only Praxion's own holds [D]. After a hand-over with no command, Praxion resumes the
  suspended task, or else chooses a goal and says so [D]. After a take-over, the interrupted task
  waits for "carry on", then the world is re-read [D].
- **Physical operation only** [D]: remote-control tools are unsupported while Praxion plays.

### 5.3 Verification: receipts

A receipt reports what the server said. The required evidence is part of each primitive's contract [D]:

| Action | Evidence that settles it |
|---|---|
| dig | block becomes air (level 2) |
| place | block state changes (level 2) |
| pickup, craft | inventory delta (level 3) |
| move, look | no server correction within N ticks: settles `confirmed` with the note `unrefuted` [D] |
| slot click | client packet sent, slot number matches, no correction |

- **Attribution** [D]: a receipt counts only if its evidence tick is at or after the intent's start
  tick on the body's single tick source (every observation is stamped on arrival), **and** the
  action's sequence number, or the injected-event nonce for non-block actions, matches the intent.
  Tick order alone never attributes; an unattributed receipt stays `unconfirmed`.
- **Settlement** [D]: a receipt settles when its evidence arrives or its window closes, whichever is
  first. Late evidence never reopens a receipt; it is logged as `late_evidence{receipt_id, kind}` and
  counts against the skill's reliability only if it refutes a settled `confirmed`.
- **Outcomes** [D]: no confirmation gives `unconfirmed`; Praxion re-reads the world, then retries.
  A task whose success cannot be computed within the perception policy is `unverifiable` and never
  counts as a success. A preempted command settles as `preempted`.
- **Measurement** [D]: matching injected input to the packets it produced is measured in E9.

### 5.4 Body port v0

The brain and body exchange three kinds of message [D]:

- **Observations**, body → brain: deltas and events.
- **Commands**, brain → body: each with an id [D] and `expires_at_tick` [D]; an expired command
  never starts and settles as `expired`.
- **Receipts**, body → brain: `pending`, then settled.
- **Liveness** [D]: `ping`/`pong` both ways at 1 Hz; any brain message counts as liveness. The 3 s
  silence rule of §3 is measured on this.

Rules [D]:

| Rule | Content |
|---|---|
| Handshake | `hello{port_version, caps}`; a mismatch is refused |
| Ordering | every body message has a `seq` |
| Resync | the body answers `resync_request` with one `snapshot{state, generation, seq = S}`, then deltas after S. While a connection resyncs, the body buffers its deltas up to a bound; on overflow it discards the buffer, sends `dropped(n)` and requires a new snapshot [D] |
| Back-pressure | bounded queues; per-key coalescing (e.g. latest position only); `dropped(n)` when anything is dropped |
| Generation | incremented only by the body: owner change, take-over, new user command, reflex preemption; directives of an old generation are rejected. A command is stamped with the generation the body returns in `command_accepted{id, generation}` (the body increments first, then accepts). If a reflex or take-over bumps the generation under a running directive, the body settles it `preempted`, keeps the intent entry and emits `replan_needed{intent_id}`. One counter only; session 7's "owner token" and "epoch" are this generation [D]. After a body restart the generation continues from the event log [D] |
| Directives | stamped with generation, tick, state hash [D] and model; adopted only if the generation is unchanged and the guards hold on the actual state |
| Transport | length-prefixed JSON over loopback TCP; remote access needs a pre-shared key [D]; MessagePack only if measured necessary |
| Fakes | the same port serves the real body, the fake world and replays |

**Intent stack** [D]: the body owns one stack, persisted in the event log. Each entry is
`{intent_id, tier, skill_ref, args, state}`, with `state` one of `running`, `suspended`,
`frozen_for_carry_on` or `done`. The brain pushes and pops entries through the port. A new command
suspends the old one, which resumes afterwards. Stop and cancel always work [D].

Stop causes and resume triggers [D]:

| Cause | Intent state | Resumes when |
|---|---|---|
| take-over | `frozen_for_carry_on` | the user says "carry on" (or an empty hand-over) [D] |
| command gesture | `suspended` | the chat closes or the command is taken |
| focus loss | `suspended` | focus returns and the world is re-read |
| pause heuristic | `suspended` | client movement packets resume |
| brain silence | `suspended` (shelter skill runs) | the brain is back and re-sends a directive |
| cost hard stop | `suspended` (LLM work only) | the user raises the limit |
| body restart | every restored entry becomes `frozen_for_carry_on` | "carry on" |

The body alone writes `done`; the brain may pop only `done` or `suspended` entries; stack depth
is capped (number by measurement).

### 5.5 Planner contract

- **Input** [D]: start, goal and the planning state (hotbar, best tool, protected blocks; known
  blocks through the perception API [D]), plus a time budget per segment.
- **Goal types** [D]: `Block(pos) | Region(min, max) | Entity(id) | Near(entity, r)`.
- **Frontier** [D]: a goal whose corridor holds unknown cells is planned to the frontier and returns
  `partial` with the cell that ended it; the caller looks, then replans. A `partial` result
  transfers no state.
- **Output** [D]: `success | partial | timeout | noPath`, with the path so far.
- **Moves** [D]: walk, jump, dig, place, bridge, pillar; descend, climb, swim and parkour as
  session 6 lists them [D]. World-changing moves cost more. Hard limits: protected blocks, no
  digging straight down or into unknown blocks. Camera-then-move is a two-phase primitive.
- **Single source** [D]: every move that digs or places invokes the matching primitive record
  (§6.2), so its effects, cancellation class and evidence exist once; each changed block gets its
  own receipt and ledger entry.
- **Stuck handling** [D]: retry counters, loop detection, a blacklist with reset conditions, typed
  `unreachable`, then an event for the LLM. Never `/tp` or any operator command.

### 5.6 Commands

- **Sources** [D]: in-game chat with a prefix (read as a packet) or the interface.
- **Sender check** [D]: the body drops chat from any sender other than the user's offline-mode UUID
  (taken from the login) before parsing. Free text never reaches a planner as an instruction [D].
- **Control verbs** [D]: stop, cancel, status, carry on and hand-over are parsed by the body.
- **Everything else** [D]: Jev maps to a closed schema [D],
  then code validates.
- **Risk** [D]: low-risk and ambiguous commands are guessed, started, and asked about without
  blocking; high-risk commands are asked first. A pending high-risk question shows in the
  indicator with a "needs you" cue (sound plus taskbar flash, no focus change) [D]. After 60 s
  without an answer Praxion cancels that command, says so in the interface and in chat, and resumes
  the previous task [D].
- **Reports** [D]: go to the interface first; in-game chat gets only short acknowledgements at safe
  moments; every claim cites a receipt.

### 5.7 The brain loop

- **Wake causes** [D]: program exhausted, monitor fired, skill failed, chat, new threat, take-over
  ended, ruleset refusal, plus a heartbeat of about 60 s with a rate limit per cause. The
  heartbeat's "did anything change?" is a code diff over briefing facts, not a model call [D].
- **On a wake** [D]: the brain reads a briefing (facts with age and source, token cap), picks or
  writes a rule list, and sends it as a stamped directive.
- **Prompt caching** [D]: a stable system prompt plus the primitive catalog form the cacheable
  prefix; the briefing follows it.

## 6. Skills and learning

A skill is a teleo-reactive rule list `condition -> action`, interpreted by the body each tick [D].
Actions are primitives or other skills. The LLM writes a step program and a goal; the compiler
derives the guards and produces the rule list [D]. LLM-written skills only combine existing
primitives and skills [D]. Size limits from sessions 5 and 11 apply: about 2 to 5 steps,
more than 30 rules rejected, composition depth capped at 2 [D, session picks]. The obtain-X solver's output is
framework-generated, comes only from verified tables, and has its own stated depth limit [D].

### 6.1 Interpreter semantics [D]

Guards use Kleene three-valued logic:

| a | b | a AND b | a OR b | NOT a |
|---|---|---|---|---|
| T | T | T | T | F |
| T | U | U | T | F |
| T | F | F | T | F |
| U | U | U | U | U |
| U | F | F | U | U |
| F | F | F | F | T |

Each tick, in this order:

1. A reflex or a take-over preempts.
2. If a discrete action (dig, place, click) is in flight, guards are evaluated but no other rule
   takes over until the action settles (receipt or timeout).
3. Otherwise the highest rule whose guard is `T` fires. A continuous action (walking a segment)
   is re-selected every tick, so a higher rule becoming `T` switches at once.
4. A rule whose guard is `U` is not eligible. The highest such rule offers an `observe(x)` subgoal
   for its unknown fact, at most one per tick; `observe` is a primitive with its own evidence and
   timeout. Exploration therefore follows from the perception policy.
5. If no rule is eligible, the result is `failed(no_rule)` and the brain is woken.

`until` [D]: a commitment guard that keeps the current rule running only while that rule's
own guard is still `T`, bounded by the action's `max_ticks`. The interpreter exports `until_active`
for the arbiter's same-tier switch rule (session 7).

Composition [D]: each running skill is a frame on a call stack; the deepest frame with a `T` guard
fires. A child's `failed(kind)` makes the calling rule ineligible for that tick, so the parent falls
to its next rule. A preemption suspends the whole stack; afterwards every frame re-evaluates from
the top. A preempted discrete action is abandoned, not resumed; its receipt settles `preempted`.

```
 [select] ──T rule──► [run discrete action] ──receipt/timeout──► [select]
    │  ▲                    │
    │  └── continuous action: re-select every tick
    │                       └─ reflex / take-over ──► [preempted] ──reflex ended / carry on──► [select]
    │ none eligible
    ▼
 [failed(no_rule)] ──► wake brain
```

### 6.2 Primitive contract [D]

One versioned record per primitive:

```
primitive dig
  effects:        generated from game data (produces, consumes, changes, reveals, duration, tool tier, p)
  preconditions:  hand-written (in reach, supported, free space, light, in sight)
  cancellation:   class (session 4)
  evidence:       block becomes air (§5.3)
  cost:           expected ticks
```

A fake-world conformance test compares declared effects with measured ones. Shortcomings of a
primitive surface as a failure kind `suspected_primitive`. These are reported to the user, never
patched by the LLM [D].

### 6.3 Admission [D]

```
candidate ─► static checks ─► practice room ─► probation ─► active
              (compile,         (headless body,   (test server,
               size, no world    k=3 smoke test)   Wilson ≥ 8 uses)
               literals)
```

- **Practice room:** a copy of the world on a test server, run on the headless protocol body after
  that body passes E16. Pass condition: 3/3 on the skill's own fixture, no regression on pooled
  fixtures, at least one success on a held-out fixture from another seed or biome, and a comparison
  against the no-skill baseline.
- **Statistics:** confidence comes from probation (95% Wilson lower bound, at least 8 uses). For
  example, 3/3 alone gives a lower bound of only 0.44.
- **Never first tried in the user's world.** A skill learned in Praxion's own play becomes active
  without asking. A skill taught through chat needs the user's approval.
- **Repair loop** [D for the cap]: a typed failure kind plus the trace go back to the writer, at
  most 3 revisions per goal. The held-out fixtures are kept out of the writer's reach [D].
- **Storage** [D]: versions in git with automatic rollback, in a per-installation data directory,
  not in the product repository. The skill writer is a low-privilege process that can write only to
  a candidates directory [D, session 15 pick].
- **Lessons** [D]: general across worlds, proposed by the LLM, accepted by code after 3
  proxy-verified successful traces. A lesson is proposed when one failure kind has occurred in two contexts and a later repair succeeded [D].

### 6.4 Curriculum [D]

A free, Voyager-style curriculum proposes goals. Each goal carries a computable completion
condition, and the ladder stays the yardstick. The user's projects come first. Long projects live
in the project ledger, which points to intent ids. Progress is recomputed from the world after a
restart.

## 7. Safety and trust boundaries

| Boundary | Rule | Enforced by |
|---|---|---|
| Zero cheats [D] | APIs expose only what a player can do and see; no operator commands, no `/tp`; test setup through a separate logged harness | raw store readable only by perception (module boundary plus lint) [D]; ledger of privileged reads |
| Game connection [D] | no Praxion crash drops it | forwarder process with no game logic |
| Take-over [D] | the user always wins within 50 ms | hooks, nonce [D], liveness probe [D], watchdog |
| Network independence [D] | reflexes, take-over, ruleset, control verbs and the indicator never use the network | body-side implementation |
| Ruleset [D] | written with an LLM, confirmed by the user, enforced before any input | body-owned read-only file with a hash check [D] |
| Chat [D] | only the user's player is obeyed; free text is never an instruction | UUID sender filter [D] |
| Keys [D] | in the OS credential store, with a `.env` file outside the repository as fallback; held only by the brain, never logged | brain; remote port needs its own pre-shared key [D] |
| World [D] | backup before every autonomous session; clean server shutdown; keep the last N | harness |
| Blocked input [D] | input that has no client packet echo within N ticks gives `input_blocked`, then stop | receipts |
| Environment [D] | at start, read-only checks of integrity levels, Sticky/Filter/Toggle Keys, pointer settings, DPI, focus, sleep; refuse with a clear message, never change settings | body |
| Privacy [D] | recordings of the user's play and all logs stay local | — |

## 8. Memory and knowledge

- **Source of truth** [D]: the body's append-only event log. Memory (places, containers), ledger
  progress, skill statistics and reports are projections rebuilt from it. The brain writes only
  proposals.
- **First memory** [D]: one JSON Lines projection per world plus an in-RAM index, places and
  containers, the event log with a read-only viewer, and a briefing with a token cap.
- **Later** [D]: SQLite, vectors, lessons and the chunk cache each wait for a measured trigger. The
  chunk cache is a stage-E snapshot (Unload Chunk with an allowlist) behind its trigger [D].
- **Remembered facts** [D]: they carry age and provenance and are re-checked when in line of sight.
  Praxion says when it is unsure.
- **Game data** [D]: comes from Mojang data reports; `minecraft-data` is the backup only.
- **Minecraft Wiki RAG** [D]: feeds the LLM only. The index is local and never distributed, because
  the wiki is CC BY-NC-SA. Every passage carries a licence field, and a build check fails if an
  NC passage reaches a distributable target [D]. The user has no commercial purpose, so the NC clause does not block use [D].
- **Separation** [D]: the game memory is separate from Mnema and the hub.

## 9. Testing and observability

**Three levels** [D], with most tests at the bottom. What each level can test [D]:

| Capability | L1 fake world | L2 protocol body | L3 real body |
|---|---|---|---|
| Rule-list semantics, guards, arbiter | yes | yes | yes |
| Decoder and perception | from captures | yes | yes |
| Movement physics and timing | no | approximate (gated by E16) | yes |
| Input timing, focus, menus, take-over | no | no | yes |
| Naturalness | no | no | yes |

- **Verdicts** [D]: a referee reads server facts; Praxion's own report never decides a verdict.
- **Claims** [D]: rest on many fresh seeds with confidence intervals and an analysis written before
  the runs. A hand-picked best run is labelled as such.
- **When tests run** [D]: fast tests on every change; levels 2 and 3 nightly or on request.
- **Observability** [D]: a live window plus the replayable event log; every claim cites a receipt id.
- **Capture corpus** [D, round 0]: stock-client 26.3 sessions recorded before decoder work; every
  replay and CI test reuses them. Not committed if the Mojang terms forbid it [D].
- **Ideas** [D], with cost and the slice they first serve: a differential decode oracle on the corpus
  (S, Demo-0); a replay harness comparing Jev, a cheap LLM and rules on logged decision points
  (M, Demo-2); a planner-only control arm in the learning test (S, Demo-2). Shadow mode and the
  death-to-fixture factory are later work in `backlog.md`.

The full inventory is in `sessions/14-testing.md`.

## 10. Repository layout **[D]**

One repository with the layers enforced by tooling [D]. Agent layers do not import body or
version-pack layers [D]. Only the directories Demo-0 needs are created first:

```
body/        Rust workspace: forwarder, decoder, perception, world, reflexes, arbiter,
             interpreter, primitives, planner, actuator (headless first), receipts, ruleset, event log
harness/     test server control, referee, fixtures, fake world, server checklist
versions/    version manifest, Azalea patches and generated data tables
docs/        decisions, sessions, research, reviews, this file
spikes/      experiment code (session 2.7)
setup.py     one-command setup; writes local/paths.toml (machine-local, ignored)
third_party/ pinned downloads and clones: Azalea, server jar, toolchain shim (ignored)
runs/        harness run directories, one fresh world per seed (ignored)
```

Other directories are added when their demo needs them: `brain/` and the interface for Demo-1 and
Demo-2. Learned skills live outside the repository, in a per-installation data directory [D].

## 11. Risks and open points

| # | Risk or open point | Resolved by | Fail path (proposed to the user) |
|---|---|---|---|
| 1 | Injected input under SDL3 may not turn the camera, or may miss endgame timing | round 0, E5 to E9 | client-mod spike |
| 2 | Azalea's 26.3 branch may not decode stock-client traffic, or may be too slow | round 0 corpus, E2, E3 | own minimal decoder for the consumed packets, MCProtocolLib as oracle |
| 3 | The tee hop may break the 5 ms budget | E1, E3 on Windows under co-load | decoder moved into the forwarder |
| 4 | The whole loop may miss the 50 ms reflex budget | E15 | profile and re-split |
| 5 | The protocol body may not stand in for the real body | E16 | practice room on the real client while the user does not play |
| 6 | The command gesture or take-over may misfire | E12 | nonce-only ownership |
| 7 | Jev's value is unproven; vendor is early access with no SLA | per-decision-point measurement, pinned version, a port with an LLM implementation | rules or a cheap LLM at that point |
| 8 | The free curriculum may produce unverifiable goals | completion-condition check, learning test | — |
| 9 | Primitive effect declarations must be exact | conformance test (§6.2) | hand-written guard fixes |
| 10 | Dedicated server: no pause, defenceless while stopped | accepted (Q6) | — |
| 11 | Cost runaway | caps, breaker, hard stop (§3) | — |
| 12 | Wiki RAG licence | closed 2026-10-31: no commercial purpose; index stays local [D] | — |
| 13 | Apache-2.0 for all code [D]; Mojang EULA and Usage Guidelines | desk check before any release | — |
| 14 | Whether the Azalea A* core lifts out of its framework | session 6 spike | own A* |
| 15 | Co-load on one PC (game, server, body, brain, embedder, test server, recorder) | E13 under co-load, process priorities | brain or practice room on a second machine |

## 12. Not in this draft

Concrete message schemas and the rule-list syntax (written with the first code, inside the
contracts above); numeric thresholds (found by measurement); Nether and End reflexes (written
before their rungs); voice commands; reinforcement learning; personalised movement style.

## Appendix A. Traceability: decisions → sections

| Decision (`decisions.md`, newest first) | Section |
|---|---|
| Menu scope of the pause rule; receipt attribution (`>=` plus causal match); 60 s timeout for high-risk questions | §5.2, §5.3, §5.6 |
| Body port v0; event log as source of truth; intent stack | §5.4, §8 |
| Three-valued guards; primitive contract; per-action receipts; planner contract | §6.1, §6.2, §5.3, §5.5 |
| Version pin 26.3; forwarder process; amended 2.7 plan; physical operation only | §1, §2, §11, §5.2 |
| ControlFrame; command gesture; command box is a take-over; safety without the brain | §5.2, §5.6, §3 |
| Dedicated-server trade-offs; `unrefuted` movement; cost brake; admission evidence | §5.2, §5.3, §3, §6.3 |
| World backup; raw data readable only by perception | §7, §5.1 |
| Python brain, Rust body; SendInput with scan codes; two mouse modes; focus loss | §4, §5.2 |
| Take-over jitter; task waits after take-over; indicator window | §5.2, §2 |
| Separate brain and body; remote brain with a key; brain silence (amended by Q5) | §2, §3 |
| Line of sight; F3 only; world state in the body; remembered facts | §5.1, §8 |
| Server's word; `unverifiable`; safe take-over exception | §5.3, §5.2 |
| Teleo-reactive skills; LLM only composes; compiler derives guards | §6 |
| Jev as middle tier by measurement; event-driven LLM; cost brake (amended by Q9); starvation gap | §3, §5.7, §1 |
| Reinforcement learning not in the first demo | §1 |
| Arbiter tiers; reflex settings; first-night reflex set | §4, §3 |
| Command sources; ambiguity policy; reports; suspend and resume; stop and cancel | §5.6, §5.4 |
| Paths may change the world; bounded stuck handling; movement layer | §5.5, §4 |
| Staged admission; auto-activation vs taught skills; recording the user's play | §6.3, §7 |
| Free curriculum; resume after empty hand-over; 30 s announcement; project ledger | §6.4, §5.2, §1 |
| Smallest staged memory; lessons; wiki RAG; separation from the hub | §8, §6.3 |
| General movement style; human limits; language and tone | §1, §3 |
| Three test levels; evidence standard; observability; test cadence | §9 |
| Brain on the same PC; one repository; keys in the credential store; Apache-2.0 | §2, §10, §7, §11 |
| 2.7 thresholds and stop rule; Mineflayer as reference; proxy forwards untouched; Mojang data reports | §11, §4, §2, §8 |
| Success measures; first demo; goals when alone; eyes on the network, hands on the keyboard | §1, §2 |
| Never change the user's settings; PC dedicated to the game | §1, §7 |
| Player-bound first demo; private local server counts as singleplayer | §1 |
| Survival, Normal, zero cheats; Minecraft only with layering; singleplayer; Java on Windows (version rule amended by Q4) | §1, §7, §10 |
| Jev with LLMs behind it; hosted models; LLMs may write code (narrowed by session 5); Mineflayer not a dependency | §3, §6 |

Gaps: none found after the draft-2 review. Every former [P] item, including minor picks from
session notes, was accepted by the user on 2026-10-29 (decisions.md, "Architecture draft 2 accepted"). Still **[O]**: the transparent-material table (decided at first code).
