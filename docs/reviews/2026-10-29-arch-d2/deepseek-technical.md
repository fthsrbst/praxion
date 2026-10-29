<!-- Reviewer: DeepSeek v4.1 Flash via OpenCode. Draft-2 review, lens: deepseek-technical. Raw output. -->

I'll start by reading the architecture draft and the review materials.
## Findings

Reviewed: `docs/architecture.md` (draft 2), `docs/decisions.md`, `docs/reviews/2026-10-29-arch-r1/{README,synthesis,critic}.md`, `docs/sessions/02`–`11`, `backlog.md`. Severity means: **blocker** = the contract cannot be implemented as written without silently changing behaviour; **major** = a real undefined case or contradiction a first implementation will hit; **minor** = small, local, cheap fix.

---

### 1. BLOCKER — §6.1 — The interpreter allows and forbids rule switching mid-primitive at the same time

**Claim.** Two mechanisms in the same paragraph and diagram contradict each other: an in-flight discrete action blocks rule switching, yet the diagram shows a higher rule's guard becoming `T` switching away from the running primitive. An implementer must choose, and the two choices give different behaviour in combat and mining.

**Evidence.**
- §6.1: "The highest rule whose guard is `T` fires." … "A discrete action in flight (dig, place, click) blocks rule switching until its receipt or its timeout."
- §6.1 diagram: `┌──────────── guard of a higher rule becomes T ────────────┐` with the arrow landing back on `[select]`, i.e. a switch *while* `[run primitive]` is active.
- `decisions.md` 2026-10-29: "an in-flight discrete action blocks until its receipt or a timeout" — one side of the contradiction is a decided item.

**Fix.** Replace the two sentences with one precedence order and delete the contradicting edge. Suggested wording, immediately after "Each tick, the interpreter does the following:": "Per tick, in this order: (1) a reflex or a take-over preempts; (2) if a discrete action is in flight, guards are evaluated but a higher rule only takes over when the action settles (receipt or timeout) — the diagram edge *guard of a higher rule becomes T* is therefore checked at settlement, not continuously; (3) otherwise the highest rule whose guard is `T` fires." Then draw the diagram edge into `[select]` from `[run primitive]` on `receipt/timeout` only.

**Confidence.** High (the text and the figure are quoted verbatim and disagree).

---

### 2. BLOCKER — §5.2 — "Never injects into a menu" forbids the menu path the same draft requires

**Claim.** `menu` is never defined. Read as any GUI screen, the sentence forbids chest, crafting-table and inventory clicks — which §5.3 verifies, §5.1 makes the only source of container facts, and the day-one command set needs. This is the most likely misreading of the whole action section.

**Evidence.**
- §5.2: "no client movement packets for N ms while active means a safe stop; Praxion never injects into a menu."
- `decisions.md` 2026-10-29 (2.3): "In menus: absolute cursor positioning, with wrong clicks caught by the slot number in the container-click packet."
- §5.3: "| slot click | client packet sent, slot number matches, no correction |" — and §5.1: "**Containers** are live only while their window is open".

**Fix.** Change the sentence to: "…means a safe stop; Praxion never injects into the pause, options, social or death screen. Container screens (inventory, chest, crafting table) are normal play and use the absolute menu mode of §4 (Hands)." This edits a `[D]` wording inherited from `decisions.md`, so it needs the user's sign-off.

**Confidence.** High that the term is undefined and self-contradictory; medium on which reading the user intended (I did not find a session note that disambiguates).

---

### 3. BLOCKER — §5.4 — The resync rule cannot execute under the back-pressure rule; the brain silently diverges

**Claim.** "Snapshot at seq S, then deltas after S" requires the body to hold every delta from S until the brain subscribes. The port also specifies bounded queues that drop. During a brain restart the queue is exactly the thing that overflows, so deltas after S are gone and nothing says what happens then. It is also unspecified what a `snapshot()` contains (the port carries "deltas and events", not state) and whether the generation survives a restart.

