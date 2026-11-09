# Movement style: research and v0 design

Status: research, 2026-11-07. Input to the movement-style layer (session 6, session 13, architecture
§5.2 / §5.5). No code was changed. Tags: [S] sourced, [G] guessed (needs measurement), [U] unverified.

## 1. Sources

| # | URL | Gives | Licence | Status |
|---|---|---|---|---|
| 1 | https://github.com/PrismarineJS/mineflayer-pathfinder/blob/master/lib/human.js | `createHuman`: min-jerk head gestures, personality distributions, pure pursuit, sprint/jump/glance rules, 0.15 deg grid. Header comment names the data: "48 players recorded walking from a lobby spawn to a row of NPCs, 1.21.4" | repo MIT | verified (read via WebFetch summary; numbers below are from that read, re-read the file before coding) |
| 2 | https://raw.githubusercontent.com/PrismarineJS/mineflayer-pathfinder/master/readme.md ("Human-like walking") | API, trait list (basePitch, turnTime, sprints, sprintDelay, jumpRate, lookAhead, stopRadius, reaction, glanceRate, strafe, deadband), setback guard | MIT | verified |
| 3 | https://github.com/openai/Video-Pre-Training (README, `data_loader.py`, `run_inverse_dynamics_model.py`, `lib/actions.py`) | contractor data format, index URLs, `CAMERA_SCALER = 360/2400 = 0.15` | repo MIT (LICENSE read). Data licence not stated on the README page | format verified; data licence [U] (a HF mirror card says MIT, secondary) |
| 4 | https://openaipublic.blob.core.windows.net/minecraft-rl/snapshots/all_6xx_Jun_29.json (and 7xx, 8xx, 9xx, 10xx, BASALT 2022 indexes) | `{basedir, relpaths[]}`; file = `<basedir><relpath>.jsonl` / `.mp4` | see #3 | verified (index 4 MB, fetched) |
| 5 | https://openaipublic.blob.core.windows.net/minecraft-rl/data/6.10/Player129-f153ac423f61-20210617-171344.jsonl | One sample: 12.7 MB, 6000 lines = 6000 ticks = 5 min at 20 Hz. mp4 of the same piece is 115 MB (do not download it) | see #3 | verified by HEAD + streamed `head`/`wc` (nothing saved) |
| 6 | https://arxiv.org/pdf/2206.11795 (VPT paper) | about 2000 h contractor data used for the IDM, about 4500 h total incl. statistics-only data | n/a | partly verified (search snippet) |
| 7 | https://arxiv.org/pdf/2107.01969 and https://arxiv.org/pdf/1907.13440 (MineRL BASALT, MineRL-v0) | per-tick (20 Hz) actions with camera pitch/yaw change in degrees; MineRL-v0: 60 M state-action pairs, 130 GB low-res. BASALT data: github.com/minerllabs/basalt-benchmark | [U] | format verified from snippets; licence, per-file size [U] |
| 8 | https://github.com/GrimAnticheat/Grim (branch 2.0): `checks/impl/aim/processor/AimProcessor.java`, `aim/AimDuplicateLook.java`, `aim/AimModulo360.java`, `breaking/RotationBreak.java`, `scaffolding/DuplicateRotPlace.java`, `baritone/Baritone.java` | what a server-side anti-cheat computes from rotation packets | GPL-3.0: read, do not copy code | verified (full source read) |
| 9 | https://windmouse.readthedocs.io/en/latest/algorithm.html | WindMouse update rules (gravity 9, wind 3, max step 15, damping) | none stated; ideas only | verified |
| 10 | Flash and Hogan 1985 (minimum jerk), via https://nbviewer.jupyter.org/github/demotu/BMC/blob/master/notebooks/MinimumJerkHypothesis.ipynb and the Edelman/Flash handwriting paper | peak/mean velocity ratio of min-jerk = 1.875; measured reaching about 1.75 | n/a | verified (search snippets) |
| 11 | https://arxiv.org/html/2604.20673 ; https://pmc.ncbi.nlm.nih.gov/articles/PMC9744923 ; https://ar5iv.labs.arxiv.org/html/2203.12050 | FPS aiming = overlapping sub-movements; kinematics depend on task; Fitts' law fits FPS pointing but slower than 2-D; mouse-sensitivity effects | n/a | abstracts only; no overshoot/peak-velocity numbers found |
| 12 | https://bionumbers.hms.harvard.edu/bionumber.aspx?id=110800 ; https://ijpp.com/IJPP archives/2019_63_2/138-144.pdf | simple visual reaction time 180-250 ms (221-225 ms measured in one study) | n/a | verified (snippets) |

