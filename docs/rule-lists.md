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

The shipped `get_stone_pickaxe.skill` is exactly these five rules. It has no
inline plank, stick or table steps: it calls `get_cobblestone`,
`get_sticks` and `get_crafting_table_near`, and each of those is at most six
rules too.

## Facts and primitives

Facts come from the perception layer; the fake world and the real body answer
the same calls.

| Fact | Type | Meaning |
|---|---|---|
| `count(item)` | `int` | items in the inventory |
| `has(item)` | `bool` | at least one item in the inventory |
| `near(block, r)` | `bool` | a block of that kind within `r` |
| `near_entity(kind, r)` | `bool` | an entity of that kind within `r` |

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
at its record's default `max_ticks` (20). `do skill(...)` calls another skill.
`done` finishes the skill; `fail(kind)` fails it.

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
  that tick, so the parent falls to its next rule. A child's `done` pops the
  frame and returns `Waiting("child done")`; the parent re-selects on the next
  tick, not in the same one. A preemption suspends the whole stack and
  abandons an in-flight discrete action (it settles `preempted`); on resume
  every frame re-evaluates from the top.
- **A failed primitive** is retried on the next tick: the outcome is recorded
  (`last_failure()`), but the world did not change, so the same rule is still
  the highest true one. A retry does not start a second action in the same
  tick. Three consecutive failures of the same rule's primitive, with no other
  rule firing in between, fail that skill with the last kind
  (`MAX_PRIMITIVE_FAILURES` = 3). A success or a different rule firing resets
  the count.
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

`get_wood(2)` in the fake world, seed 0. The skill is:

```
skill get_wood(n: int = 1)
  rules:
    count(oak_log) >= $n -> done
    true -> mine(oak_log)
```

The fake world's `mine` takes 6 ticks and its record's default `max_ticks` is
40. Both mines drop one log; there are oak trees nearby and the inventory starts
empty.

| Tick | What happens | Decision |
|---|---|---|
| 1 | guard 1 `0 >= 2` is F, guard 2 is T | `Run mine(oak_log)`, frame `get_wood` rule 2 |
| 2–6 | the mine is in flight; no switching | `Waiting("mine in flight")` |
| 7 | the mine settles `confirmed`, inventory has 1 log; guard 1 `1 >= 2` is F | `Run mine(oak_log)` |
| 8–12 | in flight | `Waiting("mine in flight")` |
| 13 | the mine settles, inventory has 2 logs; guard 1 is T | `Done` |

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
    has(wooden_pickaxe) -> mine(stone) until count(cobblestone) >= $n
    true -> do get_wooden_pickaxe()
```

| Tick | What happens | Decision |
|---|---|---|
| 1 | parent rule 3 (`0 < 3`); child rule 2 starts the commitment | `Run mine(stone)`, frames `get_stone_pickaxe` rule 3, `get_cobblestone` rule 2 |
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
would call `get_sticks(2)`. If three cobblestone never arrive, the commitment
ends at `max_ticks` (the `mine` record default, 40) and the rule is
re-selected from the top, so the skill keeps trying.

## Running it

```sh
cd body
cargo test -p praxion-rules -p praxion-interpreter -p praxion-fakeworld
```

The fake-world tests run every core skill on 50 seeds from an empty inventory,
then repeat with undo-perturbations (a crafted item removed, a placed table
broken) and assert zero duplicate placements and zero wasted crafts.