**Evidence.**
- §5.4: "| Resync | after a brain restart: `snapshot()` at seq S, then deltas after S |"
- §5.4: "| Back-pressure | bounded queues; per-key coalescing (e.g. latest position only); `dropped(n)` when anything is dropped |"
- §5.4: "| Observations | deltas and events |" — no state message exists to snapshot.

**Fix.** Rewrite the row: "| Resync | the body answers `resync_request` with one `snapshot(state, generation, seq=S)`; while any connection is being resynced the body buffers production for that connection up to a fixed bound; if the bound is hit the body discards the buffer, sends `dropped(n)`, and refuses to continue the stream — the brain must request a new snapshot. After a body restart the generation continues from the event log, so pre-restart directives stay rejected. |" Also add `snapshot` to the message list.

**Confidence.** High (the two rows are in the same table and cannot both hold).

---

### 4. BLOCKER — §5.3 — Receipts are correlated by tick order only, which both accepts and rejects the wrong evidence

**Claim.** Three defects in one mechanism. (a) The tick domain is undefined but evidence comes from two clocks (client ticks seen by the body, server packets). (b) "After" is strict, so a `block_changed_ack` that arrives in the same tick as the intent start is rejected — the common case for a one-tick dig. (c) No causal link (action sequence number / injected-event nonce) is required, so two intents in the same or adjacent ticks cannot be told apart, and a receipt for intent A can be credited to intent B.

**Evidence.**
- §5.3: "**Epoch rule** [D]: a receipt counts only if its tick is after the intent's start tick."
- §5.3: "| dig | block becomes air (level 2) |"
- `sessions/04`: "The proxy attributes each block action by its sequence number." `sessions/11`: "Block changes are attributed to Praxion's actions through sequence numbers." Neither appears in §5.3.

**Fix.** Replace with: "A receipt counts only if (1) its evidence tick is `>=` the intent's start tick on the body's single tick source (the body's own tick counter, with every observation stamped on receipt), **and** (2) the action's sequence number, or the injected-event nonce for non-block actions, matches the intent. Tick order alone never establishes attribution. A receipt that cannot be attributed stays `unconfirmed`." The "after"→">=" and the added attribution edit a `[D]` item; state it as a correction to the 2026-10-29 wording.

**Confidence.** High on (a) and (c); high on (b) that same-tick acks are normal for fast digs, medium on how often they occur in 26.3 (not measured here).

---

### 5. MAJOR — §5.4 — The generation counter invalidates the command that causes it, and churns on every reflex

**Claim.** A brain-mapped user command (Jev → closed schema) is a message over the port. The body increments `generation` on a "new user command", which invalidates the directive that carried it. Separately, a reflex preemption bumps `generation`, so every creeper dodge silently rejects the running brain-supplied path plan, and no rule says who re-plans. The same mechanism also carries three names.

**Evidence.**
- §5.4: "| Generation | incremented only by the body: owner change, take-over, new user command, reflex preemption; directives of an old generation are rejected |"
- §5.4: "| Directives | stamped with generation, tick, state hash [P] and model; adopted only if the generation is unchanged… |" and "| Commands | … each with an id and an owner token |"
- `sessions/07`: "one owner of the body at a time, with epoch-stamped owner tokens".

**Fix.** Name one counter (`generation`), delete "owner token" and the second sense of "epoch", and add: "A command is stamped with the generation the body returns in `command_accepted{id, generation}`; the body increments first, then accepts. If a reflex or take-over bumps the generation while an adopted directive is running, the body settles it as `preempted`, keeps the intent stack entry, and emits `replan_needed{intent_id}`; the plan owner (brain for LLM plans, planner for path segments) re-stamps."

**Confidence.** High for the bootstrap and the reflex churn (both follow from the row plus the existing schema); medium for the "who re-plans" gap being unintended.

---

### 6. MAJOR — §5.4 — The port has no time dimension: no heartbeat message, no command expiry

