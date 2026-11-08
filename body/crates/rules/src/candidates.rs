//! Admission checks for learned candidate skills (Demo-2).
//!
//! The core skills are fixed leaves: a candidate may call them, but a
//! candidate-to-candidate chain may be at most [`MAX_COMPOSITION_DEPTH`] frames
//! deep. Core skills keep [`CORE_COMPOSITION_DEPTH`] for the program-wide
//! freeze. Candidate names may not shadow a core skill, and every candidate
//! file gets its own report so the brain can repair one file at a time.
//!
//! ```text
//! let reports = check_candidates(&core, &candidates)?;
//! for report in reports {
//!     if !report.ok {
//!         for error in report.errors { /* kind, message, line, col */ }
//!     }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ActionKind, Pos, Skill, Span};
use crate::check::{
    CORE_COMPOSITION_DEPTH, MAX_COMPOSITION_DEPTH, MAX_RULES, PROGRAM_COMPOSITION_DEPTH,
    freeze_with_depth,
};
use crate::error::Error;
use crate::parser::parse;
use crate::registry::{Facts, Primitives};

/// The stable error kinds the brain switches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateErrorKind {
    /// The lexer or parser rejected the file.
    Parse,
    /// The freeze rejected it: type, arity, unknown fact/primitive/skill,
    /// duplicate name, cycle or the core depth cap.
    Check,
    /// A candidate skill reuses a core or earlier candidate skill name.
    NameCollision,
    /// A candidate skill has more than [`MAX_RULES`] rules.
    TooManyRules,
    /// Candidate-to-candidate composition is deeper than
    /// [`MAX_COMPOSITION_DEPTH`] frames.
    TooDeep,
    /// The file parses but defines no skill.
    NoSkill,
}

impl CandidateErrorKind {
    /// The stable string used in the JSON report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Check => "check",
            Self::NameCollision => "name_collision",
            Self::TooManyRules => "too_many_rules",
            Self::TooDeep => "too_deep",
            Self::NoSkill => "no_skill",
        }
    }
}

/// One rejection in a candidate report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateError {
    pub kind: CandidateErrorKind,
    pub message: String,
    pub line: u32,
    pub col: u32,
}

/// The check result for one candidate file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateReport {
    pub file: String,
    pub skills: Vec<String>,
    pub ok: bool,
    pub errors: Vec<CandidateError>,
}

