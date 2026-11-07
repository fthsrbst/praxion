//! The tee: a bounded queue between the forwarding threads and one subscriber (the body).
//!
//! The forwarding threads never wait here. Handing a chunk to the tee is a lock, an optional
//! copy, and a push onto a queue measured in bytes; if the queue is full or no subscriber is
//! connected the chunk is counted as dropped for its connection instead. A separate writer thread
//! (the only thread that touches the subscriber socket) drains the queue. A subscriber that stops
//! reading therefore stalls only that writer, never the game.
//!
//! A new subscriber replaces the old one. It receives the stream header, then one `Gap` for every
//! connection that is already open (it joined mid-stream and cannot decode from there), then the
//! live records. Each connection gets at most one `Gap` before its next record that fits.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use praxion_tee::{now_ns, write_header, write_record, Kind, Record, RECORD_HEAD_LEN};

use crate::stats::{Event, Stats};

/// Bytes a gap record occupies on the wire.
const GAP_LEN: usize = RECORD_HEAD_LEN + 8;

struct State {
    queue: VecDeque<Item>,
    queued_bytes: usize,
    /// A clone of the subscriber socket, kept only so a replacement can shut it down even when
    /// the writer is blocked inside a write to it.
    subscriber: Option<TcpStream>,
    generation: u64,
    /// Bytes dropped since the last gap, per connection.
    pending: HashMap<u32, u64>,
    /// Open game connections and their forwarded-byte counters.
    conns: HashMap<u32, Arc<AtomicU64>>,
}

struct Item {
    generation: u64,
    bytes: usize,
    kind: ItemKind,
}

