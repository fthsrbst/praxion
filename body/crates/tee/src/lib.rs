//! The tee record format: the forwarder's copy of both directions of every game connection.
//!
//! The forwarder never parses game bytes (docs/decisions.md, "Hard requirement: no body or decoder
//! crash may drop the user's game connection"). It wraps each raw TCP chunk it forwards in a record and
//! writes the records to the body over loopback. The same byte stream saved to a file is a capture
//! file, so live play, replays and tests all read one format.
//!
//! ```text
//! stream : header record*
//! header : 8 bytes, b"PRXCAP2\n" (b"PRXCAP1\n" is read too: the 2.7 spike captures)
//! record : u64 ts_ns     nanoseconds since the UNIX epoch when the forwarder's read() returned
//!          u32 conn_id   0, 1, 2 ... per accepted client connection
//!          u8  kind      0 serverbound, 1 clientbound, 2 opened, 3 closed, 4 gap
//!          u32 len
//!          [len] bytes   kinds 0 and 1: the raw chunk, not re-framed and not decoded
//!                        kinds 2 and 3: empty
//!                        kind 4: u64 count of bytes of this connection the tee dropped
//! ```
//!
//! All integers are little-endian. A gap means the forwarder dropped tee bytes for that connection
//! (its tee queue was full or the body was not connected); the game connection itself lost nothing.
//! After a gap the reader of that connection's stream has lost framing and must treat its world view as
//! invalid. Version 2 adds the gap kind; version 1 streams never contain one.

use std::fmt;
use std::io::{self, Read, Write};

/// Header of a stream written by this crate.
pub const HEADER: &[u8; 8] = b"PRXCAP2\n";
/// Header of the 2.7 spike captures; readable, never written.
pub const HEADER_V1: &[u8; 8] = b"PRXCAP1\n";
/// Bytes before a record's payload.
pub const RECORD_HEAD_LEN: usize = 8 + 4 + 1 + 4;
/// Largest payload the reader accepts. A forwarder read is at most one socket buffer; anything larger
/// means the stream is corrupt, and refusing it keeps a bad length from allocating gigabytes.
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// What a record says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Bytes the client sent to the server.
    Serverbound,
    /// Bytes the server sent to the client.
    Clientbound,
    /// The forwarder accepted a client connection.
    Opened,
    /// The connection closed (either side).
    Closed,
    /// The tee dropped this many bytes of the connection; the game connection did not.
    Gap { dropped_bytes: u64 },
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::Serverbound => 0,
            Kind::Clientbound => 1,
            Kind::Opened => 2,
            Kind::Closed => 3,
            Kind::Gap { .. } => 4,
        }
    }
}

/// One tee record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub ts_ns: u64,
    pub conn_id: u32,
    pub kind: Kind,
    /// Raw bytes for `Serverbound` and `Clientbound`; empty otherwise.
    pub data: Vec<u8>,
}

impl Record {
    /// Bytes this record takes on the wire.
    pub fn encoded_len(&self) -> usize {
        RECORD_HEAD_LEN + self.payload_len()
    }

    fn payload_len(&self) -> usize {
        match self.kind {
            Kind::Gap { .. } => 8,
            _ => self.data.len(),
        }
    }
}

/// Write the stream header. Call once, before the first record.
pub fn write_header<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(HEADER)
}

/// Write one record. Fails with `InvalidInput` for a payload over `MAX_PAYLOAD` or data on a kind that
/// carries none, so a writer can never produce a stream the reader refuses.
pub fn write_record<W: Write>(w: &mut W, rec: &Record) -> io::Result<()> {
    let has_data = matches!(rec.kind, Kind::Serverbound | Kind::Clientbound);
    if !has_data && !rec.data.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "only byte records carry data"));
    }
    let len = rec.payload_len();
    if len > MAX_PAYLOAD as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "record payload over MAX_PAYLOAD"));
    }
    let mut head = [0u8; RECORD_HEAD_LEN];
    head[0..8].copy_from_slice(&rec.ts_ns.to_le_bytes());
    head[8..12].copy_from_slice(&rec.conn_id.to_le_bytes());
    head[12] = rec.kind.code();
    head[13..17].copy_from_slice(&(len as u32).to_le_bytes());
    w.write_all(&head)?;
    match rec.kind {
        Kind::Gap { dropped_bytes } => w.write_all(&dropped_bytes.to_le_bytes()),
        _ => w.write_all(&rec.data),
    }
}

