//! The decoder: tee records in, typed packets out.
//!
//! ```text
//! praxion_tee::Record  ->  Framer (one per direction)  ->  decompression  ->  Azalea packet enum
//! ```
//!
//! One [`Decoder`] holds every connection in the stream, because the tee
//! interleaves them. Each direction has its own protocol state and its own
//! framer; compression belongs to the connection. A framing error, a
//! decompression error, a packet Azalea cannot read or a tee gap marks the
//! connection [`Desync`]ed. After that the connection produces no packets until
//! a new `Opened` record starts a fresh state.
//!
//! See `docs/decisions.md` 2026-11-01 ("Tee stream format") and
//! `docs/architecture.md` §4 (Decoder).

mod decode;
mod decompress;
mod framing;
mod packet;

use std::collections::BTreeMap;
use std::time::Instant;

use azalea_protocol::packets::{
    ClientIntention,
    config::ClientboundConfigPacket,
    game::ClientboundGamePacket,
    login::{ClientboundLoginPacket, ServerboundLoginPacket},
};
use praxion_tee::{Kind, Record};

pub use framing::{FrameError, Framer, MAX_FRAME_LEN};
pub use packet::{Dir, Packet, State};

/// One packet that Azalea read completely.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// Timestamp of the record that completed the frame.
    pub ts_ns: u64,
    pub conn_id: u32,
    pub dir: Dir,
    pub state: State,
    pub packet: Packet,
}

/// A connection lost framing or packet decoding and can no longer be trusted.
#[derive(Clone, Debug)]
pub struct Desync {
    pub ts_ns: u64,
    pub conn_id: u32,
    pub reason: String,
}

/// What one tee record produced.
///
/// A decoded packet is far larger than the connection events; boxing it would
/// change every `Event::Decoded` construction and pattern in the workspace for
/// one pointer-sized saving per event.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Event {
    Opened { ts_ns: u64, conn_id: u32 },
    Closed { ts_ns: u64, conn_id: u32 },
    Decoded(Decoded),
    Desync(Desync),
}

/// Per-connection counters.
#[derive(Clone, Debug, Default)]
pub struct ConnStats {
    /// Frames that left the framer, whether or not they decoded.
    pub frames: u64,
    /// Raw connection bytes in both directions, as the tee delivered them.
    pub bytes: u64,
    /// Packets decoded successfully.
    pub packets: u64,
    /// Decode failures keyed by packet id (`u32::MAX` when the id itself was
    /// unreadable, and for decompression errors).
    pub decode_failures: BTreeMap<u32, u64>,
}

#[derive(Debug)]
struct DirectionState {
    state: State,
    framer: Framer,
}

impl DirectionState {
    fn new() -> Self {
        Self {
            state: State::Handshake,
            framer: Framer::new(),
        }
    }
}

#[derive(Debug)]
struct Connection {
    serverbound: DirectionState,
    clientbound: DirectionState,
    compression: Option<u32>,
    desynced: Option<String>,
    closed: bool,
    last_ts: u64,
    /// Complete frames waiting for older frames to arrive, keyed by
    /// `(timestamp, completion order)`.
    pending: BTreeMap<(u64, u64), (Dir, Vec<u8>)>,
    next_frame_seq: u64,
    stats: ConnStats,
}

impl Connection {
    fn new() -> Self {
        Self {
            serverbound: DirectionState::new(),
            clientbound: DirectionState::new(),
            compression: None,
            desynced: None,
            closed: false,
            last_ts: 0,
            pending: BTreeMap::new(),
            next_frame_seq: 0,
            stats: ConnStats::default(),
        }
    }

    fn direction(&self, dir: Dir) -> &DirectionState {
        match dir {
            Dir::Serverbound => &self.serverbound,
            Dir::Clientbound => &self.clientbound,
        }
    }

