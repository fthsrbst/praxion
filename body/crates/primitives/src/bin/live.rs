//! `praxion-live`: run one primitive or one core skill through the headless
//! body, perception, the primitive runner and the receipts engine.
//!
//! ```text
//! praxion-live --server 127.0.0.1:PORT --tee 127.0.0.1:TEE \
//!              --run-dir runs/live/mine-log --primitive mine --arg oak_log
//! praxion-live ... --run-dir runs/live/pickaxe --skill get_wooden_pickaxe \
//!              --skills body/skills
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use praxion_headless::{ConnectOptions, Event, Session};
use praxion_primitives::{LiveDriver, LiveMode, LiveOptions};

const POLL: Duration = Duration::from_millis(50);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("praxion-live: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(std::env::args().skip(1))?;
    let mode = match (&args.primitive, &args.skill) {
        (Some(name), None) => LiveMode::Primitive {
            name: name.clone(),
            arg: args.arg.clone(),
        },
        (None, Some(name)) => LiveMode::Skill { name: name.clone() },
        _ => return Err("pass exactly one of --primitive or --skill".to_owned()),
    };
    let program = match &mode {
        LiveMode::Skill { .. } => {
            let dir = args.skills.clone().unwrap_or_else(|| PathBuf::from("skills"));
            let files = load_skill_files(&dir)?;
            Some(
                praxion_rules::load_files_with_depth(
                    &files,
                    praxion_rules::CORE_COMPOSITION_DEPTH,
                )
                .map_err(|errors| {
                    errors
                        .iter()
                        .map(|error| error.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                })?,
            )
        }
        LiveMode::Primitive { .. } => None,
    };

    std::fs::create_dir_all(&args.run_dir).map_err(|error| error.to_string())?;
    let events = args.run_dir.join("events.jsonl");
    if events.exists() {
        std::fs::remove_file(&events).map_err(|error| error.to_string())?;
    }
    let driver = LiveDriver::new(
        LiveOptions {
            mode,
            run_dir: args.run_dir.clone(),
            look: praxion_planner::LookStyle::default(),
            start_delay_ticks: args.start_delay_ticks,
        },
        program,
    )
    .map_err(|error| error.to_string())?;
    let handle: praxion_headless::DriverHandle = Arc::new(Mutex::new(Box::new(driver)));

    let options = ConnectOptions {
        auto_respawn: true,
        tune_loop: true,
        trace_packets: args.verbose,
        script: None,
        driver: Some(Arc::clone(&handle)),
        tee: Some(args.tee),
    };
    let session = Session::connect_with(args.server, &args.name, options)
        .map_err(|error| error.to_string())?;
    let deadline = args.seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));

    let mut joined = false;
    let reason = loop {
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            break None;
        }
        let finished = handle
            .lock()
            .map(|driver| driver.done())
            .unwrap_or(true);
        if finished {
            break None;
        }
        match session.recv_event_timeout(POLL) {
            Some(Event::Joined) if !joined => {
                joined = true;
                eprintln!("joined");
            }
            Some(Event::Died) => eprintln!("died"),
            Some(Event::Disconnected(reason)) => break Some(reason),
            _ => {}
        }
    };
    let reason = match reason {
        Some(reason) => reason,
        None => {
            let finished = handle
                .lock()
                .map(|driver| driver.done())
                .unwrap_or(false);
            if finished {
                // Keep the body in the world while the harness reads the
                // referee's view of the result.
                std::thread::sleep(Duration::from_secs(3));
            }
            session.disconnect();
            match session.recv_event_timeout(STOP_TIMEOUT) {
                Some(Event::Disconnected(reason)) => reason,
                _ => "the session did not report a reason".to_owned(),
            }
        }
    };

    let summary = handle
        .lock()
        .ok()
        .and_then(|driver| driver.summary_json())
        .unwrap_or_else(|| "{}".to_owned());
    println!("disconnected: {reason}");
    println!("{summary}");
    Ok(())
}

fn load_skill_files(dir: &PathBuf) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
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

struct Args {
    server: SocketAddr,
    tee: SocketAddr,
    name: String,
    run_dir: PathBuf,
    primitive: Option<String>,
    arg: Option<String>,
    skill: Option<String>,
    skills: Option<PathBuf>,
    seconds: Option<u64>,
    start_delay_ticks: u64,
    verbose: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut server = None;
        let mut tee = None;
        let mut name = "Bot1".to_owned();
        let mut run_dir = None;
        let mut primitive = None;
        let mut arg = None;
        let mut skill = None;
        let mut skills = None;
        let mut seconds = None;
        let mut start_delay_ticks = 0;
        let mut verbose = false;
        let mut args = args;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--server" => server = Some(Self::address(&mut args, "--server")?),
                "--tee" => tee = Some(Self::address(&mut args, "--tee")?),
                "--name" => name = Self::value(&mut args, "--name")?,
                "--run-dir" => run_dir = Some(PathBuf::from(Self::value(&mut args, "--run-dir")?)),
                "--primitive" => primitive = Some(Self::value(&mut args, "--primitive")?),
                "--arg" => arg = Some(Self::value(&mut args, "--arg")?),
                "--skill" => skill = Some(Self::value(&mut args, "--skill")?),
                "--skills" => skills = Some(PathBuf::from(Self::value(&mut args, "--skills")?)),
                "--seconds" => {
                    let value = Self::value(&mut args, "--seconds")?;
                    seconds = Some(
                        value
                            .parse()
                            .map_err(|_| format!("--seconds wants a number, got {value:?}"))?,
                    );
                }
                "--start-delay-ticks" => {
                    let value = Self::value(&mut args, "--start-delay-ticks")?;
                    start_delay_ticks = value
                        .parse()
                        .map_err(|_| format!("--start-delay-ticks wants a number, got {value:?}"))?;
                }
                "--verbose" => verbose = true,
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(Self {
            server: server.ok_or("--server is required")?,
            tee: tee.ok_or("--tee is required")?,
            name,
            run_dir: run_dir.ok_or("--run-dir is required")?,
            primitive,
            arg,
            skill,
            skills,
            seconds,
            start_delay_ticks,
            verbose,
        })
    }

    fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("{flag} wants a value"))
    }

    fn address(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<SocketAddr, String> {
        let value = Self::value(args, flag)?;
        value
            .parse()
            .map_err(|_| format!("{flag} wants 127.0.0.1:PORT, got {value:?}"))
    }
}
