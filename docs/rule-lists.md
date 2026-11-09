# Rule lists

A skill is a teleo-reactive rule list: an ordered set of `guard -> action`
rules. The body evaluates it every tick and runs the highest rule whose guard is
true. There is no program counter, so resuming after an interruption is the same
as starting: the world decides what happens next. This is the DSL the
hand-written core uses; later the skill writer emits the same format.

Files live in `body/skills/*.skill`. The reference implementation is
`body/crates/rules` (syntax and checking) and `body/crates/interpreter`
(the tick semantics).

## Grammar

```
file       := skill*
skill      := 'skill' NAME '(' params? ')' NEWLINE body
params     := param (',' param)*
param      := NAME ':' TYPE ('=' literal)?
body       := ('doc' STRING NEWLINE)? 'rules:' NEWLINE rule*
rule       := expr '->' action ('until' expr)? ('max_ticks' INT)?
action     := 'done'
            | 'fail' '(' NAME ')'
            | 'do' NAME '(' args? ')'
            | NAME '(' args? ')'
expr       := or
or         := and ('or' and)*
and        := not ('and' not)*
not        := 'not' not | cmp
cmp        := primary ('<' | '<=' | '==' | '!=' | '>=' | '>') primary
primary    := 'true' | 'false' | INT | '$' NAME | TAG | NAME '(' args? ')' | '(' expr ')'
args       := arg (',' arg)*
arg        := INT | '$' NAME | TAG | NAME | NAME '(' args? ')'
TYPE       := 'int' | 'item' | 'block' | 'entity_kind' | 'bool'
TAG        := '#' NAME
```

- One construct per line; indentation is ignored. `#` starts a comment unless
  it is immediately followed by an identifier, in which case it is an item or
  block tag (`#logs`). Write comments as `# text` or with any non-identifier
  character after the `#`.
  A rule belongs to the preceding `skill` line.
- Rules are ordered: the first true rule has the highest priority.
- `minecraft:` may prefix an identifier and is dropped: `minecraft:stone` is
  `stone`. The same holds inside a tag: `#minecraft:logs` is `#logs`.
- Precedence: `not` binds tighter than `and`, which binds tighter than `or`.
  Comparisons bind tighter than `not`. Comparisons do not chain.
- Parameters are positional and may have defaults. A defaulted parameter must
  come after the required ones. Call `$name` to use a parameter.

The example from the design discussion:

```
skill get_stone_pickaxe()
  doc "A stone pickaxe in the inventory."
  rules:
    count(stone_pickaxe) >= 1                                   -> done
    count(cobblestone) >= 3 and count(stick) >= 2 and near(crafting_table, 4) -> craft(stone_pickaxe)
    count(cobblestone) >= 3 and has(crafting_table)             -> place(crafting_table)
    has(wooden_pickaxe)                                         -> mine(stone) until count(cobblestone) >= 3
    true                                                        -> do get_wooden_pickaxe()
```

The shipped `get_stone_pickaxe.skill` (see the trace below) grew out of this
example. It has no inline plank, stick or table steps: it calls
`get_cobblestone`, `get_sticks` and `get_crafting_table_near`, and each of those
is at most six rules too.

## Facts and primitives

Facts come from the perception layer; the headless body and the real body answer
the same calls.

| Fact | Type | Meaning |
|---|---|---|
| `count(item)` | `int` | items in the inventory |
| `has(item)` | `bool` | at least one item in the inventory |
| `near(block, r)` | `bool` | a block of that kind within `r` of the known map |
| `near_entity(kind, r)` | `bool` | an entity of that kind within `r` |
| `food()` | `int` | the food bar (0 to 20) |
| `health()` | `int` | the health bar (0 to 20) |
| `is_night()` | `bool` | the body's night window (13000 to 23000) |
| `time_of_day()` | `int` | the world clock, 0 to 23999 |
| `count_food()` | `int` | edible items held, raw or cooked |

The `food`, `health`, `is_night` and `time_of_day` facts came with the survival
slice (decision 2026-11-09 "Five next areas"); `count_food` is what `get_food`
ends on. They have no arguments, so they are written with empty parentheses,
`health() <= 8`.

