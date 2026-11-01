//! Hand-built worlds for the line-of-sight, budget, memory, entity, cache and
//! F3 tests. Nothing here is a real chunk: empty chunks plus block updates are
//! enough to pin the rules.

use std::sync::Arc;

use azalea_block::{BlockState, BlockTrait};
use azalea_buf::AzBuf;
use azalea_core::{
    aabb::Aabb,
    delta::LpVec3,
    entity_id::MinecraftEntityId,
    game_type::GameMode,
    position::{ChunkPos, Vec3},
};
use azalea_physics::collision::BlockWithShape;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundAddEntity, ClientboundForgetLevelChunk, ClientboundGamePacket,
        ClientboundLevelChunkWithLight, ClientboundLogin, ClientboundSetHealth, ClientboundSetTime,
        ClientboundSound, ClientboundSoundEntity, ServerboundGamePacket, ServerboundMovePlayerPos,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData,
        c_set_time::ClockState,
        c_sound::SoundSource,
    },
};
use azalea_registry::{
    DataRegistry, Holder, Registry,
    builtin::{BlockKind, EntityKind, SoundEvent},
    data::{DimensionKind, WorldClock},
    identifier::Identifier,
};
use azalea_world::Chunk;
use praxion_world::{BlockPos, World};
use uuid::Uuid;

use super::*;

use crate::sight::{Sight, SightTable};

const EYE_Y: f64 = 65.0;
const FEET_Y: f64 = EYE_Y - DEFAULT_EYE_HEIGHT;

fn spawn_info() -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new("minecraft:overworld"),
        seed: 0,
        game_type: GameMode::Survival,
        previous_game_type: azalea_core::game_type::OptionalGameType(None),
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

fn base_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
    );
    world
}

fn set_player(world: &mut World, feet: Vec3) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: feet,
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
}

fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(
            azalea_protocol::packets::game::ClientboundBlockUpdate { pos, block_state: state },
        ),
    );
}

/// An empty chunk with `blocks` placed and the player's eye at `EYE_Y`.
fn world_with(blocks: &[(BlockPos, BlockState)]) -> World {
    let mut world = base_world();
    set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
    for (pos, state) in blocks {
        set_block(&mut world, *pos, *state);
    }
    world
}

/// Empty chunks `0..chunks` along x with `blocks` placed, the player standing
/// in the first one: long rays that cross more than one chunk need them.
fn world_across_chunks(chunks: i32, blocks: &[(BlockPos, BlockState)]) -> World {
    let mut world = base_world();
    for x in 1..chunks {
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, 0)),
        );
    }
    set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
    for (pos, state) in blocks {
        set_block(&mut world, *pos, *state);
    }
    world
}

fn find_block(name: &str, props: &[(&str, &str)]) -> BlockState {
    for id in 0..=u32::from(u16::MAX) {
        let Ok(state) = BlockState::try_from(id) else {
            break;
        };
        let block = Box::<dyn BlockTrait>::from(state);
        if block.id() != name {
            continue;
        }
        let map = block.property_map();
        if props.iter().all(|(key, value)| map.get(key) == Some(value)) {
            return state;
        }
    }
    panic!("block {name} with {props:?} not found");
}

fn stone() -> BlockState {
    find_block("stone", &[])
}

fn air() -> BlockState {
    BlockState::AIR
}

fn add_zombie(world: &mut World, id: i32, pos: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(id),
            uuid: Uuid::from_u128(id as u128),
            entity_type: EntityKind::Zombie,
            position: pos,
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 0,
            y_head_rot: 0,
            data: 0,
        }),
    );
}

fn cast(
    perception: &mut Perception,
    world: &World,
    from: Vec3,
    to: Vec3,
    target: BlockPos,
) -> bool {
    let mut crossed = CrossedChunks::default();
    crate::sight::line_of_sight(world, from, to, target, &mut perception.table, &mut crossed)
}

#[test]
fn wall_blocks_and_memory_fills_the_gap() {
    let target = BlockPos::new(4, 65, 0);
    let mut world = world_with(&[(target, stone())]);
    let mut p = Perception::new(Config::default());

    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_eq!(answer.provenance(), Some(Provenance::Sight), "{answer:?}");

    let wall = BlockPos::new(2, 65, 0);
    set_block(&mut world, wall, stone());
    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_eq!(answer.provenance(), Some(Provenance::Memory), "{answer:?}");
    assert_eq!(answer.age_ticks(), Some(1));
    assert_eq!(answer.value().unwrap().state, stone());
    assert_eq!(
        p.ledger().get(QueryKind::Block, FactKind::BlockState).unwrap().count(),
        1
    );

    set_block(&mut world, wall, air());
    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_eq!(answer.provenance(), Some(Provenance::Sight), "{answer:?}");
    assert_eq!(answer.age_ticks(), Some(0));
}