/// Parses and checks the core skills together with every candidate file.
///
/// Returns one report per candidate file, in file-name order. `Ok` means the
/// core skills themselves loaded; a candidate failure is inside its report.
/// An `Err` is a core parse/check failure or a missing file association, which
/// the caller treats as a usage/IO error.
pub fn check_candidates(
    core: &[(String, String)],
    candidates: &[(String, String)],
) -> Result<Vec<CandidateReport>, String> {
    let core_skills = parse_files(core)?;
    freeze_with_depth(
        core_skills.clone(),
        Facts::standard(),
        Primitives::standard(),
        CORE_COMPOSITION_DEPTH,
    )
    .map_err(|errors| format_errors("core skills", &errors))?;

    let core_positions: BTreeMap<String, (String, Pos)> = core_skills
        .iter()
        .map(|skill| {
            (
                skill.name.clone(),
                (skill.file.clone().unwrap_or_default(), skill.span.start),
            )
        })
        .collect();

    let mut inputs: Vec<&(String, String)> = candidates.iter().collect();
    inputs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut states: Vec<FileState> = Vec::with_capacity(inputs.len());
    for (file, source) in inputs {
        let mut state = FileState {
            file: file.clone(),
            skills: Vec::new(),
            errors: Vec::new(),
        };
        match parse(source) {
            Ok(mut skills) => {
                for skill in &mut skills {
                    skill.file = Some(file.clone());
                }
                if skills.is_empty() {
                    state.errors.push(CandidateError {
                        kind: CandidateErrorKind::NoSkill,
                        message: "the file defines no skill".to_owned(),
                        line: 1,
                        col: 1,
                    });
                }
                state.skills = skills;
            }
            Err(error) => state.errors.push(CandidateError {
                kind: CandidateErrorKind::Parse,
                message: error.message,
                line: error.line,
                col: error.col,
            }),
        }
        states.push(state);
    }

    // Names may not shadow a core skill or an earlier candidate skill. A
    // colliding definition is left out of the program so the freeze does not
    // report the same problem again as a duplicate.
    let mut seen: BTreeMap<String, (String, Pos)> = BTreeMap::new();
    let mut candidate_names: BTreeSet<String> = BTreeSet::new();
    let mut program_skills = core_skills;
    for state in &mut states {
        let mut kept = Vec::new();
        for skill in &state.skills {
            let collision = if let Some((file, pos)) = core_positions.get(&skill.name) {
                Some(format!(
                    "skill `{}` is already a core skill ({file}:{}:{})",
                    skill.name, pos.line, pos.col
                ))
            } else {
                seen.get(&skill.name).map(|(file, pos)| {
                    format!(
                        "skill `{}` is already defined in {file}:{}:{}",
                        skill.name, pos.line, pos.col
                    )
                })
            };
            if let Some(message) = collision {
                state.errors.push(CandidateError {
                    kind: CandidateErrorKind::NameCollision,
                    message,
                    line: skill.span.start.line,
                    col: skill.span.start.col,
                });
            } else {
                seen.insert(skill.name.clone(), (state.file.clone(), skill.span.start));
                candidate_names.insert(skill.name.clone());
                kept.push(skill.clone());
            }
        }
        for skill in &kept {
            if skill.rules.len() > MAX_RULES {
                let span = skill.rules[MAX_RULES].span;
                state.errors.push(CandidateError {
                    kind: CandidateErrorKind::TooManyRules,
                    message: format!(
                        "skill `{}` has {} rules; the limit is {MAX_RULES}",
                        skill.name,
                        skill.rules.len()
                    ),
                    line: span.start.line,
                    col: span.start.col,
                });
            }
        }
        program_skills.extend(kept);
    }

    check_candidate_depth(&program_skills, &candidate_names, &mut states);

    let by_file: BTreeMap<String, usize> = states
        .iter()
        .enumerate()
        .map(|(index, state)| (state.file.clone(), index))
        .collect();

    match freeze_with_depth(
        program_skills,
        Facts::standard(),
        Primitives::standard(),
        PROGRAM_COMPOSITION_DEPTH,
    ) {
        Ok(_) => {}
        Err(errors) => {
            for error in errors {
                let Some(file) = &error.file else {
                    return Err(format!(
                        "load error without a file: {}:{}: {}",
                        error.line, error.col, error.message
                    ));
                };
                let Some(&index) = by_file.get(file) else {
                    return Err(format_errors("core skills", &[error]));
                };
                let state = &mut states[index];
                // The explicit checks above already reported this position
                // (rule count, duplicate name); do not report it twice.
                if state
                    .errors
                    .iter()
                    .any(|known| known.line == error.line && known.col == error.col)
                {
                    continue;
                }
                state.errors.push(CandidateError {
                    kind: classify(&error.message),
                    message: error.message,
                    line: error.line,
                    col: error.col,
                });
            }
        }
    }

    let mut reports = Vec::with_capacity(states.len());
    for state in states {
        let skills = state
            .skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        let ok = state.errors.is_empty();
        let mut errors = state.errors;
        errors.sort_by_key(|error| (error.line, error.col));
        reports.push(CandidateReport {
            file: state.file,
            skills,
            ok,
            errors,
        });
    }
    Ok(reports)
}

/// Parses every file as one ruleset, naming the file in errors.
fn parse_files(files: &[(String, String)]) -> Result<Vec<Skill>, String> {
    let mut skills = Vec::new();
    for (file, source) in files {
        let mut parsed = parse(source)
            .map_err(|error| format!("{file}:{}:{}: {}", error.line, error.col, error.message))?;
        for skill in &mut parsed {
            skill.file = Some(file.clone());
        }
        skills.extend(parsed);
    }
    Ok(skills)
}

/// Freeze errors that are not already reported as one of the candidate kinds.
fn classify(message: &str) -> CandidateErrorKind {
    if message.contains("rules; the limit is") {
        CandidateErrorKind::TooManyRules
    } else if message.starts_with("composition depth ") {
        CandidateErrorKind::TooDeep
    } else {
        CandidateErrorKind::Check
    }
}