The live body answers `near` from the perception known map: a target is near
when a cell the perception instance has seen inside `r` holds it. It spends no
rays, and it answers `false` when the map knows of none, so `not near(T, R)`
can start an exploration. A tag target (`near(#logs, 5)`) matches any member.
The gathering skills use `5`, the radius of the `mine` search, so a target the
map knows about is actually in mining reach.

An `item` or `block` argument may be a tag: `count(#logs)`, `has(#planks)`,
`mine(#logs)`, `near(#logs, 4)`, `craft(#planks)`. Tags are the 26.3 data pack's
item and block tags, resolved when `praxion-gamedata` is built; a tag that the
jar does not define is rejected at load time. A tag argument in a primitive
declares the union of its members' effects (`mine(#logs)` covers every log
type), and a tag in `craft` picks a member recipe whose ingredients the player
has.

Guards use Kleene three-valued logic. A fact is `True`, `False`, or
`Unknown(fact)`:

| a | b | a AND b | a OR b | NOT a |
|---|---|---|---|---|
| T | T | T | T | F |
| T | U | U | T | F |
| T | F | F | T | F |
| U | U | U | U | U |
| U | F | F | U | U |
| F | F | F | F | T |

A comparison with an unknown side is unknown. An unknown fact is not "absent";
it is a value the perception policy could not answer.

Every action is a primitive with a versioned record
(`body/crates/rules/src/registry.rs`): typed parameters, discrete or
continuous, cancellation class, default `max_ticks`, declared effects and the
evidence kind that settles its receipt. Effects come from game data, not from
hand: `craft` consumes and produces exactly what the 26.3 recipe says
(`body/crates/gamedata`), and `mine`/`attack` carry the possible
drops, count ranges and tool requirements of the 26.3 loot tables. The tables
are read from the pinned server jar when `praxion-gamedata` is built. For a
primitive whose effect depends on its first argument, `effects_for(args)` gives
the exact effect; the record's `effects` is the union over all arguments.
`observe(fact)` is discrete, settles with evidence `FactKnown`, and times out
at its record's default `max_ticks` (100). `explore(target)` takes a block or,
since the survival slice, a mob; a mob target must be one the body hunts
(`entity_drops` in `praxion-gamedata`: cow, pig, sheep, chicken, zombie,
skeleton, spider), so `explore(creeper)` is refused when the call starts. It is
continuous with a 1200-tick budget: while no target cell is in the perception
known map within 32
blocks it walks to a frontier (a known walkable cell next to unknown cells,
scored away from cells already explored in that call). The first eight goals in
a call stay nearest-first; after that a frontier farther from the call's origin
wins, so the body heads outward instead of circling the sight ring. Descending
stays within the planner's safe three-block drop. When dry land is exhausted
(40 idle ticks) the call may swim. Once a target is known it plans straight to
it. It finishes `Confirmed` when `near(target, 5)` is known — the radius of the
`mine` search, so the target is in reach — or `Failed(not_found)` at the
budget. `do skill(...)` calls another skill. `done` finishes the skill;
`fail(kind)` fails it.

Mob literals in an action are checked against the hunt allowlist at load
(`attack(creeper)` is a load error); a guard such as `near_entity(creeper, 16)`
is a look-out, not a hunt, and stays legal. `attack(kind)` walks the body's
whole inventory for its best weapon (`weapon_rank`) and brings it into the main
hand by a hotbar key or one menu swap; it re-reads the mob's position every
tick and fails `no_target` after ten ticks without one in reach (three blocks,
the same eye-to-feet metric as `near_entity(kind, 3)`), so `hunt`'s `move_to`
takes over instead of the body swinging at an empty cell. `pickup(item)` walks
to the nearest item entity within eight blocks and settles on the inventory
gaining one `item`; a drop of another kind picked up on the way does not settle
it, and if the followed drop vanishes without the receipt the call looks at the
floor again for a farther drop of the wanted item. `hunt` therefore waits for
its drop *by kind*, not by identity: on a messy fight it may collect a drop that
is not its kill's, and a drop out of the eight-block radius ends the hunt
`done` with nothing picked up.

## One tick