#[test]
fn glass_passes_and_is_itself_visible() {
    let glass = find_block("glass", &[]);
    let target = BlockPos::new(4, 65, 0);
    let wall = BlockPos::new(2, 65, 0);
    let world = world_with(&[(wall, glass), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(
        p.block(Caller::Other, &world, wall).provenance(),
        Some(Provenance::Sight)
    );
}

#[test]
fn a_bottom_slab_blocks_only_its_half() {
    let bottom = find_block("stone_slab", &[("type", "bottom")]);
    let top = find_block("stone_slab", &[("type", "top")]);
    let wall = BlockPos::new(2, 65, 0);
    let low = BlockPos::new(4, 65, 0);
    let high = BlockPos::new(4, 66, 0);

    let world = world_with(&[(wall, bottom), (low, stone()), (high, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, low).reason(),
        Some(Reason::OutOfSight)
    );
    assert_eq!(
        p.block(Caller::Other, &world, high).provenance(),
        Some(Provenance::Sight)
    );

    let world = world_with(&[(wall, top), (low, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, low).provenance(),
        Some(Provenance::Sight),
        "the ray passes below the top slab"
    );
}

#[test]
fn lava_blocks_and_water_passes() {
    let target = BlockPos::new(4, 65, 0);
    let wall = BlockPos::new(2, 65, 0);

    let world = world_with(&[(wall, find_block("lava", &[])), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::OutOfSight)
    );

    let world = world_with(&[(wall, find_block("water", &[])), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
}

#[test]
fn diagonal_corners_follow_the_voxel_walk() {
    let target = BlockPos::new(2, 65, 2);
    let corner = BlockPos::new(1, 65, 1);

    let world = world_with(&[(corner, stone()), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::OutOfSight),
        "the diagonal passes through the corner block"
    );

    // Two blocks that touch only at the corner leave the diagonal clear when
    // the walk steps in x first.
    let world = world_with(&[
        (BlockPos::new(0, 65, 1), stone()),
        (target, stone()),
    ]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
}

#[test]
fn a_ray_exactly_on_a_block_edge_touches_the_box() {
    let wall = BlockPos::new(2, 65, 1);
    let world = world_with(&[(wall, stone())]);
    let mut p = Perception::new(Config::default());
    let from = Vec3::new(0.5, 65.5, 1.0);
    let to = Vec3::new(5.0, 65.5, 1.0);
    assert!(
        !cast(&mut p, &world, from, to, BlockPos::new(5, 65, 1)),
        "z = 1.0 is the wall's boundary and counts as a hit"
    );

    // The same segment never enters the z = 0 cell, so a wall there is missed.
    let world = world_with(&[(BlockPos::new(2, 65, 0), stone())]);
    let mut p = Perception::new(Config::default());
    assert!(cast(&mut p, &world, from, to, BlockPos::new(5, 65, 1)));
}

#[test]
fn an_eye_inside_a_non_full_block_uses_its_boxes() {
    let target = BlockPos::new(4, 65, 0);
    let eye_cell = BlockPos::new(0, 65, 0);
    let slab = find_block("stone_slab", &[("type", "bottom")]);
    let grass = find_block("short_grass", &[]);

    let world = world_with(&[(eye_cell, slab), (target, stone())]);
    let mut p = Perception::new(Config::default());
    let from = Vec3::new(0.5, 65.25, 0.5);
    assert!(
        !cast(&mut p, &world, from, Vec3::new(4.5, 65.25, 0.5), target),
        "the eye is inside the slab's box"
    );

    let world = world_with(&[(eye_cell, grass), (target, stone())]);
    let mut p = Perception::new(Config::default());
    assert!(
        cast(&mut p, &world, from, Vec3::new(4.5, 65.25, 0.5), target),
        "short grass has no box, so the eye is in open air"
    );
}

#[test]
fn a_face_ray_sees_over_a_partial_occluder() {
    let bottom = find_block("stone_slab", &[("type", "bottom")]);
    let target = BlockPos::new(4, 65, 0);
    let mut world = world_with(&[(BlockPos::new(2, 65, 0), bottom), (target, stone())]);
    let mut p = Perception::new(Config::default());
    set_player(&mut world, Vec3::new(0.5, 65.4 - DEFAULT_EYE_HEIGHT, 0.5));
    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_eq!(answer.provenance(), Some(Provenance::Sight), "{answer:?}");
    assert!(
        p.rays_used_this_tick() > 1,
        "the center ray hits the slab, so a face ray is needed"
    );
}

#[test]
fn the_reflex_reserve_is_the_only_extra_budget() {
    let config = Config { ray_budget: 2, reflex_reserve: 1, ..Config::default() };
    let wall = BlockPos::new(2, 65, 0);
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(wall, stone()), (target, stone())]);
    let mut p = Perception::new(config);
    p.begin_tick(&world);

    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::Budget)
    );
    assert_eq!(p.rays_used_this_tick(), 1, "Other may spend one of two rays");
    assert_eq!(p.rays_remaining(Caller::Other), 0);
    assert_eq!(p.rays_remaining(Caller::Reflex), 1);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::Budget)
    );
    assert_eq!(
        p.block(Caller::Reflex, &world, target).reason(),
        Some(Reason::Budget)
    );
    assert_eq!(p.rays_used_this_tick(), 2);

    p.begin_tick(&world);
    assert_eq!(p.rays_remaining(Caller::Other), 1);
    assert_eq!(p.rays_remaining(Caller::Reflex), 2);
}

#[test]
fn the_entity_cap_counts_overflow() {
    let config = Config { entity_cap: 1, ..Config::default() };
    let mut world = world_with(&[]);
    add_zombie(&mut world, 1, Vec3::new(3.0, FEET_Y, 0.5));
    add_zombie(&mut world, 2, Vec3::new(3.0, FEET_Y, 2.5));
    let mut p = Perception::new(config);
    p.begin_tick(&world);

    assert_eq!(
        p.entity(Caller::Other, &world, 1).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(
        p.entity(Caller::Other, &world, 2).reason(),
        Some(Reason::Budget)
    );
    assert_eq!(p.entity_overflow(), 1);

    p.begin_tick(&world);
    assert_eq!(
        p.entity(Caller::Other, &world, 2).provenance(),
        Some(Provenance::Sight),
        "a new tick has a new cap"
    );
    assert_eq!(p.entity_overflow(), 0);
}

#[test]
fn entities_behind_a_wall_are_out_of_sight() {
    let mut world = world_with(&[
        (BlockPos::new(2, 64, 0), stone()),
        (BlockPos::new(2, 65, 0), stone()),
    ]);
    add_zombie(&mut world, 9, Vec3::new(4.0, FEET_Y, 0.5));
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.entity(Caller::Other, &world, 9).reason(),
        Some(Reason::OutOfSight)
    );
    assert_eq!(
        p.ledger().get(QueryKind::Entity, FactKind::EntityPosition).unwrap().count(),
        1
    );
}

#[test]
fn sounds_near_use_the_hearing_radius() {
    let mut world = world_with(&[]);
    let event = Holder::Reference(SoundEvent::from_u32(0).unwrap());
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Sound(ClientboundSound {
            sound: event,
            source: SoundSource::Blocks,
            x: 8,
            y: 65 * 8,
            z: 4,
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let answer = p.sounds_near(&world, 16);
    assert_eq!(answer.provenance(), Some(Provenance::Hearing), "{answer:?}");
    assert_eq!(answer.value().unwrap().len(), 1);
    assert_eq!(answer.value().unwrap()[0].position, Vec3::new(1.0, 65.0, 0.5));

    // A sound outside the hearing radius is not heard even when the caller
    // asks for a larger radius.
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Sound(ClientboundSound {
            sound: Holder::Reference(SoundEvent::from_u32(0).unwrap()),
            source: SoundSource::Blocks,
            x: 500 * 8,
            y: 65 * 8,
            z: 0,
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
    p.begin_tick(&world);
    let answer = p.sounds_near(&world, 128);
    assert_eq!(answer.value().unwrap().len(), 1, "{answer:?}");
}

#[test]
fn a_sound_entity_uses_the_entity_position() {
    let mut world = world_with(&[]);
    add_zombie(&mut world, 4, Vec3::new(2.0, FEET_Y, 2.0));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SoundEntity(ClientboundSoundEntity {
            sound: Holder::Reference(SoundEvent::from_u32(0).unwrap()),
            source: SoundSource::Hostile,
            id: MinecraftEntityId(4),
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let answer = p.sounds_near(&world, 16);
    let sounds = answer.value().unwrap();
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0].entity, Some(4));
    assert_eq!(sounds[0].position, Vec3::new(2.0, FEET_Y, 2.0));
}

#[test]
fn f3_facts_are_allowed_without_a_ray() {
    let mut world = world_with(&[]);
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health: 18.5,
            food: 17,
            saturation: 3.0,
        }),
    );
    let clocks = [(
        WorldClock::new_raw(0),
        ClockState { total_ticks: 6000, partial_tick: 0.0, rate: 1.0 },
    )]
    .into_iter()
    .collect();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetTime(ClientboundSetTime { game_time: 42, clock_updates: clocks }),
    );
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);

    assert_eq!(
        p.own_position(&world).value().copied(),
        Some(Vec3::new(0.5, FEET_Y, 0.5))
    );
    assert_eq!(p.own_health(&world).value().unwrap().health(), 18.5);
    assert_eq!(p.own_food(&world).value().copied(), Some(17));
    assert_eq!(p.day_time(&world).value().copied(), Some(6000));
    assert_eq!(p.game_mode(&world).value().copied(), Some(GameMode::Survival));
    assert_eq!(
        p.dimension(&world).value().unwrap().dimension(),
        "minecraft:overworld"
    );
    assert_eq!(p.biome().reason(), Some(Reason::NeverSeen));
    assert_eq!(p.light_level().reason(), Some(Reason::NeverSeen));
    assert_eq!(p.rays_used_this_tick(), 0);
}

