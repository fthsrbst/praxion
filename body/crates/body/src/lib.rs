//! Praxion's Demo-0 body process: the one binary that wires tee -> decoder ->
//! world -> perception -> reflexes/arbiter/interpreter -> primitives -> actuator
//! and plays goals. One `--goal` runs as command `cli`; with `--port` the brain
//! sends commands over the port and the process settles goal after goal.
//!
//! The wire is in [`driver`]; the counters and `summary.json` in [`summary`];
//! paths and version pins in this module; the session loop in `main.rs`.

pub mod cli;
pub mod driver;
pub mod summary;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use praxion_rules::{
    CORE_COMPOSITION_DEPTH, Facts, PROGRAM_COMPOSITION_DEPTH, Primitives, Program, Skill,
    check_candidates, freeze_with_depth, parse,
};

pub use cli::{Args, CheckArgs, resolve_goal_args};
pub use driver::{BodyDriver, BodyOptions};
pub use praxion_eventlog::Versions;

/// Walk up from the current directory, then from the executable, until a
/// directory holds `versions/manifest.toml`.
pub fn find_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(current) = std::env::current_dir() {
        starts.push(current);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        starts.push(dir.to_path_buf());
    }
    for start in starts {
        let mut dir = Some(start.as_path());
        while let Some(current) = dir {
            if current.join("versions").join("manifest.toml").is_file() {
                return Some(current.to_path_buf());
            }
            dir = current.parent();
        }
    }
    None
}

/// The hand-written core skills: `body/skills`, or `skills` in an installed
/// layout.
pub fn skills_dir(root: &Path) -> PathBuf {
    let body = root.join("body").join("skills");
    if body.is_dir() {
        body
    } else {
        root.join("skills")
    }
}

/// The reflex settings: `body/config/reflexes.toml`, or `config/...`.
pub fn settings_path(root: &Path) -> PathBuf {
    let body = root.join("body").join("config").join("reflexes.toml");
    if body.is_file() {
        body
    } else {
        root.join("config").join("reflexes.toml")
    }
}

/// Read every `.skill` file in a directory, sorted by file name. An empty
/// directory is allowed; callers that require a file check the result.
pub fn read_skill_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "skill")
        {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill")
                .to_owned();
            let source = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            files.push((name, source));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Read every `.skill` file in a directory, sorted by file name. The core
/// skills must not be empty.
pub fn load_skill_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let files = read_skill_files(dir)?;
    if files.is_empty() {
        return Err(format!("no .skill files in {}", dir.display()));
    }
    Ok(files)
}

/// A program-load rejection. A candidate that shadows a core skill is its own
/// case so the binary can exit with 2 and a clear message.
#[derive(Debug)]
pub enum ProgramLoadError {
    NameCollision(String),
    /// A candidate failed `check_candidates`; playing would see a different
    /// program from `check-skills`.
    Candidate(String),
    Invalid(String),
}

impl fmt::Display for ProgramLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameCollision(message) | Self::Candidate(message) | Self::Invalid(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for ProgramLoadError {}

/// Parse and freeze the core skills plus optional candidate files as one
/// program. Candidates may call core skills but may not redefine one.
pub fn load_program(
    core_files: &[(String, String)],
    candidate_files: &[(String, String)],
) -> Result<Program, ProgramLoadError> {
    // Play mode must accept exactly the candidates `check-skills` accepts:
    // the same admission checks run before a body is started.
    if !candidate_files.is_empty() {
        let reports =
            check_candidates(core_files, candidate_files).map_err(ProgramLoadError::Invalid)?;
        let errors: Vec<String> = reports
            .iter()
            .filter(|report| !report.ok)
            .flat_map(|report| {
                report.errors.iter().map(move |error| {
                    format!(
                        "{}:{}:{}: {}",
                        report.file, error.line, error.col, error.message
                    )
                })
            })
            .collect();
        if !errors.is_empty() {
            return Err(ProgramLoadError::Candidate(errors.join("; ")));
        }
    }
    let mut skills = Vec::new();
    let mut core_names = BTreeSet::new();
    for (file, source) in core_files {
        let mut parsed = parse(source).map_err(|error| {
            ProgramLoadError::Invalid(format!(
                "{file}:{}:{}: {}",
                error.line, error.col, error.message
            ))
        })?;
        for skill in &mut parsed {
            skill.file = Some(file.clone());
        }
        core_names.extend(parsed.iter().map(|skill: &Skill| skill.name.clone()));
        skills.extend(parsed);
    }
    for (file, source) in candidate_files {
        let mut parsed = parse(source).map_err(|error| {
            ProgramLoadError::Invalid(format!(
                "{file}:{}:{}: {}",
                error.line, error.col, error.message
            ))
        })?;
        for skill in &mut parsed {
            skill.file = Some(file.clone());
            if core_names.contains(&skill.name) {
                return Err(ProgramLoadError::NameCollision(format!(
                    "{file}:{}:{}: candidate skill `{}` collides with a core skill",
                    skill.span.start.line, skill.span.start.col, skill.name
                )));
            }
        }
        skills.extend(parsed);
    }
    // Candidates were capped by `check-skills`; the program-wide cap only has
    // to admit a candidate chain on top of a full core chain.
    let depth = if candidate_files.is_empty() {
        CORE_COMPOSITION_DEPTH
    } else {
        PROGRAM_COMPOSITION_DEPTH
    };
    freeze_with_depth(skills, Facts::standard(), Primitives::standard(), depth).map_err(|errors| {
        ProgramLoadError::Invalid(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })
}

/// The git commit the body was built from, for `run_started`.
pub fn git_commit(root: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &str = "\
skill gather()
  rules:
    true -> done
";

    fn core() -> Vec<(String, String)> {
        vec![("core.skill".to_owned(), CORE.to_owned())]
    }

    fn candidate(name: &str, source: &str) -> Vec<(String, String)> {
        vec![(name.to_owned(), source.to_owned())]
    }

    #[test]
    fn a_valid_candidate_loads() {
        let program = load_program(
            &core(),
            &candidate(
                "make.skill",
                "skill make()\n  rules:\n    true -> do gather()\n",
            ),
        )
        .expect("a candidate that check-skills accepts must start");
        assert!(program.skill("make").is_some());
    }

    #[test]
    fn an_invalid_candidate_is_refused_before_start() {
        // The same admission check `check-skills` runs rejects the file, so
        // play mode cannot start a program the checker would report as broken.
        let error = load_program(
            &core(),
            &candidate(
                "bad.skill",
                "skill bad()\n  rules:\n    nope(stick) -> done\n",
            ),
        )
        .expect_err("an invalid candidate must not start");
        let ProgramLoadError::Candidate(message) = error else {
            panic!("an invalid candidate is a candidate rejection, got {error:?}");
        };
        assert!(message.contains("bad.skill"), "{message}");
        assert!(message.contains("unknown fact `nope`"), "{message}");
    }

    #[test]
    fn a_candidate_that_shadows_a_core_skill_is_refused_before_start() {
        let error = load_program(
            &core(),
            &candidate(
                "shadow.skill",
                "skill gather()\n  rules:\n    true -> done\n",
            ),
        )
        .expect_err("a shadowing candidate must not start");
        let ProgramLoadError::Candidate(message) = error else {
            panic!("a colliding candidate is a candidate rejection, got {error:?}");
        };
        assert!(message.contains("already a core skill"), "{message}");
    }
}
