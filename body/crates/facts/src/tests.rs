//! Unit tests over a hand-built perception world: one known world for every
//! registry fact, and unknown propagation for everything perception cannot
//! answer.

use std::sync::Arc;

use azalea_buf::AzBuf;
use azalea_core::{
    delta::LpVec3,
    entity_id::MinecraftEntityId,
    game_type::{GameMode, OptionalGameType},
    position::Vec3,
};
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundAddEntity, ClientboundContainerSetContent, ClientboundGamePacket,
        ClientboundLevelChunkWithLight, ClientboundLogin, ClientboundSetHealth, ClientboundSetTime,
        ClientboundBlockUpdate, ServerboundGamePacket, ServerboundMovePlayerPos,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData, c_set_time::ClockState,
    },
};
use azalea_registry::{
    DataRegistry,
    builtin::{BlockKind, EntityKind, ItemKind},
    data::{DimensionKind, WorldClock},
    identifier::Identifier,
};
use azalea_world::Chunk;
use praxion_interpreter::{IntValue, Truth};
use praxion_perception::{Config, Perception};
use praxion_rules::{ArgValue, FactCall, Facts as RegistryFacts, ValueType};
use praxion_world::{BlockPos, ItemStack, World};
use uuid::Uuid;

use super::*;

const EYE_Y: f64 = 65.0;
const FEET_Y: f64 = EYE_Y - 1.62;
/// Ticks of sweeping that cover the eye's section at the per-tick ray slice.
const SWEEP_TICKS: usize = 10;

fn spawn_info() -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
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
    }
}

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
        common: spawn_info(),
        online_mode: false,
        enforces_secure_chat: false,
    }
}

fn chunk_packet(x: i32, z: i32) -> ClientboundLevelChunkWithLight {
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    ClientboundLevelChunkWithLight {
        x,
        z,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: Vec::new(),
            buffer: Arc::new(buffer.into_boxed_slice()),
            block_entities: Vec::new(),
        },
        light_data: Default::default(),
    }
}

fn set_block(world: &mut World, pos: BlockPos, state: praxion_world::BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate { pos, block_state: state }),
    );
}

fn set_player(world: &mut World) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: Vec3::new(0.5, FEET_Y, 0.5),
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
}

fn world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
    );
    set_player(&mut world);
    // A stone two blocks east, so `near(stone, 4)` is true but farther than
    // the eye's own cell.
    set_block(
        &mut world,
        BlockPos::new(2, 65, 0),
        praxion_world::BlockState::from(BlockKind::Stone),
    );
    world
}

fn with_inventory(mut world: World) -> World {
    let mut items = vec![ItemStack::Empty; 46];
    items[36] = ItemStack::new(ItemKind::OakLog, 3);
    items[37] = ItemStack::new(ItemKind::BirchLog, 1);
    items[38] = ItemStack::new(ItemKind::StonePickaxe, 1);
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
            container_id: 0,
            state_id: 0,
            items,
            carried_item: ItemStack::Empty,
        }),
    );
    world
}

fn with_health(mut world: World, health: f32, food: u32) -> World {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health,
            food,
            saturation: 0.0,
        }),
    );
    world
}

fn with_time(mut world: World, day_time: u64) -> World {
    let mut clocks = std::collections::HashMap::new();
    clocks.insert(
        WorldClock::new_raw(0),
        ClockState { total_ticks: day_time, partial_tick: 0.0, rate: 1.0 },
    );
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetTime(ClientboundSetTime {
            game_time: day_time,
            clock_updates: clocks.into_iter().collect(),
        }),
    );
    world
}

fn with_zombie(mut world: World) -> World {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(9),
            uuid: Uuid::from_u128(9),
            entity_type: EntityKind::Zombie,
            position: Vec3::new(3.5, FEET_Y, 0.5),
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 0,
            y_head_rot: 0,
            data: 0,
        }),
    );
    world
}

fn call(name: &str, args: Vec<ArgValue>) -> FactCall {
    FactCall { name: name.to_owned(), args }
}

fn sample_arg(ty: ValueType) -> ArgValue {
    match ty {
        ValueType::Int => ArgValue::Int(4),
        ValueType::Item => ArgValue::Item("stone".to_owned()),
        ValueType::Block => ArgValue::Block("stone".to_owned()),
        ValueType::EntityKind => ArgValue::EntityKind("zombie".to_owned()),
        ValueType::Bool => ArgValue::Bool(true),
    }
}

#[test]
fn every_registry_fact_is_answered_without_turning_unknown_into_false() {
    let world = with_zombie(with_health(with_inventory(with_time(world(), 1_000)), 17.0, 14));
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    for signature in RegistryFacts::standard().iter() {
        let call = call(
            signature.name,
            signature.params.iter().copied().map(sample_arg).collect(),
        );
        match signature.ret {
            ValueType::Bool => {
                let answer = facts.bool_fact(&call);
                assert!(
                    matches!(answer, Truth::True | Truth::False),
                    "{} answered {answer:?} on a known world",
                    signature.name
                );
            }
            ValueType::Int => {
                let answer = facts.int_fact(&call);
                assert!(
                    matches!(answer, IntValue::Known(_)),
                    "{} answered {answer:?} on a known world",
                    signature.name
                );
            }
            other => panic!("a fact cannot return {other:?}"),
        }
    }
}

