//! The parent side of the `praxion-hands` pipe.
//!
//! [`HandsLink`] starts the helper (`--serve`), waits for its `READY`, arms it
//! and then speaks the one-line-per-tick protocol:
//!
//! ```text
//! body  -> helper : ARM
//!                    F <tick> <generation> <keys> <dx> <dy> <buttons> <slot>
//! helper -> body   : OK <tick> | REFUSED <tick> <reason> | TAKEOVER <reason>
//!                    IDLE | BYE <reason>
//! ```
//!
//! The helper owns every OS input call; this module owns only the child process
//! and a reader thread. Dropping the link closes the helper's stdin, which makes
//! it release every key and button and exit.
//!
//! A process that finds a dead helper can still clear the input with
//! [`release_all`], which runs the helper's `--release-all` mode once.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::Error;

/// How long the helper may take to reach `READY` (it waits for the client to
/// become frontmost, so this must cover the operator's window switch).
const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the helper may take to release everything after its stdin closes.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// Between two looks at the child while shutting down.
const WAIT_STEP: Duration = Duration::from_millis(10);

/// One line the helper wrote that the body acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandsReply {
    /// The helper applied the frame with this tick number.
    Ok(u64),
    /// The helper refused the frame; the reason is its own word
    /// (`stale`, `not_armed`, `malformed`).
    Refused(u64, String),
    /// The operator took control (foreign input, focus lost, a disabled tap).
    Takeover(String),
    /// The helper released everything because no frame arrived while it held
    /// something, and stayed armed.
    Idle,
    /// The helper is leaving (its stdin closed, a signal). It released
    /// everything first.
    Bye(String),
    /// The helper process ended without a `BYE`.
    Exited,
}

/// A line as the reader thread sees it: `READY` is consumed by
/// [`HandsLink::spawn`], never handed to a frame loop.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Wire {
    Ready,
    Reply(HandsReply),
}

/// Parse one helper line. `None` is a line the body must ignore.
fn parse_line(line: &str) -> Option<Wire> {
    let line = line.trim();
    if line == "READY" {
        return Some(Wire::Ready);
    }
    if line == "IDLE" {
        return Some(Wire::Reply(HandsReply::Idle));
    }
    // A terminal line without its reason is still terminal: losing a takeover
    // because the helper was terse is the one failure the body cannot afford.
    if line == "TAKEOVER" {
        return Some(Wire::Reply(HandsReply::Takeover("unknown".to_owned())));
    }
    if line == "BYE" {
        return Some(Wire::Reply(HandsReply::Bye("unknown".to_owned())));
    }
    let (tag, rest) = line.split_once(' ')?;
    match tag {
        "OK" => Some(Wire::Reply(HandsReply::Ok(rest.trim().parse().ok()?))),
        "REFUSED" => {
            let (tick, reason) = rest.split_once(' ')?;
            Some(Wire::Reply(HandsReply::Refused(
                tick.parse().ok()?,
                reason.trim().to_owned(),
            )))
        }
        "TAKEOVER" => Some(Wire::Reply(HandsReply::Takeover(rest.trim().to_owned()))),
        "BYE" => Some(Wire::Reply(HandsReply::Bye(rest.trim().to_owned()))),
        _ => None,
    }
}

/// A running `praxion-hands --serve` child.
pub struct HandsLink {
    child: Child,
    /// Closed by dropping it: the helper then releases everything and exits.
    stdin: Option<ChildStdin>,
    replies: Receiver<Wire>,
    reader: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for HandsLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandsLink")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl HandsLink {
    /// Start `<helper> --serve --pid <pid> [--activate]`, wait for `READY` and
    /// send `ARM`.
    ///
    /// The wait is bounded by [`READY_TIMEOUT`]; a helper that refuses at
    /// start-up (`REFUSED 0 <reason>`) or exits before `READY` is reported as
    /// an error and the child is reaped.
    pub fn spawn(
        helper: impl AsRef<Path>,
        client_pid: u32,
        activate: bool,
    ) -> Result<Self, Error> {
        let mut command = Command::new(helper.as_ref());
        command
            .arg("--serve")
            .arg("--pid")
            .arg(client_pid.to_string());
        if activate {
            command.arg("--activate");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Error::Setup("the hands helper has no stdout pipe".to_owned())
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            Error::Setup("the hands helper has no stdin pipe".to_owned())
        })?;

