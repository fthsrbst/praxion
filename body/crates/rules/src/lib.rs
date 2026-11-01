//! The rule-list DSL used by hand-written and (later) learned skills.
//!
//! The DSL is line based: one skill header, an optional `doc`, then `rules:`.
//! Every rule is `guard -> action [until guard] [max_ticks n]`, ordered highest
//! priority first. `ACTION until G` keeps running the action while the guard is
//! true and `G` is not true, and is released when `G` becomes true. `#` starts a
//! comment. Indentation never matters; a rule belongs to the preceding `skill`
//! line.
//!
//! ```
//! use praxion_rules::load;
//!
//! let program = load(
//!     "skill s()\n  rules:\n    has(stick) -> done\n    true -> mine(oak_log)\n",
//! )
//! .unwrap();
//! assert!(program.skill("s").is_some());
//! ```
//!
//! [`load`] parses, type-checks and validates the call graph. Every rejection
//! carries a line and a column ([`Error`]).

pub mod ast;
mod check;
pub mod error;
mod lexer;
mod parser;
pub mod print;
pub mod registry;

pub use ast::{
    Action, ActionKind, ArgType, ArgValue, CmpOp, Expr, ExprKind, FactCall, Param, Pos, Rule, Skill,
    Span, ValueType,
};
pub use check::{
    CORE_COMPOSITION_DEPTH, MAX_COMPOSITION_DEPTH, MAX_RULES, Program, freeze, freeze_with_depth,
};
pub use error::Error;
pub use parser::parse;
pub use print::print;
pub use registry::{
    BlockChange, BlockRef, CancellationClass, Effects, EvidenceKind, FactSig, Facts, ItemDelta,
    PrimParam, PrimitiveKind, PrimitiveRecord, Primitives,
};

/// Removes an optional `minecraft:` namespace from an identifier.
pub fn normalize_id(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_string()
}

/// Parses and checks one source file against the standard registries with the
/// default composition cap.
pub fn load(source: &str) -> Result<Program, Vec<Error>> {
    load_with_depth(source, MAX_COMPOSITION_DEPTH)
}

/// Parses and checks one source file with an explicit composition cap.
pub fn load_with_depth(source: &str, max_depth: usize) -> Result<Program, Vec<Error>> {
    let skills = parse(source).map_err(|e| vec![e])?;
    freeze_with_depth(skills, Facts::standard(), Primitives::standard(), max_depth)
}

/// Parses and checks several files as one ruleset with the default composition
/// cap. Load errors name their file.
pub fn load_files(files: &[(String, String)]) -> Result<Program, Vec<Error>> {
    load_files_with_depth(files, MAX_COMPOSITION_DEPTH)
}

/// Parses and checks several files as one ruleset with an explicit composition
/// cap. The hand-written core uses [`CORE_COMPOSITION_DEPTH`]. Load errors name
/// their file.
pub fn load_files_with_depth(
    files: &[(String, String)],
    max_depth: usize,
) -> Result<Program, Vec<Error>> {
    let mut skills = Vec::new();
    for (name, source) in files {
        let mut parsed =
            parse(source).map_err(|e| vec![e.with_file(name.clone())])?;
        for skill in &mut parsed {
            skill.file = Some(name.clone());
        }
        skills.extend(parsed);
    }
    freeze_with_depth(skills, Facts::standard(), Primitives::standard(), max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_small_program() {
        let program = load(
            r#"
skill helper(n: int = 2)
  rules:
    count(stick) >= $n -> done
    true -> craft(stick)
"#,
        )
        .unwrap();
        assert_eq!(program.skills().count(), 1);
        assert_eq!(program.skill("helper").unwrap().params[0].default, Some(ArgValue::Int(2)));
        assert_eq!(program.facts().get("count").unwrap().ret, ValueType::Int);
        assert_eq!(
            program.primitives().get("mine").unwrap().kind,
            PrimitiveKind::Discrete
        );
    }

    #[test]
    fn namespace_is_optional() {
        let program = load(
            "skill s()\n  rules:\n    has(minecraft:stick) -> mine(minecraft:stone)\n    true -> done\n",
        )
        .unwrap();
        let skill = program.skill("s").unwrap();
        match &skill.rules[0].guard.kind {
            ExprKind::Call { args, .. } => {
                assert_eq!(args[0].kind, ExprKind::Name("stick".into()));
            }
            other => panic!("unexpected guard {other:?}"),
        }
    }
}
