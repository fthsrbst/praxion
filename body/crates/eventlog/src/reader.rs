//! The replay reader.

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use crate::event::Event;

/// Why a log could not be replayed further.
#[derive(Debug)]
pub enum ReadError {
    /// The underlying file could not be read.
    Io(io::Error),
    /// `seq` was not the next number after the previous event.
    SeqGap { line: u64, expected: u64, found: u64 },
    /// A complete line is not a valid event.
    Corrupt { line: u64, message: String },
    /// The last line has no newline and is not a complete event: a writer
    /// crashed while producing it.
    Truncated { line: u64, message: String },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "event log: {e}"),
            ReadError::SeqGap { line, expected, found } => write!(
                f,
                "event log line {line}: sequence gap, expected {expected}, found {found}"
            ),
            ReadError::Corrupt { line, message } => {
                write!(f, "event log line {line}: corrupt event: {message}")
            }
            ReadError::Truncated { line, message } => write!(
                f,
                "event log line {line}: truncated last line (writer crashed?): {message}"
            ),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(value: io::Error) -> Self {
        ReadError::Io(value)
    }
}

/// Replays typed events from JSON Lines. Stops at the first error.
pub struct Reader<R> {
    inner: R,
    line: u64,
    expected: u64,
    done: bool,
    buffer: Vec<u8>,
}

impl<R: BufRead> Reader<R> {
    /// Read events whose `seq` starts at 0.
    pub fn new(inner: R) -> Self {
        Self { inner, line: 0, expected: 0, done: false, buffer: Vec::new() }
    }

    /// The line number the reader is on, one-based.
    pub fn line(&self) -> u64 {
        self.line
    }

    fn next_event(&mut self) -> Option<Result<Event, ReadError>> {
        if self.done {
            return None;
        }

        self.buffer.clear();
        match self.inner.read_until(b'\n', &mut self.buffer) {
            Ok(0) => {
                self.done = true;
                return None;
            }
            Ok(_) => {}
            Err(e) => {
                self.done = true;
                return Some(Err(ReadError::Io(e)));
            }
        }

        self.line += 1;
        let line = self.line;
        let complete = self.buffer.last() == Some(&b'\n');
        let mut end = self.buffer.len() - usize::from(complete);
        if end > 0 && self.buffer[end - 1] == b'\r' {
            end -= 1;
        }

        let parsed = std::str::from_utf8(&self.buffer[..end])
            .map_err(|e| format!("not UTF-8: {e}"))
            .and_then(|text| {
                if text.is_empty() {
                    Err("blank line".to_owned())
                } else {
                    serde_json::from_str::<Event>(text).map_err(|e| e.to_string())
                }
            });

        let event = match parsed {
            Ok(event) => event,
            Err(message) => {
                self.done = true;
                return Some(Err(if complete {
                    ReadError::Corrupt { line, message }
                } else {
                    ReadError::Truncated { line, message }
                }));
            }
        };

        if event.seq != self.expected {
            self.done = true;
            return Some(Err(ReadError::SeqGap {
                line,
                expected: self.expected,
                found: event.seq,
            }));
        }
        self.expected = self.expected.saturating_add(1);
        Some(Ok(event))
    }
}

impl<R: BufRead> Iterator for Reader<R> {
    type Item = Result<Event, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event()
    }
}

/// Everything a replay could read before it stopped.
#[derive(Debug)]
pub struct Replay {
    /// The gap-free prefix, oldest first.
    pub events: Vec<Event>,
    /// Why the replay stopped, or `None` when the whole file was valid.
    pub error: Option<ReadError>,
}

impl Replay {
    /// `true` when the file ended on a valid event.
    pub fn is_complete(&self) -> bool {
        self.error.is_none()
    }

    /// The gap-free prefix, or the error together with the events before it.
    pub fn into_result(self) -> Result<Vec<Event>, (ReadError, Vec<Event>)> {
        match self.error {
            None => Ok(self.events),
            Some(error) => Err((error, self.events)),
        }
    }
}

/// Replay a reader to the end or to its first error.
pub fn replay<R: BufRead>(reader: Reader<R>) -> Replay {
    let mut events = Vec::new();
    let mut error = None;
    for item in reader {
        match item {
            Ok(event) => events.push(event),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    Replay { events, error }
}

/// Open and replay `<path>`.
pub fn read(path: impl AsRef<Path>) -> io::Result<Replay> {
    let file = File::open(path)?;
    Ok(replay(Reader::new(BufReader::new(file))))
}
