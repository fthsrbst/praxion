//! Malformed inputs and their exact error positions.

use praxion_rules::parse;

fn fails(source: &str, line: u32, col: u32, needle: &str) {
    let error = parse(source).unwrap_err();
    assert_eq!(
        (error.line, error.col),
        (line, col),
        "wrong position for `{}`: {}",
        error.message,
        error
    );
    assert!(
        error.message.contains(needle),
        "message `{}` does not contain `{needle}`",
        error.message
    );
}

#[test]
fn missing_skill_name() {
    fails("skill\n", 1, 6, "expected a skill name");
}

#[test]
fn missing_open_paren() {
    fails("skill s\n", 1, 8, "expected `(`");
}

#[test]
fn missing_close_paren_opens_a_param() {
    fails("skill s(\n", 1, 9, "expected a parameter name");
}

#[test]
fn trailing_token_after_rule() {
    fails(
        "skill s()\n  rules:\n    true -> done extra\n",
        3,
        18,
        "unexpected `extra` after the rule",
    );
}

#[test]
fn single_equals_is_not_a_comparison() {
    fails(
        "skill s()\n  rules:\n    count(x) = 1 -> done\n",
        3,
        14,
        "expected `->` after the guard, found `=`",
    );
}

#[test]
fn fail_without_parens() {
    fails(
        "skill s()\n  rules:\n    true -> fail\n",
        3,
        17,
        "expected `(` after `fail`",
    );
}

#[test]
fn do_without_parens() {
    fails(
        "skill s()\n  rules:\n    true -> do thing\n",
        3,
        21,
        "expected `(` after the skill name",
    );
}

#[test]
fn unknown_parameter_type() {
    fails(
        "skill s(a: nope)\n",
        1,
        12,
        "unknown type `nope`",
    );
}

#[test]
fn missing_param_colon() {
    fails("skill s(a int)\n", 1, 11, "expected `:` after the parameter name `a`");
}

#[test]
fn wrong_int_default() {
    fails(
        "skill s(a: int = x)\n",
        1,
        18,
        "expected an int default",
    );
}

#[test]
fn bool_default_for_int() {
    fails(
        "skill s(a: int = true)\n",
        1,
        18,
        "expected an int default",
    );
}

#[test]
fn missing_arrow() {
    fails(
        "skill s()\n  rules:\n    true done\n",
        3,
        10,
        "expected `->`",
    );
}

#[test]
fn rule_outside_a_skill() {
    fails("rules:\n", 1, 1, "outside a skill");
}

#[test]
fn duplicate_doc() {
    fails(
        "skill s()\n  doc \"a\"\n  doc \"b\"\n  rules:\n    true -> done\n",
        3,
        3,
        "duplicate `doc`",
    );
}

#[test]
fn doc_after_rules() {
    fails(
        "skill s()\n  rules:\n    true -> done\n  doc \"late\"\n",
        4,
        3,
        "`doc` must come before `rules:`",
    );
}

#[test]
fn duplicate_rules_section() {
    fails(
        "skill s()\n  rules:\n    true -> done\n  rules:\n",
        4,
        3,
        "duplicate `rules:`",
    );
}

#[test]
fn rule_before_rules_section() {
    fails(
        "skill s()\n  true -> done\n",
        2,
        3,
        "rule before `rules:`",
    );
}

#[test]
fn until_without_expression() {
    fails(
        "skill s()\n  rules:\n    true -> done until\n",
        3,
        23,
        "expected a guard expression",
    );
}

#[test]
fn max_ticks_without_int() {
    fails(
        "skill s()\n  rules:\n    true -> done max_ticks x\n",
        3,
        28,
        "expected an int after `max_ticks`",
    );
}

#[test]
fn max_ticks_zero() {
    fails(
        "skill s()\n  rules:\n    true -> done max_ticks 0\n",
        3,
        28,
        "`max_ticks` must be a positive int",
    );
}

#[test]
fn lone_bang() {
    fails(
        "skill s()\n  rules:\n    !true -> done\n",
        3,
        5,
        "unexpected `!`",
    );
}

#[test]
fn string_as_guard() {
    fails(
        "skill s()\n  rules:\n    \"x\" -> done\n",
        3,
        5,
        "expected a guard expression, found string",
    );
}

#[test]
fn unclosed_action_call() {
    fails(
        "skill s()\n  rules:\n    true -> mine(stone\n",
        3,
        23,
        "expected `)` after the arguments",
    );
}

#[test]
fn trailing_comma_in_action() {
    fails(
        "skill s()\n  rules:\n    true -> mine(stone,)\n",
        3,
        24,
        "expected an argument",
    );
}

#[test]
fn not_without_operand() {
    fails(
        "skill s()\n  rules:\n    not -> done\n",
        3,
        9,
        "expected a guard expression, found `->`",
    );
}

#[test]
fn and_without_operand() {
    fails(
        "skill s()\n  rules:\n    true and -> done\n",
        3,
        14,
        "expected a guard expression, found `->`",
    );
}

#[test]
fn chained_comparison() {
    fails(
        "skill s()\n  rules:\n    count(x) < 3 < 4 -> done\n",
        3,
        18,
        "expected `->` after the guard, found `<`",
    );
}

#[test]
fn missing_default_value() {
    fails(
        "skill s(a: int = )\n",
        1,
        18,
        "expected an int default",
    );
}

#[test]
fn max_ticks_before_until_is_rejected() {
    fails(
        "skill s()\n  rules:\n    true -> done max_ticks 5 until true\n",
        3,
        30,
        "unexpected `until` after the rule",
    );
}

#[test]
fn missing_action() {
    fails(
        "skill s()\n  rules:\n    true ->\n",
        3,
        12,
        "expected an action",
    );
}

#[test]
fn missing_rules_section_at_end_of_file() {
    fails("skill s()\n  doc \"x\"\n", 1, 7, "has no `rules:` section");
}

#[test]
fn empty_rules_section() {
    fails("skill s()\n  rules:\n", 1, 7, "has no rules");
}