/// Rejects a candidate-to-candidate chain deeper than
/// [`MAX_COMPOSITION_DEPTH`]. Calls into a core skill or a primitive are
/// leaves and do not add a frame.
fn check_candidate_depth(
    skills: &[Skill],
    candidate_names: &BTreeSet<String>,
    states: &mut [FileState],
) {
    let mut graph: BTreeMap<&str, Vec<(&str, Span)>> = BTreeMap::new();
    for skill in skills
        .iter()
        .filter(|skill| candidate_names.contains(&skill.name))
    {
        let edges = graph.entry(skill.name.as_str()).or_default();
        for rule in &skill.rules {
            if let ActionKind::Do { skill: callee, .. } = &rule.action.kind
                && candidate_names.contains(callee)
            {
                edges.push((callee.as_str(), rule.action.span));
            }
        }
    }

    let mut memo: BTreeMap<&str, usize> = BTreeMap::new();
    let mut visiting: BTreeSet<&str> = BTreeSet::new();
    let mut cyclic = false;
    for name in graph.keys() {
        longest(name, &graph, &mut memo, &mut visiting, &mut cyclic);
    }
    if cyclic {
        // The freeze reports the cycle; a depth number would be meaningless.
        return;
    }

    for skill in skills
        .iter()
        .filter(|skill| candidate_names.contains(&skill.name))
    {
        let depth = memo.get(skill.name.as_str()).copied().unwrap_or(1);
        if depth <= MAX_COMPOSITION_DEPTH {
            continue;
        }
        let call = skill.rules.iter().find_map(|rule| {
            if let ActionKind::Do { skill: callee, .. } = &rule.action.kind
                && candidate_names.contains(callee)
                && 1 + memo.get(callee.as_str()).copied().unwrap_or(1) == depth
            {
                Some((callee.as_str(), rule.action.span))
            } else {
                None
            }
        });
        let Some((callee, span)) = call else {
            continue;
        };
        if let Some(state) = states
            .iter_mut()
            .find(|state| skill.file.as_deref() == Some(state.file.as_str()))
        {
            state.errors.push(CandidateError {
                kind: CandidateErrorKind::TooDeep,
                message: format!(
                    "skill `{}` composes to depth {depth} through `{callee}`; candidates may compose at most {MAX_COMPOSITION_DEPTH} deep (core skills are leaves)",
                    skill.name
                ),
                line: span.start.line,
                col: span.start.col,
            });
        }
    }
}

fn longest<'a>(
    name: &'a str,
    graph: &BTreeMap<&'a str, Vec<(&'a str, Span)>>,
    memo: &mut BTreeMap<&'a str, usize>,
    visiting: &mut BTreeSet<&'a str>,
    cyclic: &mut bool,
) -> usize {
    if let Some(depth) = memo.get(name) {
        return *depth;
    }
    if !visiting.insert(name) {
        *cyclic = true;
        return 0;
    }
    let mut best = 1;
    if let Some(edges) = graph.get(name) {
        for (child, _) in edges {
            best = best.max(1 + longest(child, graph, memo, visiting, cyclic));
        }
    }
    visiting.remove(name);
    memo.insert(name, best);
    best
}

