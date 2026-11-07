//! Fixtures shared by the receipts crate's unit and integration tests.
//!
//! Only the crate's public API is used, so this file compiles both as the
//! integration-test module `tests/common/mod.rs` and through the `#[path]`
//! include in `src/tests.rs`. The independent suites keep their own assertions;
//! only packet builders and the logging harness live here.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use azalea_block::BlockState;
use azalea_core::position::{BlockPos, Vec3};
use azalea_inventory::operations::ClickType;
use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea_protocol::packets::game::{
    ClientboundBlockChangedAck, ClientboundBlockUpdate, ClientboundContainerSetContent,
    ClientboundGamePacket, ClientboundPlayerPosition, ServerboundContainerClick,
    ServerboundGamePacket, s_container_click::HashedStack,
};
use azalea_registry::builtin::BlockKind;
use praxion_decoder::Packet;
use praxion_eventlog::{Event, Outcome, Writer, read};
use praxion_world::{ItemStack, World};

use super::{Engine, Intent, ReceiptId};

pub fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/receipts-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}

pub fn dirt() -> BlockState {
    BlockState::from(BlockKind::Dirt)
}

pub fn air() -> BlockState {
    BlockState::AIR
}

pub fn block_update(pos: BlockPos, state: BlockState) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
        pos,
        block_state: state,
    }))
}

pub fn block_ack(seq: u32) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockChangedAck(
        ClientboundBlockChangedAck { seq },
    ))
}

pub fn player_position() -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::PlayerPosition(
        ClientboundPlayerPosition {
            id: 1,
            change: PositionMoveRotation {
                pos: Vec3::new(0.0, 64.0, 0.0),
                delta: Vec3::ZERO,
                look_direction: Default::default(),
            },
            relative: RelativeMovements::all_absolute(),
        },
    ))
}

pub fn content(container_id: i32, state_id: u32, items: Vec<ItemStack>) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::ContainerSetContent(
        ClientboundContainerSetContent {
            container_id,
            state_id,
            items,
            carried_item: ItemStack::Empty,
        },
    ))
}

pub fn click(container_id: i32, state_id: u32, slot: i16) -> Packet {
    Packet::PlayServerbound(ServerboundGamePacket::ContainerClick(
        ServerboundContainerClick {
            container_id,
            state_id,
            slot_num: slot,
            button_num: 0,
            click_type: ClickType::Pickup,
            changed_slots: Default::default(),
            carried_item: HashedStack(None),
        },
    ))
}

pub fn content_packet(
    container_id: i32,
    state_id: u32,
    items: Vec<ItemStack>,
) -> ClientboundGamePacket {
    ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
        container_id,
        state_id,
        items,
        carried_item: ItemStack::Empty,
    })
}

pub struct Harness {
    dir: PathBuf,
    pub writer: Writer,
    pub engine: Engine,
    pub world: World,
    pub tick: u64,
    pub ts_ns: u64,
}

impl Harness {
    pub fn new(name: &str) -> Self {
        let dir = test_dir(name);
        let writer = Writer::create(&dir).unwrap();
        let mut world = World::new();
        world.follow(0);
        Self {
            dir,
            writer,
            engine: Engine::new(),
            world,
            tick: 0,
            ts_ns: 1_793_443_244_558_717_800,
        }
    }

    pub fn register(&mut self, intent: Intent) -> ReceiptId {
        self.engine
            .register(&mut self.writer, self.tick, self.ts_ns, intent, &self.world)
            .unwrap()
    }

    pub fn apply(&mut self, packet: &Packet) {
        match packet {
            Packet::PlayClientbound(packet) => {
                self.world.apply_clientbound_play(0, packet);
            }
            Packet::PlayServerbound(packet) => {
                self.world.apply_serverbound_play(0, packet);
            }
            _ => {}
        }
    }

    pub fn observe_at(&mut self, tick: u64, packet: Packet) {
        self.apply(&packet);
        self.ts_ns += 1;
        self.engine
            .observe(&mut self.writer, tick, self.ts_ns, &packet, &self.world)
            .unwrap();
    }

    pub fn advance_at(&mut self, tick: u64) {
        self.ts_ns += 1;
        self.engine
            .advance(&mut self.writer, tick, self.ts_ns)
            .unwrap();
    }

    pub fn outcome(&self, receipt: ReceiptId) -> Option<Outcome> {
        self.engine.outcome(receipt)
    }

    pub fn events(&mut self) -> Vec<Event> {
        self.writer.flush().unwrap();
        read(self.dir.join(praxion_eventlog::FILE_NAME))
            .unwrap()
            .events
    }
}
