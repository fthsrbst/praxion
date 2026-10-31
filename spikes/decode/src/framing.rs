//! Split one direction of one connection into protocol frames.
//!
//! Framing only depends on the outer VarInt length prefix, never on the
//! protocol state or on compression, so it can be done per direction before
//! the two directions are merged into global timestamp order.
//!
//! A frame's timestamp is the timestamp of the capture record that holds its
//! last byte (see the task brief and `spikes/README.md`).

use crate::capture::{Dir, Record};

/// Hard cap to turn a desynchronized stream into an error instead of an OOM.
/// Vanilla's uncompressed packet limit is 2 MiB and Azalea's
/// `MAXIMUM_UNCOMPRESSED_LENGTH` is 8 MiB; the outer frame is smaller than the
/// packet it carries.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Frame {
    pub conn_id: u32,
    pub dir: Dir,
    /// Timestamp of the record that carried the last byte of this frame.
    pub ts_unix_ns: u64,
    /// Sequence number of that record in the file, for stable ordering on ties.
    pub seq: usize,
    /// Byte offset one past the last byte of the frame in the direction stream.
    pub end_offset: usize,
    /// Frame body: everything after the outer VarInt length prefix. Still
    /// compressed when compression is enabled.
    pub wire: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FramingIssue {
    pub conn_id: u32,
    pub dir: Dir,
    pub message: String,
    /// Number of stream bytes that could not be framed.
    pub bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum VarIntError {
    /// The buffer ended in the middle of the VarInt.
    NeedMore,
    /// Five bytes with the continuation bit set.
    TooLong,
}

fn read_varint_u32(buf: &[u8]) -> Result<(u32, usize), VarIntError> {
    let mut value: u32 = 0;
    for i in 0..5usize {
        let Some(&byte) = buf.get(i) else {
            return Err(VarIntError::NeedMore);
        };
        value |= u32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err(VarIntError::TooLong)
}

/// `(start, end, ts, seq)` for every record that carried at least one byte.
type Mark = (usize, usize, u64, usize);

/// Split the concatenated byte stream of one `(conn_id, dir)` pair into frames.
///
/// `records` must be in file order and all belong to the same connection and
/// direction.
pub fn split_direction(
    conn_id: u32,
    dir: Dir,
    records: &[&Record],
) -> (Vec<Frame>, Option<FramingIssue>) {
    let mut stream: Vec<u8> = Vec::new();
    let mut marks: Vec<Mark> = Vec::new();
    for record in records {
        if record.data.is_empty() {
            continue;
        }
        let start = stream.len();
        stream.extend_from_slice(&record.data);
        marks.push((start, stream.len(), record.ts_unix_ns, record.seq));
    }

    let mut frames = Vec::new();
    let mut pos = 0usize;
    let mut mark_index = 0usize;

    while pos < stream.len() {
        let (length, varint_len) = match read_varint_u32(&stream[pos..]) {
            Ok(parsed) => parsed,
            Err(VarIntError::NeedMore) => {
                return (
                    frames,
                    Some(FramingIssue {
                        conn_id,
                        dir,
                        message: "stream ends inside a frame length VarInt".to_owned(),
                        bytes: stream.len() - pos,
                    }),
                );
            }
            Err(VarIntError::TooLong) => {
                return (
                    frames,
                    Some(FramingIssue {
                        conn_id,
                        dir,
                        message: "frame length VarInt is longer than 5 bytes (desync?)".to_owned(),
                        bytes: stream.len() - pos,
                    }),
                );
            }
        };

        let length = length as usize;
        if length > MAX_FRAME_LEN {
            return (
                frames,
                Some(FramingIssue {
                    conn_id,
                    dir,
                    message: format!("frame length {length} exceeds the {MAX_FRAME_LEN} byte cap"),
                    bytes: stream.len() - pos,
                }),
            );
        }

        let body_start = pos + varint_len;
        let end = body_start + length;
        if end > stream.len() {
            return (
                frames,
                Some(FramingIssue {
                    conn_id,
                    dir,
                    message: format!(
                        "incomplete frame: length {length} needs {} more bytes than the stream has",
                        end - stream.len()
                    ),
                    bytes: stream.len() - pos,
                }),
            );
        }

        while mark_index < marks.len() && marks[mark_index].1 < end {
            mark_index += 1;
        }
        let Some(&(_, _, ts_unix_ns, seq)) = marks.get(mark_index) else {
            return (
                frames,
                Some(FramingIssue {
                    conn_id,
                    dir,
                    message: "internal error: frame has no owning record".to_owned(),
                    bytes: stream.len() - pos,
                }),
            );
        };

        frames.push(Frame {
            conn_id,
            dir,
            ts_unix_ns,
            seq,
            end_offset: end,
            wire: stream[body_start..end].to_vec(),
        });
        pos = end;
    }

    (frames, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: usize, ts: u64, data: &[u8]) -> Record {
        Record {
            ts_unix_ns: ts,
            conn_id: 0,
            dir: Dir::Serverbound,
            seq,
            data: data.to_vec(),
        }
    }

    fn frame_bytes(len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut value = len as u32;
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
        out.extend(std::iter::repeat_n(0xAB, len));
        out
    }

    #[test]
    fn splits_across_records_and_keeps_last_byte_timestamp() {
        let bytes = frame_bytes(10);
        let records = vec![record(0, 100, &bytes[..3]), record(1, 200, &bytes[3..])];
        let refs: Vec<&Record> = records.iter().collect();

        let (frames, issue) = split_direction(0, Dir::Serverbound, &refs);
        assert!(issue.is_none());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].ts_unix_ns, 200);
        assert_eq!(frames[0].seq, 1);
        assert_eq!(frames[0].wire, vec![0xAB; 10]);
    }

    #[test]
    fn two_frames_in_one_record() {
        let mut bytes = frame_bytes(2);
        bytes.extend(frame_bytes(3));
        let records = vec![record(0, 50, &bytes)];
        let refs: Vec<&Record> = records.iter().collect();

        let (frames, issue) = split_direction(0, Dir::Serverbound, &refs);
        assert!(issue.is_none());
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.ts_unix_ns == 50));
        assert_eq!(frames[0].wire, vec![0xAB; 2]);
        assert_eq!(frames[1].wire, vec![0xAB; 3]);
    }

    #[test]
    fn incomplete_frame_is_an_issue() {
        let bytes = frame_bytes(4);
        let records = vec![record(0, 50, &bytes[..3])];
        let refs: Vec<&Record> = records.iter().collect();

        let (frames, issue) = split_direction(0, Dir::Serverbound, &refs);
        assert!(frames.is_empty());
        let issue = issue.expect("trailing bytes must be reported");
        assert_eq!(issue.bytes, 3);
    }
}
