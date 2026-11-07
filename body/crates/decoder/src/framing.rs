//! Split one direction of one connection into protocol frames.
//!
//! Framing only depends on the outer VarInt length prefix. It never looks at
//! the protocol state or at compression, so one [`Framer`] per direction is
//! enough and a frame may span any number of records.
//!
//! The frame's timestamp is the timestamp of the record that completed it,
//! i.e. the record holding its last byte, as the spike measured.

/// Hard cap that turns a desynchronised stream into an error instead of an
/// allocation. Vanilla's uncompressed packet limit is 2 MiB and Azalea's
/// `MAXIMUM_UNCOMPRESSED_LENGTH` is 8 MiB; the outer frame is never larger than
/// the packet it carries.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Why a byte stream stopped being a sequence of frames.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// Five bytes with the continuation bit set.
    VarIntTooLong,
    /// The frame length exceeds [`MAX_FRAME_LEN`].
    FrameTooLarge { length: usize },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::VarIntTooLong => {
                f.write_str("frame length VarInt is longer than 5 bytes (desync?)")
            }
            FrameError::FrameTooLarge { length } => {
                write!(f, "frame length {length} exceeds the {MAX_FRAME_LEN} byte cap")
            }
        }
    }
}

/// One direction's unframed bytes, across record boundaries.
#[derive(Debug, Default)]
pub struct Framer {
    buffer: Vec<u8>,
    /// Bytes before this index were already consumed. Keeping an offset avoids
    /// moving the buffer for every frame.
    start: usize,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one raw TCP chunk.
    pub fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        // Compact only when it pays off, so a long stream does not shift the
        // remaining bytes on every packet.
        if self.start > 0 && (self.start >= self.buffer.len() / 2 || self.start >= 64 * 1024) {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(data);
    }

    /// Bytes buffered but not yet part of a complete frame.
    pub fn pending(&self) -> usize {
        self.buffer.len() - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.pending() == 0
    }

    /// Remove and return the next complete frame body, if there is one.
    pub fn pop_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        let available = &self.buffer[self.start..];
        let Some((length, prefix_len)) = read_varint(available)? else {
            return Ok(None);
        };
        let length = length as usize;
        if length > MAX_FRAME_LEN {
            return Err(FrameError::FrameTooLarge { length });
        }
        let body_start = prefix_len;
        let end = body_start + length;
        if available.len() < end {
            return Ok(None);
        }
        let wire = available[body_start..end].to_vec();
        self.start += end;
        if self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
        Ok(Some(wire))
    }

    /// Drop all buffered bytes. Used when a connection desynchronises.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.start = 0;
    }
}

/// `None` means the VarInt is not complete yet.
fn read_varint(buf: &[u8]) -> Result<Option<(u32, usize)>, FrameError> {
    let mut value = 0u32;
    for (index, &byte) in buf.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(Some((value, index + 1)));
        }
    }
    if buf.len() < 5 {
        return Ok(None);
    }
    Err(FrameError::VarIntTooLong)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                return out;
            }
        }
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = varint(body.len() as u32);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn splits_across_arbitrary_chunks() {
        // The same multi-frame byte stream must produce the same frames for
        // every split. The pseudo-random split sequence is deterministic.
        let mut stream = Vec::new();
        let bodies: Vec<Vec<u8>> = (0..40u8)
            .map(|i| vec![i; (i as usize * 7) % 23 + 1])
            .collect();
        for body in &bodies {
            stream.extend_from_slice(&frame(body));
        }

        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..25 {
            let mut framer = Framer::new();
            let mut decoded = Vec::new();
            let mut offset = 0;
            while offset < stream.len() {
                seed ^= seed >> 12;
                seed ^= seed << 25;
                seed ^= seed >> 27;
                let chunk = 1 + (seed % 37) as usize;
                let end = (offset + chunk).min(stream.len());
                framer.push(&stream[offset..end]);
                offset = end;
                while let Some(body) = framer.pop_frame().expect("valid stream") {
                    decoded.push(body);
                }
            }
            assert!(framer.is_empty());
            assert_eq!(decoded, bodies);
        }
    }

    #[test]
    fn two_frames_in_one_push() {
        let mut framer = Framer::new();
        framer.push(&frame(b"one"));
        framer.push(&frame(b"two"));
        assert_eq!(framer.pop_frame().unwrap().unwrap(), b"one");
        assert_eq!(framer.pop_frame().unwrap().unwrap(), b"two");
        assert!(framer.pop_frame().unwrap().is_none());
    }

    #[test]
    fn incomplete_length_waits() {
        let mut framer = Framer::new();
        framer.push(&[0x80]);
        assert!(framer.pop_frame().unwrap().is_none());
        // VarInt 128 is [0x80, 0x01]; the first byte already arrived above.
        let mut rest = vec![0x01];
        rest.extend_from_slice(&[0xaa; 128]);
        framer.push(&rest);
        assert_eq!(framer.pop_frame().unwrap().unwrap(), vec![0xaa; 128]);
    }

    #[test]
    fn five_continuation_bytes_are_an_error() {
        let mut framer = Framer::new();
        framer.push(&[0x80, 0x80, 0x80, 0x80, 0x80]);
        assert_eq!(framer.pop_frame(), Err(FrameError::VarIntTooLong));
    }

    #[test]
    fn frame_over_the_cap_is_an_error() {
        let mut framer = Framer::new();
        let mut bytes = varint(MAX_FRAME_LEN as u32 + 1);
        bytes.extend_from_slice(&[0; 16]);
        framer.push(&bytes);
        assert!(matches!(framer.pop_frame(), Err(FrameError::FrameTooLarge { .. })));
    }
}
