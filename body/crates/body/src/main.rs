//! `praxion-body`: run one hand-written goal live through the forwarder.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use praxion_body::{
    Args, BodyDriver, BodyOptions, Versions, find_root, git_commit, load_skill_files,
    resolve_goal_args, settings_path, skills_dir, summary,
};
use praxion_body::summary::{SessionSummary, Shared};
use praxion_headless::{ConnectOptions, DriverHandle, Event, Session};
use praxion_manifest::Manifest;
use praxion_reflexes::Settings;
use praxion_rules::CORE_COMPOSITION_DEPTH;

const POLL: Duration = Duration::from_millis(50);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    // `--verbose` shows the debug diagnostics (packet traces); RUST_LOG overrides.
    let level = if std::env::args().any(|a| a == "--verbose") { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    match run() {
        Ok(code) => code,
        Err(message) => {
            log::error!("praxion-body: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let args = Args::parse(std::env::args().skip(1))?;
    interrupt::install();

    let root = find_root()
        .ok_or("cannot find the repository root (versions/manifest.toml)")?;
    let skills = args.skills.clone().unwrap_or_else(|| skills_dir(&root));
    let files = load_skill_files(&skills)?;
    let program = praxion_rules::load_files_with_depth(&files, CORE_COMPOSITION_DEPTH)
        .map_err(|errors| {
            errors
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
    let goal_args = resolve_goal_args(&program, &args.goal, &args.goal_args)?;

    let settings_file = args.settings.clone().unwrap_or_else(|| settings_path(&root));
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
        args.goal.clone(),
        args.goal_args.clone(),
    )));

    let driver = BodyDriver::new(
        BodyOptions {
            skill: args.goal.clone(),
            args: goal_args,
            start_delay_ticks: args.start_delay_ticks,
            look: praxion_planner::LookStyle::default(),
            versions,
        },
        args.run_dir.clone(),
        program,
        settings,
        Arc::clone(&shared),
    )
    .map_err(|error| format!("cannot start the event log: {error}"))?;
    let handle: DriverHandle = Arc::new(Mutex::new(Box::new(driver)));

    let options = ConnectOptions {
        auto_respawn: true,
        tune_loop: true,
        trace_packets: args.verbose,
        script: None,
        driver: Some(Arc::clone(&handle)),
        tee: Some(args.tee),
    };
    let session = Session::connect_with(args.forwarder, &args.name, options)
        .map_err(|error| format!("cannot connect through the forwarder: {error}"))?;
    eprintln!(
        "praxion-body: connected to {} through tee {}, goal {}",
        args.forwarder, args.tee, args.goal
    );

    let linger = Duration::from_millis(args.linger_ticks.saturating_mul(50));
    let mut settled_at: Option<Instant> = None;
    loop {
        let done = handle
            .lock()
            .map(|driver| driver.done())
            .unwrap_or(true);
        let ticks = lock(&shared).ticks;

        if lock(&shared).outcome.is_none() {
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
            break;
        }
        if settled_at.is_some_and(|started| started.elapsed() >= linger) {
            break;
        }

        match session.recv_event_timeout(POLL) {
            Some(Event::Joined) => eprintln!("joined"),
            Some(Event::Died) => {
                {
                    let mut shared = lock(&shared);
                    shared.deaths = shared.deaths.saturating_add(1);
                }
                eprintln!("died");
            }
            Some(Event::Disconnected(reason)) => {
                eprintln!("disconnected: {reason}");
                break;
            }
            _ => {}
        }
    }

    {
        let stats = session.stats();
        let world = stats.world_summary();
        lock(&shared).session = Some(SessionSummary {
            ticks_ran: stats.ticks_ran.load(std::sync::atomic::Ordering::Relaxed),
            physics_ticks: stats.physics_ticks.load(std::sync::atomic::Ordering::Relaxed),
            frames: stats.frames.load(std::sync::atomic::Ordering::Relaxed),
            frames_dropped: stats.frames_dropped.load(std::sync::atomic::Ordering::Relaxed),
            corrections: stats.corrections(),
            attack_clicks: stats.attack_clicks.load(std::sync::atomic::Ordering::Relaxed),
            use_clicks: stats.use_clicks.load(std::sync::atomic::Ordering::Relaxed),
            world,
        });
    }

    session.disconnect();
    let _ = session.recv_event_timeout(DISCONNECT_TIMEOUT);
    let outcome = lock(&shared).outcome.clone().unwrap_or_else(|| "failed(no_outcome)".to_owned());
    write_summary(&args, &shared);

    let text = serde_json::to_string_pretty(&summary::summary(&lock(&shared)))
        .unwrap_or_else(|_| "{}".to_owned());
    println!("{text}");

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
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
