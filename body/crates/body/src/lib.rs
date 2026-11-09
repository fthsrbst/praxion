//! Praxion's Demo-0 body process: the one binary that wires tee -> decoder ->
//! world -> perception -> reflexes/arbiter/interpreter -> primitives -> actuator
//! and plays goals. One `--goal` runs as command `cli`; with `--port` the brain
//! sends commands over the port and the process settles goal after goal.
//!
//! The wire is in [`driver`]; the counters and `summary.json` in [`summary`];
//! paths and version pins in this module; the session loop in `main.rs`.

pub mod chat;
pub mod cli;
pub mod driver;
pub mod summary;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// Every `.skill` file in `dir` as (file name, path), sorted by file name.
fn skill_paths(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension == "skill")
        {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("skill")
            .to_owned();
        paths.push((name, path));
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(paths)
}

/// Read every `.skill` file in a directory, sorted by file name. An empty
/// directory is allowed; callers that require a file check the result.
pub fn read_skill_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    for (name, path) in skill_paths(dir)? {
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        files.push((name, source));
    }
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

/// The program a body starts with: the core skills plus the watched
/// directory's candidates. A port session (`keep_going`) that cannot read or
/// load the candidate side starts on the core-only program and returns the
/// failure text for a `skills_reload_failed` note: a stale file left in the
/// watched directory must not keep every session from booting, and the watcher
/// retries when that file changes. A `--goal` run keeps its exit-2 contract.
pub fn load_startup_program(
    core_dir: &Path,
    candidates_dir: Option<&Path>,
    keep_going: bool,
) -> Result<(Program, Option<String>), ProgramLoadError> {
    let core_files = load_skill_files(core_dir).map_err(ProgramLoadError::Invalid)?;
    let candidate_files = match candidates_dir {
        Some(dir) => match read_skill_files(dir) {
            Ok(files) => files,
            Err(error) if keep_going => return degrade_to_core(&core_files, error),
            Err(error) => return Err(ProgramLoadError::Invalid(error)),
        },
        None => Vec::new(),
    };
    match load_program(&core_files, &candidate_files) {
        Ok(program) => Ok((program, None)),
        Err(error) if keep_going && !candidate_files.is_empty() => {
            degrade_to_core(&core_files, error.to_string())
        }
        Err(error) => Err(error),
    }
}

/// The core-only program alongside the candidate failure that forced it. A
/// broken core still fails: the fallback removes the candidate side only.
fn degrade_to_core(
    core_files: &[(String, String)],
    failure: String,
) -> Result<(Program, Option<String>), ProgramLoadError> {
    let program = load_program(core_files, &[])?;
    Ok((program, Some(failure)))
}

/// One `.skill` file as the watcher sees it: file name, byte length and
/// modification time. A changed list is what makes a port session reload the
/// program; the size and the mtime catch an edit in place.
pub type SkillStamp = Vec<(String, u64, Option<SystemTime>)>;

/// The names, lengths and mtimes of every `.skill` file in `dir`, sorted by
/// file name so two stamps compare equal for the same directory state.
pub fn skill_stamp(dir: &Path) -> Result<SkillStamp, String> {
    let mut stamp = Vec::new();
    for (name, path) in skill_paths(dir)? {
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        stamp.push((name, metadata.len(), metadata.modified().ok()));
    }
    Ok(stamp)
}

/// A port session's watched skill directory: the core skills always, plus
/// every `.skill` in `dir` (the brain's admitted skills). The body reloads the
/// program when `dir`'s files change, so a skill admitted after the process
/// started is playable by the next command.
pub struct SkillsWatch {
    core: PathBuf,
    dir: PathBuf,
    stamp: SkillStamp,
    /// The errors of the last failed reload, so the same failure is reported
    /// once instead of on every accepted command.
    last_error: Option<Vec<String>>,
}

/// A reloaded program and the skills that came from the watched directory.
pub struct ReloadedSkills {
    pub program: Program,
    pub names: Vec<String>,
}

impl SkillsWatch {
    /// Take the directory's stamp before the caller reads the files, so a
    /// skill written while the body starts is reloaded on the first command
    /// rather than missed.
    pub fn new(core: PathBuf, dir: PathBuf) -> Result<Self, String> {
        let stamp = skill_stamp(&dir)?;
        Ok(Self {
            core,
            dir,
            stamp,
            last_error: None,
        })
    }

