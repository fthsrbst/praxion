//! The load-time type checker and the checked [`Program`].
//!
//! Rejections (with line and column): unknown fact, primitive or skill; type
//! errors; more than 30 rules; call cycles; composition depth over the
//! configured cap; a rule after an unconditional `true` rule (unreachable);
//! duplicate skill or parameter names; a required parameter after a defaulted
//! one; and file-level syntax errors from the parser.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ActionKind, ArgType, Expr, ExprKind, Skill, Span, ValueType};
use crate::error::Error;
use crate::registry::{Facts, Primitives};

/// The maximum number of rules in one skill (session 5).
pub const MAX_RULES: usize = 30;
/// The default maximum number of skill frames on the call stack, root
/// included. LLM-written skills and admission (Demo-2) keep this cap.
pub const MAX_COMPOSITION_DEPTH: usize = 2;
/// The cap for the hand-written core skills in `body/skills/`: root plus
/// three children. They are reviewed by hand, so they may compose deeper.
pub const CORE_COMPOSITION_DEPTH: usize = 4;
/// The program-wide cap once candidate skills are loaded: a candidate chain of
/// at most [`MAX_COMPOSITION_DEPTH`] frames may end in a full core chain (a
/// candidate `get_furnace` calling the depth-4 `get_cobblestone` chain).
pub const PROGRAM_COMPOSITION_DEPTH: usize = CORE_COMPOSITION_DEPTH + MAX_COMPOSITION_DEPTH;

/// A checked ruleset: the skills plus the registries every call was checked
/// against. It also carries the composition depth it was checked with, so the
/// interpreter enforces the same cap at run time.
#[derive(Clone, Debug)]
pub struct Program {
    skills: BTreeMap<String, Skill>,
    facts: Facts,
    primitives: Primitives,
    max_depth: usize,
}

impl Program {
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    pub fn primitives(&self) -> &Primitives {
        &self.primitives
    }

    /// The composition cap this program was checked with.
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
}

/// Type-checks parsed skills against the registries with the default
/// composition cap ([`MAX_COMPOSITION_DEPTH`]).
pub fn freeze(
    skills: Vec<Skill>,
    facts: Facts,
    primitives: Primitives,
) -> Result<Program, Vec<Error>> {
    freeze_with_depth(skills, facts, primitives, MAX_COMPOSITION_DEPTH)
}

/// Type-checks parsed skills against the registries with an explicit
/// composition cap. The core skills in `body/skills/` use
/// [`CORE_COMPOSITION_DEPTH`].
pub fn freeze_with_depth(
    skills: Vec<Skill>,
    facts: Facts,
    primitives: Primitives,
    max_depth: usize,
) -> Result<Program, Vec<Error>> {
    let mut errors = Vec::new();
    let mut map: BTreeMap<String, Skill> = BTreeMap::new();

    for skill in skills {
        if let Some(previous) = map.get(&skill.name) {
            errors.push(err(
                &skill,
                format!(
                    "duplicate skill `{}` (first defined at {}:{})",
                    skill.name, previous.span.start.line, previous.span.start.col
                ),
                skill.span,
            ));
            continue;
        }
        map.insert(skill.name.clone(), skill);
    }

    for skill in map.values() {
        check_skill(skill, &facts, &primitives, &map, &mut errors);
    }

    if !errors.is_empty() {
        // Unknown skills make the call graph incomplete; report what we have.
        errors.sort_by_key(|e| (e.line, e.col));
        return Err(errors);
    }

    check_call_graph(&map, &mut errors, max_depth);
    errors.sort_by_key(|e| (e.line, e.col));
    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(Program {
        skills: map,
        facts,
        primitives,
        max_depth,
    })
}

