//! Praxion's Demo-0 body process: the one binary that wires tee -> decoder ->
//! world -> perception -> reflexes/arbiter/interpreter -> primitives -> actuator
//! and plays one goal.
//!
//! The wire is in [`driver`]; the counters and `summary.json` in [`summary`];
//! paths and version pins in this module; the session loop in `main.rs`.

pub mod cli;
pub mod driver;
pub mod summary;

use std::path::{Path, PathBuf};

pub use cli::{Args, resolve_goal_args};
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
    if body.is_dir() { body } else { root.join("skills") }
}

/// The reflex settings: `body/config/reflexes.toml`, or `config/...`.
pub fn settings_path(root: &Path) -> PathBuf {
    let body = root.join("body").join("config").join("reflexes.toml");
    if body.is_file() { body } else { root.join("config").join("reflexes.toml") }
}

/// Read every `.skill` file in a directory, sorted by file name.
pub fn load_skill_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "skill") {
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
    if files.is_empty() {
        return Err(format!("no .skill files in {}", dir.display()));
    }
    Ok(files)
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
