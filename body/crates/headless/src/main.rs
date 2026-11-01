//! `praxion-headless`: log in, print `joined`, then stay until stopped.
//!
//! ```text
//! praxion-headless --server 127.0.0.1:PORT [--name Bot1] [--seconds N]
//!                  [--auto-respawn] [--verbose]
//! ```
//!
//! Prints `joined` once the server has us in play state, `died` on death, and
//! `disconnected: <reason>` when the session ends. Stops on `--seconds`, on
//! Ctrl+C (Windows) or on a server disconnect.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use praxion_headless::{ConnectOptions, Event, Session};

/// How long a stop waits for the session to report its disconnect reason.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the event loop looks at the clock and the interrupt flag.
const POLL: Duration = Duration::from_millis(50);

const USAGE: &str = "usage: praxion-headless --server 127.0.0.1:PORT [--name Bot1] \
                     [--seconds N] [--auto-respawn] [--verbose]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("praxion-headless: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = Args::parse(args)?;
    let options = ConnectOptions {
        auto_respawn: args.auto_respawn,
        tune_loop: true,
        trace_packets: args.verbose,
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
        match session.recv_event_timeout(POLL) {
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
    Ok(())
}

struct Args {
    server: SocketAddr,
    name: String,
    seconds: Option<u64>,
    auto_respawn: bool,
    verbose: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut server = None;
        let mut name = "Bot1".to_owned();
        let mut seconds = None;
        let mut auto_respawn = false;
        let mut verbose = false;

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
