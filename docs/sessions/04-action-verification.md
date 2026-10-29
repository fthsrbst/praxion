# Session 4: Action layer and verification

Decided 2026-10-29. Inputs: systems-review (mineflayer `dig`, `placeBlock`, sequence acks,
`syncWindow`; Craftonomous failure kinds), survey/01 idea 3 (evidence receipts), survey/03 idea 3
(cancellation contract), session 2.5 (receipts and `preempted`).

## The problem
"The action returned" is not "the world changed". Mineflayer's `dig()` resolves on its own local
prediction and never waits for the server; it also ignores the server's `block_changed_ack`
(1.19+). `placeBlock` does wait for the server. The proxy sees both the client's packets and the
server's answers, so Praxion can do better.

## Evidence ladder
0 sent (input injected) - 1 client agreed (the client's packet shows the action started) -
2 server confirmed (for example `block_changed_ack` with our sequence number, block changed) -
3 effect seen (item picked up, inventory changed). A receipt reports the level reached and a
status: `confirmed`, `refuted`, `unrefuted` or `unconfirmed`, and whether the input came from
Praxion or the user.

## Decided (user, 2026-10-29)
- **Success needs the server's word.** World-changing actions succeed at level 2; actions with
  an effect (pickup, craft) need level 3. No confirmation in the settle window gives
  `unconfirmed`: neither success nor failure. Praxion then re-reads the truth, and only then
  retries. Rejected: "the client agreed" (false successes) and "final effect only" (look and
  move could never be verified).
- **Unverifiable tasks are never counted as successes.** A task is tied to a condition the world
  model can compute (for example "enclosed, lit"). If it cannot be computed within the perception
  policy it is marked `unverifiable`, excluded from success rates and reported. Rejected: an LLM
  judging its own result (principle 6) and asking the user every time (breaks smoothness).
- **Safe take-over: dangerous moments hold their keys.** Every action declares a cancellation
  class: `free` (walking), `lossy(expected_loss)` (digging), `unsafe_until(condition, max_ticks)`
  (bridge-edge sneak, mid-air jump), `compensable(action)` (an open chest). In an
  `unsafe_until` moment the keys it needs stay held until the user touches that key or any
  movement key, or the condition holds, or `max_ticks` passes; all other keys are released at
  once. **This adds an exception to the 2.4 rule "release all keys on take-over"**; everything
  else in 2.4 stands.

## Minor picks by Claude (user may overrule)
- **Failure kinds** (typed, with `retryable`): Craftonomous's `timeout`, `precondition`,
  `unreachable`, `interrupted`, `world-changed`, `invalid-input`, `not-permitted`, `rate-limited`,
  `unknown` (should shrink over time), plus `preempted` (2.5), `unconfirmed`, `unverifiable` and
  `world-unsuitable` (the world offers no tree, not a broken skill).
- **Only real skill failures count against a skill's reliability.** `preempted`, `unconfirmed`,
  `unverifiable`, `world-unsuitable` and `precondition` do not. Craftonomous had to add a
  `NEVER_RETIRE` list because they were counted.
- **"Re-read the truth" primitive** after an unconfirmed result, modelled on mineflayer's
  `syncWindow` (a no-op click with `stateId: -1` forces the full window state). Since 1.17 a
  `stateId` match is never a positive witness.
- **Settle windows** (how long to wait for a confirmation) are per action kind, set from
  measurements, not guessed. The proxy attributes each block action by its sequence number.
- **Fault-injection test:** inject refused digs, dropped acks and stale inventories; the rate of
  false `confirmed` must be zero (survey/01 idea 3).

## Left open, with owner
- The primitive catalog (which actions exist) and skill-level contracts: session 5.
- Retry policy and blacklists: sessions 5 and 6.
- Exact settle windows and the cancellation class of every action: measured during implementation.