enum ItemKind {
    Record(Record),
    Subscriber {
        stream: TcpStream,
        gaps: Vec<(u32, u64)>,
    },
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    max_bytes: usize,
    /// Test-only fault injection: fail the next `subscribe` clone.
    #[cfg(test)]
    fail_subscriber_clone: AtomicBool,
    /// Test-only fault injection: panic while handling the next queued writer item.
    #[cfg(test)]
    panic_writer_item: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Cloneable handle to the tee.
#[derive(Clone)]
pub struct Tee {
    shared: Arc<Shared>,
    stats: Stats,
}

impl Tee {
    /// Start the subscriber listener and the writer thread. Neither can stop the process: a panic
    /// is caught and the thread restarts. Until a subscriber connects, every chunk is a gap.
    pub fn start(listener: TcpListener, max_bytes: usize, stats: Stats) -> Self {
        let tee = Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    queue: VecDeque::new(),
                    queued_bytes: 0,
                    subscriber: None,
                    generation: 0,
                    pending: HashMap::new(),
                    conns: HashMap::new(),
                }),
                cv: Condvar::new(),
                max_bytes,
                #[cfg(test)]
                fail_subscriber_clone: AtomicBool::new(false),
                #[cfg(test)]
                panic_writer_item: AtomicBool::new(false),
            }),
            stats,
        };

        let writer = tee.clone();
        let mut first_start = true;
        supervise("tee-writer", move || {
            let recovering = !std::mem::take(&mut first_start);
            writer.writer_body(recovering);
        });

        let acceptor = tee.clone();
        supervise("tee-listener", move || acceptor.listener_body(&listener));

        tee
    }

    /// A game connection was accepted: register it and emit `Opened`.
    pub fn opened(&self, conn: u32) {
        let mut state = self.shared.lock();
        state
            .conns
            .insert(conn, Arc::new(AtomicU64::new(0)));
        self.queue_control(
            &mut state,
            Record {
                ts_ns: now_ns(),
                conn_id: conn,
                kind: Kind::Opened,
                data: Vec::new(),
            },
        );
        drop(state);
        self.shared.cv.notify_one();
    }

    /// A game connection ended: emit `Closed` and forget its counters.
    pub fn closed(&self, conn: u32) {
        let mut state = self.shared.lock();
        self.queue_control(
            &mut state,
            Record {
                ts_ns: now_ns(),
                conn_id: conn,
                kind: Kind::Closed,
                data: Vec::new(),
            },
        );
        state.conns.remove(&conn);
        state.pending.remove(&conn);
        drop(state);
        self.shared.cv.notify_one();
    }

    /// Hand one forwarded chunk to the tee. Never waits for the subscriber or the writer.
    ///
    /// `ts_ns` is when the read that produced the chunk returned. Must be called while the chunk's
    /// connection is open.
    pub fn forwarded(&self, conn: u32, ts_ns: u64, kind: Kind, data: &[u8]) {
        let mut state = self.shared.lock();
        if let Some(counter) = state.conns.get(&conn) {
            counter.fetch_add(data.len() as u64, Ordering::Relaxed);
        }
        if state.subscriber.is_none() {
            if !data.is_empty() {
                self.drop_bytes(&mut state, conn, data.len() as u64);
            }
            return;
        }
        let record_len = RECORD_HEAD_LEN + data.len();
        let pending = state.pending.get(&conn).copied().unwrap_or(0);
        let needed = record_len + if pending > 0 { GAP_LEN } else { 0 };
        if state.queued_bytes + needed > self.shared.max_bytes {
            self.drop_bytes(&mut state, conn, data.len() as u64);
            return;
        }
        self.emit_pending_gap(&mut state, conn);
        let generation = state.generation;
        state.queued_bytes += record_len;
        state.queue.push_back(Item {
            generation,
            bytes: record_len,
            kind: ItemKind::Record(Record {
                ts_ns,
                conn_id: conn,
                kind,
                data: data.to_vec(),
            }),
        });
        drop(state);
        self.shared.cv.notify_one();
    }

    /// A subscriber asked to receive the stream. `peer` is for the log only.
    ///
    /// The fallible step (cloning the socket for the writer) happens before any shared state is
    /// touched: a failed clone leaves the old subscriber, the generation and the pending drops
    /// exactly as they were.
    fn subscribe(&self, stream: TcpStream, peer: &str) {
        let _ = stream.set_nodelay(true);
        let _ = stream.set_nonblocking(false);
        let size = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| peer.into());
        let clone = match self.clone_subscriber(&stream) {
            Ok(clone) => clone,
            Err(error) => {
                log::warn!("forwarder: tee subscriber {size} cannot be cloned: {error}");
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
        };
        let replaced = {
            let mut state = self.shared.lock();
            let replaced = state.subscriber.is_some();
            if let Some(old) = state.subscriber.take() {
                let _ = old.shutdown(Shutdown::Both);
            }
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            let gaps: Vec<(u32, u64)> = state
                .conns
                .iter()
                .map(|(conn, counter)| (*conn, counter.load(Ordering::Relaxed)))
                .collect();
            // The gaps below already account for everything forwarded so far, so no connection
            // carries an old drop into this subscriber's stream.
            state.pending.clear();
            state.subscriber = Some(clone);
            state.queue.push_back(Item {
                generation,
                bytes: 0,
                kind: ItemKind::Subscriber { stream, gaps },
            });
            replaced
        };
        self.shared.cv.notify_one();
        self.stats.send(Event::TeeSubscribed {
            peer: size,
            replaced,
        });
    }

    fn clone_subscriber(&self, stream: &TcpStream) -> io::Result<TcpStream> {
        #[cfg(test)]
        if self
            .shared
            .fail_subscriber_clone
            .swap(false, Ordering::SeqCst)
        {
            return Err(io::Error::other("injected clone failure"));
        }
        stream.try_clone()
    }

    /// Test-only: make the next [`Tee::subscribe`] fail its socket clone.
    #[cfg(test)]
    pub(crate) fn fail_next_subscriber_clone(&self) {
        self.shared
            .fail_subscriber_clone
            .store(true, Ordering::SeqCst);
    }

    /// Test-only: panic while handling the next item the writer pops.
    #[cfg(test)]
    pub(crate) fn panic_next_writer_item(&self) {
        self.shared
            .panic_writer_item
            .store(true, Ordering::SeqCst);
    }

    /// Queue a zero-data record. Control records keep the stream readable even when the byte
    /// budget is exhausted: losing `Opened`/`Closed` would be worse than exceeding it by 17 bytes.
    fn queue_control(&self, state: &mut State, record: Record) {
        if state.subscriber.is_none() {
            // Nobody can see it; the next subscriber gets a gap for a still-open connection.
            state.pending.remove(&record.conn_id);
            return;
        }
        self.emit_pending_gap(state, record.conn_id);
        let len = record.encoded_len();
        let generation = state.generation;
        state.queued_bytes += len;
        state.queue.push_back(Item {
            generation,
            bytes: len,
            kind: ItemKind::Record(record),
        });
    }

    fn emit_pending_gap(&self, state: &mut State, conn: u32) {
        let Some(dropped) = state.pending.remove(&conn) else {
            return;
        };
        let len = GAP_LEN;
        let generation = state.generation;
        state.queued_bytes += len;
        state.queue.push_back(Item {
            generation,
            bytes: len,
            kind: ItemKind::Record(Record {
                ts_ns: now_ns(),
                conn_id: conn,
                kind: Kind::Gap {
                    dropped_bytes: dropped,
                },
                data: Vec::new(),
            }),
        });
        self.stats.send(Event::Gap {
            conn,
            dropped_bytes: dropped,
        });
    }

    fn drop_bytes(&self, state: &mut State, conn: u32, bytes: u64) {
        if bytes == 0 {
            return;
        }
        if state.conns.contains_key(&conn) {
            *state.pending.entry(conn).or_insert(0) += bytes;
        }
        self.stats.send(Event::TeeDropped { conn, bytes });
    }

    fn listener_body(&self, listener: &TcpListener) {
        for incoming in listener.incoming() {
            let stream = match incoming {
                Ok(stream) => stream,
                Err(error) => {
                    log::warn!("forwarder: tee accept failed: {error}");
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            let peer = stream
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "unknown".into());
            let step = catch_unwind(AssertUnwindSafe(|| self.subscribe(stream, &peer)));
            if step.is_err() {
                log::warn!("forwarder: tee listener panicked while accepting {peer}; continuing");
            }
        }
    }

    fn writer_body(&self, recovering: bool) {
        // Only a restart after a panic has to make the state agree with the lost thread: turn
        // anything already queued into dropped bytes so a later subscriber sees an honest gap.
        // The first start must not touch a subscriber that connected while the thread was starting.
        if recovering {
            self.reset_writer();
        }
        let mut current: Option<(TcpStream, u64)> = None;
        loop {
            let item = {
                let mut state = self.shared.lock();
                while state.queue.is_empty() {
                    state = self
                        .shared
                        .cv
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                let item = state.queue.pop_front().expect("queue is not empty");
                state.queued_bytes = state.queued_bytes.saturating_sub(item.bytes);
                item
            };
            let item_generation = item.generation;
            let step = catch_unwind(AssertUnwindSafe(|| {
                self.write_item(item, &mut current);
            }));
            if step.is_err() {
                log::warn!("forwarder: tee writer panicked while writing; dropping subscriber");
                // A panic before the writer assigned `current` (for example while writing a new
                // subscriber's header) still has to unregister that generation: otherwise live
                // records keep queueing against a dead socket.
                let generation = current
                    .as_ref()
                    .map(|(_, generation)| *generation)
                    .unwrap_or(item_generation);
                if let Some((stream, _)) = current.take() {
                    let _ = stream.shutdown(Shutdown::Both);
                }
                self.clear_subscriber(generation);
            }
        }
    }

    fn write_item(&self, item: Item, current: &mut Option<(TcpStream, u64)>) {
        #[cfg(test)]
        if self.shared.panic_writer_item.swap(false, Ordering::SeqCst) {
            panic!("injected writer panic");
        }
        match item.kind {
            ItemKind::Subscriber { stream, gaps } => {
                if let Some((old, _)) = current.take() {
                    let _ = old.shutdown(Shutdown::Both);
                }
                let mut stream = stream;
                if let Err(error) = write_header(&mut stream) {
                    log::warn!("forwarder: tee header write failed: {error}");
                    self.clear_subscriber(item.generation);
                    return;
                }
                for (conn, dropped_bytes) in gaps {
                    let gap = Record {
                        ts_ns: now_ns(),
                        conn_id: conn,
                        kind: Kind::Gap { dropped_bytes },
                        data: Vec::new(),
                    };
                    if let Err(error) = write_record(&mut stream, &gap) {
                        log::warn!("forwarder: tee gap write failed: {error}");
                        self.clear_subscriber(item.generation);
                        return;
                    }
                    self.stats.send(Event::Gap {
                        conn,
                        dropped_bytes,
                    });
                }
                *current = Some((stream, item.generation));
            }
            ItemKind::Record(record) => {
                let live = matches!(
                    current.as_ref(),
                    Some((_, generation)) if *generation == item.generation
                );
                if !live {
                    self.drop_record(record, item.generation);
                    return;
                }
                let Some((stream, _)) = current.as_mut() else {
                    self.drop_record(record, item.generation);
                    return;
                };
                let error = write_record(stream, &record).err();
                if let Some(error) = error {
                    log::warn!("forwarder: tee write failed: {error}");
                    let generation = item.generation;
                    let _ = stream.shutdown(Shutdown::Both);
                    *current = None;
                    self.clear_subscriber(generation);
                    self.drop_record(record, generation);
                }
            }
        }
    }

    /// A record never reached the subscriber. Its bytes count as dropped only for the current
    /// subscription: after a replacement the new subscriber's mid-join gap already covers
    /// everything queued for the old one.
    fn drop_record(&self, record: Record, generation: u64) {
        if !matches!(record.kind, Kind::Serverbound | Kind::Clientbound) {
            return;
        }
        let mut state = self.shared.lock();
        if state.generation == generation && state.conns.contains_key(&record.conn_id) {
            *state.pending.entry(record.conn_id).or_insert(0) += record.data.len() as u64;
        }
        let bytes = record.data.len() as u64;
        drop(state);
        if bytes > 0 {
            self.stats.send(Event::TeeDropped {
                conn: record.conn_id,
                bytes,
            });
        }
    }

    fn clear_subscriber(&self, generation: u64) {
        let mut state = self.shared.lock();
        if state.generation != generation {
            return;
        }
        if let Some(stream) = state.subscriber.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn reset_writer(&self) {
        let mut state = self.shared.lock();
        if let Some(stream) = state.subscriber.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        state.generation = state.generation.wrapping_add(1);
        let items: Vec<Item> = state.queue.drain(..).collect();
        state.queued_bytes = 0;
        let mut dropped: Vec<(u32, u64)> = Vec::new();
        for item in items {
            if let ItemKind::Record(record) = item.kind
                && matches!(record.kind, Kind::Serverbound | Kind::Clientbound)
            {
                let bytes = record.data.len() as u64;
                if state.conns.contains_key(&record.conn_id) {
                    *state.pending.entry(record.conn_id).or_insert(0) += bytes;
                }
                if bytes > 0 {
                    dropped.push((record.conn_id, bytes));
                }
            }
        }
        drop(state);
        for (conn, bytes) in dropped {
            self.stats.send(Event::TeeDropped { conn, bytes });
        }
    }
}

/// Run `body` on its own thread; if it panics, log and run it again. A panic in a tee thread must
/// never end the process or a game connection.
fn supervise(name: &'static str, mut body: impl FnMut() + Send + 'static) {
    let spawned = thread::Builder::new().name(name.into()).spawn(move || loop {
        let step = catch_unwind(AssertUnwindSafe(&mut body));
        match step {
            Ok(()) => break,
            Err(_) => {
                log::warn!("forwarder: {name} panicked; restarting it");
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    if let Err(error) = spawned {
        log::warn!("forwarder: cannot start {name}: {error}; continuing without it");
    }
}
