//! Parser for the `*.prxcap` capture format written by `spikes/proxy`.
//!
//! Layout (all integers little-endian), see `spikes/README.md`:
//!
//! ```text
//! header : 8 bytes  b"PRXCAP1\n"
//! record : u64 ts_unix_ns
//!          u32 conn_id
//!          u8  dir       0 = serverbound, 1 = clientbound,
//!                        2 = connection opened, 3 = connection closed
//!          u32 len
//!          [len] bytes    raw TCP chunk, never re-framed by the proxy
//! ```

use std::fmt;

pub const MAGIC: &[u8] = b"PRXCAP1\n";
pub const RECORD_HEADER_LEN: usize = 8 + 4 + 1 + 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Dir {
    Serverbound,
    Clientbound,
    Opened,
    Closed,
}

impl Dir {
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Dir::Serverbound),
            1 => Some(Dir::Clientbound),
            2 => Some(Dir::Opened),
            3 => Some(Dir::Closed),
            _ => None,
        }
    }

    /// Direction as used in the capture file.
    pub const fn as_u8(self) -> u8 {
        match self {
            Dir::Serverbound => 0,
            Dir::Clientbound => 1,
            Dir::Opened => 2,
            Dir::Closed => 3,
        }
    }

    /// Direction as used in reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Dir::Serverbound => "serverbound",
            Dir::Clientbound => "clientbound",
            Dir::Opened => "opened",
            Dir::Closed => "closed",
        }
    }

    /// Whether records in this direction carry connection bytes.
    pub const fn is_data(self) -> bool {
        matches!(self, Dir::Serverbound | Dir::Clientbound)
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    /// Time the proxy's `read()` returned, nanoseconds since the UNIX epoch.
    pub ts_unix_ns: u64,
    pub conn_id: u32,
    pub dir: Dir,
    /// Position of the record in the file, used to keep file order on ties.
    pub seq: usize,
    /// The raw TCP chunk exactly as read.
    pub data: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Capture {
    pub records: Vec<Record>,
    /// Some(offset) when the file ends in the middle of a record.
    /// The records parsed before that point are still returned.
    pub truncated_at: Option<usize>,
}

impl Capture {
    pub fn count(&self, dir: Dir) -> usize {
        self.records.iter().filter(|r| r.dir == dir).count()
    }

    pub fn conn_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.records.iter().map(|r| r.conn_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

#[derive(Debug)]
pub enum CaptureError {
    Header { found: Vec<u8> },
    BadDir { offset: usize, value: u8 },
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Header { found } => write!(
                f,
                "not a PRXCAP1 file: first bytes are {:?}, expected {:?}",
                String::from_utf8_lossy(found),
                String::from_utf8_lossy(MAGIC)
            ),
            CaptureError::BadDir { offset, value } => {
                write!(f, "record at byte {offset} has unknown dir {value}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

pub fn parse(bytes: &[u8]) -> Result<Capture, CaptureError> {
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Err(CaptureError::Header {
            found: bytes[..bytes.len().min(MAGIC.len())].to_vec(),
        });
    }

    let mut capture = Capture::default();
    let mut offset = MAGIC.len();
    let mut seq = 0;

    while offset < bytes.len() {
        if bytes.len() - offset < RECORD_HEADER_LEN {
            capture.truncated_at = Some(offset);
            break;
        }
        let ts_unix_ns = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let conn_id = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
        let dir_raw = bytes[offset + 12];
        let len = u32::from_le_bytes(bytes[offset + 13..offset + 17].try_into().unwrap()) as usize;
        let Some(dir) = Dir::from_u8(dir_raw) else {
            return Err(CaptureError::BadDir { offset, value: dir_raw });
        };

        let data_start = offset + RECORD_HEADER_LEN;
        if bytes.len() - data_start < len {
            capture.truncated_at = Some(offset);
            break;
        }
        capture.records.push(Record {
            ts_unix_ns,
            conn_id,
            dir,
            seq,
            data: bytes[data_start..data_start + len].to_vec(),
        });
        seq += 1;
        offset = data_start + len;
    }

    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(ts: u64, conn: u32, dir: Dir, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&ts.to_le_bytes());
        out.extend_from_slice(&conn.to_le_bytes());
        out.push(dir.as_u8());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn roundtrip() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&record(7, 0, Dir::Serverbound, &[1, 2, 3]));
        bytes.extend_from_slice(&record(8, 0, Dir::Opened, &[]));

        let capture = parse(&bytes).unwrap();
        assert!(capture.truncated_at.is_none());
        assert_eq!(capture.records.len(), 2);
        assert_eq!(capture.records[0].ts_unix_ns, 7);
        assert_eq!(capture.records[0].dir, Dir::Serverbound);
        assert_eq!(capture.records[0].data, vec![1, 2, 3]);
        assert_eq!(capture.records[1].dir, Dir::Opened);
    }

    #[test]
    fn truncated_record_is_reported() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&record(1, 0, Dir::Serverbound, &[0; 4]));
        bytes.extend_from_slice(&record(2, 0, Dir::Serverbound, &[0; 4])[..9]);

        let capture = parse(&bytes).unwrap();
        assert_eq!(capture.records.len(), 1);
        assert_eq!(capture.truncated_at, Some(MAGIC.len() + 21));
    }

    #[test]
    fn bad_header() {
        let err = parse(b"nope\n").unwrap_err();
        assert!(err.to_string().contains("not a PRXCAP1 file"));
    }
}