**Claim.** §3 declares the brain silent from a "heartbeat gap" and runs a safety state machine on it, but the port has three message kinds and no heartbeat. Nothing defines how staleness is detected or what a stalled brain's commands mean. A command queued behind back-pressure can still be adopted minutes later; "guards hold on the actual state" is the only filter, and many guards (e.g. `has(stone_pickaxe)`) still hold.

**Evidence.**
- §3: "if the brain is silent for about 3 s (heartbeat gap, a 2.5 pick [P])…"
- §5.4: "the brain and body exchange three kinds of message [D]: Observations… Commands… Receipts…" — no heartbeat.
- §5.4: "| Commands | brain → body: each with an id and an owner token. |" — no deadline.

**Fix.** Add to the message list: "`ping`/`pong`, brain → body and body → brain, 1 Hz; any brain message counts as liveness, so silence is measured on the last of `ping` or `command`." Add to the Commands bullet: "each command carries `expires_at_tick` (default: one segment budget); an expired command never starts and settles as `expired`, a typed failure kind."

**Confidence.** High (the gap is a missing row, not an interpretation).

---

### 7. MAJOR — §5.2 — The `ControlFrame` is narrower than the decided frame and cannot express three things the draft verifies

**Claim.** The example frame carries held keys, a look delta and clicks. It has no slot action, no absolute look target, no per-key hold/toggle mode, and no tick identity. §5.3 verifies slot clicks, §4 calibrates the look gain closed-loop against an aim target, and `toggleSneak`/`toggleSprint` make "held" ambiguous. There is also no rule for converting a per-tick look delta into the per-render-frame SendInput counts the client consumes.

**Evidence.**
- §5.2: "`ControlFrame { held: {forward, sneak}, look: Δ(+3°, −1°), clicks: [Attack] }`"
- `decisions.md` 2026-10-29: "a per-tick `ControlFrame` (held controls, look delta **or target**, clicks **and slot actions**)" — the draft drops two of the four decided fields.
- `sessions/07`: "with `toggleSneak` or `toggleSprint` set in `options.txt`, hold and release differ; the input layer models toggle versus hold per key."

**Fix.** State the frame as the decided four fields plus identity: "`ControlFrame { tick, held: {key: down|up, mode: hold|toggle}, look: Delta(Δyaw, Δpitch) | Aim(yaw, pitch), clicks: [Attack|Use], menu: Option<{window, slot, button, mode}> }`; the actuator divides a tick's look delta across the render frames it will occupy and reports the applied counts back for gain calibration."

**Confidence.** High that the fields are missing (direct comparison with the decision); medium on the exact per-frame split rule, which should be measured in E5/E6 rather than fixed here.

---

### 8. MAJOR — §5.2 — The danger-held set has no path across the held-only seam

**Claim.** Take-over must release everything except danger-held keys, and those are decided per action by cancellation class. The frame that reaches the input layer carries only held state, so the input layer cannot classify a held key. Either the exception is lost (all keys released — a bridge fall) or everything is kept (unsafe), and both are silent failures.

**Evidence.**
- §5.2: "Cancellation classes stay on actions; the frame carries only held state [D]." … "danger-held keys stay held until the user touches them or the moment passes, and no tier can revoke them."
- `sessions/04`: "In an `unsafe_until` moment the keys it needs stay held until the user touches that key or any movement key, or the condition holds, or `max_ticks` passes; all other keys are released at once."

**Fix.** Pick one and say it: either add `hold_class: free | dangerous` per held key to the frame (this contradicts "the frame carries only held state" and needs the user), or add a second seam record: "`HoldGuard { tick, dangerous: {keys}, until_condition, max_ticks }` written by the interpreter/primitive at the same tick cadence and read by the input layer on take-over. It carries no control intent, only which held keys may not be released."

**Confidence.** High (the two requirements are in the same subsection and the information has no carrier).

---

### 9. MAJOR — §5.2 — Frames carry no owner/generation, so a pre-take-over frame can still be injected

