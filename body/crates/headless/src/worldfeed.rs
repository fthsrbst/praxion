//! The world feed: the body's own tee subscriber, decoder and raw store.
//!
//! In live play the body runs the tee subscriber -> decoder -> world pipeline and
//! the actuator reads that world for collision and fluids. This module is that
//! pipeline for one process, so the headless binary can run with `--tee` before
//! the full body binary exists. It is a blocking thread on purpose: the session
//! owns the socket thread and the tick loop, and the world is shared behind a
//! mutex that the tick loop holds for one physics step at a time.
//!
//! ```text
//! TCP (tee)  ->  praxion_tee::Reader  ->  praxion_decoder::Decoder  ->  praxion_world::World
//! ```
//!
//! The followed connection is the one whose Play `login` packet arrives, the
//! same rule as `praxion-decode`. A desync marks the world invalid, which makes
//! the tick loop refuse to move.

use std::io::BufReader;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use praxion_decoder::{Decoder, Event, Packet};
use praxion_tee::Reader;
use praxion_world::World;

/// Bound on reaching the forwarder's tee listener.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The raw store, fed from the tee by its own thread.
pub struct WorldFeed {
    world: Arc<Mutex<World>>,
}

impl WorldFeed {
    /// Subscribe to the tee at `addr`.
    ///
    /// Blocks until the stream header has arrived, so once this returns the
    /// subscriber is registered and a game connection opened afterwards cannot
    /// be missed.
    pub fn connect(addr: SocketAddr) -> Result<Self, String> {
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|error| format!("cannot reach the tee at {addr}: {error}"))?;
        stream
            .set_nodelay(true)
            .map_err(|error| format!("tee socket: {error}"))?;
        let reader = Reader::new(BufReader::new(stream))
            .map_err(|error| format!("tee stream at {addr}: {error}"))?;

        let world = Arc::new(Mutex::new(World::new()));
        let thread_world = Arc::clone(&world);
        // The reader thread is detached: it blocks on the tee socket and ends
        // with the process. A stalled tee must not be able to hang its owner.
        thread::Builder::new()
            .name("praxion-world-feed".to_owned())
            .spawn(move || run(reader, thread_world))
            .map_err(|error| format!("cannot start the world feed thread: {error}"))?;

        Ok(Self { world })
    }

    /// The world as of the last record the decoder applied.
    pub fn lock(&self) -> MutexGuard<'_, World> {
        self.world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn run<R: std::io::Read>(mut reader: Reader<R>, world: Arc<Mutex<World>>) {
    let mut decoder = Decoder::new();
    let mut followed = None;
    for record in &mut reader {
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                eprintln!("world feed: {error}");
                return;
            }
        };
        for event in decoder.feed(&record) {
            apply(&world, &mut followed, &event);
        }
    }
}

fn apply(world: &Arc<Mutex<World>>, followed: &mut Option<u32>, event: &Event) {
    let mut world = world
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match event {
        Event::Opened { conn_id, .. } => world.opened(*conn_id),
        Event::Closed { conn_id, .. } => world.closed(*conn_id),
        Event::Desync(desync) => {
            eprintln!(
                "world feed: connection {} desynchronised: {}",
                desync.conn_id, desync.reason
            );
            world.invalidate(desync.conn_id, desync.reason.clone());
        }
        Event::Decoded(decoded) => match &decoded.packet {
            Packet::ConfigurationClientbound(packet) => {
                world.apply_clientbound_config(decoded.conn_id, packet);
            }
            Packet::PlayClientbound(packet) => {
                if decoded.packet.name() == "login" && *followed != Some(decoded.conn_id) {
                    world.follow(decoded.conn_id);
                    *followed = Some(decoded.conn_id);
                }
                world.apply_clientbound_play(decoded.conn_id, packet);
            }
            Packet::PlayServerbound(packet) => {
                world.apply_serverbound_play(decoded.conn_id, packet);
            }
            _ => {}
        },
    }
}
