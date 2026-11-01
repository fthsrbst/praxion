//! Independent tests for the rule-list DSL (step 7 review).

use praxion_rules::{parse, print};

fn valid_skill() -> String {
    r#"
skill demo(a: int = 3, b: item)
  doc "hi"
  rules:
    count(x) >= 3 and (near(t, 4) or not has(y)) -> mine(stone) until count(x) >= 5 max_ticks 40
    true -> do other(1, $a, stone)
"#.to_string()
}

fn lines_of(s: &str) -> Vec<&str> {
    s.lines().collect()
}

fn position_inside(input: &str, line: u32, col: u32) -> bool {
    let lines: Vec<&str> = lines_of(input);
    if line == 0 || line as usize > lines.len() {
        // Allow a trailing position one past the last line? Parser reports
        // line numbers that exist in the input for every error path.
        return false;
    }
    let text = lines[(line as usize) - 1];
    // Column is one-based; allow up to length + 1 (end of line).
    col >= 1 && col as usize <= text.chars().count() + 1
}

// Parser robustness: 1000 random byte strings never panic and always return an
// error whose position lies inside the input.
#[test]
fn parser_robust_random_byte_strings() {
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..1000usize {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (rng % 80) as usize + 1;
        let mut s = String::new();
        for _ in 0..len {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            // Include a guaranteed-invalid character so the input is not a
            // valid (possibly empty) program.
            let ch = match rng % 7 {
                0 => '@',
                1 => '\x01',
                2 => '#',
                3 => 'x',
                4 => ' ',
                5 => '\n',
                _ => '0',
            };
            s.push(ch);
        }
        // Guarantee an invalid token outside any comment.
        s.push_str("\n@");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(&s)));
        let err = match result {
            Ok(Ok(_)) => panic!("iteration {i}: random input parsed successfully"),
            Ok(Err(e)) => e,
            Err(_) => panic!("iteration {i}: parser panicked on {s:?}"),
        };
        assert!(
            position_inside(&s, err.line, err.col),
            "iteration {i}: position {}:{} outside input {:?}",
            err.line,
            err.col,
            s
        );
    }
}

// Parser robustness: 1000 mutated valid skill files never panic and always
// return an error with a position inside the input.
#[test]
fn parser_robust_mutated_skill_files() {
    let base = valid_skill();
    let mut rng: u64 = 0xc2b2_ae3d_27d4_eb4f;
    for i in 0..1000usize {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut chars: Vec<char> = base.chars().collect();
        // Insert a guaranteed-invalid character at a random position.
        let pos = (rng as usize) % (chars.len() + 1);
        chars.insert(pos, '@');
        // Guarantee an invalid trailing token outside any comment.
        chars.extend(['\n', '@']);
        let s: String = chars.into_iter().collect();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parse(&s)));
        let err = match result {
            Ok(Ok(_)) => panic!("iteration {i}: mutated input parsed successfully"),
            Ok(Err(e)) => e,
            Err(_) => panic!("iteration {i}: parser panicked"),
        };
        assert!(
            position_inside(&s, err.line, err.col),
            "iteration {i}: position {}:{} outside input",
            err.line,
            err.col
        );
    }
}

// Property: random skills generated within the grammar round-trip through
// print/parse.
#[test]
fn property_random_skills_round_trip() {
    let guards = [
        "true",
        "false",
        "count(x) >= 0",
        "count(x) < 5",
        "has(x)",
        "near(t, 4)",
        "not has(y)",
        "count(x) >= 1 and has(y)",
        "(true or false)",
    ];
    let actions = [
        "done",
        "fail(bad)",
        "mine(stone)",
        "place(stone)",
        "craft(stick)",
        "wait(1)",
        "eat(apple)",
        "equip(sword)",
    ];
    let mut rng: u64 = 0x1234_5678_9abc_def0;
    for case in 0..500usize {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n_rules = (rng % 4) as usize + 1;
        let mut src = String::from("skill s()\n  rules:\n");
        for _ in 0..n_rules {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let g = guards[(rng as usize) % guards.len()];
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let a = actions[(rng as usize) % actions.len()];
            src.push_str(&format!("    {g} -> {a}\n"));
        }
        // Avoid an unconditional true followed by more rules (unreachable is a
        // load-time rejection, not a parse rejection); parse still accepts it,
        // but keep generation simple and parse-only.
        let first = parse(&src).unwrap_or_else(|e| panic!("case {case}: parse failed: {e}\n{src}"));
        let printed = print(&first);
        let second = parse(&printed).unwrap_or_else(|e| {
            panic!("case {case}: reparse failed: {e}\n{printed}")
        });
        assert_eq!(first, second, "case {case}: round trip mismatch");
    }
}

// Conformance-style: every construct from the grammar parses.
#[test]
fn parser_every_construct() {
    let source = r#"
skill demo(a: int = 3, b: item, c: block = minecraft:stone, d: entity_kind, e: bool = true)
  doc "hello \"world\""
  rules:
    true -> done
    false -> fail(x)
    count(x) >= 1 -> mine(stone) until count(x) >= 2 max_ticks 10
    true -> do other(1, $a, stone)
"#;
    let skills = parse(source).unwrap();
    assert_eq!(skills.len(), 1);
    let printed = print(&skills);
    let reparsed = parse(&printed).unwrap();
    assert_eq!(skills, reparsed);
}