        let (sender, replies) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("praxion-hands-reader".to_owned())
            .spawn(move || {
                let mut saw_bye = false;
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    match parse_line(&line) {
                        Some(wire @ Wire::Reply(HandsReply::Bye(_))) => {
                            saw_bye = true;
                            if sender.send(wire).is_err() {
                                return;
                            }
                        }
                        Some(wire) => {
                            if sender.send(wire).is_err() {
                                return;
                            }
                        }
                        None => log::debug!("hands: ignoring {:?}", line),
                    }
                }
                // `BYE` already said why the helper went away; anything else is
                // an exit the body did not ask for.
                if !saw_bye {
                    let _ = sender.send(Wire::Reply(HandsReply::Exited));
                }
            })
            .map_err(Error::Io)?;

        let mut link = Self {
            child,
            stdin: Some(stdin),
            replies,
            reader: Some(reader),
        };

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(link.fail("the hands helper did not say READY in 20 s"));
            }
            match link.replies.recv_timeout(left) {
                Ok(Wire::Ready) => break,
                Ok(Wire::Reply(HandsReply::Refused(_tick, reason))) => {
                    return Err(link.fail(format!("the hands helper refused: {reason}")));
                }
                Ok(Wire::Reply(HandsReply::Exited)) => {
                    return Err(link.fail("the hands helper exited before READY"));
                }
                Ok(Wire::Reply(other)) => {
                    log::debug!("hands: {other:?} before READY");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(link.fail("the hands helper did not say READY in 20 s"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(link.fail("the hands helper's stdout closed before READY"));
                }
            }
        }

        if let Err(error) = link.send("ARM") {
            return Err(link.fail(format!("cannot arm the hands helper: {error}")));
        }
        Ok(link)
    }

    /// Write one line, newline included, and flush it.
    pub fn send(&mut self, line: &str) -> Result<(), Error> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(Error::Setup(
                "the hands helper's stdin is already closed".to_owned(),
            ));
        };
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    /// The replies read so far, without blocking.
    pub fn poll(&mut self) -> Vec<HandsReply> {
        let mut replies = Vec::new();
        loop {
            match self.replies.try_recv() {
                Ok(Wire::Ready) => {}
                Ok(Wire::Reply(reply)) => replies.push(reply),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The reader thread ended without saying Exited (only
                    // possible when its channel send failed); nothing more will
                    // arrive.
                    break;
                }
            }
        }
        replies
    }

    /// Tear the link down after a failed spawn or a silent helper: close stdin,
    /// kill the child and reap it, then return the error to the caller.
    fn fail(&mut self, message: impl Into<String>) -> Error {
        self.kill();
        Error::Setup(message.into())
    }

    fn kill(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for HandsLink {
    fn drop(&mut self) {
        // Closing stdin is the helper's own watchdog: it releases every key and
        // button and exits by itself.
        self.stdin.take();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(WAIT_STEP),
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Err(_) => break,
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Run `<helper> --release-all --pid <pid>` once, for a helper that died while
/// it may still have held keys or buttons.
pub fn release_all(helper: impl AsRef<Path>, client_pid: u32) -> Result<(), Error> {
    let status = Command::new(helper.as_ref())
        .arg("--release-all")
        .arg("--pid")
        .arg(client_pid.to_string())
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(Error::Setup(format!(
            "the hands helper's release-all exited with {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parser_reads_every_line_kind() {
        assert_eq!(parse_line("READY"), Some(Wire::Ready));
        assert_eq!(parse_line("OK 12"), Some(Wire::Reply(HandsReply::Ok(12))));
        assert_eq!(
            parse_line("REFUSED 3 stale"),
            Some(Wire::Reply(HandsReply::Refused(3, "stale".to_owned())))
        );
        assert_eq!(
            parse_line("TAKEOVER foreign_input"),
            Some(Wire::Reply(HandsReply::Takeover("foreign_input".to_owned())))
        );
        assert_eq!(parse_line("IDLE"), Some(Wire::Reply(HandsReply::Idle)));
        assert_eq!(
            parse_line("BYE stdin_eof"),
            Some(Wire::Reply(HandsReply::Bye("stdin_eof".to_owned())))
        );
        // A reason may contain a space; keep all of it.
        assert_eq!(
            parse_line("TAKEOVER focus lost"),
            Some(Wire::Reply(HandsReply::Takeover("focus lost".to_owned())))
        );
        // A terse terminal line is still terminal.
        assert_eq!(
            parse_line("TAKEOVER"),
            Some(Wire::Reply(HandsReply::Takeover("unknown".to_owned())))
        );
        assert_eq!(
            parse_line("BYE"),
            Some(Wire::Reply(HandsReply::Bye("unknown".to_owned())))
        );
    }

    #[test]
    fn the_parser_ignores_malformed_lines() {
        for line in [
            "",
            "HELLO",
            "OK",
            "OK many",
            "REFUSED 3",
            "REFUSED many stale",
            "READY extra",
        ] {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
    }
}