    fn direction_mut(&mut self, dir: Dir) -> &mut DirectionState {
        match dir {
            Dir::Serverbound => &mut self.serverbound,
            Dir::Clientbound => &mut self.clientbound,
        }
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

/// The decoder for one tee stream.
/// Frames are reordered by timestamp for this long before they are decoded.
/// The tee writes records from two forwarding threads, so a server reply can
/// reach the file a few milliseconds before the request it answers. Five
/// milliseconds hides that without adding visible latency to a live stream.
pub const DEFAULT_REORDER_WINDOW_NS: u64 = 5_000_000;

#[derive(Debug)]
pub struct Decoder {
    connections: BTreeMap<u32, Connection>,
    timings: Option<Vec<u64>>,
    reorder_ns: u64,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            connections: BTreeMap::new(),
            timings: None,
            reorder_ns: DEFAULT_REORDER_WINDOW_NS,
        }
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also record how long each packet's Azalea decode took, in nanoseconds.
    /// Off by default: the body measures the end-to-end path, the CLI compares
    /// against the spike.
    pub fn with_timings() -> Self {
        Self { timings: Some(Vec::new()), ..Self::default() }
    }

    /// How long to hold a frame for an older one with a smaller timestamp.
    /// Offline replay of whole captures can use a value larger than the capture
    /// (for example 60 s); a live stream should keep the default.
    pub fn set_reorder_window(&mut self, ns: u64) {
        self.reorder_ns = ns;
    }

    /// Per-packet decode times, when [`Decoder::with_timings`] was used.
    pub fn decode_timings(&self) -> Option<&[u64]> {
        self.timings.as_deref()
    }

    /// Counters per connection id, including connections that closed. This is
    /// a snapshot: the counters keep changing as more records arrive.
    pub fn stats(&self) -> BTreeMap<u32, ConnStats> {
        self.connections
            .iter()
            .map(|(id, connection)| (*id, connection.stats.clone()))
            .collect()
    }

    /// Feed one tee record and return everything it produced.
    pub fn feed(&mut self, record: &Record) -> Vec<Event> {
        if let Some(connection) = self.connections.get_mut(&record.conn_id) {
            connection.last_ts = connection.last_ts.max(record.ts_ns);
        }
        match &record.kind {
            Kind::Opened => {
                let connection = self.connections.entry(record.conn_id).or_default();
                *connection = Connection::new();
                connection.last_ts = record.ts_ns;
                vec![Event::Opened { ts_ns: record.ts_ns, conn_id: record.conn_id }]
            }
            Kind::Closed => self.closed(record),
            Kind::Gap { dropped_bytes } => {
                // A gap can arrive before any `Opened` record when the tee
                // subscriber joined an already-open connection.
                let connection = self.connections.entry(record.conn_id).or_default();
                connection.last_ts = connection.last_ts.max(record.ts_ns);
                // Frames completed before the gap are still sound.
                let mut events = self.drain_ready(record.conn_id, true);
                events.extend(self.desync(
                    record.ts_ns,
                    record.conn_id,
                    format!("tee gap: {dropped_bytes} bytes were dropped"),
                ));
                events
            }
            Kind::Serverbound => self.data(record, Dir::Serverbound),
            Kind::Clientbound => self.data(record, Dir::Clientbound),
        }
    }

    /// Report connections that were still inside a frame when the stream
    /// ended. A live decoder never needs this; the CLI calls it at EOF.
    pub fn finish(&mut self) -> Vec<Event> {
        let all: Vec<u32> = self.connections.keys().copied().collect();
        let mut events = Vec::new();
        for id in &all {
            events.extend(self.drain_ready(*id, true));
        }

        let ids: Vec<u32> = self
            .connections
            .iter()
            .filter(|(_, c)| !c.closed && c.desynced.is_none())
            .filter(|(_, c)| !c.serverbound.framer.is_empty() || !c.clientbound.framer.is_empty())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let (ts, pending) = self
                .connections
                .get(&id)
                .map(|c| (c.last_ts, c.serverbound.framer.pending() + c.clientbound.framer.pending()))
                .unwrap_or((0, 0));
            events.extend(self.desync(
                ts,
                id,
                format!("end of stream inside a frame ({pending} bytes pending)"),
            ));
        }
        events
    }

    fn data(&mut self, record: &Record, dir: Dir) -> Vec<Event> {
        let connection = self.connections.entry(record.conn_id).or_default();
        connection.last_ts = connection.last_ts.max(record.ts_ns);
        connection.stats.bytes += record.data.len() as u64;
        if connection.desynced.is_some() || connection.closed {
            return Vec::new();
        }

        let mut frames = Vec::new();
        let mut error = None;
        {
            let direction = connection.direction_mut(dir);
            direction.framer.push(&record.data);
            loop {
                match direction.framer.pop_frame() {
                    Ok(Some(frame)) => frames.push(frame),
                    Ok(None) => break,
                    Err(e) => {
                        error = Some(e);
                        direction.framer.reset();
                        break;
                    }
                }
            }
        }

        let mut events = Vec::new();
        if let Some(connection) = self.connections.get_mut(&record.conn_id) {
            for frame in frames {
                let seq = connection.next_frame_seq;
                connection.next_frame_seq += 1;
                connection.pending.insert((record.ts_ns, seq), (dir, frame));
            }
        }
        events.extend(self.drain_ready(record.conn_id, false));

        if let Some(error) = error {
            // The frames before the bad bytes are still valid; decode them in
            // timestamp order, then stop believing the connection.
            events.extend(self.drain_ready(record.conn_id, true));
            events.extend(self.desync(record.ts_ns, record.conn_id, error.to_string()));
        }
        events
    }