**Claim.** `generation` exists only for brain directives. The interpreter and primitives run in the body and keep producing frames in the tick they were in when the take-over flag was set. Nothing makes the actuator reject a frame produced before the take-over, so "the user always wins within 50 ms" rests on an unstated check inside one module.

**Evidence.**
- §5.4: "directives of an old generation are rejected" — directives only.
- §5.2: "any physical key, button, wheel or clear mouse move takes control back within 50 ms" and "Every injected event carries a per-run nonce".
- `sessions/02` 2.4: "the input thread checks it before every send" — a minor pick, not in the contract.

**Fix.** Add to the actuator bullet: "Every `ControlFrame` is stamped with the generation. The actuator drops any frame whose generation is not current, and the take-over path increments the generation before it releases keys, so no frame produced in the same tick can be injected."

**Confidence.** High (the race is real; the mitigation exists only in a session minor pick, not in the draft).

---

### 10. MAJOR — §5.2 — The command gesture has no defined ending and collides with the pause heuristic

**Claim.** Four cases are undefined: chat opened and closed with no message sent; how long Praxion waits; whether reflexes stay armed during the wait; and who owns the body during the wait. The last one is not academic: with all keys released and no movement packets, the pause heuristic fires "safe stop" while the gesture says Praxion keeps control.

**Evidence.**
- §5.2: "a physical press of the bound chat or command key releases all keys except danger-held ones and waits while chat is open. If the next chat the client sends is a prefixed command, Praxion takes it and keeps control; anything else is a take-over."
- §5.2: "no client movement packets for N ms while active means a safe stop" — the gesture's own wait produces exactly that silence.

**Fix.** Add: "Chat closed without a sent message resumes the interrupted task (no take-over, no state change). The wait is bounded (30 s), after which Praxion says so and holds. During the wait reflexes stay armed and the pause heuristic is suppressed for the chat-screen case. Ownership during the wait is the user's but not committed: the intent stack entry is `suspended`, not `frozen_for_carry_on`." Also state that a prefixed *control verb* is still parsed by the body (§5.6), so the gesture cannot be used to bypass it.

**Confidence.** High for the missing cases and the pause-heuristic collision (both follow from the quoted rules); medium on the chosen bound.

---

### 11. MAJOR — §6.1 — `until` is unbounded and has no precedence against the rule's own guard falling to `F`

**Claim.** `until` is defined as "the current rule keeps running". It does not say whether it survives the rule's guard turning `F` (a setback — the core teleo-reactive behaviour), it has no maximum duration, and no signal exports "an `until` guard is active", which session 7's arbiter needs to refuse same-tier switches. An action can therefore stay committed to a precondition that no longer holds, with no bound.

**Evidence.**
- §6.1: "`until` is a commitment guard: while it holds, the current rule keeps running."
- §6.1: "Setbacks fall to a lower rule" is `sessions/05`'s premise: "each tick the highest true rule fires. No program counter, so resume equals start. Setbacks fall to a lower rule, luck jumps higher."
- `sessions/07`: "A switch inside one tier happens only when the gain exceeds `k x loss_on_interrupt` and no `until` guard is active."
- `sessions/04` bounds the analogous commitment: `unsafe_until(condition, max_ticks)`.

**Fix.** "`until` keeps the current rule running only while its guard is still `T`; if the guard becomes `F` the rule stops at the next tick boundary and the action settles per §5.3. `until` is bounded by the action's `max_ticks`. The interpreter exports `until_active` for the arbiter." Leave the numeric `max_ticks` to measurement.

**Confidence.** High on the undefined precedence and the missing signal; medium on whether the author intended `until` to survive a `F` guard.

---

### 12. MAJOR — §6.1 — Composition has no semantics: child failure, preemption, `observe` selection

