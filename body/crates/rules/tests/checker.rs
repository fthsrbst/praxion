//! Load-time rejections: unknown names, type errors and structural limits.

use praxion_rules::{CORE_COMPOSITION_DEPTH, load, load_with_depth};

fn reject(source: &str, line: u32, col: u32, needle: &str) {
    let errors = load(source).expect_err("expected a load error");
    assert_error(&errors, line, col, needle);
}

fn reject_with_depth(
    source: &str,
    max_depth: usize,
    line: u32,
    col: u32,
    needle: &str,
) {
    let errors =
        load_with_depth(source, max_depth).expect_err("expected a load error");
    assert_error(&errors, line, col, needle);
}

fn assert_error(errors: &[praxion_rules::Error], line: u32, col: u32, needle: &str) {
    let error = errors
        .iter()
        .find(|e| e.message.contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "no error containing `{needle}`; got {:?}",
                errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        (error.line, error.col),
        (line, col),
        "wrong position for `{}`: {error}",
        error.message
    );
}

#[test]
fn unknown_fact() {
    reject(
        "skill s()\n  rules:\n    nope(x) -> done\n",
        3,
        5,
        "unknown fact `nope`",
    );
}

#[test]
fn unknown_primitive() {
    reject(
        "skill s()\n  rules:\n    true -> nope(x)\n",
        3,
        13,
        "unknown primitive `nope`",
    );
}

#[test]
fn unknown_skill() {
    reject(
        "skill s()\n  rules:\n    true -> do nope()\n",
        3,
        13,
        "unknown skill `nope`",
    );
}

#[test]
fn wrong_fact_arity() {
    reject(
        "skill s()\n  rules:\n    count(x, y) -> done\n",
        3,
        5,
        "fact `count` takes 1 arguments, got 2",
    );
}

#[test]
fn wrong_fact_argument_type() {
    reject(
        "skill s()\n  rules:\n    count(3) -> done\n",
        3,
        11,
        "expected an argument of type item, found the int 3",
    );
}

#[test]
fn comparison_operand_must_be_int() {
    reject(
        "skill s()\n  rules:\n    count(x) >= true -> done\n",
        3,
        17,
        "expected int, found bool",
    );
}

#[test]
fn guard_must_be_bool() {
    reject(
        "skill s()\n  rules:\n    count(x) -> done\n",
        3,
        5,
        "a rule guard must be bool, found int",
    );
}

#[test]
fn until_must_be_bool() {
    reject(
        "skill s()\n  rules:\n    true -> done until count(x)\n",
        3,
        24,
        "an `until` guard must be bool, found int",
    );
}

#[test]
fn unknown_parameter() {
    reject(
        "skill s()\n  rules:\n    true -> mine($nope)\n",
        3,
        18,
        "unknown parameter `$nope`",
    );
}

#[test]
fn rule_after_true_is_unreachable() {
    reject(
        "skill s()\n  rules:\n    true -> done\n    true -> done\n",
        4,
        5,
        "unreachable rule after an unconditional `true` rule",
    );
}

#[test]
fn more_than_thirty_rules() {
    let mut source = String::from("skill s()\n  rules:\n");
    for _ in 0..31 {
        source.push_str("    true -> done\n");
    }
    // The 31st rule starts on line 33 (header + rules line + 30 rules).
    reject(&source, 33, 5, "has 31 rules; the limit is 30");
}

#[test]
fn call_cycle() {
    reject(
        "skill a()\n  rules:\n    true -> do b()\nskill b()\n  rules:\n    true -> do a()\n",
        6,
        13,
        "call cycle: a -> b -> a",
    );
}

#[test]
fn composition_depth_three() {
    reject(
        "skill a()\n  rules:\n    true -> do b()\nskill b()\n  rules:\n    true -> do c()\nskill c()\n  rules:\n    true -> done\n",
        3,
        13,
        "composition depth 3 from `a` through `b` exceeds the limit of 2",
    );
}

const CHAIN_FOUR: &str = "skill a()\n  rules:\n    true -> do b()\nskill b()\n  rules:\n    true -> do c()\nskill c()\n  rules:\n    true -> do d()\nskill d()\n  rules:\n    true -> done\n";
const CHAIN_FIVE: &str = "skill a()\n  rules:\n    true -> do b()\nskill b()\n  rules:\n    true -> do c()\nskill c()\n  rules:\n    true -> do d()\nskill d()\n  rules:\n    true -> do e()\nskill e()\n  rules:\n    true -> done\n";

