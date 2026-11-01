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

use std::collections::VecDeque;
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
/// Packets kept for the decision layer between two ticks. A busy chunk batch is
/// thousands of packets; the layer only needs the recent ones, and the receipts
/// engine settles within its windows.
const PACKET_QUEUE_CAPACITY: usize = 8_192;

/// One packet the tee saw, with the connection it belongs to.
#[derive(Clone, Debug)]
pub struct ObservedPacket {
    pub conn_id: u32,
    pub packet: Packet,
}

/// The raw store, fed from the tee by its own thread.
pub struct WorldFeed {
    world: Arc<Mutex<World>>,
    packets: Arc<Mutex<VecDeque<ObservedPacket>>>,
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
        let packets = Arc::new(Mutex::new(VecDeque::with_capacity(256)));
        let thread_world = Arc::clone(&world);
        let thread_packets = Arc::clone(&packets);
        // The reader thread is detached: it blocks on the tee socket and ends
        // with the process. A stalled tee must not be able to hang its owner.
        thread::Builder::new()
            .name("praxion-world-feed".to_owned())
            .spawn(move || run(reader, thread_world, thread_packets))
            .map_err(|error| format!("cannot start the world feed thread: {error}"))?;

        Ok(Self { world, packets })
    }

    /// The world as of the last record the decoder applied.
    pub fn lock(&self) -> MutexGuard<'_, World> {
        self.world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes the packets the tee saw since the last call, oldest first.
    pub fn take_packets(&self) -> Vec<ObservedPacket> {
        let mut packets = self
            .packets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        packets.drain(..).collect()
    }
}

fn run<R: std::io::Read>(
    mut reader: Reader<R>,
    world: Arc<Mutex<World>>,
    packets: Arc<Mutex<VecDeque<ObservedPacket>>>,
) {
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
            apply(&world, &packets, &mut followed, &event);
        }
    }
}

fn apply(
    world: &Arc<Mutex<World>>,
    packets: &Arc<Mutex<VecDeque<ObservedPacket>>>,
    followed: &mut Option<u32>,
    event: &Event,
) {
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
        Event::Decoded(decoded) => {
            let mut queue = packets
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if queue.len() >= PACKET_QUEUE_CAPACITY {
                queue.pop_front();
            }
            queue.push_back(ObservedPacket {
                conn_id: decoded.conn_id,
                packet: decoded.packet.clone(),
            });
            drop(queue);
            match &decoded.packet {
                Packet::ConfigurationClientbound(packet) => {
                    world.apply_clientbound_config(decoded.conn_id, packet);
                }
                Packet::PlayClientbound(packet) => {
                    if decoded.packet.name() == "login" && *followed != Some(decoded.conn_id) {
                        world.follow(decoded.conn_id);
                        *followed = Some(decoded.conn_id);
                    }
                    world.apply_clientbound_play(decoded.conn_id, packet);
                    mirror_player_slots(&mut world, decoded.conn_id, packet);
                }
                Packet::PlayServerbound(packet) => {
                    world.apply_serverbound_play(decoded.conn_id, packet);
                }
                _ => {}
            }
        }
    }
}

/// A menu's last 36 slots are the player's inventory (27 storage slots plus the
/// 9-slot hotbar), and the server updates them in place while the menu is open.
/// `World` keeps those updates in the open container, so after the menu closes
/// the player inventory would stay stale: the server does not resend container
/// 0 on close. Fold each player-slot update into container 0 as well, so the
/// facts and the body's inventory agree with the server again once the menu is
/// gone.
fn mirror_player_slots(
    world: &mut World,
    conn: u32,
    packet: &azalea_protocol::packets::game::ClientboundGamePacket,
) {
    use azalea_protocol::packets::game::ClientboundGamePacket as Packet;
    const PLAYER_STORAGE_SLOTS: usize = 36;
    let Packet::ContainerSetSlot(set) = packet else {
        return;
    };
    if set.container_id == 0 {
        return;
    }
    let Some(container) = world.open_container() else {
        return;
    };
    if container.id() != set.container_id {
        return;
    }
    let slots = container.slots().len();
    let slot = set.slot as usize;
    if slots < PLAYER_STORAGE_SLOTS || slot < slots - PLAYER_STORAGE_SLOTS {
        return;
    }
    let inventory_slot = 9 + (slot - (slots - PLAYER_STORAGE_SLOTS));
    let Ok(inventory_slot) = u16::try_from(inventory_slot) else {
        return;
    };
    let mirrored = azalea_protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot {
        container_id: 0,
        slot: inventory_slot,
        ..set.clone()
    };
    world.apply_clientbound_play(conn, &Packet::ContainerSetSlot(mirrored));
}

#[cfg(test)]
mod tests {
    use azalea_core::entity_id::MinecraftEntityId;
    use azalea_core::game_type::{GameMode, OptionalGameType};
    use azalea_protocol::packets::common::CommonPlayerSpawnInfo;
    use azalea_protocol::packets::game::{
        ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundGamePacket,
        ClientboundLogin, ClientboundOpenScreen,
    };
    use azalea_registry::builtin::{ItemKind, MenuKind};
    use azalea_registry::data::DimensionKind;
    use azalea_registry::identifier::Identifier;
    use azalea_registry::DataRegistry;
    use praxion_world::{ItemStack, World};

    use super::mirror_player_slots;

    fn login() -> ClientboundLogin {
        ClientboundLogin {
            player_id: MinecraftEntityId(7),
            hardcore: false,
            levels: Vec::new(),
            max_players: 20,
            chunk_radius: 8,
            simulation_distance: 8,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            common: CommonPlayerSpawnInfo {
                dimension_type: DimensionKind::new_raw(0),
                dimension: Identifier::new("minecraft:overworld"),
                seed: 0,
                game_type: GameMode::Survival,
                previous_game_type: OptionalGameType(None),
                is_debug: false,
                is_flat: false,
                last_death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            online_mode: false,
            enforces_secure_chat: false,
        }
    }

    #[test]
    fn a_menu_player_slot_update_reaches_the_player_inventory() {
        let mut world = World::new();
        world.follow(0);
        world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
        // The server sends the player's inventory as container 0 on join.
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
                container_id: 0,
                state_id: 0,
                items: vec![ItemStack::Empty; 46],
                carried_item: ItemStack::Empty,
            }),
        );
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
                container_id: 1,
                menu_type: MenuKind::Crafting,
                title: "Crafting".into(),
            }),
        );
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
                container_id: 1,
                state_id: 0,
                items: vec![ItemStack::Empty; 46],
                carried_item: ItemStack::Empty,
            }),
        );
        let set = ClientboundContainerSetSlot {
            container_id: 1,
            state_id: 1,
            slot: 45,
            item_stack: ItemStack::new(ItemKind::WoodenPickaxe, 1),
        };
        world.apply_clientbound_play(0, &ClientboundGamePacket::ContainerSetSlot(set.clone()));
        assert!(world.inventory()[44].is_empty(), "the world keeps it in the menu");
        mirror_player_slots(&mut world, 0, &ClientboundGamePacket::ContainerSetSlot(set));
        assert_eq!(world.inventory()[44].kind(), ItemKind::WoodenPickaxe);
    }
}