#[test]
fn known_inventory_facts_count_items_and_tags() {
    let world = with_inventory(world());
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    assert_eq!(
        facts.int_fact(&call("count", vec![ArgValue::Item("oak_log".into())])),
        IntValue::Known(3)
    );
    assert_eq!(
        facts.int_fact(&call("count", vec![ArgValue::Item("#logs".into())])),
        IntValue::Known(4),
        "the tag counts every log type"
    );
    assert_eq!(
        facts.bool_fact(&call("has", vec![ArgValue::Item("stone_pickaxe".into())])),
        Truth::True
    );
    assert_eq!(
        facts.bool_fact(&call("has", vec![ArgValue::Item("wooden_pickaxe".into())])),
        Truth::False
    );
    assert_eq!(
        facts.bool_fact(&call("has", vec![ArgValue::Item("#planks".into())])),
        Truth::False
    );
}

#[test]
fn known_block_facts_use_the_known_map() {
    let world = with_zombie(world());
    let mut perception = Perception::new(Config::default());
    // The sweep spends a bounded slice of rays per tick, so filling the map
    // around the eye takes a few ticks.
    for _ in 0..SWEEP_TICKS {
        perception.begin_tick(&world);
        perception.sweep(&world);
    }
    assert!(
        perception.known(BlockPos::new(2, 65, 0)).is_some(),
        "the sweep reached the stone"
    );
    let mut facts = Facts::new(&mut perception, &world);

    assert_eq!(
        facts.bool_fact(&call(
            "near",
            vec![ArgValue::Block("stone".into()), ArgValue::Int(4)]
        )),
        Truth::True
    );
    assert_eq!(
        facts.bool_fact(&call(
            "near",
            vec![ArgValue::Block("diamond_ore".into()), ArgValue::Int(4)]
        )),
        Truth::False,
        "a block the known map holds none of is absent, not unknown"
    );
    assert_eq!(
        facts.bool_fact(&call(
            "near",
            vec![ArgValue::Block("#logs".into()), ArgValue::Int(4)]
        )),
        Truth::False,
        "a tagged known-map query that holds no member answers false"
    );
    // No sweep at all: the map is empty, so the same answer as an empty map.
    let mut empty = Perception::new(Config::default());
    empty.begin_tick(&world);
    let mut facts = Facts::new(&mut empty, &world);
    assert_eq!(
        facts.bool_fact(&call(
            "near",
            vec![ArgValue::Block("stone".into()), ArgValue::Int(4)]
        )),
        Truth::False,
        "an unseen world has no known target, which starts exploration"
    );
    assert_eq!(
        facts.bool_fact(&call(
            "near_entity",
            vec![ArgValue::EntityKind("zombie".into()), ArgValue::Int(8)]
        )),
        Truth::True
    );
    assert_eq!(
        facts.bool_fact(&call(
            "near_entity",
            vec![ArgValue::EntityKind("cow".into()), ArgValue::Int(8)]
        )),
        Truth::Unknown(call(
            "near_entity",
            vec![ArgValue::EntityKind("cow".into()), ArgValue::Int(8)]
        )),
        "an entity nobody saw is unknown"
    );
}

#[test]
fn f3_facts_read_health_food_and_the_day_clock() {
    let world = with_time(with_health(world(), 17.5, 14), 13_000);
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    assert_eq!(facts.int_fact(&call("health", vec![])), IntValue::Known(17));
    assert_eq!(facts.int_fact(&call("food", vec![])), IntValue::Known(14));
    assert_eq!(facts.bool_fact(&call("is_night", vec![])), Truth::True);
    assert_eq!(facts.int_fact(&call("time_of_day", vec![])), IntValue::Known(13_000));
    assert_eq!(facts.int_fact(&call("day_time", vec![])), IntValue::Known(13_000));
}

#[test]
fn air_and_burning_are_unknown_until_the_server_says() {
    let world = world();
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    assert_eq!(
        facts.int_fact(&call("air", vec![])),
        IntValue::Unknown(call("air", vec![]))
    );
    assert_eq!(
        facts.bool_fact(&call("burning", vec![])),
        Truth::Unknown(call("burning", vec![]))
    );
}

#[test]
fn unknown_world_makes_every_fact_unknown() {
    // A desynced connection: perception answers Unknown, and the facts pass
    // the same reason through instead of defaulting to false or zero.
    let mut world = World::new();
    world.follow(0);
    world.invalidate(0, "test");
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    for (name, args) in [
        ("count", vec![ArgValue::Item("oak_log".into())]),
        ("has", vec![ArgValue::Item("#logs".into())]),
        ("near", vec![ArgValue::Block("stone".into()), ArgValue::Int(4)]),
        ("near_entity", vec![ArgValue::EntityKind("zombie".into()), ArgValue::Int(4)]),
    ] {
        let answer = facts.bool_fact(&call(name, args.clone()));
        assert!(
            matches!(&answer, Truth::Unknown(fact) if fact.name == name),
            "{name} answered {answer:?}"
        );
        let answer = facts.int_fact(&call(name, args));
        assert!(
            matches!(&answer, IntValue::Unknown(fact) if fact.name == name),
            "{name} (int half) answered {answer:?}"
        );
    }
}

#[test]
fn unknown_helper_facts_propagate() {
    let mut world = World::new();
    world.follow(0);
    world.invalidate(0, "test");
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let mut facts = Facts::new(&mut perception, &world);

    for name in ["health", "food", "air", "burning", "is_night"] {
        assert_eq!(
            facts.bool_fact(&call(name, vec![])),
            Truth::Unknown(call(name, vec![])),
            "{name}"
        );
    }
    for name in ["health", "food", "air", "time_of_day"] {
        assert_eq!(
            facts.int_fact(&call(name, vec![])),
            IntValue::Unknown(call(name, vec![])),
            "{name}"
        );
    }
}

#[test]
fn night_boundaries() {
    assert!(!is_night(0));
    assert!(!is_night(12_000));
    assert!(is_night(13_000));
    assert!(is_night(22_999));
    assert!(!is_night(23_000));
    assert!(is_night(13_000 + DAY_TICKS));
    assert_eq!(time_of_day(47_500), 23_500);
}
