//! `--dump`: one JSON line per selected decoded packet.
//!
//! The line format is fixed by the E7-E9 experiment plan:
//!
//! ```text
//! {"t": <ts_unix_ns>, "conn": <id>, "state": "<state>", "dir": "<dir>",
//!  "name": "<packet name>", "seq": <index in the (conn, dir) stream>,
//!  "debug": "<Debug of the Azalea packet>", <parsed fields>}
//! ```
//!
//! Parsed fields are added for the packets that E7-E9 consume directly:
//! `player_input` gets its seven booleans, `move_player_rot` and
//! `move_player_pos_rot` get `yaw`/`pitch`. They are read from the Azalea
//! struct, never parsed back out of the debug string.

use std::fmt::Write as _;
use std::io::{self, Write};

use crate::analysis::PacketRecord;

/// Which packet names to dump. `*` selects every decoded packet.
#[derive(Clone, Debug, Default)]
pub struct DumpFilter {
    names: Vec<String>,
    all: bool,
}

impl DumpFilter {
    /// Parse a comma-separated list such as `player_input,client_tick_end`.
    /// `*` anywhere in the list selects all packets; empty entries are
    /// ignored, an empty list is a usage error.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut names = Vec::new();
        let mut all = false;
        for part in spec.split(',') {
            let name = part.trim();
            if name.is_empty() {
                continue;
            }
            if name == "*" {
                all = true;
            } else {
                names.push(name.to_owned());
            }
        }
        if names.is_empty() && !all {
            return Err("--dump: the packet list is empty".to_owned());
        }
        Ok(Self { names, all })
    }

    pub fn matches(&self, name: &str) -> bool {
        self.all || self.names.iter().any(|wanted| wanted == name)
    }
}

/// The `player_input` flags, read from `ServerboundPlayerInput`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub shift: bool,
    pub sprint: bool,
}

/// Parsed fields added next to `debug` for a dumped packet.
#[derive(Clone, Copy, Debug, Default)]
pub enum DumpExtras {
    #[default]
    None,
    Input(PlayerInput),
    Look { yaw: f32, pitch: f32 },
}

/// Everything a dump line needs that is not already on `PacketRecord`.
#[derive(Clone, Debug)]
pub struct PacketDump {
    /// `format!("{packet:?}")` of the Azalea packet value.
    pub debug: String,
    pub extras: DumpExtras,
}

/// Append `s` to `out` as a JSON string literal, quotes included.
pub fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `s` as a JSON string literal, quotes included.
pub fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_json_string(&mut out, s);
    out
}

fn write_f32(out: &mut String, value: f32) {
    // JSON has no NaN/Infinity; the protocol should never carry them, but a
    // malformed packet must not produce an unparsable line.
    if value.is_finite() {
        let _ = write!(out, "{value}");
    } else {
        out.push_str("null");
    }
}

/// Render one `PacketRecord` as a JSON line (without the trailing newline).
///
/// Only records whose `dump` is set should be rendered; callers write the
/// selected records in capture order.
pub fn render_line(record: &PacketRecord) -> String {
    let mut out = String::with_capacity(256);
    let _ = write!(
        out,
        "{{\"t\": {}, \"conn\": {}, \"state\": ",
        record.ts_unix_ns, record.conn_id
    );
    write_json_string(&mut out, record.state.as_str());
    out.push_str(", \"dir\": ");
    write_json_string(&mut out, record.dir.as_str());
    out.push_str(", \"name\": ");
    write_json_string(&mut out, &record.name);
    let _ = write!(out, ", \"seq\": {}, \"debug\": ", record.stream_index);
    write_json_string(&mut out, record.dump.as_ref().map(|d| d.debug.as_str()).unwrap_or(""));

    if let Some(dump) = &record.dump {
        match dump.extras {
            DumpExtras::None => {}
            DumpExtras::Input(input) => {
                let _ = write!(
                    out,
                    ", \"forward\": {}, \"backward\": {}, \"left\": {}, \"right\": {}, \
                     \"jump\": {}, \"shift\": {}, \"sprint\": {}",
                    input.forward,
                    input.backward,
                    input.left,
                    input.right,
                    input.jump,
                    input.shift,
                    input.sprint
                );
            }
            DumpExtras::Look { yaw, pitch } => {
                out.push_str(", \"yaw\": ");
                write_f32(&mut out, yaw);
                out.push_str(", \"pitch\": ");
                write_f32(&mut out, pitch);
            }
        }
    }

    out.push('}');
    out
}

/// Write every record that carries a `PacketDump` as one JSON line, in the
/// order of `records`. Returns how many lines were written.
pub fn write_records<W: Write>(writer: &mut W, records: &[PacketRecord]) -> io::Result<u64> {
    let mut lines = 0;
    for record in records {
        if record.dump.is_some() {
            writer.write_all(render_line(record).as_bytes())?;
            writer.write_all(b"\n")?;
            lines += 1;
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_parsing() {
        let filter = DumpFilter::parse(" player_input, client_tick_end ,").unwrap();
        assert!(filter.matches("player_input"));
        assert!(filter.matches("client_tick_end"));
        assert!(!filter.matches("move_player_rot"));

        let all = DumpFilter::parse("*").unwrap();
        assert!(all.matches("anything"));

        assert!(DumpFilter::parse(" , ").is_err());
    }
}
