//! Offline decoder for `*.prxcap` capture files (E2/E3/E4 of session 2.7).
//!
//! ```text
//! decode <file.prxcap> [--json-out <path>] [--block X Y Z]...
//! ```
//!
//! The capture format is defined in `spikes/README.md`; the proxy that writes it
//! never parses bytes, so framing, decompression and protocol state tracking all
//! happen here, with Azalea's packet enums doing the field decoding.
//!
//! Layout:
//!
//! - `capture`: the `PRXCAP1` record format.
//! - `framing`: one direction's byte stream -> VarInt-length frames, each with
//!   the timestamp of the record that carried its last byte.
//! - `analysis`: the protocol state machine across both directions, packet
//!   decoding, world updates and the E3/E4 measurements.
//! - `world`: the chunk/block model (Azalea `Chunk`, no ECS).
//! - `report`: the stdout summary and the same data as JSON.
//! - `dump`: the optional per-packet JSONL dump (`--dump`).
//!
//! Product code does not live here; this is throwaway experiment code.

pub mod analysis;
pub mod capture;
pub mod dump;
pub mod framing;
pub mod report;
pub mod world;

#[cfg(test)]
mod tests;

pub use analysis::{Analysis, Options, State, analyze};
pub use capture::{Capture, Dir, parse as parse_capture};
pub use dump::{DumpFilter, PacketDump};
pub use report::Json;

/// Parse and decode a capture held in memory. Used by the binary and the tests.
pub fn analyze_bytes(
    capture_name: &str,
    bytes: &[u8],
    options: &Options,
) -> Result<Analysis, String> {
    let capture = parse_capture(bytes).map_err(|e| e.to_string())?;
    Ok(analyze(capture_name, &capture, options))
}