**Claim.** Actions may be other skills (depth 2), but §6.1 defines one flat rule list. It never says what a child's `failed(kind)` or `failed(no_rule)` does to the parent's rule selection, who owns the tick while a child runs, what a preemption inside a child suspends, which rule's `observe(x)` is chosen when several have `U` guards, what happens when the observation never becomes known, or whether `observe` is a primitive at all.

**Evidence.**
- §6.1: "Actions are primitives or other skills." (from §6) and "If no rule is eligible, the result is `failed(no_rule)` and the brain is woken." — no caller is named.
- §6.1: "A rule whose guard is `U` is not eligible. It offers an `observe(x)` subgoal for the unknown fact" — no selection rule, no failure mode, no primitive record.
- `sessions/05`: "a rule's action may call another registered skill (`do: skill(args)`)" with results "`ok`, `failed(kind)` or `preempted`"; `sessions/11`: "depth is capped at 2".

**Fix.** Add a short call-frame paragraph: "Each skill runs as a frame; the deepest frame with a `T` guard fires. A child returning `failed(kind)` makes the calling rule ineligible for that tick and the parent falls to the next lower rule; `preempted` suspends the whole stack and, after the preemption, each frame re-evaluates its rules from the top (session 7). `observe(x)` is taken from the highest rule with a `U` guard, at most one per tick, and `observe` is in the primitive catalog with its own evidence and timeout; if the fact stays `U` the rule is ineligible and `failed(no_rule)` follows."

**Confidence.** High that none of this is in the draft; medium on the best semantics (a user decision).

---

### 13. MAJOR — §5.5 — The planner's vocabulary cannot express the decided world

**Claim.** The move set omits parkour, descend, climb and swim. Goals are a single opaque `goal`, though the day-one command set includes "go to me", "come here", "follow me" (entity goals). Unknown space is a normal state of the world (perception stops at 64 blocks and a wall hides everything), but the contract's only answer to unknown blocks is "no digging into unknown blocks" plus `noPath`, with no frontier behaviour. `partial` has no defined consumer.

**Evidence.**
- §5.5: "**Moves** [D]: walk, jump, dig, place, bridge, pillar." vs `sessions/06`: "Each move (walk, jump, parkour, descend, climb, swim, dig, place) has a cost in expected ticks."
- §5.5: "**Input** [D]: start, goal and the planning state…" vs `sessions/06`: "Unknown cells are a frontier: plan to the frontier, look, plan again (segmented plans)."
- `sessions/09`: day-one commands "go to a named place or to me; follow me; come here".

**Fix.** "**Goal types:** `Block(pos) | Region(min,max) | Entity(id) | Near(entity, r)`. **Frontier:** a goal whose corridor contains unknown cells is planned to the frontier and returns `partial` with the frontier segment and the unknown cell that ended it; the caller looks and replans. A `partial` result transfers no state — the caller owns the path." Restore the four missing moves.

**Confidence.** High (session 6 and session 9 are explicit; the draft narrows both).

---

### 14. MAJOR — §5.5 — Planner world-changing moves are not tied to the primitive contracts

**Claim.** The planner digs and places along a path, but nothing says those moves use the §6.2 primitive records. Without that link, effects, cancellation classes and required evidence exist twice or not at all, and session 6's rule that every changed block is receipted and ledgered has no home.

**Evidence.**
- §5.5: "A path may change the world, anywhere" and "World-changing moves cost more."
- §6.2: "One versioned record per primitive" with `effects`, `cancellation`, `evidence`, `cost` — the catalog is primitive-level only; §5.5 never references it.
- `sessions/06`: "Every block a path changes is written into the receipt and the ledger."

**Fix.** Add to §5.5: "Every move that digs or places invokes the matching primitive record from §6.2, so its effects, cancellation class and evidence are single-sourced; each changed block produces its own receipt and a ledger entry, attributed by the path's sequence numbers."

**Confidence.** High (no cross-reference exists anywhere in the draft).

---

### 15. MAJOR — §5.5 and §4 — Two components claim the camera

