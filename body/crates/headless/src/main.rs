//! `praxion-headless`: log in, play, print `joined`, then stay until stopped.
//!
//! ```text
//! praxion-headless --server 127.0.0.1:PORT [--name Bot1] [--seconds N]
//!                  [--auto-respawn] [--verbose]
//!                  [--tee 127.0.0.1:PORT] [--script walk.json]
//! ```
//!
//! `--tee` subscribes to the forwarder's tee and runs the decoder and world
//! in-process; without it there is no world and movement is refused. `--script`
//! drives the body from a JSON list of `{ticks, held, look}` steps.
//!
//! Prints `joined` once the server has us in play state, `died` on death, and
//! `disconnected: <reason>`, `stats:` and the spawn/final position when the
//! session ends. Stops on `--seconds`, on Ctrl+C (Windows) or on a server
//! disconnect.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use praxion_headless::{ConnectOptions, Event, Script, Session, TICK};

/// How long a stop waits for the session to report its disconnect reason.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

const USAGE: &str = "usage: praxion-headless --server 127.0.0.1:PORT [--name Bot1] \
                     [--seconds N] [--auto-respawn] [--verbose] \
                     [--tee 127.0.0.1:PORT] [--script walk.json]";

fn main() -> ExitCode {
    // `--verbose` shows the debug diagnostics (packet traces); RUST_LOG overrides.
    let level = if std::env::args().any(|a| a == "--verbose") { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            log::error!("praxion-headless: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = Args::parse(args)?;
    let script = match &args.script {
        None => None,
        Some(path) => {
            // Movement is simulated against the world; without a world there is
            // nothing to collide with, so a script that moves is refused here
            // rather than moving through everything.
            if args.tee.is_none() {
                return Err(format!(
                    "{} needs a world: pass --tee 127.0.0.1:PORT (the forwarder's tee)",
                    path.display()
                ));
            }
            let script = Script::load(path)?;
            eprintln!(
                "script: {} ticks in {} steps",
                script.total_ticks(),
                path.display()
            );
            Some(Arc::new(script))
        }
    };

    let options = ConnectOptions {
        auto_respawn: args.auto_respawn,
        tune_loop: true,
        trace_packets: args.verbose,
        script,
        driver: None,
        tee: args.tee,
    };

    interrupt::install();
    let session = Session::connect_with(args.server, &args.name, options)
        .map_err(|error| error.to_string())?;
    let deadline = args.seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));

    let reason = loop {
        if interrupt::requested() {
            break None;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break None;
        }
        match session.recv_event_timeout(TICK) {
            Some(Event::Joined) => println!("joined"),
            Some(Event::Died) => println!("died"),
            Some(Event::Disconnected(reason)) => break Some(reason),
            None => {}
        }
    };

    let reason = match reason {
        Some(reason) => reason,
        None => {
            session.disconnect();
            match session.recv_event_timeout(STOP_TIMEOUT) {
                Some(Event::Disconnected(reason)) => reason,
                _ => "the session did not report a reason".to_owned(),
            }
        }
    };
    println!("disconnected: {reason}");
    flush_stdout();

    let stats = session.stats();
    println!("stats:");
    println!("{}", indent(&stats.summary()));
    if let Some(world) = stats.world_summary() {
        println!("world: {world}");
    }
    if let Some(origin) = stats.origin() {
        println!("spawn: {:.6} {:.6} {:.6}", origin.x, origin.y, origin.z);
    }
    if let Some(position) = stats.position() {
        println!("position: {:.6} {:.6} {:.6}", position.x, position.y, position.z);
    }
    Ok(())
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flush stdout so a parent process reading the pipe sees the lines before the
/// process exits.
fn flush_stdout() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

struct Args {
    server: SocketAddr,
    name: String,
    seconds: Option<u64>,
    auto_respawn: bool,
    verbose: bool,
    tee: Option<SocketAddr>,
    script: Option<PathBuf>,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut server = None;
        let mut name = "Bot1".to_owned();
        let mut seconds = None;
        let mut auto_respawn = false;
        let mut verbose = false;
        let mut tee = None;
        let mut script = None;

        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => server = Some(Self::value(&mut args, "--server")?),
                "--name" => name = Self::value(&mut args, "--name")?,
                "--seconds" => {
                    let value = Self::value(&mut args, "--seconds")?;
                    seconds = Some(
                        value
                            .parse()
                            .map_err(|_| format!("--seconds wants a number, got {value:?}"))?,
                    );
                }
                "--tee" => tee = Some(Self::address(&mut args, "--tee")?),
                "--script" => {
                    script = Some(PathBuf::from(Self::value(&mut args, "--script")?));
                }
                "--auto-respawn" => auto_respawn = true,
                "--verbose" => verbose = true,
                "--help" | "-h" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
            }
        }

        let server = server.ok_or_else(|| format!("--server is required\n{USAGE}"))?;
        let server = server
            .parse()
            .map_err(|_| format!("--server wants 127.0.0.1:PORT, got {server:?}"))?;
        Ok(Self {
            server,
            name,
            seconds,
            auto_respawn,
            verbose,
            tee,
            script,
        })
    }

    /// The word after `flag`.
    fn value(
        args: &mut impl Iterator<Item = String>,
        flag: &str,
    ) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("{flag} wants a value\n{USAGE}"))
    }

    /// The address after `flag`.
    fn address(
        args: &mut impl Iterator<Item = String>,
        flag: &str,
    ) -> Result<SocketAddr, String> {
        let value = Self::value(args, flag)?;
        value
            .parse()
            .map_err(|_| format!("{flag} wants 127.0.0.1:PORT, got {value:?}"))
    }
}

/// Ctrl+C: ask the loop to stop instead of ending the process, so the socket is
/// closed and the reason printed. Windows only; elsewhere Ctrl+C ends the process.
#[cfg(windows)]
mod interrupt {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    static REQUESTED: AtomicBool = AtomicBool::new(false);

    pub(super) fn install() {
        // SAFETY: the handler only stores to an atomic, which is signal safe.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }

    pub(super) fn requested() -> bool {
        REQUESTED.load(Ordering::Relaxed)
    }

    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        REQUESTED.store(true, Ordering::SeqCst);
        1
    }
}

#[cfg(not(windows))]
mod interrupt {
    pub(super) fn install() {}

    pub(super) fn requested() -> bool {
        false
    }
}
