# Session 5: Skill definition and execution model

Decided 2026-10-29. Inputs: survey/03 (teleo-reactive programs, condition kinds, ideas 1-3 and 7),
survey/02 (guarded rule programs, plan-carried monitors), survey/04 (trust zones), systems-review
(Voyager and mindcraft scripts, altoclef task tree, Craftonomous skill contract).

## Two styles
- **Script** (Voyager, mindcraft): step-by-step code. Resuming after an interruption needs a
  program counter, and the world may have changed since.
- **Rule list, teleo-reactive (TR)** (Nilsson 1994; altoclef's tree is a cousin): an ordered list
  `condition -> action`; each tick the highest true rule fires. No program counter, so resume equals
  start. Setbacks fall to a lower rule, luck jumps higher.

```
get_stone_pickaxe:
  has(stone_pickaxe)                        -> nil
  cobble>=3 & sticks>=2 & table_in_reach    -> craft(stone_pickaxe)
  cobble>=3 & sticks>=2 & has(table)        -> place(table)
  has(wooden_pickaxe)                       -> mine(stone)
```

## Decided (user, 2026-10-29)
- **Skills are TR rule lists that the body (Rust) interprets.** The body keeps playing while the
  brain thinks, which serves the "never freeze" measure of 1.6. Delicate micro behaviour (combat,
  bucket clutch) stays in hand-written Rust primitives. Rejected: Python scripts (the brain
  runs them, so the game freezes when it stalls) and a free mix of both (two execution paths, two
  test suites).
- **LLM-written skills only combine existing primitives and skills.** A new low-level behaviour is
  written by hand in Rust. Rejected for now: LLM-written Python leaf code (slow path, needs a
  sandbox, unusable in reflexes) and LLM-written Rust primitives. **This narrows the 2026-10-21
  decision "LLMs may write code, for skills"**: they write rule lists, not primitives.
- **The compiler derives the guards.** The LLM writes steps and a goal; the compiler computes
  conditions such as "needs 4 or more planks to place a table" from the primitives' effects
  (generated from game data). LLMs write wrong guards for consumed items (survey/03). The LLM's
  own `ensures` is only a cross-check.

## Minor picks by Claude (user may overrule)
- **Skill contract:** name; summary and when not to use it; typed inputs; expected effects;
  side-effect declaration; cancellation class (session 4); time estimate; preconditions. Results
  are `ok`, `failed(kind)` or `preempted` (2.5, session 4).
- **Composition:** a rule's action may call another registered skill (`do: skill(args)`);
  dependencies are what a skill calls. Admission rejects unknown skills, type errors, cycles, more
  than 30 rules, and rules whose effect satisfies neither a higher guard nor the goal.
- **Size:** small skills of about 2 to 5 steps that call each other, not deep graphs.
- **Format:** Praxion's own small structured text format (not BT XML); the exact syntax is chosen
  when the first code is written.
- **Trust tiers:** a hand-written, human-reviewed core (the first demo rests on it); LLM skills enter
  only through an admission gate (session 11). The gate includes an interruption-injection test
  (survey/03 idea 2): inject a reflex, a take-over and hand-back, a revised command and an undone
  postcondition at every primitive boundary.

## Left open, with owner
- Whether an LLM skill may enter without the user's approval, the gate thresholds, versioning:
  session 11.
- Where the arbiter and reflexes sit and the tiers: session 7. How Jev and the LLM pick skills: session 8.
- Whether the format stays readable at 20 to 30 rules, and how accurate effect declarations are:
  measured during implementation (backlog).