    /// Decode pending frames whose predecessors can no longer arrive, or all
    /// of them when `flush` is set.
    fn drain_ready(&mut self, conn_id: u32, flush: bool) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(connection) = self.connections.get(&conn_id) {
            if connection.pending.is_empty() {
                break;
            }
            let clientbound_handshake = connection.clientbound.state == State::Handshake;

            // A clientbound frame before the serverbound handshake is not a
            // packet in the Handshake state: it is a status/login reply whose
            // record reached the file before the client's request (the old
            // proxy did this). Hold it until the handshake moves the state on.
            let mut ready = None;
            let mut blocked = None;
            for (&(ts, seq), (dir, _)) in &connection.pending {
                let key = (ts, seq);
                if *dir == Dir::Clientbound && clientbound_handshake {
                    blocked.get_or_insert(key);
                    continue;
                }
                if !flush && ts.saturating_add(self.reorder_ns) >= connection.last_ts {
                    // The first non-blocked frame is not old enough yet, and
                    // later ones are even newer.
                    break;
                }
                ready = Some(key);
                break;
            }
            let selected = match ready {
                Some(key) => key,
                None if flush => match blocked {
                    Some(key) => key,
                    None => break,
                },
                None => break,
            };
            let (ts, seq) = selected;
            let Some((dir, wire)) =
                self.connections.get_mut(&conn_id).and_then(|c| c.pending.remove(&(ts, seq)))
            else {
                break;
            };
            let produced = self.process_frame(conn_id, dir, ts, wire);
            let failed = produced.iter().any(|e| matches!(e, Event::Desync(_)));
            events.extend(produced);
            if failed {
                if let Some(connection) = self.connections.get_mut(&conn_id) {
                    connection.pending.clear();
                }
                break;
            }
        }
        events
    }

    fn closed(&mut self, record: &Record) -> Vec<Event> {
        let connection = self.connections.entry(record.conn_id).or_default();
        connection.last_ts = connection.last_ts.max(record.ts_ns);
        let mut events = self.drain_ready(record.conn_id, true);

        let Some(connection) = self.connections.get(&record.conn_id) else {
            events.push(Event::Closed { ts_ns: record.ts_ns, conn_id: record.conn_id });
            return events;
        };
        let pending = connection.serverbound.framer.pending() + connection.clientbound.framer.pending();
        if pending > 0 && connection.desynced.is_none() {
            events.extend(self.desync(
                record.ts_ns,
                record.conn_id,
                format!("connection closed inside a frame ({pending} bytes pending)"),
            ));
        }
        if let Some(connection) = self.connections.get_mut(&record.conn_id) {
            connection.closed = true;
            connection.serverbound.framer.reset();
            connection.clientbound.framer.reset();
            connection.pending.clear();
        }
        events.push(Event::Closed { ts_ns: record.ts_ns, conn_id: record.conn_id });
        events
    }

    fn process_frame(
        &mut self,
        conn_id: u32,
        dir: Dir,
        ts_ns: u64,
        wire: Vec<u8>,
    ) -> Vec<Event> {
        let Some(connection) = self.connections.get(&conn_id) else {
            return Vec::new();
        };
        if connection.desynced.is_some() {
            return Vec::new();
        }
        let state = connection.direction(dir).state;
        let compression = connection.compression;
        if let Some(connection) = self.connections.get_mut(&conn_id) {
            connection.stats.frames += 1;
        }

        let raw = match compression {
            None => wire,
            Some(threshold) => match decompress::decompress(&wire, threshold) {
                Ok(raw) => raw,
                Err(error) => {
                    // Text is either "decompressed size mismatch" or
                    // "decompression failed: ..."; a frame whose stream does not
                    // produce exactly the declared size cannot be trusted.
                    self.count_failure(conn_id, u32::MAX);
                    return self.desync(ts_ns, conn_id, error.to_string());
                }
            },
        };
        let started = self.timings.as_ref().map(|_| Instant::now());
        let decoded = decode::decode(state, dir, &raw);
        if let (Some(start), Some(timings)) = (started, self.timings.as_mut()) {
            timings.push(start.elapsed().as_nanos() as u64);
        }

        let (packet, consumed) = match decoded {
            Ok(decoded) => decoded,
            Err(error) => {
                let reason = format!(
                    "packet id {}: {} ({} bytes left unread)",
                    error.id, error.message, error.bytes_left
                );
                self.count_failure(conn_id, error.id);
                return self.desync(ts_ns, conn_id, reason);
            }
        };
        if consumed != raw.len() {
            let left = unread_bytes(raw.len(), consumed);
            let reason = format!(
                "{} left {left} of {} bytes unread",
                packet.name(),
                raw.len()
            );
            self.count_failure(conn_id, packet.id());
            return self.desync(ts_ns, conn_id, reason);
        }

        let transition = transition(&packet);
        if let Some(connection) = self.connections.get_mut(&conn_id) {
            connection.stats.packets += 1;
        }
        let mut events = vec![Event::Decoded(Decoded {
            ts_ns,
            conn_id,
            dir,
            state,
            packet,
        })];

        match transition {
            Transition::None => {}
            Transition::Both(next) => {
                if let Some(connection) = self.connections.get_mut(&conn_id) {
                    connection.serverbound.state = next;
                    connection.clientbound.state = next;
                }
            }
            Transition::Clientbound(next) => {
                if let Some(connection) = self.connections.get_mut(&conn_id) {
                    connection.clientbound.state = next;
                }
            }
            Transition::Serverbound(next) => {
                if let Some(connection) = self.connections.get_mut(&conn_id) {
                    connection.serverbound.state = next;
                }
            }
            Transition::Compression(threshold) => {
                // The threshold is a VarInt the server picks. Vanilla keeps the
                // raw int and compares sizes against it; a negative value has no
                // protocol meaning we can honour, so the connection stops here
                // instead of silently decoding later frames uncompressed.
                if threshold < 0 {
                    events.extend(self.desync(
                        ts_ns,
                        conn_id,
                        format!("server set a negative compression threshold ({threshold})"),
                    ));
                } else if let Some(connection) = self.connections.get_mut(&conn_id) {
                    connection.compression = u32::try_from(threshold).ok();
                }
            }
            Transition::Encryption => {
                events.extend(self.desync(
                    ts_ns,
                    conn_id,
                    "server requested online-mode encryption; later bytes are encrypted".to_owned(),
                ));
            }
        }
        events
    }

    fn count_failure(&mut self, conn_id: u32, id: u32) {
        if let Some(connection) = self.connections.get_mut(&conn_id) {
            *connection.stats.decode_failures.entry(id).or_insert(0) += 1;
        }
    }

    fn desync(&mut self, ts_ns: u64, conn_id: u32, reason: String) -> Vec<Event> {
        let Some(connection) = self.connections.get_mut(&conn_id) else {
            return Vec::new();
        };
        if connection.desynced.is_some() {
            return Vec::new();
        }
        connection.desynced = Some(reason.clone());
        connection.serverbound.framer.reset();
        connection.clientbound.framer.reset();
        connection.pending.clear();
        vec![Event::Desync(Desync { ts_ns, conn_id, reason })]
    }
}

