//! Zlib decompression with a hard output cap.
//!
//! A compressed frame starts with the uncompressed length (a VarInt) and then a
//! zlib stream. Azalea's `compression_decoder` reads that stream with
//! `read_to_end` and only *reserves* the declared length, so a frame that
//! declares 8 MiB may expand to gigabytes: the declared length is never checked
//! against what the stream really produces. That is a decompression bomb, and
//! the tee decodes bytes that came off a network.
//!
//! This module decodes the stream itself and stops the moment the output would
//! pass the declared length. The zlib decoder is wrapped in
//! `Read::take(declared + 1)`, so at most one byte past the declared size can be
//! produced, and `declared` is capped at [`MAXIMUM_UNCOMPRESSED_LENGTH`] before
//! any work starts. Memory is therefore bounded by the declared length. A stream
//! that ends early or runs long is a size mismatch, not a packet: the caller
//! desynchronises the connection instead of guessing.
//!
//! The DEFLATE work, including the Adler-32 trailer, belongs to `flate2`, the
//! same crate `azalea-protocol` already links (with the `zlib-rs` backend).
//! Reusing it beats maintaining a second inflater in this crate.

use std::io::Read;

use azalea_protocol::read::MAXIMUM_UNCOMPRESSED_LENGTH;
use flate2::read::ZlibDecoder;

/// Why a frame did not decompress to exactly its declared length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DecompressError {
    /// The stream produced more or fewer bytes than the frame declared. The
    /// text is fixed: the CLI and the tests key on it.
    SizeMismatch,
    /// The frame is not a usable compressed stream.
    Format(String),
}

impl std::fmt::Display for DecompressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecompressError::SizeMismatch => f.write_str("decompressed size mismatch"),
            DecompressError::Format(message) => write!(f, "decompression failed: {message}"),
        }
    }
}

/// Decompress one frame body, data length VarInt included, for a connection
/// whose compression threshold is `threshold`.
///
/// The result is never longer than the declared length and never longer than
/// [`MAXIMUM_UNCOMPRESSED_LENGTH`].
pub(crate) fn decompress(frame: &[u8], threshold: u32) -> Result<Vec<u8>, DecompressError> {
    let (declared, prefix) = read_varint(frame).ok_or_else(|| {
        DecompressError::Format("frame does not start with a data length VarInt".to_owned())
    })?;
    let body = &frame[prefix..];
    let maximum = MAXIMUM_UNCOMPRESSED_LENGTH as usize;

    if declared == 0 {
        // Data length 0 means the rest of the frame is the packet itself.
        if body.len() > maximum {
            return Err(DecompressError::Format(format!(
                "uncompressed packet of {} bytes exceeds the {maximum} byte maximum",
                body.len()
            )));
        }
        return Ok(body.to_vec());
    }

    if declared < threshold {
        return Err(DecompressError::Format(format!(
            "declared size {declared} is below the connection threshold of {threshold}"
        )));
    }
    if declared as usize > maximum {
        return Err(DecompressError::Format(format!(
            "declared size {declared} exceeds the {maximum} byte maximum"
        )));
    }

    let output = inflate(body, declared as usize)?;
    if output.len() != declared as usize {
        return Err(DecompressError::SizeMismatch);
    }
    Ok(output)
}

/// Decode a zlib stream, holding at most `declared + 1` bytes.
///
/// The extra byte is what tells an exact stream from one that runs long without
/// ever expanding a bomb: a stream that produces more is reported as a size
/// mismatch by the caller.
fn inflate(data: &[u8], declared: usize) -> Result<Vec<u8>, DecompressError> {
    let mut output = Vec::with_capacity(declared);
    let mut decoder = ZlibDecoder::new(data).take(declared as u64 + 1);
    decoder
        .read_to_end(&mut output)
        .map_err(|error| DecompressError::Format(error.to_string()))?;
    Ok(output)
}

/// `None` when the VarInt is incomplete or longer than five bytes.
fn read_varint(buf: &[u8]) -> Option<(u32, usize)> {
    let mut value = 0u32;
    for (index, &byte) in buf.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}