#[test]
fn the_ledger_bounds_repeats_per_tick() {
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[
        (BlockPos::new(2, 65, 0), stone()),
        (target, stone()),
    ]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::OutOfSight)
    );
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::OutOfSight)
    );
    let entry = *p.ledger().get(QueryKind::Block, FactKind::BlockState).unwrap();
    assert_eq!(entry.count(), 2);
    assert_eq!(entry.first_tick(), entry.last_tick());
    assert_eq!(p.ledger().len(), 1);

    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::OutOfSight)
    );
    let entry = *p.ledger().get(QueryKind::Block, FactKind::BlockState).unwrap();
    assert_eq!(entry.count(), 3);
    assert!(entry.last_tick() > entry.first_tick());

    assert_eq!(
        p.entity_health(Caller::Other, 1).reason(),
        Some(Reason::Denied)
    );
}

#[test]
fn a_denied_region_is_ledgered() {
    let world = world_with(&[]);
    let config = Config { max_region_blocks: 8, ..Config::default() };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    let answer = p.blocks_in(
        Caller::Other,
        &world,
        BlockRegion::around(BlockPos::new(0, 65, 0), 2),
        BlockFilter::Any,
    );
    assert_eq!(answer.reason(), Some(Reason::Denied));
    assert!(p.ledger().get(QueryKind::BlocksIn, FactKind::RegionSize).is_some());
}