Not found: sigma-lognormal numbers for gaming; any published head-gaze main sequence for Minecraft; Vulcan's source
(closed). [U] for all of those.

### 1.1 The session-13 claim about `human.js`
Session 13 says: "`human.js` from mineflayer-pathfinder (MIT; calibrated on 48 recorded players: head-turn
distributions, sprint delay, jump rate, pure-pursuit steering)".

- The file exists: `lib/human.js` (plus `examples/human.js`), not at repo root. MIT. [S]
- "48 players" is real, but only as a header comment: it lists measured **targets** from the recordings, not a
  fitted model: head turn per 100 ms while moving p25/med/p75/p90/p99 = 8/14/24/44/107 deg; while still med 20 deg,
  p90 354 deg (snap turns); first step to sprint 25 % immediate, med 0.2 s; sprint jumps/s p10/med/p90 = 0.4/1.0/1.6;
  pitch med 10 deg down; motion heading vs head yaw med 4 deg, 21 % over 25 deg. [S]
- The personality distributions (turnTime U(0.75,1.3), reaction N(0.25,0.1) etc.) are hand-set to resemble those targets.
  No fitting procedure, no published dataset, no evaluation is documented. [U whether the 48 recordings are published]
- Context is narrow: walking from lobby spawn to NPCs. No mining, building, combat. Do not treat its numbers as
  statistics of general Minecraft play. Treat them as a starting point to be checked against VPT (§6).
- Correct session 13 to: "defaults taken from `human.js`, whose header lists targets from 48 recorded players".

## 2. Diagnosis: why it looks like a bot

The orchestrator's reading is mostly right. Two corrections: the camera is not "constant speed", it is
"max speed or exact snap"; and `aim_precise` ignores the style speed entirely.

| # | Tell | Where | Detail |
|---|---|---|---|
| T1 | Turns of up to 30 deg yaw / 20 deg pitch complete in ONE tick, landing exactly on the target | `look.rs:77-83` | `step = clamp(err, -30, 30)`; `Aim{current + step}` equals the target whenever err <= 30. A 30 deg flick in 50 ms is faster than a human wrist yet lands exactly. Bigger turns are bang-bang 30 deg/tick (600 deg/s) from the first tick, zero acceleration. |
| T2 | No arrival profile; aim stops 0-4 deg off and then holds | `look.rs:73-76` | inside tolerance returns `Look::None` forever, so the heading sits at an arbitrary offset until the next node, then snaps. |
| T3 | Interaction aim is an instant exact snap of ANY angle, ignoring the style speed (comment says "bounded by the style's speed", code does not) | `runner.rs:382-394` (callers `:717`, `:929`, `:2143`) | `Look::Aim{yaw, pitch}` straight to the target. 180 deg in one tick, error 0.0. Also `yaw` comes from `atan2` in [-180, 180], not unwrapped near the current yaw, so a crossing sends a ~360 deg delta in one packet: exactly Grim's `AimModulo360` pattern (`|deltaYaw| > 320` after `|last| < 30`). |
| T4 | Turn-then-walk: forward is held only when aligned within 4 deg; every corner is stop, turn, go | `follow.rs:103-106`, `:129-131` | friction is 0.546/tick so the body decelerates visibly at every node. |
| T5 | Walk target = block centre of each step, reached at 0.35 blocks | `follow.rs:98`, `:215-219` | per-node yaw corrections; any grid staircase becomes zigzag of exact 45/90 deg yaw jumps. |
| T6 | Pitch pinned to exactly 0 while walking; exact pinpoint pitch when interacting | `follow.rs:99-101` | humans look ~10 deg down (human.js header: med 10 deg); bot's pitch histogram is a spike at 0.000. |
| T7 | Never sprints in the follower; sprint exists only in reflexes | no `sprint` in `planner/src/follow.rs`; `reflexes/src/motion.rs:184` | constant 4.3 b/s walking. Jumps only for planned step-ups (`follow.rs:134-138`) or the stall hack (`:164-177`). |
| T8 | Zero reaction delay | `runner.rs:372-375`, `reflexes/src/motion.rs:207-208` | new target acts on the same tick. |
| T9 | No idle gaze: `Look::None` when idle or done | `runner.rs:368-370`, `follow.rs:87-89` | rotation packets stop completely for seconds: pure stillness. |
| T10 | No sensitivity grid: deltas are arbitrary f32 | all of the above | Grim `AimProcessor` takes the GCD of consecutive rotation deltas (needs > 15 samples, deltas < 5 deg) and converts to a sensitivity with `cbrt(gcd/0.15/8)`. A client whose deltas have no common divisor is the signature of a rotation hack. [S] |
| T11 | Repeated identical deltas / micro pitch-only moves | n/a today, risk for the new design | Grim `DuplicateRotPlace` flags the same yaw delta (diff < 0.0001) at consecutive placements when delta > 2 deg; `Baritone` flags pitch-only deltas 0 < d < 1 deg with no yaw change whose divisor is off-grid, more than 8 times. Both disappear if deltas sit on the grid and are not constant. [S] |