fn format_errors(prefix: &str, errors: &[Error]) -> String {
    let joined = errors
        .iter()
        .map(Error::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    format!("{prefix}: {joined}")
}

struct FileState {
    file: String,
    skills: Vec<Skill>,
    errors: Vec<CandidateError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &str = "\
skill get_cobblestone(n: int = 1)
  rules:
    count(cobblestone) >= $n -> done
    true -> mine(stone)
skill get_crafting_table_near()
  rules:
    near(crafting_table, 4) -> done
    true -> explore(stone)
";

    const GET_FURNACE: &str = r#"skill get_furnace()
  doc "Have a furnace."
  rules:
    count(furnace) >= 1 -> done
    count(cobblestone) >= 8 and near(crafting_table, 4) -> craft(furnace)
    count(cobblestone) < 8 -> do get_cobblestone(8)
    true -> do get_crafting_table_near()
"#;

    fn core() -> Vec<(String, String)> {
        vec![("core.skill".to_owned(), CORE.to_owned())]
    }

    fn one(name: &str, source: &str) -> Vec<(String, String)> {
        vec![(name.to_owned(), source.to_owned())]
    }

    fn kinds(report: &CandidateReport) -> Vec<CandidateErrorKind> {
        report.errors.iter().map(|error| error.kind).collect()
    }

    #[test]
    fn a_valid_candidate_passes() {
        let reports = check_candidates(&core(), &one("get_furnace.skill", GET_FURNACE)).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].ok, "{:?}", reports[0].errors);
        assert_eq!(reports[0].skills, ["get_furnace"]);
        assert!(reports[0].errors.is_empty());
    }

    #[test]
    fn a_parse_error_is_parse() {
        let reports = check_candidates(&core(), &one("broken.skill", "skill broken(\n")).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::Parse]);
        assert!(!reports[0].ok);
    }

    #[test]
    fn a_check_error_is_check() {
        let source = "skill bad()\n  rules:\n    nope(stick) -> done\n";
        let reports = check_candidates(&core(), &one("bad.skill", source)).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::Check]);
        assert!(reports[0].errors[0].message.contains("unknown fact `nope`"));
    }

    #[test]
    fn a_core_name_collision_is_reported() {
        let source = "skill get_cobblestone()\n  rules:\n    true -> done\n";
        let reports = check_candidates(&core(), &one("shadow.skill", source)).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::NameCollision]);
        assert!(
            reports[0].errors[0]
                .message
                .contains("already a core skill")
        );
    }

    #[test]
    fn a_candidate_name_collision_is_reported_on_the_later_file() {
        let source = "skill duplicate()\n  rules:\n    true -> done\n";
        let candidates = vec![
            ("a.skill".to_owned(), source.to_owned()),
            ("b.skill".to_owned(), source.to_owned()),
        ];
        let reports = check_candidates(&core(), &candidates).unwrap();
        assert!(reports[0].ok, "{:?}", reports[0].errors);
        assert_eq!(kinds(&reports[1]), [CandidateErrorKind::NameCollision]);
    }

    #[test]
    fn more_than_thirty_rules_is_too_many_rules() {
        let mut source = String::from("skill many()\n  rules:\n");
        for _ in 0..=MAX_RULES {
            source.push_str("    count(stick) >= 0 -> done\n");
        }
        let reports = check_candidates(&core(), &one("many.skill", &source)).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::TooManyRules]);
        assert!(reports[0].errors[0].message.contains("the limit is 30"));
    }

    #[test]
    fn three_candidate_frames_is_too_deep() {
        let source = "\
skill a()
  rules:
    true -> do b()
skill b()
  rules:
    true -> do c()
skill c()
  rules:
    true -> done
";
        let reports = check_candidates(&core(), &one("chain.skill", source)).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::TooDeep]);
        assert!(reports[0].errors[0].message.contains("depth 3"));
    }

    #[test]
    fn two_candidate_frames_pass() {
        let source = "\
skill a()
  rules:
    true -> do b()
skill b()
  rules:
    true -> done
";
        let reports = check_candidates(&core(), &one("pair.skill", source)).unwrap();
        assert!(reports[0].ok, "{:?}", reports[0].errors);
    }

    #[test]
    fn a_file_without_a_skill_is_no_skill() {
        let reports = check_candidates(&core(), &one("empty.skill", "# nothing\n")).unwrap();
        assert_eq!(kinds(&reports[0]), [CandidateErrorKind::NoSkill]);
        assert!(reports[0].skills.is_empty());
    }

    #[test]
    fn core_may_still_compose_to_its_own_limit() {
        let source = "\
skill c1()
  rules:
    true -> do c2()
skill c2()
  rules:
    true -> do c3()
skill c3()
  rules:
    true -> do c4()
skill c4()
  rules:
    true -> done
";
        let candidate = "skill leaf()\n  rules:\n    true -> done\n";
        let reports =
            check_candidates(&one("core.skill", source), &one("leaf.skill", candidate)).unwrap();
        assert!(reports[0].ok, "{:?}", reports[0].errors);

        // Two candidate frames on top of the full depth-4 core chain still pass:
        // core skills are leaves for the candidate cap.
        let chain = "\
skill top()
  rules:
    true -> do mid()
skill mid()
  rules:
    true -> do c1()
";
        let reports =
            check_candidates(&one("core.skill", source), &one("chain.skill", chain)).unwrap();
        assert!(reports[0].ok, "{:?}", reports[0].errors);
    }

    #[test]
    fn a_core_problem_is_a_load_error() {
        let error = check_candidates(
            &one(
                "core.skill",
                "skill core()\n  rules:\n    nope(x) -> done\n",
            ),
            &[],
        )
        .unwrap_err();
        assert!(error.contains("unknown fact `nope`"), "{error}");
    }
}