#[test]
fn blocks_in_and_nearest_block_skip_hidden_blocks() {
    let log = find_block("oak_log", &[]);
    let near = BlockPos::new(3, 65, 0);
    let far = BlockPos::new(6, 70, 0);
    let world = world_with(&[
        (BlockPos::new(2, 65, 0), stone()),
        (near, log),
        (far, log),
    ]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);

    let region = BlockRegion::new(BlockPos::new(0, 64, -1), BlockPos::new(7, 70, 1));
    let answer = p.blocks_in(Caller::Other, &world, region, BlockFilter::Kind(BlockKind::OakLog));
    let blocks = answer.value().unwrap();
    assert_eq!(blocks.len(), 1, "{answer:?}");
    assert_eq!(blocks[0].pos, far);

    p.begin_tick(&world);
    let answer = p.nearest_block(Caller::Other, &world, BlockKind::OakLog, 16);
    assert_eq!(answer.value().unwrap().pos, far, "{answer:?}");
}

#[test]
fn a_desynced_world_answers_never_seen() {
    let target = BlockPos::new(4, 65, 0);
    let mut world = world_with(&[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert!(p.block(Caller::Other, &world, target).is_known());

    world.invalidate(0, "test");
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).reason(),
        Some(Reason::NeverSeen)
    );
    assert_eq!(
        p.own_position(&world).reason(),
        Some(Reason::NeverSeen)
    );
}