fn check_skill(
    skill: &Skill,
    facts: &Facts,
    primitives: &Primitives,
    all: &BTreeMap<String, Skill>,
    errors: &mut Vec<Error>,
) {
    let mut seen = BTreeSet::new();
    let mut defaulted = false;
    for param in &skill.params {
        if !seen.insert(param.name.as_str()) {
            errors.push(err(
                skill,
                format!("duplicate parameter `${}`", param.name),
                param.span,
            ));
        }
        if param.default.is_some() {
            defaulted = true;
        } else if defaulted {
            errors.push(err(
                skill,
                format!(
                    "required parameter `${}` follows a parameter with a default",
                    param.name
                ),
                param.span,
            ));
        }
    }

    if skill.rules.len() > MAX_RULES {
        let span = skill.rules[MAX_RULES].span;
        errors.push(err(
            skill,
            format!(
                "skill `{}` has {} rules; the limit is {MAX_RULES}",
                skill.name,
                skill.rules.len()
            ),
            span,
        ));
    }

    let mut saw_unconditional = false;
    for rule in &skill.rules {
        if saw_unconditional {
            errors.push(err(
                skill,
                "unreachable rule after an unconditional `true` rule",
                rule.span,
            ));
        }
        if matches!(rule.guard.kind, ExprKind::Bool(true)) {
            saw_unconditional = true;
        }

        match type_of(&rule.guard, None, skill, facts) {
            Ok(ValueType::Bool) => {}
            Ok(other) => errors.push(err(
                skill,
                format!("a rule guard must be bool, found {}", other.name()),
                rule.guard.span,
            )),
            Err(e) => errors.push(e),
        }
        if let Some(until) = &rule.until {
            match type_of(until, None, skill, facts) {
                Ok(ValueType::Bool) => {}
                Ok(other) => errors.push(err(
                    skill,
                    format!("an `until` guard must be bool, found {}", other.name()),
                    until.span,
                )),
                Err(e) => errors.push(e),
            }
        }

        match &rule.action.kind {
            ActionKind::Done => {}
            ActionKind::Fail(kind) => {
                if kind.is_empty() {
                    errors.push(err(skill, "a failure kind cannot be empty", rule.action.span));
                }
            }
            ActionKind::Primitive { name, args } => match primitives.get(name) {
                Some(record) => {
                    if args.len() != record.params.len() {
                        errors.push(err(
                            skill,
                            format!(
                                "primitive `{name}` takes {} arguments, got {}",
                                record.params.len(),
                                args.len()
                            ),
                            rule.action.span,
                        ));
                    } else {
                        for (arg, param) in args.iter().zip(&record.params) {
                            if let Err(e) = check_arg(arg, param.ty, skill, facts) {
                                errors.push(e);
                            }
                        }
                    }
                }
                None => errors.push(err(
                    skill,
                    format!("unknown primitive `{name}`"),
                    rule.action.span,
                )),
            },
            ActionKind::Do { skill: callee, args } => match all.get(callee) {
                Some(target) => {
                    if args.len() < target.required_params() || args.len() > target.params.len() {
                        errors.push(err(
                            skill,
                            format!(
                                "skill `{callee}` takes {} to {} arguments, got {}",
                                target.required_params(),
                                target.params.len(),
                                args.len()
                            ),
                            rule.action.span,
                        ));
                    } else {
                        for (arg, param) in args.iter().zip(&target.params) {
                            if let Err(e) =
                                check_arg(arg, ArgType::Value(param.ty), skill, facts)
                            {
                                errors.push(e);
                            }
                        }
                    }
                }
                None => errors.push(err(
                    skill,
                    format!("unknown skill `{callee}`"),
                    rule.action.span,
                )),
            },
        }
    }
}

/// The type of a guard expression. `expected` guides bare identifiers.
fn type_of(
    expr: &Expr,
    expected: Option<ValueType>,
    skill: &Skill,
    facts: &Facts,
) -> Result<ValueType, Error> {
    match &expr.kind {
        ExprKind::Bool(_) => Ok(ValueType::Bool),
        ExprKind::Int(_) => Ok(ValueType::Int),
        ExprKind::Param(name) => {
            let index = skill
                .param_index(name)
                .ok_or_else(|| err(skill, format!("unknown parameter `${name}`"), expr.span))?;
            Ok(skill.params[index].ty)
        }
        ExprKind::Name(name) => match expected {
            Some(ty @ (ValueType::Item | ValueType::Block | ValueType::EntityKind)) => {
                check_tag(name, ty, skill, expr.span)?;
                Ok(ty)
            }
            _ => Err(err(
                skill,
                format!(
                    "identifier `{name}` is only allowed where an item, block or entity kind is expected"
                ),
                expr.span,
            )),
        },
        ExprKind::Call { name, args } => {
            let sig = facts
                .get(name)
                .ok_or_else(|| err(skill, format!("unknown fact `{name}`"), expr.span))?;
            if args.len() != sig.params.len() {
                return Err(err(
                    skill,
                    format!(
                        "fact `{name}` takes {} arguments, got {}",
                        sig.params.len(),
                        args.len()
                    ),
                    expr.span,
                ));
            }
            for (arg, ty) in args.iter().zip(&sig.params) {
                check_arg(arg, ArgType::Value(*ty), skill, facts)?;
            }
            Ok(sig.ret)
        }
        ExprKind::Not(inner) => {
            expect_type(inner, ValueType::Bool, skill, facts)?;
            Ok(ValueType::Bool)
        }
        ExprKind::And(left, right) | ExprKind::Or(left, right) => {
            expect_type(left, ValueType::Bool, skill, facts)?;
            expect_type(right, ValueType::Bool, skill, facts)?;
            Ok(ValueType::Bool)
        }
        ExprKind::Cmp { left, right, .. } => {
            expect_type(left, ValueType::Int, skill, facts)?;
            expect_type(right, ValueType::Int, skill, facts)?;
            Ok(ValueType::Bool)
        }
    }
}