    /// Reload core plus the watched directory only when its files changed.
    ///
    /// `Ok(None)` means "nothing new": the files are unchanged, or the same
    /// failure as last time (already in the log). The stamp moves only after a
    /// successful load, so a failed read or load is retried on the next
    /// command rather than waiting for a change that may never come.
    pub fn reload(&mut self) -> Result<Option<ReloadedSkills>, Vec<String>> {
        match self.attempt() {
            Ok(reloaded) => {
                self.last_error = None;
                Ok(reloaded)
            }
            Err(errors) => {
                if self.last_error.as_ref() == Some(&errors) {
                    Ok(None)
                } else {
                    self.last_error = Some(errors.clone());
                    Err(errors)
                }
            }
        }
    }

    fn attempt(&mut self) -> Result<Option<ReloadedSkills>, Vec<String>> {
        let stamp = skill_stamp(&self.dir).map_err(|error| vec![error])?;
        if stamp == self.stamp {
            return Ok(None);
        }
        let core = load_skill_files(&self.core).map_err(|error| vec![error])?;
        let candidates = read_skill_files(&self.dir).map_err(|error| vec![error])?;
        let files: BTreeSet<String> = candidates
            .iter()
            .map(|(file, _)| file.clone())
            .collect();
        let program = load_program(&core, &candidates).map_err(|error| vec![error.to_string()])?;
        let names = program
            .skills()
            .filter(|skill| {
                skill
                    .file
                    .as_deref()
                    .is_some_and(|file| files.contains(file))
            })
            .map(|skill| skill.name.clone())
            .collect();
        self.stamp = stamp;
        Ok(Some(ReloadedSkills { program, names }))
    }
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
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const CORE: &str = "\
skill gather()
  rules:
    true -> done
";

    /// A scratch directory under `target/test-runs`, as the driver tests use,
    /// so the startup tests read real files and never write outside the repo.
    fn scratch_dir(name: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-runs");
        let dir = base.join(format!(
            "lib-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch dir");
        dir
    }

    fn dirs(name: &str, candidate: &[u8]) -> (PathBuf, PathBuf) {
        let root = scratch_dir(name);
        let core = root.join("core");
        let active = root.join("active");
        std::fs::create_dir_all(&core).expect("the core dir");
        std::fs::create_dir_all(&active).expect("the watched dir");
        std::fs::write(core.join("core.skill"), CORE).expect("the core skill");
        std::fs::write(active.join("bad.skill"), candidate).expect("the candidate");
        (core, active)
    }

    #[test]
    fn a_broken_candidate_does_not_stop_a_port_start() {
        let (core_dir, active) = dirs(
            "degrade",
            b"skill bad()\n  rules:\n    nope(stick) -> done\n",
        );

        let (program, failure) = load_startup_program(&core_dir, Some(&active), true)
            .expect("a port session boots on the core-only program");
        assert!(program.skill("gather").is_some());
        assert!(
            program.skill("bad").is_none(),
            "the rejected candidate is not in the program"
        );
        let failure = failure.expect("the failure is reported for the note");
        assert!(failure.contains("bad.skill"), "{failure}");
        assert!(failure.contains("unknown fact `nope`"), "{failure}");
    }

    #[test]
    fn a_broken_candidate_still_stops_a_cli_goal() {
        let (core_dir, active) = dirs(
            "cli-exit-2",
            b"skill bad()\n  rules:\n    nope(stick) -> done\n",
        );

        let error = load_startup_program(&core_dir, Some(&active), false)
            .expect_err("a CLI run must not start a program check-skills rejects");
        let ProgramLoadError::Candidate(message) = error else {
            panic!("an invalid candidate is a candidate rejection, got {error:?}");
        };
        assert!(message.contains("bad.skill"), "{message}");
    }

    #[test]
    fn an_unreadable_candidate_degrades_a_port_start_but_not_a_cli_goal() {
        // A leftover file that is not text: the watcher could never load it,
        // so the port session must boot without the candidate side.
        let (core_dir, active) = dirs("unreadable", &[0xff, 0xfe, 0x00]);

        let (program, failure) = load_startup_program(&core_dir, Some(&active), true)
            .expect("an unreadable candidate must not stop a port session");
        assert!(program.skill("gather").is_some());
        assert!(failure.expect("the failure is reported").contains("bad.skill"));

        let error = load_startup_program(&core_dir, Some(&active), false)
            .expect_err("a CLI run still refuses an unreadable candidate");
        assert!(matches!(error, ProgramLoadError::Invalid(_)), "{error:?}");
    }

    #[test]
    fn a_broken_core_still_stops_a_port_start() {
        let root = scratch_dir("broken-core");
        let core = root.join("core");
        let active = root.join("active");
        std::fs::create_dir_all(&core).expect("the core dir");
        std::fs::create_dir_all(&active).expect("the watched dir");

        let error = load_startup_program(&core, Some(&active), true)
            .expect_err("the fallback removes the candidate side only");
        assert!(matches!(error, ProgramLoadError::Invalid(_)), "{error:?}");
    }

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