```
1. A reflex or take-over preempts (external call).
2. If a discrete action is in flight, nothing else takes over until it
   settles (receipt or timeout).
3. Otherwise the highest rule whose guard is T fires. A continuous action is
   re-selected every tick, so a higher rule becoming T switches at once.
4. If no rule is T, the highest rule whose guard is U offers one
   observe(fact) for its unknown fact. At most one per tick.
5. If nothing is eligible, the skill fails with failed(no_rule).
```

```
 [select] ──T rule──► [run discrete action] ──receipt/timeout──► [select]
    │  ▲                    │
    │  └── continuous action: re-select every tick
    │                       └─ reflex / take-over ──► [preempted] ──reflex ended / carry on──► [select]
    │ none eligible
    ▼
 [failed(no_rule)] ──► wake brain
```

- **`until`** reads as English: `ACTION until G` commits to the rule's action
  until `G` becomes true. The rule keeps running while its own guard is T and
  `G` is not T (F or U), and is released when `G` is T, when its own guard
  stops being T, or at `max_ticks` (the rule's, else the primitive record's
  default). A non-primitive action (`do`, `done`, `fail`) has no record, so its
  default bound is `DEFAULT_MAX_TICKS` = 1200 ticks. `until_active()` exposes
  this to the arbiter's same-tier switch rule. So `mine(stone) until
  count(cobblestone) >= 3` mines until three cobblestone are in the inventory.
- **Composition:** a call is a frame on a stack; the deepest frame with a T
  rule fires. A child's `failed(kind)` makes the calling rule ineligible for
  that tick, so the parent falls to its next rule; if no other rule is true,
  the parent waits and calls the child again on the next tick. A child's
  `done` pops the frame and returns `Waiting("child done")`; the parent
  re-selects on the next tick, not in the same one. A preemption suspends the
  whole stack and abandons an in-flight discrete action (it settles
  `preempted`); on resume every frame re-evaluates from the top.
- **A failed primitive** is retried on the next tick: the outcome is recorded
  (`last_failure()`), but the world did not change, so the same rule is still
  the highest true one. A retry does not start a second action in the same
  tick. Three consecutive failures of the same rule, primitive or child skill,
  fail that skill with the last kind (`MAX_PRIMITIVE_FAILURES` = 3). Each rule
  in a frame keeps its own count: a rule's own success (its primitive settling
  confirmed, or its child's `done`) clears only that rule's count, and another
  rule firing leaves it untouched, so A-B-A alternation still fails A on A's
  third failure. The child's kind is the parent's failure kind.
- **Preemption** is not a failure. The skill keeps its stack and continues.

Limits, checked at load with a line and column: at most 30 rules per skill,
no call cycles, no rule after an unconditional `true` rule, and no unknown
fact, primitive, skill, parameter, or type mismatch.

**Composition depth is a loader setting.** The cap counts frames, root
included. The hand-written core in `body/skills/` loads with a cap of 4
(`CORE_COMPOSITION_DEPTH`: root + three children), because a human reviews it.
Everything else keeps the default cap of 2 (`load`, `load_files`,
`freeze`): LLM-written skills, and admission in Demo-2. The checked `Program`
carries its cap, and the interpreter enforces the same one at run time. A
depth-5 chain is rejected for the core.

The core skill sizes are small on purpose: 2 to 6 rules each, and a step is
composed with `do` rather than inlined. A leaf can be reached at more than one
depth, so its depth is not a property of the skill itself. The deepest chain
is:

```
get_stone_pickaxe                    root, depth 1
  get_sticks                         depth 2, a leaf
  get_crafting_table_near            depth 2, a leaf
  get_cobblestone                    depth 2
    get_wooden_pickaxe               depth 3
      get_planks                     depth 4, a leaf
      get_sticks                     depth 4, a leaf
      get_crafting_table_near        depth 4, a leaf
```

`get_planks`, `get_sticks` and `get_crafting_table_near` are leaves: at depth
4 they mine and craft their own inputs instead of calling further down. The
same leaves are also called directly by `get_stone_pickaxe` at depth 2.
`get_crafting_table` is the composed variant used on its own, and `get_wood`
remains the basic log skill.

## Worked example, tick by tick

`get_wood(2)` walked through tick by tick. The skill is:

```
skill get_wood(n: int = 1)
  rules:
    count(oak_log) >= $n -> done
    not near(oak_log, 5) -> explore(oak_log)
    true -> mine(oak_log)