fn expect_type(
    expr: &Expr,
    want: ValueType,
    skill: &Skill,
    facts: &Facts,
) -> Result<(), Error> {
    let got = type_of(expr, Some(want), skill, facts)?;
    if got == want {
        Ok(())
    } else {
        Err(err(
            skill,
            format!("expected {}, found {}", want.name(), got.name()),
            expr.span,
        ))
    }
}

/// Checks one argument against a primitive or skill parameter type.
fn check_arg(
    expr: &Expr,
    want: ArgType,
    skill: &Skill,
    facts: &Facts,
) -> Result<(), Error> {
    match want {
        ArgType::Value(ty) => check_value(expr, ty, skill, facts),
        ArgType::BlockOrEntity => match &expr.kind {
            ExprKind::Name(name) => {
                if name.starts_with('#') {
                    check_tag(name, ValueType::Block, skill, expr.span)?;
                }
                Ok(())
            }
            ExprKind::Param(name) => {
                let index = skill
                    .param_index(name)
                    .ok_or_else(|| err(skill, format!("unknown parameter `${name}`"), expr.span))?;
                match skill.params[index].ty {
                    ValueType::Block | ValueType::EntityKind | ValueType::Item => Ok(()),
                    other => Err(err(
                        skill,
                        format!(
                            "expected a block or entity_kind argument, found ${name} of type {}",
                            other.name()
                        ),
                        expr.span,
                    )),
                }
            }
            _ => Err(err(
                skill,
                format!(
                    "expected a block or entity_kind identifier, found {}",
                    describe(expr)
                ),
                expr.span,
            )),
        },
        ArgType::Fact => match &expr.kind {
            ExprKind::Call { .. } => type_of(expr, None, skill, facts).map(|_| ()),
            _ => Err(err(
                skill,
                format!(
                    "expected a fact call, for example `observe(count(x))`, found {}",
                    describe(expr)
                ),
                expr.span,
            )),
        },
    }
}

/// Rejects a tag that the 26.3 data pack does not define for this type.
fn check_tag(
    name: &str,
    ty: ValueType,
    skill: &Skill,
    span: Span,
) -> Result<(), Error> {
    let Some(tag) = name.strip_prefix('#') else {
        return Ok(());
    };
    let exists = match ty {
        ValueType::Item => praxion_gamedata::item_tag(tag).is_some(),
        ValueType::Block => praxion_gamedata::block_tag(tag).is_some(),
        ValueType::EntityKind | ValueType::Int | ValueType::Bool => false,
    };
    if exists {
        Ok(())
    } else {
        Err(err(
            skill,
            format!("unknown {} tag `#{tag}`", ty.name()),
            span,
        ))
    }
}

fn check_value(
    expr: &Expr,
    want: ValueType,
    skill: &Skill,
    _facts: &Facts,
) -> Result<(), Error> {
    match &expr.kind {
        ExprKind::Int(_) if want == ValueType::Int => Ok(()),
        ExprKind::Bool(_) if want == ValueType::Bool => Ok(()),
        ExprKind::Name(name)
            if matches!(
                want,
                ValueType::Item | ValueType::Block | ValueType::EntityKind
            ) =>
        {
            check_tag(name, want, skill, expr.span)?;
            Ok(())
        }
        ExprKind::Param(name) => {
            let index = skill
                .param_index(name)
                .ok_or_else(|| err(skill, format!("unknown parameter `${name}`"), expr.span))?;
            let have = skill.params[index].ty;
            if have == want {
                Ok(())
            } else {
                Err(err(
                    skill,
                    format!(
                        "expected an argument of type {}, found `${name}` of type {}",
                        want.name(),
                        have.name()
                    ),
                    expr.span,
                ))
            }
        }
        ExprKind::Call { name, .. } => Err(err(
            skill,
            format!(
                "expected an argument of type {}, found the fact call `{name}(...)`",
                want.name()
            ),
            expr.span,
        )),
        ExprKind::Int(n) => Err(err(
            skill,
            format!(
                "expected an argument of type {}, found the int {n}",
                want.name()
            ),
            expr.span,
        )),
        ExprKind::Bool(b) => Err(err(
            skill,
            format!("expected an argument of type {}, found `{b}`", want.name()),
            expr.span,
        )),
        ExprKind::Name(name) => Err(err(
            skill,
            format!(
                "expected an argument of type {}, found the identifier `{name}` (an item, block or entity kind)",
                want.name()
            ),
            expr.span,
        )),
        ExprKind::Not(_) | ExprKind::And(_, _) | ExprKind::Or(_, _) | ExprKind::Cmp { .. } => {
            Err(err(
                skill,
                format!(
                    "expected an argument of type {}, found a boolean expression",
                    want.name()
                ),
                expr.span,
            ))
        }
    }
}