#[test]
fn depth_four_is_rejected_with_the_default_cap() {
    reject(
        CHAIN_FOUR,
        3,
        13,
        "composition depth 4 from `a` through `b` exceeds the limit of 2",
    );
}

#[test]
fn depth_four_is_accepted_for_the_core() {
    let program = load_with_depth(CHAIN_FOUR, CORE_COMPOSITION_DEPTH)
        .expect("depth 4 is the core cap");
    assert_eq!(program.max_depth(), CORE_COMPOSITION_DEPTH);
    assert_eq!(CORE_COMPOSITION_DEPTH, 4);
}

#[test]
fn depth_five_is_rejected_for_the_core() {
    reject_with_depth(
        CHAIN_FIVE,
        CORE_COMPOSITION_DEPTH,
        3,
        13,
        "composition depth 5 from `a` through `b` exceeds the limit of 4",
    );
}

#[test]
fn duplicate_skill() {
    reject(
        "skill a()\n  rules:\n    true -> done\nskill a()\n  rules:\n    true -> done\n",
        4,
        7,
        "duplicate skill `a`",
    );
}

#[test]
fn required_after_default() {
    reject(
        "skill s(a: int = 1, b: int)\n  rules:\n    true -> done\n",
        1,
        21,
        "required parameter `$b` follows a parameter with a default",
    );
}

#[test]
fn duplicate_parameter() {
    reject(
        "skill s(a: int, a: int)\n  rules:\n    true -> done\n",
        1,
        17,
        "duplicate parameter `$a`",
    );
}

#[test]
fn primitive_argument_type() {
    reject(
        "skill s()\n  rules:\n    true -> mine(3)\n",
        3,
        18,
        "expected an argument of type block, found the int 3",
    );
}

#[test]
fn move_to_needs_a_name() {
    reject(
        "skill s()\n  rules:\n    true -> move_to(3)\n",
        3,
        21,
        "expected a block or entity_kind identifier",
    );
}

#[test]
fn observe_needs_a_fact() {
    reject(
        "skill s()\n  rules:\n    true -> observe(3)\n",
        3,
        21,
        "expected a fact call",
    );
}

#[test]
fn observe_accepts_a_fact() {
    let program = load("skill s()\n  rules:\n    true -> observe(count(oak_log))\n").unwrap();
    assert!(program.skill("s").is_some());
}

#[test]
fn skill_argument_type() {
    reject(
        "skill a()\n  rules:\n    true -> do b(1)\nskill b(x: item)\n  rules:\n    true -> done\n",
        3,
        18,
        "expected an argument of type item, found the int 1",
    );
}

#[test]
fn skill_argument_count() {
    reject(
        "skill a()\n  rules:\n    true -> do b()\nskill b(x: item)\n  rules:\n    true -> done\n",
        3,
        13,
        "skill `b` takes 1 to 1 arguments, got 0",
    );
}

#[test]
fn attack_and_hunt_only_name_mobs_the_body_may_hunt() {
    // Decision 2026-11-09 "Five next areas": creepers and Endermen are never
    // hunted. The allowlist is the loot-table list, enforced at load time so a
    // learned or hand-written skill cannot bypass the core callers (review
    // finding 4).
    reject(
        "skill s()\n  rules:\n    true -> attack(creeper)\n",
        3,
        20,
        "`creeper` is not hunted",
    );
    reject(
        "skill hunt(mob: entity_kind, drop: item)\n  rules:\n    true -> attack($mob)\nskill s()\n  rules:\n    true -> do hunt(creeper, gunpowder)\n",
        6,
        21,
        "`creeper` is not hunted",
    );
    reject(
        "skill s(mob: entity_kind)\n  rules:\n    true -> attack($mob)\nskill t()\n  rules:\n    true -> do s(enderman)\n",
        6,
        18,
        "`enderman` is not hunted",
    );
    // A guard that only looks for a creeper stays legal: the reflex keeps its
    // distance, and a skill may want to know whether one is near.
    load("skill s()\n  rules:\n    near_entity(creeper, 16) -> wait(1)\n").expect("a look-out");
}

#[test]
fn explore_keeps_accepting_blocks_and_huntable_mobs() {
    // The block-or-mob half is split at run time (`Explore::new`), where the
    // game's block vocabulary lives; the load-time half checks every
    // unambiguous entity kind above.
    load("skill s()\n  rules:\n    true -> explore(oak_log)\n").expect("a block");
    load("skill s()\n  rules:\n    true -> explore(cow)\n").expect("a huntable mob");
}