Also relevant: `RotationBreak` (flags a break when the ray does not hit the block) shows that interaction aim
must keep the crosshair on the target cell, which is why the 0.25 deg tolerance exists. A human
approach plus correction is fine as long as the click waits until the ray hits.

## 3. Models considered

| Model | Reproduces | Cost | Verdict |
|---|---|---|---|
| Spring-damper (2nd order) | bell-ish velocity, overshoot via zeta; retarget is trivial | movement time independent of amplitude (linear system) unless gain-scheduled; overshoot tuned by zeta, not by data | rejected alternative |
| WindMouse (#9) | jagged curved 2-D path | noise-driven, no amplitude/time law, built for pixel cursors | no |
| Sigma-lognormal | best fit of real strokes, overshoot, sub-movement overlap | needs fitting to data we do not have yet | later (§5) |
| Fitts' law alone | movement time vs amplitude/width | gives time only, no trajectory | used only as the shape of duration |
| **Minimum jerk (quintic) with fixed duration `T(A)`, bias at the landing point, corrective gesture** | bell-shaped speed, peak/mean ratio 1.875 [S #10], `T` grows with sqrt(amplitude), landing error and corrective sub-movement | ~80 lines, one closed form | **recommended** (this is also what human.js does [S #1]) |

## 4. Recommended v0 design

### 4.1 Shape

```
 walk/path ──► Route (smoothed polyline) ─┐
 primitive (dig/place/attack target) ─────┼─► GazeGoal ──► Gaze (ONE per body, stateful) ──► Look::Delta (quantised) ──► ControlFrame
 idle / waiting ──────────────────────────┘        ▲  personality(seed), reaction, gestures
                                                   └── body look (measured each tick)
 Follower: reads Gaze.heading_error() to gate forward / sprint
```

Rules:
- `Gaze` (new, replaces `look_towards`) lives in `planner/src/look.rs` and is the ONLY producer of `Look`
  (architecture §5.2). One instance per body, owned by the driver and passed in `ctx` (replaces `ctx.look: LookStyle`
  and the `LookStyle::default()` in `reflexes/src/motion.rs:208`).
- Output is always `Look::Delta` with quantised values; never `Look::Aim` (except after a teleport/respawn where
  state is reset).
- Personality is drawn once from the run seed and written to the event log, so runs are reproducible.

### 4.2 Gesture: minimum-jerk sweep, retargetable

State per gesture: `t0`, `T`, start `x0`, `v0`, goal `x1` (yaw and pitch as a 2-vector, yaw unwrapped near current).

```
A      = hypot(wrap(dyaw), dpitch)                       // deg, angular distance in yaw/pitch space
T      = max(0.10, (0.08 + 0.30*sqrt(A/90)) * turn_time * (walking ? 1.0 : 0.6))   // s   [S #1]
x1'    = x1 + (r-0.5)*0.12*(x1-x0)       per axis, r~U(0,1): lands up to 6 % off      [S #1]
e(tau) = (1-tau)^3 * (q0 + q1*tau + q2*tau^2),  tau=(t-t0)/T,  x(t) = x1' + e(tau)
         q0 = e0 = x0-x1',  q1 = v0*T + 3*e0,  q2 = a0*T^2/2 + 6*e0 + 3*v0*T
```
`q` is the quintic with p(0)=e0, p'(0)=v0 T, p''(0)=a0 T^2 and p, p', p'' = 0 at tau=1 (derived here,
check in a unit test: zero velocity and acceleration at the end, peak speed = 1.875 A/T from rest).
Set `a0 = 0` on retarget and take `v0` from the last two ideal samples. [G, simplification; invisible at 20 Hz]
Yaw and pitch share `T` so the path in angle space is a straight line. [G]

Each tick: `ideal = x(t_tick)`; `delta = ideal - emitted_so_far`; see §4.5 for quantisation.

Properties to test in unit tests: from rest `peak_speed = 1.875*A/T`; `x(T)=x1'`; speed profile symmetric;
retarget mid-gesture keeps velocity continuous.

Numbers this gives (turn_time 1): moving 10 deg 0.18 s (peak 104 deg/s, 5 deg/tick); moving 90 deg 0.38 s (444 deg/s,
22 deg/tick); standing 180 deg 0.30 s (1125 deg/s, 56 deg/tick, a flick). Check these against VPT (§6) before trusting.

### 4.3 Landing error, correction, "settled"

- Landing bias per gesture as above (up to +-6 % of the error [S #1]). Typical: 90 deg turn lands up to 5.4 deg off.
- After a gesture ends, if `|err| > tol_settle`, wait `corr_pause = 2 ticks` (100 ms) [G: sub-movement latency of
  visual feedback is 100-200 ms] then start a corrective gesture (same law, so its own bias is 6 % of the residual,
  min duration 2 ticks). Converges geometrically: 5.4 deg, 0.3 deg, done.
- `tol_settle`: 0.25 deg for interaction goals (= `INTERACT_TOLERANCE_DEG`, the click gate), `deadband` for walking goals.
- `Gaze::step` returns `settled = gesture finished && |err| <= tol && 1 tick elapsed`. The click waits for `settled`.
  That 1-tick wait is the dwell between crosshair landing and click [G].

### 4.4 Walking while turning

Replace turn-then-walk with pure pursuit over a smoothed route.

1. `Route::from_steps(steps, world)` (new, planner crate): string-pull consecutive flat `Walk` steps at the same y:
   drop waypoint i if the straight segment from the kept waypoint to i+1 is walkable (sample every 0.25 block, lateral
   +-0.3, solid floor, feet and head clear) [S #1 `straightWalkable`]. Every non-`Walk` step (StepUp, Descend,
   Bridge, Pillar, Swim...) keeps its exact block-centre waypoint and the current tight reached rule (0.35 horizontal,
   `follow.rs:215-219`), because jumps and drops need alignment.
2. Carrot: the point `look_ahead` blocks along the route from the body's projection onto it; `look_ahead ~ U(1.8, 3.0)`
   [S #1]. The yaw goal is the bearing to the carrot; pitch goal is §4.6.
3. Steering is gestural, not continuous: start a new yaw gesture only when `|bearing - yaw| > deadband`,
   `deadband ~ U(4, 12) deg` [S #1]. This produces the "discrete mouse-like gestures" instead of a servo.
4. Forward gate: hold forward when heading error < 50 deg standing / < 75 deg already moving [S #1] on open flat Walk
   runs; < 20 deg when the next step is not a `Walk` or a Descend/Bridge is within 3 steps [G, safety]. Minecraft walks
   along the look yaw, so body drift during a turn is the pursuit the carrot corrects.
5. Waypoint passing on flat Walk: within 0.6 block, or within 1.2 once past it along the next segment [S #1].
6. Sprint: allowed only on flat Walk runs when `remaining > 2.5` blocks, heading error < 40 deg, `sprint_delay` elapsed
   (25 % zero, else U(0.1, 0.8) s); release for 600 ms after a replan [S #1].
   Never while the next steps are Bridge/Descend/Swim/StepUp. Final node keeps the existing 0.35 rule.
   **Updated 2026-11-09** (decision "Five next areas", D6): every personality sprints; the 80 % draw is
   gone, because a run that drew the other 20 % looked like a body that cannot run. The other gates stay,
   and a new hunger gate refuses to *start* a sprint at food 6 or less (a sprint already running goes
   on, as in the game). `--style-seed` still reproduces a body's other traits; it no longer changes
   whether it sprints. Code: `planner/src/look.rs` (`Personality::draw`), `planner/src/follow.rs`
   (`SPRINT_FOOD_MIN`).
7. Unchanged: step-up jumps, stall hop (`follow.rs:164-177`), `air_forward`, `max_ticks_per_step`.

### 4.5 Sensitivity quantisation

Real clients rotate by `counts * 0.15 * 8 * s^3` degrees per tick (s = 0.6*sens + 0.2), the inverse of Grim's
`convertToSensitivity` [S #8]. Default sensitivity 0.5 gives a step of exactly 0.15 deg, the same grid human.js uses
[S #1].
```
STEP = 0.15
n = round((ideal - emitted)/STEP)            // integer counts this tick, per axis
emitted += n*STEP ; emit Look::Delta{dyaw: n*STEP(yaw), dpitch: n*STEP(pitch)}
```
Because the error is recomputed against the ideal trajectory, it never accumulates (a real mouse drops the sub-count
remainder; this is equivalent). Pitch clamp +-90 is applied by `ControlFrame::apply`. Never send yaw wrapped to
[-180,180] (T3); always accumulate. `STEP` is a config constant; make it the user's real sensitivity if the hands
(Demo-1) path later turns real mouse counts, where the actuator spreads counts over render frames (§5.2).
Do not emit a pitch-only micro-move of 1-2 counts repeatedly (Grim `Baritone`): the OU pitch noise below is applied
through gestures with a pitch deadband of 1.5 deg [G], not per tick.

### 4.6 Pitch and idle gaze

- Walking pitch goal: `base_pitch = clamp(6 + 5*gauss, -5, 16)` deg down (positive = down in Minecraft) [S #1],
  plus Ornstein-Uhlenbeck noise `x += -x/2.5*dt + 2deg*sqrt(dt)*(2r-1)*1.7` only while walking [S #1], plus a slope
  term `0.5*atan(dy/ahead)` clamped +-10 deg toward steps up/down [G]. The pitch gesture fires when error > 1.5 deg [G].
- Interaction: pitch/yaw goal is the target point; the walking pitch is dropped.
- Idle gaze (no goal, or goal kind `Wait`): every `U(1.5, 5) s` [G] start a gesture to yaw offset N(0, 12 deg), pitch
  `base_pitch + N(0, 5 deg)` [G]; 30 % of the time return to the last work point [G]. Never while a primitive holds
  aim (a dig in progress keeps the target; micro jitter is below the 0.15 grid and is dropped). Glance amplitude while
  walking (human.js: +-(15 + 25 r) deg for 400-900 ms) is NOT in v0 (§5).
- Idle-gaze parameters are guesses; the VPT idle ticks (no keys held) are the data to fit them (§6).

### 4.7 Reaction delay

- Applies to externally triggered goals (new intent, new entity target, new command), identified by a `key` on the
  `GazeGoal`; a changed `key` starts a delay, a moved carrot or a tracked mob's position does not.
- `reaction = max(0.15, 0.25 + 0.10*gauss) s`, per personality, per event times `(0.5 + r)` [human.js shape, floor
  from session 13: 150 ms]. Quantised to whole ticks, min 3 ticks. [S shape, G floor interplay]
- Bypass: lethal-danger reflexes (lava, fall, session 7) pass `urgent: true` and skip the delay. Their gesture still
  uses the same law with `walking=false` (0.6 x duration).
- Reaction is not applied to continuing path steps or to the corrective gestures.

### 4.8 Interaction aim (dig / place at 0.25 deg) coexisting with human aim

`aim_precise` becomes a thin call: `gaze.goal(Interact{point, tol: 0.25, key})`. The gesture carries the camera to
the point with the human profile; the corrective gesture closes to <= 0.25 deg (0.25 > STEP/2, so reachable); the
primitive clicks only when `settled`. Contract unchanged for primitives (`(Look, aligned)`), only the look is human.
Cost: a 90 deg standing turn takes about 5 ticks plus 2 pause plus 2-3 correction ticks, against 1 tick today.
Primitive tick budgets (`EXPLORE_STEP_TICKS`, rule budgets, `runner.rs` timeouts) must be re-checked. [U which
budgets are tight]
Backlog item, not v0: scatter the aim point inside the target face (needs the face rectangle from the primitive) so
the crosshair is not always the exact face centre; a click-timing jitter layer (attack CPS) is also separate.

### 4.9 Size estimate

| Piece | Lines (Rust) |
|---|---|
| `Personality` (seeded Mulberry32/any RNG, 11 traits) | ~45 |
| `Gesture` (quintic, retarget, tests excluded) | ~60 |
| `Gaze` (goals, reaction, correction, quantisation, idle, OU) | ~140 |
| `Route` string pulling + carrot | ~80 |
| `Follower` changes (gate, sprint, passing) | ~60 net |
| Total | ~385 (tests separate) |

Risks to note before coding: existing tests assume instant aim (`look.rs` tests, `follow.rs:249-281`); perception must not
depend on pitch (unknown: check `perception` crate uses of `look`); RNG makes tests need a fixed seed; the new gate
means the body moves while still turning, so near edges the 20 deg rule is the safety, verify with the existing
cliff/water integration tests (`planner/tests/*.rs`).

## 5. Out of v0

| Item | Why |
|---|---|
| Strafing while turning (human.js `strafe`) | adds A/D key logic and collision risk; low visual gain vs pursuit. |
| Sprint-jump hops (`jumpRate`) | changes timing and landing cells, interacts with planned jumps; add after v0 passes the walking stats. |
| Glances while walking | a yaw offset of 15-40 deg changes the walking direction in Minecraft; needs a decoupled head, which a keyboard/mouse body does not have. |
| Fatigue, fake AFK, scripted mistakes | session 13: rejected. |
| Personalising to the user's own play | session 13: later option, after baseline. |
| Sigma-lognormal / data-fitted sub-movement model | needs the §6 dataset analysis first; v0 min-jerk plus correction is the testable baseline. |
| Aim-point scatter inside faces, click-timing jitter, hotbar/inventory pacing | need primitive-side data; separate layers. |
| Mouse acceleration, raw-input gain modelling | belongs to the Demo-1 SendInput actuator (E5 gain calibration). |

## 6. Measurement plan

### 6.1 Reference data
- Primary (session 13): held-out recordings of the user's play. The tee (`PRXCAP2`, `body/crates/tee`) captures
  serverbound packets, so per-tick yaw, pitch, x, y, z, on-ground and key-derived motion can be parsed with the
  same decoder for both the human and the bot (no domain gap). [U whether session-11 recordings exist with enough
  hours; n = 1 human]
- Population reference: VPT contractor data (#3-#5): per tick `yaw`, `pitch` (absolute), `xpos/ypos/zpos`,
  `keyboard.keys`, `mouse.dx/dy`, `isGuiOpen`, `tick`; 20 Hz; 5 min per file, 12.7 MB per jsonl. Drop ticks with
  `isGuiOpen`. `dx*0.15` = camera degrees only if the contractors' sensitivity matched (`CAMERA_SCALER`
  comment, #3); prefer differencing the `yaw`/`pitch` fields. Need about 20-50 files (< 1 GB jsonl). Download needs
  the user's go-ahead (rule: downloads need permission); I only streamed the head of one file.
- Do this analysis BEFORE tuning: first compute the §6.2 table on VPT and compare against the human.js defaults
  in §4. Replace any default that disagrees by more than the human-vs-human spread.

### 6.2 Statistics (computed identically on human and bot streams)
Windowing: per tick records `(tick, yaw, pitch, x, y, z, forward, on_ground)`; bot side joins event-log intent ids.

1. Rotation speed per tick `|dyaw|`, `|dpitch|`, split moving (position changes) / standing; log-scale histograms.
2. Head turn per 100 ms (sum of 2 ticks) while moving: p25/50/75/90/99 vs the human.js targets 8/14/24/44/107 deg.
3. Gesture segmentation: a gesture = maximal run of ticks with angular speed > 15 deg/s (merge gaps <= 1 tick).
   Per gesture: amplitude A, duration, peak speed. Fit `T ~ a + b*sqrt(A)` and `peak ~ c*A/T`; compare slope and
   residual spread (this tests the main-sequence law, the core of §4.2).
4. Overshoot / correction rate: fraction of gestures with A > 10 deg followed within 400 ms by an opposite-sign
   gesture; settle time = end of last gesture within 600 ms; number of sub-movements per aim. (Free play has no
   explicit target, so this is a proxy; for bot dig/place there is an explicit target and exact error.)
5. Pitch: distribution while walking (median ~10 deg down, [S #1 header]); fraction of walking ticks with pitch
   change = 0; autocorrelation of pitch at 1-2 s.
6. Idle: fraction of standing ticks with zero rotation; inter-gaze interval distribution; amplitude distribution.
7. Walking: heading error `|atan2(velocity) - yaw|` (target med 4 deg, 21 % > 25 deg [S #1]); fraction of walking
   ticks with forward held and heading error > 30 deg; stop-and-go count per 100 blocks; time from first move to
   sprint (25 % immediate, med 0.2 s); sprint jumps/s of sprinting (p10/med/p90 0.4/1.0/1.6); fraction of
   sprinting time; path length / straight-line length on the same task.
8. Quantisation: GCD of nonzero per-tick deltas (Grim style, deltas < 5 deg): must equal STEP for the bot; for the
   human, the estimated grid is their own sensitivity step.
9. Reaction: stimulus (new entity in view, chat command, new intent) to first rotation > 1 deg; floor >= 150 ms,
   compare median with the human value.

### 6.3 Tests and pass thresholds (session 13, made operational)
- Distribution tests: KS and energy distance per feature. With thousands of ticks, a plain p-value always
  rejects, so the threshold is "not significant" **and** the effect size is no larger than the human-vs-human distance
  between two disjoint sets of human sessions (bootstrap 95th percentile of that null). [G operationalisation]
- Detector: gradient-boosted or logistic classifier on 2 s windows (40 ticks) of features 1-8, session-grouped
  cross-validation, AUC <= 0.6 (session 13).
- Blind paired forced-choice with OBS recordings of the real window, as in session 13 (not ReplayMod). Pre-register n
  and accept if accuracy is not significantly above 50 % (binomial). Number of pairs [G: >= 30 pairs per viewer
  group].
- Before any of these: unit tests (§4.2), a determinism test (same seed, same frames), and a scripted-walk
  regression that the follower still reaches its goal in the existing terrain tests.
- Needs: a small `analysis` script under `spikes/` (allowed) that reads PRXCAP2 and VPT jsonl into the §6.2 table.
  [U] whether the event log already carries per-tick look; if not, the tee reconstruction above is enough.

## 7. Rejected main alternative
A spring-damper (critically/slightly underdamped second-order) camera controller: simpler (about 20 lines, stateless apart from
velocity, retarget is free) but its movement time does not grow with amplitude and overshoot is set by one damping number rather than a
sourced duration law, so it cannot reproduce the movement-time-vs-amplitude statistic we plan to test.

## 8. Summary and unverified list

1. Replace `look_towards` and `aim_precise` by one stateful `Gaze` owner of look (planner crate), per body, seeded personality.
2. Camera = minimum-jerk quintic gesture, duration `(0.08+0.3*sqrt(A/90))*turn_time*(walking?1:0.6)`, min 2 ticks.
3. Each gesture lands up to 6 % off; a corrective gesture follows after 2 ticks until within tolerance (0.25 deg for interaction).
4. Output only quantised `Look::Delta` on a 0.15 deg grid (s=0.5 default), yaw never wrapped.
5. Walking = pure pursuit on a string-pulled route, look-ahead 1.8-3.0, steering deadband 4-12 deg, forward at < 50/75 deg (20 deg near non-Walk steps).
6. Pitch ~6-16 deg down plus OU noise while walking; idle gaze gestures when no goal.
7. Reaction `max(0.15, N(0.25,0.1))` s on new external goals only; danger reflexes bypass.
8. Sprint on flat runs only; strafe, hops, walking glances out of v0.
9. Defaults come from human.js (targets from 48 players, narrow context), so run the VPT statistics first and retune.
10. Pass: energy distance below the human-vs-human null, detector AUC <= 0.6, blind paired test.

Unverified claims:
- Whether the human.js "48 players" data is published; that its personality distributions were fitted (the header lists targets only).
- All human.js numbers are from a summarised read of the file; re-read the source before coding (the file length "about 570 lines" is an estimate).
- Data licence of the VPT contractor files (repo is MIT; data terms not on the README); MineRL/BASALT licence and sizes.
- VPT contractors' mouse sensitivity (dx to degrees); number of files in the VPT indexes.
- Every [G] parameter: corr_pause, dwell, pitch deadband 1.5 deg, slope term, idle-gaze timings, 20 deg safety gate, same-T for both axes, a0 = 0 on retarget, KS effect-size rule, 30 pairs.
- Whether session-11 user recordings are sufficient; whether the event log has per-tick look.
- Other Grim checks beyond the six files read, Vulcan and other anti-cheats; Grim's Baritone check is disabled upstream per its own comment (it is cited as a pattern, not as live detection).
- Head-gaze main-sequence numbers and Fitts/overshoot values for Minecraft: none found.
- Whether primitive tick budgets and perception tolerate slower aiming and a non-level pitch.