/// A short description of an expression for a "found" clause.
fn describe(expr: &Expr) -> &'static str {
    match &expr.kind {
        ExprKind::Int(_) => "an int",
        ExprKind::Bool(_) => "a bool",
        ExprKind::Param(_) => "a parameter",
        ExprKind::Name(_) => "an identifier",
        ExprKind::Call { .. } => "a fact call",
        ExprKind::Not(_) | ExprKind::And(_, _) | ExprKind::Or(_, _) | ExprKind::Cmp { .. } => {
            "a boolean expression"
        }
    }
}

fn check_call_graph(
    all: &BTreeMap<String, Skill>,
    errors: &mut Vec<Error>,
    max_depth: usize,
) {
    let mut graph: BTreeMap<String, Vec<(String, Span)>> = BTreeMap::new();
    for skill in all.values() {
        for rule in &skill.rules {
            if let ActionKind::Do { skill: callee, .. } = &rule.action.kind
                && all.contains_key(callee)
            {
                graph
                    .entry(skill.name.clone())
                    .or_default()
                    .push((callee.clone(), rule.action.span));
            }
        }
    }

    // Cycle detection: 0 unseen, 1 on the stack, 2 finished.
    let mut color: BTreeMap<String, u8> = BTreeMap::new();
    let mut stack: Vec<String> = Vec::new();
    let mut cycle = false;
    for name in all.keys() {
        if color.get(name).copied().unwrap_or(0) == 0 {
            visit(name, all, &graph, &mut color, &mut stack, errors, &mut cycle);
        }
    }
    if cycle {
        return;
    }

    let mut memo: BTreeMap<String, usize> = BTreeMap::new();
    for name in all.keys() {
        longest(name, &graph, &mut memo);
    }
    let mut reported = BTreeSet::new();
    for (caller, edges) in &graph {
        for (callee, span) in edges {
            let depth = 1 + memo.get(callee).copied().unwrap_or(1);
            if depth > max_depth && reported.insert((span.start.line, span.start.col)) {
                let skill = &all[caller];
                errors.push(err(
                    skill,
                    format!(
                        "composition depth {depth} from `{caller}` through `{callee}` exceeds the limit of {max_depth}"
                    ),
                    *span,
                ));
            }
        }
    }
}

fn visit(
    name: &str,
    all: &BTreeMap<String, Skill>,
    graph: &BTreeMap<String, Vec<(String, Span)>>,
    color: &mut BTreeMap<String, u8>,
    stack: &mut Vec<String>,
    errors: &mut Vec<Error>,
    cycle: &mut bool,
) {
    color.insert(name.to_string(), 1);
    stack.push(name.to_string());
    if let Some(edges) = graph.get(name) {
        for (child, span) in edges {
            match color.get(child).copied().unwrap_or(0) {
                0 => visit(child, all, graph, color, stack, errors, cycle),
                1 => {
                    *cycle = true;
                    let start = stack.iter().position(|s| s == child).unwrap_or(0);
                    let mut chain = stack[start..].to_vec();
                    chain.push(child.clone());
                    errors.push(err(
                        &all[name],
                        format!("call cycle: {}", chain.join(" -> ")),
                        *span,
                    ));
                }
                _ => {}
            }
        }
    }
    stack.pop();
    color.insert(name.to_string(), 2);
}

fn longest(
    name: &str,
    graph: &BTreeMap<String, Vec<(String, Span)>>,
    memo: &mut BTreeMap<String, usize>,
) -> usize {
    if let Some(depth) = memo.get(name) {
        return *depth;
    }
    let mut best = 1;
    if let Some(edges) = graph.get(name) {
        for (child, _) in edges {
            best = best.max(1 + longest(child, graph, memo));
        }
    }
    memo.insert(name.to_string(), best);
    best
}

fn err(skill: &Skill, message: impl Into<String>, span: Span) -> Error {
    let error = Error::span(message, span);
    match &skill.file {
        Some(file) => error.with_file(file.clone()),
        None => error,
    }
}
