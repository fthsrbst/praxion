//! The buffered append-only writer.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::event::{Attribution, Event, EventKind, EvidenceRef, Outcome, Versions};

/// File name inside a run directory.
pub const FILE_NAME: &str = "events.jsonl";

/// Writes one run's event log.
///
/// The writer owns the sequence counter. Events are serialised into an
/// in-memory buffer and written when the tick changes, when [`Writer::flush`]
/// is called, and when the writer is dropped. A crash therefore loses at most
/// the tick that was in progress.
pub struct Writer {
    file: BufWriter<File>,
    path: PathBuf,
    next_seq: u64,
    buffer: Vec<u8>,
    buffered_tick: Option<u64>,
}

impl Writer {
    /// Create `<run_dir>/events.jsonl`, creating `run_dir` if needed.
    ///
    /// A run has one log, so an existing file is an error rather than an
    /// overwrite: appending to a finished run would break its sequence.
    pub fn create(run_dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::create_at(run_dir.as_ref().join(FILE_NAME))
    }

    /// Create a log at an explicit path, creating its parent directory.
    pub fn create_at(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            file: BufWriter::new(file),
            path: path.to_path_buf(),
            next_seq: 0,
            buffer: Vec::new(),
            buffered_tick: None,
        })
    }

    /// The file this writer appends to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sequence the next event will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Append one event to the current tick's buffer.
    ///
    /// Changing the tick first writes the previous tick's events, so the file
    /// never holds two ticks in one unflushed buffer.
    pub fn append(&mut self, tick: u64, ts_ns: u64, event: EventKind) -> io::Result<u64> {
        if self.buffered_tick.is_some_and(|buffered| buffered != tick) {
            self.flush()?;
        }
        let seq = self.next_seq;
        let next_seq = seq
            .checked_add(1)
            .ok_or_else(|| io::Error::other("event log sequence number overflowed"))?;
        let line = Event { seq, tick, ts_ns, event };
        let start = self.buffer.len();
        if let Err(error) = serde_json::to_writer(&mut self.buffer, &line) {
            // Never leave half a line in the buffer: a later event would
            // otherwise append to a corrupt JSON line.
            self.buffer.truncate(start);
            return Err(io::Error::other(error));
        }
        self.buffer.push(b'\n');
        self.next_seq = next_seq;
        self.buffered_tick = Some(tick);
        Ok(seq)
    }

    /// Write everything buffered so far.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.file.write_all(&self.buffer)?;
        self.file.flush()?;
        self.buffer.clear();
        self.buffered_tick = None;
        Ok(())
    }

    pub fn run_started(
        &mut self,
        tick: u64,
        ts_ns: u64,
        versions: Versions,
    ) -> io::Result<u64> {
        self.append(tick, ts_ns, EventKind::RunStarted { versions })
    }

    pub fn intent_started(
        &mut self,
        tick: u64,
        ts_ns: u64,
        intent_id: u64,
        primitive: impl Into<String>,
        args: Vec<String>,
        start_tick: u64,
        attribution: Attribution,
    ) -> io::Result<u64> {
        self.append(
            tick,
            ts_ns,
            EventKind::IntentStarted {
                intent_id,
                primitive: primitive.into(),
                args,
                start_tick,
                attribution,
            },
        )
    }

    pub fn receipt_settled(
        &mut self,
        tick: u64,
        ts_ns: u64,
        receipt_id: u64,
        intent_id: u64,
        outcome: Outcome,
        evidence: Vec<EvidenceRef>,
        note: Option<String>,
    ) -> io::Result<u64> {
        self.append(
            tick,
            ts_ns,
            EventKind::ReceiptSettled {
                receipt_id,
                intent_id,
                outcome,
                evidence,
                note,
            },
        )
    }

    pub fn late_evidence(
        &mut self,
        tick: u64,
        ts_ns: u64,
        receipt_id: u64,
        evidence_kind: impl Into<String>,
    ) -> io::Result<u64> {
        self.append(
            tick,
            ts_ns,
            EventKind::LateEvidence { receipt_id, evidence_kind: evidence_kind.into() },
        )
    }

    pub fn world_desync(
        &mut self,
        tick: u64,
        ts_ns: u64,
        reason: impl Into<String>,
    ) -> io::Result<u64> {
        self.append(tick, ts_ns, EventKind::WorldDesync { reason: reason.into() })
    }

    pub fn ledger(
        &mut self,
        tick: u64,
        ts_ns: u64,
        query: impl Into<String>,
        fact: impl Into<String>,
        count: u64,
        first_tick: u64,
        last_tick: u64,
    ) -> io::Result<u64> {
        self.append(
            tick,
            ts_ns,
            EventKind::Ledger {
                query: query.into(),
                fact: fact.into(),
                count,
                first_tick,
                last_tick,
            },
        )
    }

    pub fn note(&mut self, tick: u64, ts_ns: u64, text: impl Into<String>) -> io::Result<u64> {
        self.append(tick, ts_ns, EventKind::Note { text: text.into() })
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // A drop cannot report an error. The body flushes explicitly at the end
        // of a run; this only saves the last tick on an ordinary shutdown.
        let _ = self.flush();
    }
}