// --- The visibility cache across ticks ----------------------------------------

#[test]
fn the_block_cache_survives_ticks_and_spends_no_ray() {
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let first = p.block(Caller::Other, &world, target);
    assert_eq!(first.provenance(), Some(Provenance::Sight), "{first:?}");
    assert!(p.rays_used_this_tick() > 0);

    for tick in 0..3 {
        p.begin_tick(&world);
        let again = p.block(Caller::Other, &world, target);
        assert_eq!(again, first, "tick {tick} must answer exactly the same");
        assert_eq!(
            p.rays_used_this_tick(),
            0,
            "tick {tick} must answer from the cache, not from a ray"
        );
    }
}

#[test]
fn a_block_change_in_a_crossed_chunk_invalidates_the_cache() {
    // The eye is in chunk (0,0), the target in chunk (2,0) and the wall that
    // appears later in chunk (1,0): a chunk the rays cross but that holds
    // neither the eye nor the target.
    let target = BlockPos::new(40, 65, 0);
    let wall = BlockPos::new(20, 65, 0);
    assert_eq!((target.x >> 4, wall.x >> 4), (2, 1));
    let mut world = world_across_chunks(4, &[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );

    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(p.rays_used_this_tick(), 0, "nothing changed, so the cache holds");

    set_block(&mut world, wall, stone());
    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_ne!(
        answer.provenance(),
        Some(Provenance::Sight),
        "a wall in a crossed chunk hides the target: {answer:?}"
    );
    assert!(
        p.rays_used_this_tick() > 0,
        "the changed chunk must force a new ray"
    );
}

#[test]
fn an_eye_move_to_another_cell_invalidates_the_cache() {
    let target = BlockPos::new(4, 65, 0);
    let mut world = world_with(&[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );

    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(p.rays_used_this_tick(), 0);

    // One cell along x is a new cell, so the entry no longer applies.
    set_player(&mut world, Vec3::new(1.5, FEET_Y, 0.5));
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
    assert!(
        p.rays_used_this_tick() > 0,
        "the eye moved to another cell, so the ray must run again"
    );
}

#[test]
fn a_chunk_unload_invalidates_the_cache() {
    let target = BlockPos::new(40, 65, 0);
    let mut world = world_across_chunks(4, &[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );

    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, target).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(p.rays_used_this_tick(), 0);

    // Unloading a chunk the ray crosses leaves a hole: the walk cannot prove
    // the path any more, and the cached "visible" must not survive.
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ForgetLevelChunk(ClientboundForgetLevelChunk {
            pos: ChunkPos::new(1, 0),
        }),
    );
    p.begin_tick(&world);
    let answer = p.block(Caller::Other, &world, target);
    assert_ne!(
        answer.provenance(),
        Some(Provenance::Sight),
        "the gap hides the target: {answer:?}"
    );
    assert!(p.rays_used_this_tick() > 0, "the unload must force a new ray");
}

#[test]
fn an_entity_cache_entry_survives_a_tick_and_dies_with_its_chunk() {
    let mut world = world_across_chunks(4, &[]);
    add_zombie(&mut world, 5, Vec3::new(40.5, FEET_Y, 0.5));
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.entity(Caller::Other, &world, 5).provenance(),
        Some(Provenance::Sight)
    );
    assert!(p.rays_used_this_tick() > 0);

    p.begin_tick(&world);
    assert_eq!(
        p.entity(Caller::Other, &world, 5).provenance(),
        Some(Provenance::Sight)
    );
    assert_eq!(p.rays_used_this_tick(), 0, "the entity answer is cached");

    // A wall in a chunk the entity rays cross hides it again.
    for y in [63, 64, 65] {
        set_block(&mut world, BlockPos::new(20, y, 0), stone());
    }
    p.begin_tick(&world);
    let answer = p.entity(Caller::Other, &world, 5);
    assert_eq!(answer.reason(), Some(Reason::OutOfSight), "{answer:?}");
    assert!(p.rays_used_this_tick() > 0);
}

// --- The sight table against the step-4 per-cell rule ------------------------

