//! Length-prefixed JSON framing: 4-byte big-endian length + UTF-8 JSON.
//!
//! [`FrameReader`] keeps partial frames in an internal buffer, so a read that
//! ends early (a socket timeout, a short read) does not corrupt the stream.

use std::fmt;
use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The longest frame the port accepts or emits, in bytes.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

const HEADER_SIZE: usize = 4;

/// Why a frame could not be read or written.
#[derive(Debug)]
pub enum FrameError {
    /// The underlying reader or writer failed.
    Io(io::Error),
    /// The frame's declared or produced length exceeds [`MAX_FRAME_SIZE`].
    TooLarge { len: usize },
    /// The stream ended in the middle of a frame.
    Truncated { expected: usize, got: usize },
    /// The payload is not valid JSON for the requested type.
    Json(serde_json::Error),
}

impl FrameError {
    /// True when the reader has no complete frame yet and the caller should
    /// try again: a non-blocking socket, a timed-out read or an interrupted
    /// read. The reader keeps every byte it already consumed.
    pub fn is_incomplete(&self) -> bool {
        matches!(
            self,
            FrameError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                )
        )
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Io(error) => write!(f, "port frame: {error}"),
            FrameError::TooLarge { len } => {
                write!(f, "port frame: {len} bytes exceeds the {MAX_FRAME_SIZE}-byte limit")
            }
            FrameError::Truncated { expected, got } => {
                write!(f, "port frame: stream ended after {got} of {expected} bytes")
            }
            FrameError::Json(error) => write!(f, "port frame: invalid JSON: {error}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::Io(error) => Some(error),
            FrameError::Json(error) => Some(error),
            FrameError::TooLarge { .. } | FrameError::Truncated { .. } => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(value: io::Error) -> Self {
        FrameError::Io(value)
    }
}

/// Reads frames from a stream, buffering whatever does not yet form a frame.
pub struct FrameReader<R> {
    inner: R,
    buffer: Vec<u8>,
    payload_len: Option<usize>,
    eof: bool,
}

impl<R: Read> FrameReader<R> {
    /// Wrap a reader. The reader may return short reads and time out; the
    /// in-progress frame is kept either way.
    pub fn new(inner: R) -> Self {
        Self { inner, buffer: Vec::new(), payload_len: None, eof: false }
    }

    /// Read the next payload. `Ok(None)` means a clean end of stream at a frame
    /// boundary; a stream that ends mid-frame is [`FrameError::Truncated`].
    pub fn read_payload(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            if let Some(payload) = self.take_frame()? {
                return Ok(Some(payload));
            }
            if self.eof {
                return Ok(None);
            }
            let mut chunk = [0_u8; 8 * 1024];
            match self.inner.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    if self.buffer.is_empty() {
                        return Ok(None);
                    }
                    let expected = self.payload_len.map_or(HEADER_SIZE, |len| HEADER_SIZE + len);
                    return Err(FrameError::Truncated { expected, got: self.buffer.len() });
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) => return Err(FrameError::Io(error)),
            }
        }
    }

    /// Read and deserialise the next JSON frame.
    pub fn read_json<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        match self.read_payload()? {
            Some(payload) => {
                serde_json::from_slice(&payload).map(Some).map_err(FrameError::Json)
            }
            None => Ok(None),
        }
    }

    /// Pull one complete frame out of the buffer, if it holds one.
    fn take_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if self.payload_len.is_none() {
            if self.buffer.len() < HEADER_SIZE {
                return Ok(None);
            }
            let header: [u8; HEADER_SIZE] = self.buffer[..HEADER_SIZE]
                .try_into()
                .expect("the header slice has four bytes");
            let len = u32::from_be_bytes(header) as usize;
            if len > MAX_FRAME_SIZE {
                return Err(FrameError::TooLarge { len });
            }
            self.payload_len = Some(len);
        }
        let len = self.payload_len.expect("set just above");
        if self.buffer.len() < HEADER_SIZE + len {
            return Ok(None);
        }
        let mut frame = std::mem::take(&mut self.buffer);
        let payload = frame[HEADER_SIZE..HEADER_SIZE + len].to_vec();
        self.buffer = frame.split_off(HEADER_SIZE + len);
        self.payload_len = None;
        Ok(Some(payload))
    }
}

/// Write one JSON value as a frame.
pub fn write_frame<W: Write, T: Serialize + ?Sized>(
    writer: &mut W,
    value: &T,
) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value).map_err(FrameError::Json)?;
    write_payload(writer, &payload)
}

/// Write an already serialised payload as a frame.
pub fn write_payload<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    if payload.len() > MAX_FRAME_SIZE {
        return Err(FrameError::TooLarge { len: payload.len() });
    }
    let len = u32::try_from(payload.len()).expect("the frame limit is below u32::MAX");
    writer.write_all(&len.to_be_bytes()).map_err(FrameError::Io)?;
    writer.write_all(payload).map_err(FrameError::Io)?;
    writer.flush().map_err(FrameError::Io)?;
    Ok(())
}
