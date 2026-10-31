//! Rendering of the analysis: a readable stdout report and the same data as
//! JSON (`--json-out`).

use std::fmt::Write as _;

use crate::analysis::{
    Analysis, BlockAnswer, DimensionInfo, E3Stats, E4Stats, MoveStats, PacketRecord, PercentilesUs,
    State,
};
use crate::capture::Dir;

impl Analysis {
    pub fn text_report(&self) -> String {
        let mut out = String::new();

        let _ = writeln!(out, "capture: {}", self.capture_name);
        let _ = writeln!(
            out,
            "records: {} (serverbound {}, clientbound {}, opened {}, closed {})",
            self.records.serverbound
                + self.records.clientbound
                + self.records.opened
                + self.records.closed,
            self.records.serverbound,
            self.records.clientbound,
            self.records.opened,
            self.records.closed
        );
        let _ = writeln!(
            out,
            "connections: {}  packets: {}  decode errors: {}  unconsumed packets: {}",
            self.connections.len(),
            self.total_packets,
            self.total_errors,
            self.unconsumed_packets
        );
        let _ = writeln!(out);

        for connection in &self.connections {
            let _ = writeln!(out, "connection {}", connection.conn_id);
            if let Some(dimension) = &connection.dimension {
                let _ = writeln!(out, "  dimension: {}", dimension_line(dimension));
            } else {
                let _ = writeln!(out, "  dimension: none (no game login packet)");
            }
            let _ = writeln!(out, "  chunks loaded at end: {}", connection.chunks_loaded);
            if let Some(player) = connection.player {
                let _ = writeln!(
                    out,
                    "  player: x {:.3} y {:.3} z {:.3} yaw {:.3} pitch {:.3}",
                    player.pos.x, player.pos.y, player.pos.z, player.yaw, player.pitch
                );
            }
            if let Some(stopped) = &connection.stopped {
                let _ = writeln!(out, "  STOPPED: {stopped}");
            }
            for note in &connection.notes {
                let _ = writeln!(out, "  note: {note}");
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(out, "per state and direction");
        for state in State::ALL {
            for dir in [Dir::Serverbound, Dir::Clientbound] {
                let Some(group) = self.groups.get(&(state, dir)) else {
                    continue;
                };
                if group.packets() == 0 && group.errors == 0 {
                    continue;
                }
                let _ = writeln!(
                    out,
                    "  {}.{}: {} packets, {} errors, {} unconsumed",
                    state.as_str(),
                    dir.as_str(),
                    group.packets(),
                    group.errors,
                    group.unconsumed
                );
                for (name, count) in &group.counts {
                    let _ = writeln!(out, "      {name} x{count}");
                }
                for example in &group.examples {
                    let _ = writeln!(
                        out,
                        "      error: id {} {}: {} ({} bytes left)",
                        example.id, example.name, example.error, example.bytes_left
                    );
                }
            }
        }
        let _ = writeln!(out);

        let _ = writeln!(out, "framing / format errors");
        if self.framing_issues.is_empty() && self.format_errors.is_empty() {
            let _ = writeln!(out, "  none");
        } else {
            for issue in &self.framing_issues {
                let _ = writeln!(
                    out,
                    "  connection {} {}: {} ({} bytes)",
                    issue.conn_id,
                    issue.dir.as_str(),
                    issue.message,
                    issue.bytes
                );
            }
            for error in &self.format_errors {
                let _ = writeln!(out, "  capture: {error}");
            }
        }
        let _ = writeln!(out);

        write_e3(&mut out, &self.e3);
        write_e4(&mut out, &self.e4);

        let _ = writeln!(out, "blocks at the end of the capture");
        if self.blocks.is_empty() {
            let _ = writeln!(out, "  none requested");
        } else {
            for answer in &self.blocks {
                let _ = writeln!(out, "  {}", block_line(answer));
            }
        }
        let _ = writeln!(out);

        let _ = writeln!(out, "{}", self.result_line());
        out
    }

    pub fn json(&self) -> Json {
        let mut obj = vec![
            ("capture", Json::str(self.capture_name.clone())),
            (
                "records",
                Json::obj(vec![
                    ("serverbound", Json::uint(self.records.serverbound as u64)),
                    ("clientbound", Json::uint(self.records.clientbound as u64)),
                    ("opened", Json::uint(self.records.opened as u64)),
                    ("closed", Json::uint(self.records.closed as u64)),
                ]),
            ),
            ("total_packets", Json::uint(self.total_packets)),
            ("total_errors", Json::uint(self.total_errors)),
            ("unconsumed_packets", Json::uint(self.unconsumed_packets)),
        ];

        let connections: Vec<Json> = self
            .connections
            .iter()
            .map(|connection| {
                Json::obj(vec![
                    ("conn_id", Json::uint(connection.conn_id as u64)),
                    (
                        "dimension",
                        connection
                            .dimension
                            .as_ref()
                            .map(dimension_json)
                            .unwrap_or(Json::Null),
                    ),
                    (
                        "chunks_loaded",
                        Json::uint(connection.chunks_loaded as u64),
                    ),
                    (
                        "player",
                        match connection.player {
                            Some(player) => Json::obj(vec![
                                ("x", Json::num(player.pos.x)),
                                ("y", Json::num(player.pos.y)),
                                ("z", Json::num(player.pos.z)),
                                ("yaw", Json::num(player.yaw as f64)),
                                ("pitch", Json::num(player.pitch as f64)),
                            ]),
                            None => Json::Null,
                        },
                    ),
                    (
                        "notes",
                        Json::Arr(
                            connection
                                .notes
                                .iter()
                                .map(|n| Json::str(n.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "stopped",
                        connection
                            .stopped
                            .as_ref()
                            .map(|s| Json::str(s.clone()))
                            .unwrap_or(Json::Null),
                    ),
                ])
            })
            .collect();
        obj.push(("connections", Json::Arr(connections)));

        let groups: Vec<Json> = self
            .groups
            .iter()
            .map(|((state, dir), group)| {
                Json::obj(vec![
                    ("state", Json::str(state.as_str().to_owned())),
                    ("dir", Json::str(dir.as_str().to_owned())),
                    ("packets", Json::uint(group.packets())),
                    ("errors", Json::uint(group.errors)),
                    ("unconsumed", Json::uint(group.unconsumed)),
                    (
                        "names",
                        Json::obj(
                            group
                                .counts
                                .iter()
                                .map(|(name, count)| (name.clone(), Json::uint(*count)))
                                .collect(),
                        ),
                    ),
                    (
                        "error_examples",
                        Json::Arr(
                            group
                                .examples
                                .iter()
                                .map(|example| {
                                    Json::obj(vec![
                                        ("conn_id", Json::uint(example.conn_id as u64)),
                                        ("id", Json::uint(example.id as u64)),
                                        ("name", Json::str(example.name.clone())),
                                        ("error", Json::str(example.error.clone())),
                                        ("bytes_left", Json::uint(example.bytes_left as u64)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        obj.push(("groups", Json::Arr(groups)));

        obj.push((
            "framing_issues",
            Json::Arr(
                self.framing_issues
                    .iter()
                    .map(|issue| {
                        Json::obj(vec![
                            ("conn_id", Json::uint(issue.conn_id as u64)),
                            ("dir", Json::str(issue.dir.as_str().to_owned())),
                            ("message", Json::str(issue.message.clone())),
                            ("bytes", Json::uint(issue.bytes as u64)),
                        ])
                    })
                    .collect(),
            ),
        ));
        obj.push((
            "format_errors",
            Json::Arr(self.format_errors.iter().map(|e| Json::str(e.clone())).collect()),
        ));

        obj.push(("e3", e3_json(&self.e3)));
        obj.push(("e4", e4_json(&self.e4)));
        obj.push((
            "blocks",
            Json::Arr(self.blocks.iter().map(block_json).collect()),
        ));
        obj.push((
            "e2_result",
            Json::str(if self.passed() { "PASS" } else { "FAIL" }.to_owned()),
        ));

        Json::obj(obj)
    }

    /// One line per packet, for tooling (`--packets-json` is not in the brief,
    /// so this is only used by tests and available programmatically).
    pub fn packet_lines(&self) -> Vec<String> {
        self.packets
            .iter()
            .map(|packet| {
                format!(
                    "{} {} {} #{} {} {}B{}",
                    packet.conn_id,
                    packet.state.as_str(),
                    packet.dir.as_str(),
                    packet.id,
                    packet.name,
                    packet.size,
                    match &packet.error {
                        Some(error) => format!(" ERROR {error}"),
                        None => String::new(),
                    }
                )
            })
            .collect()
    }
}

fn dimension_line(dimension: &DimensionInfo) -> String {
    format!(
        "{} (dimension_type {}, height {}, min_y {}, from {})",
        dimension.dimension,
        dimension.dimension_type,
        dimension.height,
        dimension.min_y,
        dimension.source
    )
}

fn dimension_json(dimension: &DimensionInfo) -> Json {
    Json::obj(vec![
        ("dimension", Json::str(dimension.dimension.clone())),
        (
            "dimension_type",
            Json::str(dimension.dimension_type.clone()),
        ),
        ("source", Json::str(dimension.source.to_owned())),
        ("height", Json::uint(dimension.height as u64)),
        ("min_y", Json::int(dimension.min_y as i64)),
    ])
}

fn block_line(answer: &BlockAnswer) -> String {
    let pos = format!("{} {} {}", answer.pos.x, answer.pos.y, answer.pos.z);
    match (&answer.block, answer.conn_id) {
        (Some(block), Some(conn_id)) => {
            format!("{pos} -> {} (connection {conn_id})", block.display())
        }
        _ => format!(
            "{pos} -> unknown ({})",
            answer.note.as_deref().unwrap_or("not available")
        ),
    }
}

fn block_json(answer: &BlockAnswer) -> Json {
    let (name, properties) = match &answer.block {
        Some(block) => (
            Json::str(block.name.clone()),
            Json::obj(
                block
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), Json::str(v.clone())))
                    .collect(),
            ),
        ),
        None => (Json::Null, Json::Null),
    };
    Json::obj(vec![
        ("x", Json::int(answer.pos.x as i64)),
        ("y", Json::int(answer.pos.y as i64)),
        ("z", Json::int(answer.pos.z as i64)),
        (
            "conn_id",
            answer
                .conn_id
                .map(|id| Json::uint(id as u64))
                .unwrap_or(Json::Null),
        ),
        ("block", name),
        ("properties", properties),
        (
            "note",
            answer
                .note
                .as_ref()
                .map(|n| Json::str(n.clone()))
                .unwrap_or(Json::Null),
        ),
    ])
}

fn percentiles_text(label: &str, p: &PercentilesUs) -> String {
    if p.count == 0 {
        format!("  {label:<20} n=0")
    } else {
        format!(
            "  {label:<20} n={:<7} p50={:.3} p99={:.3} p99.9={:.3} max={:.3}",
            p.count, p.p50, p.p99, p.p99_9, p.max
        )
    }
}

fn write_e3(out: &mut String, e3: &E3Stats) {
    let _ = writeln!(out, "E3 decode time (microseconds)");
    let _ = writeln!(out, "{}", percentiles_text("all packets", &e3.decode_all));
    let _ = writeln!(
        out,
        "{}",
        percentiles_text("game steady state", &e3.decode_game_steady)
    );
    let _ = writeln!(
        out,
        "{}",
        percentiles_text("game chunk packets", &e3.decode_game_chunk)
    );
    let _ = writeln!(out, "E3 world apply time (microseconds)");
    let _ = writeln!(out, "{}", percentiles_text("chunk apply", &e3.apply_chunk));
    let _ = writeln!(
        out,
        "{}",
        percentiles_text("block apply", &e3.apply_block)
    );
    let _ = writeln!(out);
}

fn gaps_text(gaps: &crate::analysis::GapBins) -> String {
    format!(
        "0-10={} 10-40={} 40-60={} 60-100={} 100-1000={} >1000={}",
        gaps.b0_10, gaps.b10_40, gaps.b40_60, gaps.b60_100, gaps.b100_1000, gaps.b_gt1000
    )
}

fn move_line(label: &str, stats: &MoveStats) -> String {
    format!(
        "  {label:<26} count={:<7} {:.3}/s   gaps(ms): {}",
        stats.count,
        stats.per_second,
        gaps_text(&stats.gaps)
    )
}

fn write_e4(out: &mut String, e4: &E4Stats) {
    let _ = writeln!(
        out,
        "E4 serverbound game packets (game state {:.3} s)",
        e4.game_duration_s
    );
    for name in crate::analysis::MOVE_PACKET_NAMES {
        let stats = e4.per_name.get(name).cloned().unwrap_or_default();
        let _ = writeln!(out, "{}", move_line(name, &stats));
    }
    let _ = writeln!(out, "{}", move_line("all pose packets", &e4.all_moves));
    let _ = writeln!(out);
}

fn e3_json(e3: &E3Stats) -> Json {
    let p = |value: &PercentilesUs| {
        Json::obj(vec![
            ("count", Json::uint(value.count as u64)),
            ("p50", Json::num(value.p50)),
            ("p99", Json::num(value.p99)),
            ("p99_9", Json::num(value.p99_9)),
            ("max", Json::num(value.max)),
        ])
    };
    Json::obj(vec![
        (
            "decode_us",
            Json::obj(vec![
                ("all", p(&e3.decode_all)),
                ("game_steady", p(&e3.decode_game_steady)),
                ("game_chunk", p(&e3.decode_game_chunk)),
            ]),
        ),
        (
            "world_apply_us",
            Json::obj(vec![
                ("chunk", p(&e3.apply_chunk)),
                ("block", p(&e3.apply_block)),
            ]),
        ),
    ])
}

fn e4_json(e4: &E4Stats) -> Json {
    let move_json = |stats: &MoveStats| {
        Json::obj(vec![
            ("count", Json::uint(stats.count)),
            ("per_second", Json::num(stats.per_second)),
            (
                "gaps_ms",
                Json::obj(vec![
                    ("0-10", Json::uint(stats.gaps.b0_10)),
                    ("10-40", Json::uint(stats.gaps.b10_40)),
                    ("40-60", Json::uint(stats.gaps.b40_60)),
                    ("60-100", Json::uint(stats.gaps.b60_100)),
                    ("100-1000", Json::uint(stats.gaps.b100_1000)),
                    (">1000", Json::uint(stats.gaps.b_gt1000)),
                ]),
            ),
        ])
    };
    Json::obj(vec![
        ("game_duration_s", Json::num(e4.game_duration_s)),
        (
            "per_packet",
            Json::obj(
                e4.per_name
                    .iter()
                    .map(|(name, stats)| (name.clone(), move_json(stats)))
                    .collect(),
            ),
        ),
        ("all_move_packets", move_json(&e4.all_moves)),
    ])
}

// ---------------------------------------------------------------------------
// A tiny JSON writer so the spike carries no extra dependency
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(value: String) -> Self {
        Json::Str(value)
    }

    pub fn uint(value: u64) -> Self {
        Json::UInt(value)
    }

    pub fn int(value: i64) -> Self {
        Json::Int(value)
    }

    pub fn num(value: f64) -> Self {
        Json::Float(value)
    }

    pub fn obj(fields: Vec<(impl Into<String>, Json)>) -> Self {
        Json::Obj(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Json::UInt(value) => {
                let _ = write!(out, "{value}");
            }
            Json::Float(value) => {
                if value.is_finite() {
                    let _ = write!(out, "{value:.3}");
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(value) => write_json_string(out, value),
            Json::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    push_indent(out, indent + 1);
                    item.write(out, indent + 1);
                    if index + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, indent);
                out.push(']');
            }
            Json::Obj(fields) => {
                if fields.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (index, (key, value)) in fields.iter().enumerate() {
                    push_indent(out, indent + 1);
                    write_json_string(out, key);
                    out.push_str(": ");
                    value.write(out, indent + 1);
                    if index + 1 < fields.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, indent);
                out.push('}');
            }
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Only used by tests: a compact per-packet dump.
pub fn packets_json(packets: &[PacketRecord]) -> Json {
    Json::Arr(
        packets
            .iter()
            .map(|packet| {
                Json::obj(vec![
                    ("conn_id", Json::uint(packet.conn_id as u64)),
                    ("state", Json::str(packet.state.as_str().to_owned())),
                    ("dir", Json::str(packet.dir.as_str().to_owned())),
                    ("id", Json::uint(packet.id as u64)),
                    ("name", Json::str(packet.name.clone())),
                    ("size", Json::uint(packet.size as u64)),
                    ("bytes_left", Json::uint(packet.bytes_left as u64)),
                    (
                        "error",
                        packet
                            .error
                            .as_ref()
                            .map(|e| Json::str(e.clone()))
                            .unwrap_or(Json::Null),
                    ),
                    ("decode_ns", Json::uint(packet.decode_ns)),
                ])
            })
            .collect(),
    )
}
