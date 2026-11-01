//! The pretty-printer. `parse(print(ast)) == ast` for every parsed ruleset:
//! the printer emits the canonical form, with parentheses only where the
//! precedence rules need them.

use crate::ast::{Action, ActionKind, ArgValue, Expr, ExprKind, Rule, Skill};

/// Prints a whole ruleset.
pub fn print(skills: &[Skill]) -> String {
    let mut out = String::new();
    for skill in skills {
        print_skill(skill, &mut out);
    }
    out
}

/// Prints one skill.
pub fn print_skill(skill: &Skill, out: &mut String) {
    out.push_str("skill ");
    out.push_str(&skill.name);
    out.push('(');
    for (i, param) in skill.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&param.name);
        out.push_str(": ");
        out.push_str(param.ty.name());
        if let Some(default) = &param.default {
            out.push_str(" = ");
            out.push_str(&default.to_string());
        }
    }
    out.push_str(")\n");
    if let Some(doc) = &skill.doc {
        out.push_str("  doc \"");
        escape_string(doc, out);
        out.push_str("\"\n");
    }
    out.push_str("  rules:\n");
    for rule in &skill.rules {
        out.push_str("    ");
        print_rule(rule, out);
        out.push('\n');
    }
}

fn print_rule(rule: &Rule, out: &mut String) {
    print_expr(&rule.guard, 0, out);
    out.push_str(" -> ");
    print_action(&rule.action, out);
    if let Some(until) = &rule.until {
        out.push_str(" until ");
        print_expr(until, 0, out);
    }
    if let Some(max_ticks) = rule.max_ticks {
        out.push_str(" max_ticks ");
        out.push_str(&max_ticks.to_string());
    }
}

fn print_action(action: &Action, out: &mut String) {
    match &action.kind {
        ActionKind::Done => out.push_str("done"),
        ActionKind::Fail(kind) => {
            out.push_str("fail(");
            out.push_str(kind);
            out.push(')');
        }
        ActionKind::Primitive { name, args } => {
            out.push_str(name);
            print_args(args, out);
        }
        ActionKind::Do { skill, args } => {
            out.push_str("do ");
            out.push_str(skill);
            print_args(args, out);
        }
    }
}

fn print_args(args: &[Expr], out: &mut String) {
    out.push('(');
    for (i, arg) in args.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        print_expr(arg, 0, out);
    }
    out.push(')');
}

fn precedence(expr: &Expr) -> u8 {
    match &expr.kind {
        ExprKind::Or(..) => 1,
        ExprKind::And(..) => 2,
        ExprKind::Not(..) => 3,
        ExprKind::Cmp { .. } => 4,
        ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Param(_)
        | ExprKind::Name(_)
        | ExprKind::Call { .. } => 5,
    }
}

fn print_expr(expr: &Expr, minimum: u8, out: &mut String) {
    let own = precedence(expr);
    let parens = own < minimum;
    if parens {
        out.push('(');
    }
    match &expr.kind {
        ExprKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        ExprKind::Int(n) => out.push_str(&n.to_string()),
        ExprKind::Param(name) => {
            out.push('$');
            out.push_str(name);
        }
        ExprKind::Name(name) => out.push_str(name),
        ExprKind::Call { name, args } => {
            out.push_str(name);
            print_args(args, out);
        }
        ExprKind::Not(inner) => {
            out.push_str("not ");
            print_expr(inner, own, out);
        }
        ExprKind::And(left, right) => {
            print_expr(left, own, out);
            out.push_str(" and ");
            print_expr(right, own + 1, out);
        }
        ExprKind::Or(left, right) => {
            print_expr(left, own, out);
            out.push_str(" or ");
            print_expr(right, own + 1, out);
        }
        ExprKind::Cmp { op, left, right } => {
            print_expr(left, 5, out);
            out.push(' ');
            out.push_str(op.symbol());
            out.push(' ');
            print_expr(right, 5, out);
        }
    }
    if parens {
        out.push(')');
    }
}

fn escape_string(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// Prints one argument value (a parameter default).
pub fn print_arg_value(value: &ArgValue) -> String {
    value.to_string()
}

/// A small helper for tests and callers that want a single rule.
pub fn print_one_rule(rule: &Rule) -> String {
    let mut out = String::new();
    print_rule(rule, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    fn round_trip(source: &str) {
        let first = parse(source).unwrap();
        let printed = super::print(&first);
        let second = parse(&printed).unwrap_or_else(|e| panic!("reparse failed: {e}\n{printed}"));
        assert_eq!(first, second, "round trip changed the tree:\n{printed}");
        let printed_again = super::print(&second);
        assert_eq!(printed, printed_again, "printing is not stable");
    }

    #[test]
    fn round_trip_constructs() {
        round_trip(
            r#"
skill demo(a: int = 3, b: item, c: block = minecraft:stone, d: entity_kind, e: bool = true)
  doc "quotes \" and \\ and\nnewline"
  rules:
    count(oak_log) >= 3 and (near(crafting_table, 4) or not has(x)) -> craft(oak_planks) until count(oak_planks) < 4 max_ticks 20
    true -> do helper($a, stone, cow)
"#,
        );
        round_trip(
            r#"
skill other()
  rules:
    not not true -> fail(broken)
    (count(x) >= 1 or count(y) <= 2) and count(z) != 0 -> observe(count(x))
"#,
        );
    }

    #[test]
    fn precedence_survives_the_round_trip() {
        round_trip(
            r#"
skill p()
  rules:
    true and false or not true -> done
    (true and false) or true -> done
"#,
        );
    }
}