**Claim.** §4 places movement style "above the planner" and gives it "human camera"; §5.5 says camera-then-move is a planner two-phase primitive. Both emit the single look field of the frame. There is no rule for which one owns look, nor what happens when a human-style smoothing is applied to a planner aim.

**Evidence.**
- §4: "| Movement style | Rust | human camera, speed and timing **above the planner** |"
- §5.5: "Camera-then-move is a two-phase primitive."
- §5.2 diagram: `planner ► movement style ► ControlFrame` — the same arrow.

**Fix.** "Movement style owns all look output; the planner requests `Aim(yaw, pitch)` as a subgoal and the style layer realises it within its human limits. A path segment is executed only after the aim tolerance is met." Or the reverse — but pick one and delete the other's claim.

**Confidence.** High (a direct structural contradiction between two adjacent sections).

---

### 16. MAJOR — §5.1 — The line-of-sight cache has no invalidation rule and no defined behaviour when the ray budget runs out

**Claim.** The mechanism caches visibility per `(eye chunk, target)`. Nothing invalidates on a block change, an eye move inside a chunk, a chunk unload, or an entity move. A cached `visible` after a wall is built is an x-ray, which is exactly what the policy forbids. Separately, queries exceed the per-tick ray budget routinely (every guard is a query), and nothing says what a query returns then — `unknown`, a block, or a stall. A reflex that stalls is a survival bug.

**Evidence.**
- §5.1: "bounded ray tests per candidate at query time, cached per (eye chunk, target), with an explicit ray budget per tick"
- §5.1: "Out of sight, a query answers `unknown`." and §7: "raw store readable only by perception" — the policy is deny-by-default, so a stale positive is a policy breach.
- §3: "under 50 ms | Reflexes… body, pure functions, no network".

**Fix.** Add: "Cache invalidation: a block change in the target's chunk, any eye move, a chunk unload, or an entity move invalidates the affected entries; the cache stores only block facts. Budget: when the tick's budget is exhausted a query returns `unknown(reason = budget)`, never a stale value; reflexes get a reserved share of the budget that other callers cannot consume."

**Confidence.** High on the missing invalidation (necessary for correctness given constant digging); high on the missing budget-exhaustion rule.

---

### 17. MAJOR — §5.1 — Entity visibility has no mechanism, and it is the one reflexes need

**Claim.** The stated cache key `(eye chunk, target)` is block-shaped. Entities move every tick, so a per-target cache never hits and the ray cost per mob is paid per query. The "entity-tracking bound" is `[O]`, yet §3 promises a 50 ms creeper response and §4 makes reflexes `evaluate(world)` on every tick. The most latency-critical perception path is the one left open.

**Evidence.**
- §5.1: "cached per (eye chunk, target)… the transparent-material table and the entity-tracking bound are decided at first code [O]."
- `sessions/03`: "A ray from the eye decides whether a **block or entity** is known"; the decision covers entities.
- §3: "under 50 ms | Reflexes, take-over | lava, falls, fire, creeper, low health".

**Fix.** Replace `[O]` with a v0 rule for entities, e.g.: "Entity visibility is evaluated per candidate entity with one ray per tick per tracked entity, capped at 16 tracked entities, cached per `(eye cell, entity id, entity cell)` and invalidated when either moves; entities beyond the cap are `unknown` and the fact is logged. The cap is a measured number." Decide it before Demo-0, not "at first code".

**Confidence.** High that the mechanism as written does not cover entities; medium on the cap value.

---

### 18. MAJOR — §5.3 — Evidence that arrives after settlement has no handling, and `unrefuted` is never mapped to a settled status

**Claim.** A receipt settles at its settle window. If the ack or the server correction arrives afterwards, nothing says whether the receipt reopens, is ignored, or generates a separate refutation — and `unconfirmed` triggers a retry, so a late ack yields a duplicate dig or a false failure in the skill statistics. For move and look, "no correction within N ticks" is a positive observation, but the table does not say it settles as `confirmed`; the ladder's four statuses are never mapped to it.