#[derive(Clone, Copy, Debug)]
enum Transition {
    None,
    Both(State),
    Clientbound(State),
    Serverbound(State),
    Compression(i32),
    Encryption,
}

#[cfg(test)]
mod tests;

/// Bytes a frame still holds after the packet inside it ended.
///
/// `consumed` is a cursor position over the frame, so it cannot exceed
/// `frame_len`; the saturation only keeps a violated invariant from turning a
/// desync diagnostic into a panic.
fn unread_bytes(frame_len: usize, consumed: usize) -> usize {
    frame_len.saturating_sub(consumed)
}

fn transition(packet: &Packet) -> Transition {
    match packet {
        Packet::HandshakeServerbound(
            azalea_protocol::packets::handshake::ServerboundHandshakePacket::Intention(p),
        ) => Transition::Both(match p.intention {
            ClientIntention::Status => State::Status,
            // Transfer (3) starts a login on the new server too.
            ClientIntention::Login | ClientIntention::Transfer => State::Login,
        }),
        Packet::LoginClientbound(ClientboundLoginPacket::LoginCompression(p)) => {
            Transition::Compression(p.compression_threshold)
        }
        Packet::LoginClientbound(ClientboundLoginPacket::LoginFinished(_)) => {
            Transition::Clientbound(State::Configuration)
        }
        Packet::LoginClientbound(ClientboundLoginPacket::Hello(_)) => Transition::Encryption,
        Packet::LoginServerbound(ServerboundLoginPacket::LoginAcknowledged(_)) => {
            Transition::Serverbound(State::Configuration)
        }
        Packet::LoginServerbound(ServerboundLoginPacket::Key(_)) => Transition::Encryption,
        Packet::ConfigurationClientbound(ClientboundConfigPacket::FinishConfiguration(_)) => {
            Transition::Clientbound(State::Play)
        }
        Packet::ConfigurationServerbound(
            azalea_protocol::packets::config::ServerboundConfigPacket::FinishConfiguration(_),
        ) => Transition::Serverbound(State::Play),
        Packet::PlayClientbound(ClientboundGamePacket::StartConfiguration(_)) => {
            Transition::Clientbound(State::Configuration)
        }
        Packet::PlayServerbound(
            azalea_protocol::packets::game::ServerboundGamePacket::ConfigurationAcknowledged(_),
        ) => Transition::Serverbound(State::Configuration),
        _ => Transition::None,
    }
}