```

In this example `mine` takes 6 ticks and its record's default `max_ticks` is
240. Both mines drop one log; there are oak trees nearby (so `near(oak_log, 5)`
is true and the explore rule is skipped) and the inventory starts empty.

| Tick | What happens | Decision |
|---|---|---|
| 1 | guard 1 `0 >= 2` is F, guard 2 is F (an oak log is near), guard 3 is T | `Run mine(oak_log)`, frame `get_wood` rule 3 |
| 2–6 | the mine is in flight; no switching | `Waiting("mine in flight")` |
| 7 | the mine settles `confirmed`, inventory has 1 log; guard 1 `1 >= 2` is F | `Run mine(oak_log)` |
| 8–12 | in flight | `Waiting("mine in flight")` |
| 13 | the mine settles, inventory has 2 logs; guard 1 is T | `Done` |

On a free spawn with no log in sight, guard 2 is T: the skill calls
`explore(oak_log)` and walks to the nearest frontier of the known map until a
log is within 5 blocks or the 1200-tick budget ends (`failed(not_found)`).

If the mine at tick 7 had reported `failed(block_resisted)`, tick 7 would be
`Waiting("retry")` and tick 8 would start the same mine again. The skill state is
unchanged, which is the teleo-reactive property: a setback falls back to the
rule that is still true, and luck jumps to a higher one.

A composition and `until` trace with the shipped `get_stone_pickaxe`. Start: a
wooden pickaxe, two sticks, no cobblestone, and a crafting table already placed
within 4 blocks. Stone is nearby.

```
skill get_stone_pickaxe()
  rules:
    count(stone_pickaxe) >= 1 -> done
    count(cobblestone) >= 3 and count(stick) >= 2 and near(crafting_table, 4) -> craft(stone_pickaxe)
    count(cobblestone) < 3 -> do get_cobblestone(3)
    count(stick) < 2 -> do get_sticks(2)
    true -> do get_crafting_table_near()

skill get_cobblestone(n: int = 3)
  rules:
    count(cobblestone) >= $n -> done
    has(wooden_pickaxe) and not near(stone, 5) and near(crafting_table, 4) and not has(crafting_table) -> mine(crafting_table)
    has(wooden_pickaxe) and not near(stone, 5) -> explore(stone)
    has(wooden_pickaxe) -> mine(stone) until count(cobblestone) >= $n
    true -> do get_wooden_pickaxe()
```

| Tick | What happens | Decision |
|---|---|---|
| 1 | parent rule 3 (`0 < 3`); child rule 1 is F, rules 2 and 3 are F (stone is in reach, so the table stays and no explore starts), rule 4 starts the commitment | `Run mine(stone)`, frames `get_stone_pickaxe` rule 3, `get_cobblestone` rule 4 |
| 2–6 | the mine is in flight | `Waiting("mine in flight")` |
| 7 | confirmed, 1 cobblestone; the goal `1 >= 3` is F, so the commitment holds | `Run mine(stone)` |
| 8–12 | in flight | `Waiting("mine in flight")` |
| 13 | confirmed, 2 cobblestone; goal F | `Run mine(stone)` |
| 14–18 | in flight | `Waiting("mine in flight")` |
| 19 | confirmed, 3 cobblestone; the goal turns T, so the commitment is released **on this tick** and child rule 1 fires | `Waiting("child done")` |
| 20 | the parent re-evaluates; rule 2 is T (3 cobblestone, 2 sticks, table near) | `Run craft(stone_pickaxe)` |
| 21–23 | craft in flight | `Waiting("craft in flight")` |
| 24 | confirmed, 1 stone pickaxe; parent rule 1 is T | `Done` |

A failed mine does not end the skill: tick 7 would be `Waiting("retry")` and
tick 8 starts it again. If no table were near, rule 5 would call
`get_crafting_table_near` first; if fewer than two sticks were left, rule 4
would call `get_sticks(2)`. Had no stone been in reach, child rule 2 would
first mine the placed table, so the body carries it to the stone, and rule 5
would place it again there. If three cobblestone never arrive, the commitment
ends at `max_ticks` (the `mine` record default, 240) and the rule is
re-selected from the top, so the skill keeps trying.

## Running it

```sh
cd body
cargo test -p praxion-rules -p praxion-interpreter
```