/// Why a stream could not be read.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// The first 8 bytes are neither header.
    BadHeader([u8; 8]),
    /// The stream ended inside a record.
    Truncated,
    /// A record has an unknown kind, a payload over `MAX_PAYLOAD`, a gap of the wrong length, data on
    /// a kind that carries none, or a gap in a version 1 stream.
    Corrupt { offset: u64, reason: &'static str },
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "tee stream: {e}"),
            ReadError::BadHeader(h) => write!(f, "tee stream: bad header {h:?}"),
            ReadError::Truncated => write!(f, "tee stream: ends inside a record"),
            ReadError::Corrupt { offset, reason } => {
                write!(f, "tee stream: corrupt record at byte {offset}: {reason}")
            }
        }
    }
}

impl std::error::Error for ReadError {}

/// Reads records from a stream. Iterates `Result<Record, ReadError>`; after an error it yields nothing.
pub struct Reader<R> {
    inner: R,
    version: u8,
    offset: u64,
    done: bool,
}

impl<R: Read> Reader<R> {
    /// Read and check the header.
    pub fn new(mut inner: R) -> Result<Self, ReadError> {
        let mut header = [0u8; 8];
        read_exact_or(&mut inner, &mut header, ReadError::BadHeader([0u8; 8]))?;
        let version = match &header {
            h if h == HEADER => 2,
            h if h == HEADER_V1 => 1,
            _ => return Err(ReadError::BadHeader(header)),
        };
        Ok(Self { inner, version, offset: 8, done: false })
    }

    /// 1 for spike captures, 2 for streams written by this crate.
    pub fn version(&self) -> u8 {
        self.version
    }

    fn next_record(&mut self) -> Result<Option<Record>, ReadError> {
        let mut head = [0u8; RECORD_HEAD_LEN];
        // A clean end of stream is only allowed between records.
        let first = loop {
            match self.inner.read(&mut head[..1]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(ReadError::Io(e)),
            }
        };
        if first == 0 {
            return Ok(None);
        }
        read_exact_or(&mut self.inner, &mut head[1..], ReadError::Truncated)?;
        let start = self.offset;
        let ts_ns = u64::from_le_bytes(head[0..8].try_into().unwrap());
        let conn_id = u32::from_le_bytes(head[8..12].try_into().unwrap());
        let code = head[12];
        let len = u32::from_le_bytes(head[13..17].try_into().unwrap());
        let corrupt = |reason| ReadError::Corrupt { offset: start, reason };
        if len > MAX_PAYLOAD {
            return Err(corrupt("payload over MAX_PAYLOAD"));
        }
        let mut data = vec![0u8; len as usize];
        read_exact_or(&mut self.inner, &mut data, ReadError::Truncated)?;
        self.offset += (RECORD_HEAD_LEN as u64) + len as u64;
        let kind = match code {
            0 => Kind::Serverbound,
            1 => Kind::Clientbound,
            2 | 3 if len != 0 => return Err(corrupt("open or close record with data")),
            2 => Kind::Opened,
            3 => Kind::Closed,
            4 if self.version < 2 => return Err(corrupt("gap record in a version 1 stream")),
            4 if len != 8 => return Err(corrupt("gap record payload is not 8 bytes")),
            4 => {
                let dropped_bytes = u64::from_le_bytes(data[..].try_into().unwrap());
                data.clear();
                Kind::Gap { dropped_bytes }
            }
            _ => return Err(corrupt("unknown record kind")),
        };
        Ok(Some(Record { ts_ns, conn_id, kind, data }))
    }
}