**Evidence.**
- §5.3: "| move, look | `unrefuted`: no server correction within N ticks |" and "no confirmation gives `unconfirmed`; Praxion re-reads the world, then retries."
- `sessions/04`: the ladder's statuses are "`confirmed`, `refuted`, `unrefuted` or `unconfirmed`", with settle windows "per action kind, set from measurements".

**Fix.** Add: "A receipt settles when its window closes or its evidence arrives, whichever is first. A move or look receipt whose window closes without a correction settles as `confirmed`, with the note `unrefuted`; it is a positive observation, not a missing one. Evidence arriving after settlement never reopens the receipt: it emits `late_evidence{receipt_id, kind}` to the ledger and counts against the skill's reliability only if it refutes a settled `confirmed`."

**Confidence.** High on the gap; high that `unrefuted` is unmapped.

---

### 19. MAJOR — §5.4 and §5.2 — Five ways to stop, one intent state

**Claim.** Take-over, focus loss, the pause heuristic, brain silence and the hard stop all stop or suspend play, but the intent stack defines exactly one non-running state (`frozen_for_carry_on`) and one resume trigger ("carry on"). Whether focus loss resumes automatically when focus returns — it "stops and tells the user", which reads like an automatic path — is undefined and observable. The stack also has no depth cap, no rule for who writes `done`, and no restart rule, although it is persisted in the event log; a body restart restores `running` entries with no instruction to freeze them.

**Evidence.**
- §5.2: "on focus loss Praxion releases its keys, stops and tells the user"; "After a take-over, the interrupted task waits for 'carry on', then the world is re-read."
- §5.4: "Each entry is `{intent_id, tier, skill_ref, args, state}`, with `state` one of `running`, `suspended`, `frozen_for_carry_on` or `done`." — no mapping from cause to state, and "persisted in the event log".
- `sessions/02` 2.4: "never from Praxion's own memory".

**Fix.** Add a three-column table "stop cause → intent state → resume trigger" and three sentences: "Focus loss: entry `suspended`, resume when focus returns and the world has been re-read (no 'carry on' needed)". "Pause heuristic, brain silence: entry `suspended`, resume per §3." "Body restart: every restored entry becomes `frozen_for_carry_on`." Plus "the body sets `done`; the brain may pop only entries in `done` or `suspended`; depth is capped (number by measurement)."