/// The step-4 description: an override decides, otherwise the collision shape.
/// Kept here verbatim, so the test compares the table against the rule it
/// replaced instead of against itself.
fn old_description(state: BlockState) -> (bool, Vec<Aabb>) {
    let block = Box::<dyn BlockTrait>::from(state);
    let name = block.as_block_kind().to_str().trim_start_matches("minecraft:");
    let shape = state.base_collision_shape();
    let (pass, boxes) = match crate::see_through::see_through_override(name) {
        Some(pass) => (pass, Vec::new()),
        None if shape.is_empty() => (true, Vec::new()),
        None => (false, shape.to_aabbs()),
    };
    let unit = Aabb {
        min: Vec3::ZERO,
        max: Vec3::new(1.0, 1.0, 1.0),
    };
    let boxes = if !pass && boxes.is_empty() { vec![unit] } else { boxes };
    (pass, boxes)
}

/// The step-4 per-cell test: `true` when the cell's boxes stop the segment.
fn old_blocks(from: Vec3, delta: Vec3, cell: BlockPos, pass: bool, boxes: &[Aabb]) -> bool {
    if pass {
        return false;
    }
    let (cx, cy, cz) = (f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    boxes.iter().any(|bx| {
        crate::sight::ray_hits_aabb(
            from,
            delta,
            Vec3::new(cx + bx.min.x, cy + bx.min.y, cz + bx.min.z),
            Vec3::new(cx + bx.max.x, cy + bx.max.y, cz + bx.max.z),
        )
    })
}

/// Segments that cross the cell at `cell`, so the walk would visit it: along an
/// axis, diagonally, through the middle, through a quarter, grazing a face,
/// exactly on an edge, and only touching an end.
fn cell_probes(cell: BlockPos) -> Vec<(Vec3, Vec3)> {
    let o = Vec3::new(f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    let local = |from: (f64, f64, f64), delta: (f64, f64, f64)| {
        (
            Vec3::new(o.x + from.0, o.y + from.1, o.z + from.2),
            Vec3::new(delta.0, delta.1, delta.2),
        )
    };
    vec![
        local((-1.0, 0.5, 0.5), (2.0, 0.0, 0.0)),
        local((0.5, -1.0, 0.5), (0.0, 2.0, 0.0)),
        local((0.5, 0.5, -1.0), (0.0, 0.0, 2.0)),
        local((-1.0, -1.0, 0.5), (2.5, 2.5, 0.0)),
        local((-0.5, -0.5, -0.5), (2.0, 2.0, 2.0)),
        local((0.5, 0.5, 0.5), (0.01, 0.01, 0.01)),
        local((-1.0, 0.25, 0.25), (2.0, 0.0, 0.0)),
        local((-1.0, 0.75, 0.75), (2.0, 0.0, 0.0)),
        local((-1.0, 1.0, 0.5), (2.0, 0.0, 0.0)),
        local((-1.0, 0.5, 0.0), (2.0, 0.0, 0.0)),
        local((0.0, 0.0, -1.0), (0.0, 0.0, 2.0)),
        local((2.0, 0.5, 0.5), (-2.0, 0.0, 0.0)),
    ]
}

#[test]
fn the_sight_table_agrees_with_the_old_per_cell_rule_for_every_state() {
    let mut table = SightTable::new();
    let cell = BlockPos::new(4, -3, 7);
    let probes = cell_probes(cell);
    let mut described = 0usize;
    let mut state_id = 0u32;
    while let Ok(state) = BlockState::try_from(state_id) {
        state_id += 1;
        described += 1;
        let (pass, boxes) = old_description(state);
        match table.sight(state) {
            Sight::Pass => assert!(pass, "{state:?}: the table passes a blocking state"),
            Sight::Opaque => {
                assert!(!pass, "{state:?}: the table blocks a passing state");
                assert!(
                    boxes.iter().any(|bx| bx.min == Vec3::ZERO
                        && bx.max == Vec3::new(1.0, 1.0, 1.0)),
                    "{state:?}: opaque without a full cell box"
                );
            }
            Sight::Boxes(_) => assert!(!pass, "{state:?}: boxed state that passes"),
            Sight::Unfilled => panic!("{state:?}: sight() did not fill the entry"),
        }
        for (from, delta) in &probes {
            let old = old_blocks(*from, *delta, cell, pass, &boxes);
            let new = crate::sight::blocks_cell(&mut table, state, *from, *delta, cell);
            assert_eq!(
                old, new,
                "{state:?} disagrees on the segment {:?}..{:?}",
                from, *from + *delta
            );
        }
    }
    assert_eq!(described, usize::from(BlockState::MAX_STATE) + 1);
}