impl<R: Read> Iterator for Reader<R> {
    type Item = Result<Record, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(rec)) => Some(Ok(rec)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

fn read_exact_or<R: Read>(r: &mut R, buf: &mut [u8], on_eof: ReadError) -> Result<(), ReadError> {
    match r.read_exact(buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(on_eof),
        Err(e) => Err(ReadError::Io(e)),
    }
}

/// Nanoseconds since the UNIX epoch: the clock of every record (spikes/README.md "Clock").
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(conn_id: u32, kind: Kind, data: &[u8]) -> Record {
        Record { ts_ns: 1_793_000_000_000_000_000 + conn_id as u64, conn_id, kind, data: data.to_vec() }
    }

    fn sample() -> Vec<Record> {
        vec![
            bytes(0, Kind::Opened, b""),
            bytes(0, Kind::Serverbound, b"\x10\x00\x89\x06"),
            bytes(0, Kind::Clientbound, &[7u8; 3000]),
            bytes(0, Kind::Gap { dropped_bytes: 12_345 }, b""),
            bytes(1, Kind::Opened, b""),
            bytes(0, Kind::Closed, b""),
        ]
    }

    fn encode(records: &[Record]) -> Vec<u8> {
        let mut out = Vec::new();
        write_header(&mut out).unwrap();
        for r in records {
            write_record(&mut out, r).unwrap();
        }
        out
    }

    #[test]
    fn round_trip() {
        let records = sample();
        let out = encode(&records);
        let expected_len: usize = 8 + records.iter().map(Record::encoded_len).sum::<usize>();
        assert_eq!(out.len(), expected_len);
        let reader = Reader::new(&out[..]).unwrap();
        assert_eq!(reader.version(), 2);
        let back: Vec<Record> = reader.map(Result::unwrap).collect();
        assert_eq!(back, records);
    }

    #[test]
    fn reads_version_1_captures() {
        let mut out = encode(&sample()[..3]);
        out[..8].copy_from_slice(HEADER_V1);
        let reader = Reader::new(&out[..]).unwrap();
        assert_eq!(reader.version(), 1);
        assert_eq!(reader.count(), 3);
    }

    #[test]
    fn gap_in_version_1_is_corrupt() {
        let mut out = encode(&sample()[3..4]);
        out[..8].copy_from_slice(HEADER_V1);
        let err = Reader::new(&out[..]).unwrap().next().unwrap().unwrap_err();
        assert!(matches!(err, ReadError::Corrupt { offset: 8, .. }), "{err}");
    }

    #[test]
    fn bad_header() {
        assert!(matches!(Reader::new(&b"PRXCAP9\nxxxx"[..]), Err(ReadError::BadHeader(_))));
        assert!(matches!(Reader::new(&b"PRX"[..]), Err(ReadError::BadHeader(_))));
    }

    #[test]
    fn truncated_inside_head_and_payload() {
        let out = encode(&sample()[1..2]);
        for cut in [9, 8 + RECORD_HEAD_LEN, out.len() - 1] {
            let mut reader = Reader::new(&out[..cut]).unwrap();
            assert!(matches!(reader.next(), Some(Err(ReadError::Truncated))), "cut at {cut}");
            assert!(reader.next().is_none(), "reader stops after an error");
        }
    }

    #[test]
    fn corrupt_records() {
        let base = encode(&sample()[1..2]);
        // unknown kind
        let mut out = base.clone();
        out[8 + 12] = 9;
        assert!(matches!(Reader::new(&out[..]).unwrap().next(), Some(Err(ReadError::Corrupt { .. }))));
        // oversized length
        let mut out = base.clone();
        out[8 + 13..8 + 17].copy_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        assert!(matches!(Reader::new(&out[..]).unwrap().next(), Some(Err(ReadError::Corrupt { .. }))));
        // open record with data
        let mut out = base.clone();
        out[8 + 12] = 2;
        assert!(matches!(Reader::new(&out[..]).unwrap().next(), Some(Err(ReadError::Corrupt { .. }))));
    }

    #[test]
    fn writer_refuses_what_the_reader_would_refuse() {
        let mut sink = Vec::new();
        assert!(write_record(&mut sink, &bytes(0, Kind::Closed, b"x")).is_err());
        assert!(write_record(&mut sink, &bytes(0, Kind::Gap { dropped_bytes: 1 }, b"x")).is_err());
        assert!(sink.is_empty());
    }

    #[test]
    fn empty_stream_after_header_is_fine() {
        assert_eq!(Reader::new(&HEADER[..]).unwrap().count(), 0);
    }
}