**Confidence.** High that the mapping is absent; medium on the intended focus-loss behaviour (the draft's wording is ambiguous by itself).

---

### 20. MINOR — §5.3 — "level 2 or 3" loosens the decided evidence ladder

**Claim.** The ladder assigns pickups and crafts to level 3 (an effect), because the inventory delta is the effect. "2 or 3" lets a craft be confirmed by a container-state change, which is exactly the false-success the ladder rejects.

**Evidence.** §5.3: "| pickup, craft | inventory delta (level 2 or 3) |" vs `sessions/04`: "World-changing actions succeed at level 2; actions with an effect (pickup, craft) need level 3."

**Fix.** "| pickup, craft | inventory delta (level 3) |".

**Confidence.** High.

---

### 21. MINOR — §6.1 — The `[preempted]` state is a dead end

**Claim.** The diagram enters `[preempted]` from `[run primitive]` and never leaves. The text says a reflex preemption settles the command as `preempted`, but not what the interpreter does next (re-evaluate which frame, whether an interrupted discrete action restarts or is abandoned, and what its receipt says).

**Evidence.** §6.1 diagram: `└─ preempted (reflex, take-over) ──► [preempted]` — no outgoing edge. §5.3: "A preempted command settles as `preempted`."

**Fix.** Add the edge `[preempted] ──reflex ended / carry on──► [select]` and a sentence: "A preempted discrete action is abandoned, not resumed; its receipt settles as `preempted` and the rule's guard is re-evaluated on the actual state."

**Confidence.** High.

---

### 22. MINOR — §5.6 — The unanswered high-risk question can stall the character forever

**Claim.** Asking first and waiting has no timeout. On the dedicated server the world keeps ticking and the character is defenceless while the indicator waits for an answer that may never come. The draft marks this `[O]`, but `[O]` here means a decided safety rule has no bound.

**Evidence.** §5.6: "high-risk commands are asked first… What happens when it is never answered is **[O]**." §5.2/§3: the world keeps ticking when stopped (dedicated server).

**Fix.** "If a high-risk question is unanswered for 60 s, Praxion cancels that command, says so in the interface and in chat, and resumes the previous task."

**Confidence.** High that the gap is real; medium on the bound.

---

### 23. MINOR — §5.1 — The denied-attempt ledger has no volume rule

**Claim.** Every guard evaluation is a query; guards that touch out-of-sight facts are denied every tick. "Denied access attempts are logged" with no sampling or rotation makes the ledger the largest file in the system and couples perception latency to disk write rate.

**Evidence.** §5.1: "denied access attempts are logged, not denied facts [P]." §6.1 requires guard evaluation every tick.

**Fix.** "Denied attempts are logged at a bounded rate (first occurrence per `(query, fact kind)` per tick, plus a counter), not per call."

**Confidence.** High on the mechanism, medium on the practical severity before measurement.

---

### 24. MINOR — §5.2 — A stuck physical key makes hand-over impossible with no escape

**Claim.** Hand-over waits until no physical key is held, with no abort timer (deliberately, R1-M5). A mechanically stuck key or a held gamepad button therefore blocks hand-over indefinitely, and the user's only signal is nothing happening.

**Evidence.** §5.2: "It waits until no physical key is held, with no abort timer [P]."

**Fix.** "If no key is released for 5 s, the indicator names the held key and offers 'hand over anyway', which releases only Praxion's own holds and starts with the user's keys as they are."

**Confidence.** High (a direct consequence of the stated rule).

---

**Checked with no finding:** the Kleene table's six rows are arithmetically correct and complete (AND/OR/NOT agree with standard Kleene, including `U AND F = F` and `U OR T = T`); §5.5's output statuses match `decisions.md`; §5.4's back-pressure and coalescing rules match the decision recorded on 2026-10-29; §5.2's take-over latency, nonce and liveness probe are consistent with §3 and §7.

**Not reviewed here (outside the lens):** §7–§9 safety/test wording, admission statistics, the traceability table's completeness, and the physical-input facts inherited from round 1 (SDL3, `hDevice`), which I did not re-verify.

---

### Verdict

Round 0 of the 2.7 experiments is desk work — the version-pin manifest, reading the 26.3 input path and recording a stock-client capture corpus — and none of the 24 findings touch it, so it can start now. Round 1 (E1–E4, decoder and perception plumbing) is also unblocked apart from finding 16, which only matters once perception queries are written. Rounds 2–4 are not: E5/E10/E12 measure the frame, menu mode, danger-hold and command gesture, and E12's pass criteria cannot even be written while findings 1, 2, 7, 8, 9 and 10 stand. First product code is blocked on four contracts — the interpreter (§6.1, findings 1, 11, 12, 21), the port (§5.4, findings 3, 5, 6, 19), receipts (§5.3, findings 4, 18, 20) and the planner (§5.5, findings 13, 14, 15) — plus the two perception mechanism gaps (16, 17) that Demo-0's reflexes depend on. The common shape is not redesign: each finding is a missing precedence rule, a missing message, a missing field, or a sentence that forbids what another sentence requires. Three of the fixes (2, 4, 8) edit `[D]` wording inherited from `decisions.md`, so they need the user's sign-off rather than a silent rewrite, and the corrected decisions should go back into `decisions.md` with the reason. My recommendation: fix §5.2 first (it gates the experiments that are closest to running), then §5.3/§5.4/§6.1/§5.5 as one pass with the user in the room, then start round 0 in parallel because it depends on none of them.

FINAL_REPORT 4 blockers / 15 majors / 5 minors
