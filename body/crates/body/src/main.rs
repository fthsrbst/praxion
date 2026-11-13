//! `praxion-body`: run one hand-written goal live through the forwarder.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use praxion_body::summary::{SessionSummary, Shared};
use praxion_body::{
    Args, BlueprintCheckArgs, BodyDriver, BodyOptions, CheckArgs, ExportArgs, ProgramLoadError,
    SkillsWatch, Versions, find_root, git_commit, load_skill_files, load_startup_program,
    read_skill_files, resolve_goal_args, settings_path, skills_dir, summary,
};
use praxion_blueprint::{Limits, parse, validate};
use praxion_headless::{ConnectOptions, DriverHandle, Event, Host, Session, ShadowSession, TICK};
use praxion_manifest::Manifest;
use praxion_port::{PortConfig, PortServer};
use praxion_reflexes::Settings;
use praxion_rules::check_candidates;
use praxion_world::BlockPos;

const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A blueprint is a small text file (the design's 64 KiB bound); anything
/// larger is not a design and is refused before it is parsed.
const MAX_BLUEPRINT_BYTES: usize = 64 * 1024;

fn main() -> ExitCode {
    if std::env::args().skip(1).any(|arg| arg == "--help" || arg == "-h") {
        print!("{}", praxion_body::cli::USAGE);
        return ExitCode::SUCCESS;
    }
    // `--verbose` shows the debug diagnostics (packet traces); RUST_LOG overrides.
    let level = if std::env::args().any(|a| a == "--verbose") {
        "debug"
    } else {
        "info"
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    let mut args = std::env::args().skip(1);
    // One token decides the subcommand; `run` reads the arguments again, so
    // consuming it here is harmless. Matching both names against separate
    // `next()` calls would swallow the first flag of the second subcommand.
    match args.next().as_deref() {
        Some("check-skills") => {
            return match run_check_skills(args) {
                Ok(code) => code,
                Err(message) => {
                    eprintln!("praxion-body: {message}");
                    ExitCode::from(2)
                }
            };
        }
        Some("check-blueprint") => {
            return match run_check_blueprint(args) {
                Ok(code) => code,
                Err(message) => {
                    eprintln!("praxion-body: {message}");
                    ExitCode::from(2)
                }
            };
        }
        Some("export-gamedata") => {
            return match run_export_gamedata(args) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("praxion-body: {message}");
                    ExitCode::from(2)
                }
            };
        }
        _ => {}
    }
    match run() {
        Ok(code) => code,
        Err(message) => {
            log::error!("praxion-body: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The core skills directory for `check-skills`: an explicit `--skills` path
/// needs no repository root; only the default is derived from it.
fn check_skills_dir(skills: Option<&std::path::Path>) -> Result<std::path::PathBuf, String> {
    match skills {
        Some(dir) => Ok(dir.to_path_buf()),
        None => {
            let root =
                find_root().ok_or("cannot find the repository root (versions/manifest.toml)")?;
            Ok(skills_dir(&root))
        }
    }
}

/// `check-skills`: no server and no network; one JSON object on stdout.
fn run_check_skills<I: Iterator<Item = String>>(args: I) -> Result<ExitCode, String> {
    let args = CheckArgs::parse(args)?;
    let skills = check_skills_dir(args.skills.as_deref())?;
    let core = load_skill_files(&skills)?;
    let candidates = read_skill_files(&args.candidates)?;
    let reports = check_candidates(&core, &candidates)?;

    let ok = reports.iter().all(|report| report.ok);
    let candidate_json: Vec<_> = reports
        .iter()
        .map(|report| {
            let errors: Vec<_> = report
                .errors
                .iter()
                .map(|error| {
                    serde_json::json!({
                        "kind": error.kind.as_str(),
                        "message": &error.message,
                        "line": error.line,
                        "col": error.col,
                    })
                })
                .collect();
            serde_json::json!({
                "file": &report.file,
                "skills": &report.skills,
                "ok": report.ok,
                "errors": errors,
            })
        })
        .collect();
    let text = serde_json::to_string(&serde_json::json!({
        "ok": ok,
        "candidates": candidate_json,
    }))
    .map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `check-blueprint`: no server and no network; one JSON object on stdout and
/// exit 1 when the design is rejected, the same shape as `check-skills`. The
/// brain's repair loop reads `kind`, `message` and `cells`; the harness reads
/// `--emit-plan` so the referee never re-parses the format.
fn run_check_blueprint<I: Iterator<Item = String>>(args: I) -> Result<ExitCode, String> {
    let args = BlueprintCheckArgs::parse(args)?;
    let text = std::fs::read_to_string(&args.file)
        .map_err(|error| format!("cannot read {}: {error}", args.file.display()))?;
    if text.len() > MAX_BLUEPRINT_BYTES {
        return Err(format!(
            "{} is {} bytes; a blueprint is at most {MAX_BLUEPRINT_BYTES}",
            args.file.display(),
            text.len()
        ));
    }
    let name = args
        .file
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let limits = Limits {
        max_placements: args.max_placements.unwrap_or(Limits::SLICE0.max_placements),
        ..Limits::SLICE0
    };
    let anchor = BlockPos::new(args.anchor.0, args.anchor.1, args.anchor.2);

    let result = parse(&text).and_then(|blueprint| {
        validate(&blueprint, &limits, args.inventory.as_ref())
            .map(|warnings| (blueprint, warnings))
    });
    let (ok, verdict) = match result {
        Ok((blueprint, warnings)) => {
            let mut verdict = serde_json::json!({
                "ok": true,
                "file": args.file.display().to_string(),
                "blueprint": name,
                "anchor": [anchor.x, anchor.y, anchor.z],
                "size": [
                    blueprint.size().x,
                    blueprint.size().z,
                    blueprint.size().y,
                ],
                "placements": blueprint.materials().values().sum::<u32>(),
                "materials": blueprint.materials(),
                "warnings": warnings
                    .iter()
                    .map(|warning| serde_json::json!({
                        "kind": warning.kind,
                        "message": warning.message,
                    }))
                    .collect::<Vec<_>>(),
            });
            if args.emit_plan {
                verdict["plan"] = serde_json::json!(
                    blueprint
                        .placements(anchor)
                        .into_iter()
                        .map(|placement| serde_json::json!({
                            "cell": [placement.cell.0, placement.cell.1, placement.cell.2],
                            "pos": [placement.pos.x, placement.pos.y, placement.pos.z],
                            "block": placement.block,
                            "kind": placement.kind.as_str(),
                        }))
                        .collect::<Vec<_>>()
                );
            }
            (true, verdict)
        }
        Err(error) => {
            let mut verdict = serde_json::json!({
                "ok": false,
                "file": args.file.display().to_string(),
                "blueprint": name,
                "anchor": [anchor.x, anchor.y, anchor.z],
                "kind": error.kind.as_str(),
                "message": error.message,
                "cells": error
                    .cells
                    .iter()
                    .map(|cell| [
                        anchor.x + cell.x,
                        anchor.y + cell.y,
                        anchor.z + cell.z,
                    ])
                    .collect::<Vec<_>>(),
            });
            // The repair loop reads `kind` and the prose; a caller that has to
            // act on a shortfall (a gather goal, design §3.4) reads numbers.
            if let Some(shortfall) = &error.shortfall {
                verdict["block"] = serde_json::json!(shortfall.block);
                verdict["need"] = serde_json::json!(shortfall.need);
                verdict["have"] = serde_json::json!(shortfall.have);
            }
            (false, verdict)
        }
    };
    let text = serde_json::to_string(&verdict).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `export-gamedata`: no server and no network; writes the jar's recipes,
/// smelting recipes and item tags as JSON and prints the path.
fn run_export_gamedata<I: Iterator<Item = String>>(args: I) -> Result<(), String> {
    let args = ExportArgs::parse(args)?;
    let out = match args.out {
        Some(out) => out,
        None => {
            let root =
                find_root().ok_or("cannot find the repository root (versions/manifest.toml)")?;
            root.join("local").join("gamedata").join("26.3.json")
        }
    };
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&out, praxion_gamedata::export_json())
        .map_err(|error| format!("{}: {error}", out.display()))?;
    println!("{}", out.display());
    Ok(())
}

fn run() -> Result<ExitCode, String> {
    let args = Args::parse(std::env::args().skip(1))?;
    interrupt::install();

    let root = find_root().ok_or("cannot find the repository root (versions/manifest.toml)")?;
    let skills = args.skills.clone().unwrap_or_else(|| skills_dir(&root));
    // A port session watches `--candidates`: its files are the brain's admitted
    // skills, and one written after this process started must be playable. The
    // stamp is taken before the files are read, so a skill added during startup
    // is reloaded on the first command instead of missed.
    let watch = match (&args.candidates, args.port.is_some()) {
        (Some(dir), true) => Some(SkillsWatch::new(skills.clone(), dir.clone())?),
        _ => None,
    };
    // A port session must boot: a stale file left in the watched directory
    // degrades to the core-only program and is retried when it changes. A
    // `--goal` run keeps its exit-2 contract for a rejected candidate.
    let (program, startup_failure) = match load_startup_program(
        &skills,
        args.candidates.as_deref(),
        args.port.is_some(),
    ) {
        Ok(started) => started,
        Err(ProgramLoadError::NameCollision(message) | ProgramLoadError::Candidate(message)) => {
            log::error!("praxion-body: {message}");
            return Ok(ExitCode::from(2));
        }
        Err(error) => return Err(error.to_string()),
    };
    let goal_args = match &args.goal {
        Some(goal) => resolve_goal_args(&program, goal, &args.goal_args)?,
        None => Vec::new(),
    };

    let settings_file = args
        .settings
        .clone()
        .unwrap_or_else(|| settings_path(&root));
    let settings = Settings::load(&settings_file).map_err(|error| error.to_string())?;

    let manifest = Manifest::load(root.join("versions").join("manifest.toml"))
        .map_err(|error| error.to_string())?;
    let commit = git_commit(&root);
    let versions = Versions {
        praxion: format!("{} ({commit})", env!("CARGO_PKG_VERSION")),
        minecraft: manifest.minecraft.version.clone(),
        protocol: manifest.minecraft.protocol,
        azalea: manifest.azalea.commit.clone(),
        rust: manifest.rust.toolchain.clone(),
    };

    std::fs::create_dir_all(&args.run_dir).map_err(|error| error.to_string())?;
    let run_id = args
        .run_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run")
        .to_owned();
    let shared = Arc::new(Mutex::new(Shared::new(
        run_id,
        args.goal.clone().unwrap_or_default(),
        args.goal_args.clone(),
    )));

    // The hands: the helper, the client it posts to and that client's own
    // options. Parsed before the driver starts, so a non-default binding (or an
    // inverted mouse) refuses the run instead of pressing the wrong keys later.
    let hands = match (&args.hands, args.client_pid, &args.client_options) {
        (Some(helper), Some(client_pid), Some(options)) => {
            let text = std::fs::read_to_string(options)
                .map_err(|error| format!("cannot read {}: {error}", options.display()))?;
            let options = praxion_headless::ClientOptions::parse(&text)
                .map_err(|error| format!("{}: {error}", options.display()))?;
            Some(praxion_headless::HandsConfig {
                helper: helper.clone(),
                client_pid,
                activate: args.hands_activate,
                options,
                look_latency_ticks: args.hands_look_latency,
            })
        }
        _ => None,
    };
    let hands_mode = hands.is_some();

    // The body listens when the brain is in the loop (proposal §2.1). The
    // driver owns the event, snapshot and command channels; main keeps a
    // handle to send `bye shutdown` at exit.
    let port = match args.port {
        Some(address) => {
            let config = PortConfig {
                body: versions.praxion.clone(),
                run_id: lock(&shared).run_id.clone(),
            };
            let server = PortServer::bind(address, config)
                .map_err(|error| format!("cannot listen on {address}: {error}"))?;
            eprintln!("praxion-body: brain port on {}", server.local_addr());
            Some(Arc::new(server))
        }
        None => None,
    };

    let mut driver = BodyDriver::new(
        BodyOptions {
            skill: args.goal.clone(),
            args: goal_args,
            start_delay_ticks: args.start_delay_ticks,
            style_seed: args.style_seed,
            brain_silence: praxion_body::driver::DEFAULT_BRAIN_SILENCE,
            versions,
            own_name: args.name.clone(),
            operators: args.operators.clone(),
            chat_prefix: args.chat_prefix.clone(),
            // The client's own dig/place packet arrives a tick or two after the
            // frame in hands mode; a logged-in body reports the same tick.
            report_grace_ticks: if hands_mode { 8 } else { 0 },
        },
        args.run_dir.clone(),
        program,
        watch,
        settings,
        Arc::clone(&shared),
        port.clone(),
    )
    .map_err(|error| format!("cannot start the event log: {error}"))?;
    if let Some(failure) = startup_failure {
        // The session runs on the core-only program; the brain sees why and
        // the watcher retries when the file is fixed or removed.
        log::warn!("praxion-body: {failure}");
        driver.startup_reload_failed(&failure);
    }
    let handle: DriverHandle = Arc::new(Mutex::new(Box::new(driver)));

    // Two play modes on one event loop: a logged-in body of our own, or shadow
    // mode on the operator's client, where every frame is computed and dropped.
    let session: Box<dyn Host> = if args.real_client {
        if args.forwarder.is_some() {
            log::warn!("praxion-body: --forwarder is ignored with --real-client");
        }
        match hands {
            Some(config) => {
                let session =
                    ShadowSession::start_with_hands(args.tee, Arc::clone(&handle), config)
                        .map_err(|error| {
                            format!("cannot start the hands on tee {}: {error}", args.tee)
                        })?;
                eprintln!(
                    "praxion-body: hands mode on tee {}: frames are applied to the operator's client",
                    args.tee
                );
                Box::new(session)
            }
            None => {
                let session = ShadowSession::start(args.tee, Arc::clone(&handle))
                    .map_err(|error| format!("cannot attach to the tee at {}: {error}", args.tee))?;
                eprintln!(
                    "praxion-body: shadow mode on tee {}: frames are computed and dropped, nothing is sent",
                    args.tee
                );
                Box::new(session)
            }
        }
    } else {
        let forwarder = args
            .forwarder
            .ok_or("--forwarder is required without --real-client")?;
        let options = ConnectOptions {
            auto_respawn: true,
            tune_loop: true,
            trace_packets: args.verbose,
            script: None,
            driver: Some(Arc::clone(&handle)),
            tee: Some(args.tee),
        };
        let session = Session::connect_with(forwarder, &args.name, options)
            .map_err(|error| format!("cannot connect through the forwarder: {error}"))?;
        eprintln!(
            "praxion-body: connected to {forwarder} through tee {}, goal {}",
            args.tee,
            args.goal.as_deref().unwrap_or("(brain)")
        );
        Box::new(session)
    };

    let linger = Duration::from_millis(args.linger_ticks.saturating_mul(50));
    let mut settled_at: Option<Instant> = None;
    let port_mode = port.is_some();
    let mut exit = ExitCode::SUCCESS;
    loop {
        let done = handle.lock().map(|driver| driver.done()).unwrap_or(true);
        let ticks = lock(&shared).ticks;

        if port_mode {
            // A brain session has no single goal: it settles goal after goal.
            // The process exits on a signal, a disconnect, or the process-wide
            // tick cap (`--max-ticks`, a safety net, not a goal budget).
            if let Some(max) = args.max_ticks
                && ticks >= max
            {
                {
                    let mut shared = lock(&shared);
                    if shared.outcome.is_none() {
                        shared.outcome = Some("max_ticks".to_owned());
                    }
                }
                write_summary(&args, &shared);
                eprintln!("praxion-body: max ticks reached ({ticks})");
                exit = ExitCode::from(2);
                break;
            }
        } else if lock(&shared).outcome.is_none() {
            if done {
                // The driver settled the goal and wrote the first summary.
                settled_at = Some(Instant::now());
            } else if let Some(max) = args.max_ticks
                && ticks >= max
            {
                {
                    let mut shared = lock(&shared);
                    shared.outcome = Some("max_ticks".to_owned());
                }
                write_summary(&args, &shared);
                settled_at = Some(Instant::now());
                eprintln!("praxion-body: max ticks reached ({ticks})");
            }
        }

        if interrupt::requested() {
            {
                let mut shared = lock(&shared);
                if shared.outcome.is_none() {
                    shared.outcome = Some("interrupted".to_owned());
                }
            }
            if port_mode {
                exit = ExitCode::from(3);
            }
            break;
        }
        if !port_mode && settled_at.is_some_and(|started| started.elapsed() >= linger) {
            break;
        }

        match session.recv_event_timeout(TICK) {
            Some(Event::Joined) => eprintln!("joined"),
            Some(Event::Died { message }) => {
                {
                    let mut shared = lock(&shared);
                    shared.deaths = shared.deaths.saturating_add(1);
                }
                // The driver forgets the old life (reflex state, a meal, the
                // primitive in flight) and moves to a new generation.
                lock(&handle).on_death(message);
                eprintln!("died");
            }
            Some(Event::TakenOver { reason }) => {
                // The helper released every key and button; the frames are
                // dropped again. The body generation already moved on.
                lock(&handle).on_takeover(&reason);
                eprintln!("praxion-body: taken over ({reason}): frames are dropped again");
            }
            Some(Event::Disconnected(reason)) => {
                eprintln!("disconnected: {reason}");
                if port_mode {
                    // A brain session that loses the game is not a clean end:
                    // the caller (the harness or the supervisor) must see it.
                    exit = ExitCode::FAILURE;
                }
                break;
            }
            _ => {}
        }
    }

    {
        let stats = session.stats();
        let world = stats.world_summary();
        // The shadow counters exist only in real-client mode; the summary must
        // not claim the body was in shadow mode when it logged in instead.
        let shadow = args.real_client;
        lock(&shared).session = Some(SessionSummary {
            ticks_ran: stats.ticks_ran.load(std::sync::atomic::Ordering::Relaxed),
            physics_ticks: stats
                .physics_ticks
                .load(std::sync::atomic::Ordering::Relaxed),
            frames: stats.frames.load(std::sync::atomic::Ordering::Relaxed),
            frames_dropped: stats
                .frames_dropped
                .load(std::sync::atomic::Ordering::Relaxed),
            corrections: stats.corrections(),
            attack_clicks: stats
                .attack_clicks
                .load(std::sync::atomic::Ordering::Relaxed),
            use_clicks: stats.use_clicks.load(std::sync::atomic::Ordering::Relaxed),
            world,
            shadow_wanted: shadow.then(|| {
                stats
                    .shadow_wanted
                    .load(std::sync::atomic::Ordering::Relaxed)
            }),
            shadow: shadow.then_some(true),
            hands_frames: hands_mode
                .then(|| stats.hands_frames.load(std::sync::atomic::Ordering::Relaxed)),
            hands_refused: hands_mode
                .then(|| stats.hands_refused.load(std::sync::atomic::Ordering::Relaxed)),
            hands_unsupported: hands_mode
                .then(|| stats.hands_unsupported.load(std::sync::atomic::Ordering::Relaxed)),
            hands_takeovers: hands_mode
                .then(|| stats.hands_takeovers.load(std::sync::atomic::Ordering::Relaxed)),
        });
    }

    session.disconnect();
    let _ = session.recv_event_timeout(DISCONNECT_TIMEOUT);
    // A clean end tells the brain why the link is closing (proposal §2.2).
    if let Some(port) = &port {
        port.shutdown();
    }
    let outcome = lock(&shared)
        .outcome
        .clone()
        .unwrap_or_else(|| "failed(no_outcome)".to_owned());
    write_summary(&args, &shared);

    let text = serde_json::to_string_pretty(&summary::summary(&lock(&shared)))
        .unwrap_or_else(|_| "{}".to_owned());
    println!("{text}");

    if port_mode {
        return Ok(exit);
    }
    Ok(match outcome.as_str() {
        "done" => ExitCode::SUCCESS,
        "max_ticks" => ExitCode::from(2),
        "interrupted" => ExitCode::from(3),
        _ => ExitCode::FAILURE,
    })
}

fn write_summary(args: &Args, shared: &Arc<Mutex<Shared>>) {
    if let Err(error) = summary::write_summary(&args.run_dir, &lock(shared)) {
        log::warn!("praxion-body: cannot write summary.json: {error}");
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_skills_dir_does_not_need_the_repository_root() {
        // A given `--skills` path is used as is: `find_root` runs only for the
        // default. A check from a checkout without versions/manifest.toml
        // therefore still works when the caller names the directory.
        let dir = check_skills_dir(Some(std::path::Path::new("body/skills")))
            .expect("an explicit skills dir is enough");
        assert_eq!(dir, std::path::PathBuf::from("body/skills"));
    }
}

mod interrupt {
    #[cfg(windows)]
    use std::sync::atomic::{AtomicBool, Ordering};

    #[cfg(windows)]
    static REQUESTED: AtomicBool = AtomicBool::new(false);

    #[cfg(windows)]
    pub fn install() -> bool {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
    }

    #[cfg(windows)]
    unsafe extern "system" fn handler(_control: u32) -> i32 {
        REQUESTED.store(true, Ordering::SeqCst);
        1
    }

    #[cfg(windows)]
    pub fn requested() -> bool {
        REQUESTED.load(Ordering::SeqCst)
    }

    #[cfg(not(windows))]
    pub fn install() -> bool {
        false
    }

    #[cfg(not(windows))]
    pub fn requested() -> bool {
        false
    }
}
