//! Hand-built worlds for the line-of-sight, budget, memory, entity, cache and
//! F3 tests. Nothing here is a real chunk: empty chunks plus block updates are
//! enough to pin the rules.

use std::sync::Arc;
use std::time::Instant;

use azalea_block::{BlockState, BlockTrait};
use azalea_buf::AzBuf;
use azalea_core::{
    aabb::Aabb,
    delta::LpVec3,
    entity_id::MinecraftEntityId,
    game_type::GameMode,
    position::{ChunkPos, Vec3},
};
use azalea_entity::LookDirection;
use azalea_physics::collision::BlockWithShape;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundAddEntity, ClientboundForgetLevelChunk, ClientboundGamePacket,
        ClientboundLevelChunkWithLight, ClientboundLogin, ClientboundSetEntityData,
        ClientboundSetHealth, ClientboundSetTime, ClientboundSound, ClientboundSoundEntity,
        ServerboundGamePacket, ServerboundMovePlayerPos, ServerboundMovePlayerRot,
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

fn set_rotation(world: &mut World, yaw: f32, pitch: f32) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerRot(ServerboundMovePlayerRot {
            look_direction: LookDirection::new(yaw, pitch),
            flags: Default::default(),
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

/// The 3 x 3 chunks around the origin, so a radius-8 sweep at (0.5, ., 0.5)
/// never runs past a loaded chunk.
fn world_around(blocks: &[(BlockPos, BlockState)]) -> World {
    world_area(1, blocks)
}

/// Chunks `-radius..=radius` on both axes, so a 64-block view scan reaches
/// loaded chunks in every direction.
fn world_area(radius: i32, blocks: &[(BlockPos, BlockState)]) -> World {
    let mut world = base_world();
    for x in -radius..=radius {
        for z in -radius..=radius {
            if x == 0 && z == 0 {
                continue;
            }
            world.apply_clientbound_play(
                0,
                &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, z)),
            );
        }
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
    add_entity(world, id, EntityKind::Zombie, pos);
}

fn add_entity(world: &mut World, id: i32, kind: EntityKind, pos: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(id),
            uuid: Uuid::from_u128(id as u128),
            entity_type: kind,
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
fn first_blocking_cell_uses_the_sight_table_and_spends_one_ray() {
    let glass = find_block("glass", &[]);
    let dirt = find_block("dirt", &[]);
    let through = BlockPos::new(2, 65, 0);
    let blocker = BlockPos::new(3, 65, 0);
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(through, glass), (blocker, dirt)]);
    let from = Vec3::new(0.5, 65.0, 0.5);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.first_blocking_cell(Caller::Other, &world, from, target),
        Some(blocker),
        "glass passes and the dirt behind it blocks"
    );
    assert_eq!(p.rays_used_this_tick(), 1, "one ray per call");

    let mut p = Perception::new(Config {
        ray_budget: 0,
        ..Config::default()
    });
    p.begin_tick(&world);
    assert_eq!(
        p.first_blocking_cell(Caller::Other, &world, from, target),
        None,
        "no budget, no answer"
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
fn reflex_spending_its_reserve_does_not_shrink_the_other_share() {
    // N = 6, R = 2: Other always has 4 rays, whatever Reflex spent from the
    // reserve before it queries.
    let config = Config { ray_budget: 6, reflex_reserve: 2, ..Config::default() };
    let targets = [BlockPos::new(4, 65, 0), BlockPos::new(0, 65, 4)];
    let blocks: Vec<(BlockPos, BlockState)> = targets.iter().map(|pos| (*pos, stone())).collect();
    let world = world_with(&blocks);
    let mut p = Perception::new(config);
    p.begin_tick(&world);

    for target in targets {
        assert_eq!(
            p.block(Caller::Reflex, &world, target).provenance(),
            Some(Provenance::Sight),
            "one open-air center ray reaches {target:?}"
        );
    }
    assert_eq!(p.rays_used_this_tick(), 2, "Reflex spent its reserve only");
    assert_eq!(
        p.rays_remaining(Caller::Other),
        4,
        "Other keeps the whole N - R share after Reflex ran first"
    );
}

#[test]
fn other_spending_its_share_does_not_shrink_the_reflex_reserve() {
    // The same N = 6, R = 2 with the other order: Other spends its 4 rays
    // first, then Reflex still has its 2 reserved rays.
    let config = Config { ray_budget: 6, reflex_reserve: 2, ..Config::default() };
    let targets = [
        BlockPos::new(4, 65, 0),
        BlockPos::new(0, 65, 4),
        BlockPos::new(4, 64, 0),
        BlockPos::new(4, 66, 0),
        BlockPos::new(4, 65, 4),
        BlockPos::new(8, 65, 2),
    ];
    let blocks: Vec<(BlockPos, BlockState)> = targets.iter().map(|pos| (*pos, stone())).collect();
    let world = world_with(&blocks);
    let mut p = Perception::new(config);
    p.begin_tick(&world);

    for target in &targets[..4] {
        assert_eq!(
            p.block(Caller::Other, &world, *target).provenance(),
            Some(Provenance::Sight),
            "one open-air center ray reaches {target:?}"
        );
    }
    assert_eq!(p.rays_used_this_tick(), 4, "Other spent its whole share");
    assert_eq!(p.rays_remaining(Caller::Other), 0);
    assert_eq!(
        p.rays_remaining(Caller::Reflex),
        2,
        "Reflex keeps its reserve after Other ran first"
    );

    for target in &targets[4..] {
        assert_eq!(
            p.block(Caller::Reflex, &world, *target).provenance(),
            Some(Provenance::Sight),
            "the reserve still casts rays"
        );
    }
    assert_eq!(p.rays_used_this_tick(), 6);
    assert_eq!(p.rays_remaining(Caller::Reflex), 0);
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
fn own_air_and_burning_are_hud_facts() {
    let mut world = world_with(&[]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    // Nothing sent yet: unknown, and no ray was spent.
    assert_eq!(p.air(&world).reason(), Some(Reason::NeverSeen));
    assert_eq!(p.burning(&world).reason(), Some(Reason::NeverSeen));
    assert_eq!(p.rays_used_this_tick(), 0);

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(7),
            packed_items: azalea_entity::EntityMetadataItems(vec![
                azalea_entity::EntityDataItem {
                    index: 0,
                    value: azalea_entity::EntityDataValue::Byte(0b1),
                },
                azalea_entity::EntityDataItem {
                    index: 1,
                    value: azalea_entity::EntityDataValue::Int(80),
                },
            ]),
        }),
    );
    p.begin_tick(&world);
    let air = p.air(&world);
    assert_eq!(air.value().copied(), Some(80));
    assert_eq!(air.provenance(), Some(Provenance::Sight));
    assert_eq!(air.age_ticks(), Some(0));
    let burning = p.burning(&world);
    assert_eq!(burning.value().copied(), Some(true));
    assert_eq!(burning.provenance(), Some(Provenance::Sight));
    assert_eq!(p.rays_used_this_tick(), 0, "a HUD fact costs no ray");
}

#[test]
fn f3_facts_are_allowed_without_a_ray() {    let mut world = world_with(&[]);
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
    assert_eq!(p.biome(&world).reason(), Some(Reason::NeverSeen));
    assert_eq!(p.light_level(&world).reason(), Some(Reason::NeverSeen));
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
fn blocks_in_remembers_every_block_it_sees() {
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);

    let answer = p.blocks_in(
        Caller::Other,
        &world,
        BlockRegion::around(target, 1),
        BlockFilter::Kind(BlockKind::Stone),
    );
    assert_eq!(answer.value().map(Vec::len), Some(1), "{answer:?}");
    let (remembered, _age) = p
        .remembered(target)
        .expect("a block seen by blocks_in must go to memory");
    assert_eq!(remembered.state, stone());
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

// --- The known map and the per-tick sweep ------------------------------------

fn flat_floor(y: i32, radius: i32) -> Vec<(BlockPos, BlockState)> {
    let mut blocks = Vec::new();
    for x in -radius..=radius {
        for z in -radius..=radius {
            blocks.push((BlockPos::new(x, y, z), stone()));
        }
    }
    blocks
}

#[test]
fn the_sweep_fills_the_known_map_and_every_cell_was_reachable() {
    let world = world_around(&flat_floor(62, 8));
    let config = Config {
        sweep_radius: 8,
        ray_budget: 4_000,
        reflex_reserve: 1_000,
        ..Config::default()
    };
    let mut p = Perception::new(config);
    let mut rays = 0;
    let mut probes = 0;
    for _ in 0..10 {
        p.begin_tick(&world);
        let sweep = p.sweep(&world);
        rays += sweep.rays;
        probes += sweep.probes;
    }
    assert!(rays > 0, "the sweep must spend the leftover rays");
    assert!(
        p.memory_len() > 1_000,
        "only {} cells became known after {rays} rays and {probes} probes",
        p.memory_len()
    );
    assert!(
        p.known(BlockPos::new(4, 64, 0)).is_some(),
        "open air near the player must be known"
    );

    // Brute check: every cell the map claims was reached by a ray from the
    // eye it was seen at (the eye never moved in this test).
    let eye = p.eye(&world).expect("eye");
    let mut table = SightTable::new();
    let mut reader = CellReader::new();
    for (pos, _) in p.known.seen_entries() {
        reader.prime(&world, pos);
        let mut points = [Vec3::ZERO; 7];
        let count = crate::sight::target_points(&world, pos, &mut table, &mut points, &mut reader);
        let reached = points[..count].iter().any(|point| {
            let mut crossed = CrossedChunks::default();
            crate::sight::line_of_sight(&world, eye, *point, pos, &mut table, &mut crossed)
        });
        assert!(
            reached,
            "{pos:?} is in the known map but no ray reaches it"
        );
    }
}

#[test]
fn the_sweep_never_spends_the_reflex_reserve() {
    let world = world_with(&flat_floor(62, 8));
    let config = Config {
        sweep_radius: 8,
        ray_budget: 10,
        reflex_reserve: 4,
        ..Config::default()
    };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, BlockPos::new(4, 65, 0))
            .provenance(),
        Some(Provenance::Sight),
        "a caller spends one ray first"
    );
    let sweep = p.sweep(&world);
    assert!(sweep.rays > 0);
    assert_eq!(p.rays_used_this_tick(), 6, "only the unreserved share is spendable");
    assert_eq!(
        p.rays_remaining(Caller::Reflex),
        4,
        "the reflex reserve is untouched"
    );
    assert!(p.rays_remaining(Caller::Other) == 0);
}

#[test]
fn a_log_in_view_enters_the_known_map_after_one_scan() {
    let log_state = find_block("oak_log", &[]);
    let log = BlockPos::new(50, 65, 0);
    let mut world = world_area(4, &[(log, log_state)]);
    // Looking east (+X), straight at the log 50 blocks away.
    set_rotation(&mut world, 270.0, 0.0);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    p.scan_view(&world);
    assert_eq!(
        p.known(log).map(|seen| seen.state),
        Some(log_state),
        "a log 50 blocks ahead must be seen"
    );
}

#[test]
fn a_log_behind_the_look_stays_unknown() {
    let log = BlockPos::new(50, 65, 0);
    let mut world = world_area(4, &[(log, find_block("oak_log", &[]))]);
    // Looking west (-X), away from the log.
    set_rotation(&mut world, 90.0, 0.0);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    p.scan_view(&world);
    assert_eq!(p.known(log), None, "the scan reports only what is in view");
}

#[test]
fn a_log_between_two_half_fan_rows_is_seen_by_one_scan() {
    // The old scan cast every other row: rows offset by multiples of five
    // degrees. Put the log's bearing on an odd row (2.5 degrees off), so one
    // full-fan scan sees it while the old half-fan could not.
    let log_state = find_block("oak_log", &[]);
    let log = BlockPos::new(50, 65, 2);
    let mut world = world_area(4, &[(log, log_state)]);
    let dx = f64::from(log.x) + 0.5 - 0.5;
    let dz = f64::from(log.z) + 0.5 - 0.5;
    let bearing = (-dx).atan2(dz).to_degrees().rem_euclid(360.0);
    set_rotation(&mut world, (bearing - 2.5) as f32, 0.0);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    p.scan_view(&world);
    assert_eq!(
        p.known(log).map(|seen| seen.state),
        Some(log_state),
        "one full-fan scan must see a log whose bearing falls between two old half-fan rows"
    );
}

#[test]
fn a_changed_chunk_revalidates_its_seen_cells_against_the_world() {
    let log_state = find_block("oak_log", &[]);
    let log = BlockPos::new(50, 65, 0);
    let mut world = world_across_chunks(4, &[(log, log_state)]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(
        p.block(Caller::Other, &world, log).provenance(),
        Some(Provenance::Sight)
    );
    assert!(
        p.nearest_known(&world, 64, |_| false, |_, state| state == log_state)
            .is_some(),
        "the fresh sighting is returned"
    );

    // A change elsewhere in the log's chunk must not blind the remembered
    // cell: the log is still there, so it still answers.
    set_block(&mut world, BlockPos::new(50, 64, 0), stone());
    p.begin_tick(&world);
    assert_eq!(
        p.nearest_known(&world, 64, |_| false, |_, state| state == log_state)
            .map(|(pos, _)| pos),
        Some(log),
        "an unrelated change in the chunk keeps the log"
    );

    // The log itself removed: the changed chunk is re-validated and the ghost
    // is gone, although no ray has seen the cell since.
    set_block(&mut world, log, BlockState::AIR);
    p.begin_tick(&world);
    assert_eq!(
        p.nearest_known(&world, 64, |_| false, |_, state| state == log_state),
        None,
        "a remembered log that is gone must not answer nearest_known"
    );
}

#[test]
fn for_each_known_visits_only_seen_cells_within_the_radius() {
    let near = BlockPos::new(2, 65, 6);
    let far = BlockPos::new(40, 65, 0);
    let near_state = find_block("oak_log", &[]);
    let far_state = find_block("coal_ore", &[]);
    let world = world_area(4, &[(near, near_state), (far, far_state)]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert!(p.block(Caller::Other, &world, near).is_known());
    assert!(p.block(Caller::Other, &world, far).is_known());
    let mut seen = Vec::new();
    assert_eq!(
        p.for_each_known(&world, 16, |pos, state| seen.push((pos, state))),
        Some(()),
        "the eye is known, so the visit runs"
    );
    assert_eq!(seen, vec![(near, near_state)], "only the cell inside 16 blocks");

    // A wider radius reaches the farther cell; a cell nobody saw is not in the
    // map to begin with, so it can never be visited.
    let mut wide = Vec::new();
    p.for_each_known(&world, 64, |pos, state| wide.push((pos, state)));
    assert_eq!(wide, vec![(near, near_state), (far, far_state)]);

    // Walking the remembered set spends no ray: the ledger does not move.
    let before = p.ledger().total();
    p.for_each_known(&world, 64, |_, _| {});
    assert_eq!(p.ledger().total(), before, "a known cell costs no ray");

    // An unknown eye is not "nothing is known".
    let mut fresh = Perception::new(Config::default());
    assert_eq!(fresh.for_each_known(&World::new(), 16, |_, _| {}), None);
    fresh.begin_tick(&World::new());
    assert_eq!(
        fresh.for_each_known(&World::new(), 16, |_, _| {}),
        None,
        "a valid world without a position has no eye to measure from"
    );
}

#[test]
fn a_remembered_cell_gone_from_the_world_is_revalidated() {
    // The same re-validation `nearest_known` does: a changed chunk shows the
    // live state, so a caller counts what is there now, not a ghost.
    let log = BlockPos::new(50, 65, 0);
    let log_state = find_block("oak_log", &[]);
    let mut world = world_across_chunks(4, &[(log, log_state)]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert!(p.block(Caller::Other, &world, log).is_known());
    set_block(&mut world, log, BlockState::AIR);
    p.begin_tick(&world);

    let mut seen = Vec::new();
    p.for_each_known(&world, 64, |pos, state| seen.push((pos, state)));
    assert_eq!(seen, vec![(log, BlockState::AIR)], "the removed log is aired");
}

#[test]
fn entity_kinds_near_counts_visible_mobs_and_skips_items_orbs_and_the_player() {
    let mut world = world_with(&[]);
    add_zombie(&mut world, 10, Vec3::new(2.5, FEET_Y, 0.5));
    add_zombie(&mut world, 11, Vec3::new(3.5, FEET_Y, 0.5));
    add_entity(&mut world, 12, EntityKind::Item, Vec3::new(4.5, FEET_Y, 0.5));
    add_entity(
        &mut world,
        13,
        EntityKind::ExperienceOrb,
        Vec3::new(5.5, FEET_Y, 0.5),
    );
    // The store redirects the own player's entity data, but a server may still
    // spawn it: it is never part of the body's surroundings.
    add_entity(&mut world, 7, EntityKind::Player, Vec3::new(1.5, FEET_Y, 0.5));

    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let answer = p.entity_kinds_near(Caller::Other, &world, 32);
    let counts = answer.value().unwrap_or_else(|| panic!("{answer:?}"));
    assert_eq!(counts.get(&EntityKind::Zombie), Some(&2));
    for skipped in [EntityKind::Item, EntityKind::ExperienceOrb, EntityKind::Player] {
        assert_eq!(counts.get(&skipped), None, "{skipped:?} is not surroundings");
    }
    assert_eq!(counts.values().sum::<u32>(), 2);

    // A desynced world cannot answer, exactly like every other query.
    world.invalidate(0, "test");
    p.begin_tick(&world);
    assert_eq!(
        p.entity_kinds_near(Caller::Other, &world, 32).reason(),
        Some(Reason::NeverSeen)
    );
}

#[test]
fn workable_keeps_fluids_out_except_for_the_blocks_skills_use() {
    use std::collections::HashMap;

    let water = find_block("water", &[]);
    let dry = BlockPos::new(0, 64, 0);
    let wet = BlockPos::new(2, 64, 0);
    // A lava floor is just as bad as a water ceiling.
    let over_lava = BlockPos::new(4, 64, 0);
    let mut map: HashMap<BlockPos, BlockState> = HashMap::new();
    map.insert(dry, stone());
    map.insert(wet, stone());
    map.insert(BlockPos::new(2, 65, 0), water);
    map.insert(over_lava, stone());
    map.insert(BlockPos::new(4, 63, 0), find_block("lava", &[]));
    let lookup = |pos: BlockPos| map.get(&pos).copied();

    assert!(workable(dry, stone(), lookup), "dry stone is workable");
    assert!(
        !workable(wet, stone(), lookup),
        "a stone holding back water is not a target"
    );
    assert!(exposes_fluid(wet, &lookup));
    assert!(!workable(over_lava, stone(), lookup));

    // A workstation is used where it stands, not mined: water beside it does
    // not hide it.
    for name in [
        "crafting_table",
        "furnace",
        "chest",
        "smoker",
        "blast_furnace",
        "barrel",
    ] {
        let pos = BlockPos::new(6, 64, 0);
        let state = find_block(name, &[]);
        let mut station: HashMap<BlockPos, BlockState> = HashMap::new();
        station.insert(pos, state);
        station.insert(BlockPos::new(6, 65, 0), water);
        let lookup = |cell: BlockPos| station.get(&cell).copied();
        assert!(exposes_fluid(pos, &lookup), "{name} is next to water");
        assert!(workable(pos, state, lookup), "{name} still counts");
    }
}

#[test]
fn the_view_scan_never_spends_the_reflex_reserve() {
    let world = world_area(1, &[]);
    let config = Config {
        ray_budget: 10,
        reflex_reserve: 4,
        ..Config::default()
    };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    p.scan_view(&world);
    assert_eq!(
        p.rays_used_this_tick(),
        6,
        "the scan spends only the unreserved share"
    );
    assert_eq!(p.rays_remaining(Caller::Reflex), 4);
    assert_eq!(p.rays_remaining(Caller::Other), 0);
}

#[test]
fn the_sweep_runs_the_view_scan_on_its_period() {
    let log = BlockPos::new(50, 65, 0);
    let mut world = world_area(4, &[(log, find_block("oak_log", &[]))]);
    set_rotation(&mut world, 270.0, 0.0);
    let config = Config {
        sweep_radius: 1,
        ..Config::default()
    };
    let mut p = Perception::new(config);
    for _ in 0..9 {
        p.begin_tick(&world);
        p.sweep(&world);
    }
    assert_eq!(p.known(log), None, "no scan before the period");
    p.begin_tick(&world);
    p.sweep(&world);
    assert!(
        p.known(log).is_some(),
        "the tenth tick scans the current view"
    );
}

#[test]
#[ignore = "timing measurement: cargo test -p praxion-perception --release -- --ignored bench_view_scan --nocapture"]
fn bench_view_scan() {
    let world = world_area(4, &flat_floor(62, 64));
    let mut p = Perception::new(Config::default());
    let mut samples = Vec::new();
    for _ in 0..50 {
        p.begin_tick(&world);
        let started = Instant::now();
        p.scan_view(&world);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let p50 = samples[samples.len() / 2];
    let max = *samples.last().expect("samples");
    println!(
        "view scan: p50 {p50:.3} ms, max {max:.3} ms over {} scans",
        samples.len()
    );
}

#[test]
#[ignore = "timing measurement: cargo test -p praxion-perception --release -- --ignored bench_nearest_known_worst_case --nocapture"]
fn bench_nearest_known_worst_case() {
    let world = world_area(4, &[]);
    let config = Config {
        memory_capacity: 3_000_000,
        ..Config::default()
    };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    let eye_cell = p.eye_cell(&world).expect("eye");
    let focus = section_key_of(eye_cell);
    let air = BlockState::AIR;
    let radius = 64;
    for x in eye_cell.x - radius..=eye_cell.x + radius {
        for y in eye_cell.y - radius..=eye_cell.y + radius {
            for z in eye_cell.z - radius..=eye_cell.z + radius {
                let pos = BlockPos::new(x, y, z);
                // The cell is fresh in its chunk, so the revision check does
                // not skip it and the bench measures the full scan.
                let revision = world.chunk_revision(ChunkPos::from(pos)).unwrap_or(0);
                p.known.record_seen(pos, air, 1, revision, focus);
            }
        }
    }
    let stone = stone();
    let mut samples = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let found = p.nearest_known(&world, radius as u32, |_| false, |_, state| state == stone);
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        assert!(found.is_none(), "the full map holds no stone");
    }
    samples.sort_by(f64::total_cmp);
    let p50 = samples[samples.len() / 2];
    let max = *samples.last().expect("samples");
    println!(
        "nearest_known(radius 64, full map, no match): p50 {p50:.3} ms, max {max:.3} ms"
    );
}

#[test]
fn a_hidden_cell_changing_keeps_its_old_state_and_age() {
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(target, stone())]);
    let config = Config { sweep_radius: 8, ..Config::default() };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    p.sweep(&world);
    assert_eq!(p.known(target).map(|seen| seen.state), Some(stone()));

    // A wall appears between the eye and the target, and the hidden target
    // changes. The sweep may test it again, but no ray reaches it.
    let mut world = world;
    set_block(&mut world, BlockPos::new(2, 65, 0), stone());
    set_block(&mut world, target, find_block("dirt", &[]));
    p.begin_tick(&world);
    p.sweep(&world);
    let (remembered, age) = p.remembered(target).expect("the old sighting stays");
    assert_eq!(remembered.state, stone(), "an unseen change is never known");
    assert!(age >= 1, "the fact keeps its age");
}

#[test]
fn the_known_map_keeps_the_sections_within_its_bound() {
    // Radius 16 keeps 3 * 3 * 3 = 27 sections. Sweeping a much larger radius
    // must still respect the bound (the sweep's own radius is the config's
    // sweep_radius, so move the player between sweeps).
    let config = Config {
        known_radius: 16,
        sweep_radius: 8,
        ray_budget: 4_000,
        reflex_reserve: 1_000,
        ..Config::default()
    };
    let mut world = world_across_chunks(10, &flat_floor(62, 8));
    let mut p = Perception::new(config);
    for x in [0, 16, 32, 48, 64] {
        set_player(&mut world, Vec3::new(f64::from(x) + 0.5, FEET_Y, 0.5));
        for _ in 0..5 {
            p.begin_tick(&world);
            p.sweep(&world);
        }
    }
    assert!(
        p.known_sections() <= p.known_section_capacity(),
        "{} sections exceed the bound {}",
        p.known_sections(),
        p.known_section_capacity()
    );
    assert_eq!(p.known_section_capacity(), 27);
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

#[test]
fn a_confirmed_change_enters_the_known_map_at_once() {
    // A placed table must be near on the tick its receipt confirms, not when
    // the sweep next reaches its cell.
    let table = BlockPos::new(2, 65, 0);
    let mut world = world_with(&[]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert!(p.known(table).is_none());

    set_block(&mut world, table, find_block("crafting_table", &[]));
    p.note_changed(&world, table);
    let remembered = p.known(table).expect("the change is remembered");
    assert_eq!(remembered.state, find_block("crafting_table", &[]));
}

#[test]
fn a_cell_changed_back_to_air_is_no_longer_scaffolding() {
    // Review finding 3: the scaffolding set cannot be write-only. The body can
    // dig its own column down (`dig_down`, `branch_mine`) or a block can be
    // broken; a stale entry would hide a real block that later fills the cell
    // and count a column that no longer exists.
    let cell = BlockPos::new(2, 65, 0);
    let log = find_block("oak_log", &[]);
    let mut world = world_with(&[(cell, log)]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    scaffold::note(cell);
    p.note_changed(&world, cell);
    assert!(
        scaffold::is_scaffold(cell),
        "a cell that still holds its block stays scaffolding"
    );

    set_block(&mut world, cell, BlockState::from(BlockKind::Air));
    p.note_changed(&world, cell);
    assert!(
        !scaffold::is_scaffold(cell),
        "the emptied cell is scaffolding no more"
    );
}

#[test]
fn a_scaffold_cell_a_query_sees_empty_is_forgotten() {
    // Review finding 3, the other half: a column broken by something that is
    // not our own block intent (a creeper, another player) never reaches
    // `note_changed`. Any sight query that reads the cell as air forgets it,
    // so the set is not write-only.
    let cell = BlockPos::new(2, 65, 0);
    let mut world = world_with(&[(cell, find_block("oak_log", &[]))]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    scaffold::note(cell);

    set_block(&mut world, cell, BlockState::from(BlockKind::Air));
    let answer = p.block(Caller::Other, &world, cell);
    assert!(answer.value().is_some(), "the empty cell is seen");
    assert!(
        !scaffold::is_scaffold(cell),
        "the query that saw it empty forgot it"
    );
}

fn set_dying(world: &mut World, id: i32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(id),
            packed_items: azalea_entity::EntityMetadataItems(vec![azalea_entity::EntityDataItem {
                index: 6,
                value: azalea_entity::EntityDataValue::Pose(azalea_entity::Pose::Dying),
            }]),
        }),
    );
}

/// A killed mob stays in the store about 20 ticks with the `Dying` pose. It is
/// neither a target nor a threat: seen live on 2026-11-09, `hunt` kept the dead
/// cow as its target, timed out `no_target` and walked off after a sheep.
#[test]
fn a_dying_mob_is_neither_the_nearest_nor_in_the_list() {
    let mut world = world_with(&[]);
    add_zombie(&mut world, 1, Vec3::new(2.5, FEET_Y, 0.5));
    add_zombie(&mut world, 2, Vec3::new(5.5, FEET_Y, 0.5));
    set_dying(&mut world, 1);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);

    let nearest = p.nearest_entity(Caller::Other, &world, EntityKind::Zombie, 16);
    assert_eq!(nearest.value().map(|fact| fact.id), Some(2), "the live one, farther off");
    let listed: Vec<i32> = p
        .entities_near(Caller::Other, &world, 16, EntityFilter::Any)
        .value()
        .map(|facts| facts.iter().map(|fact| fact.id).collect())
        .unwrap_or_default();
    assert_eq!(listed, vec![2]);
}

#[test]
fn an_item_entity_fact_carries_the_kind_of_its_stack() {
    use azalea_registry::builtin::ItemKind;
    let mut world = world_with(&[]);
    add_entity(&mut world, 21, EntityKind::Item, Vec3::new(2.5, FEET_Y, 0.5));
    add_entity(&mut world, 22, EntityKind::Item, Vec3::new(2.5, FEET_Y, 2.5));
    add_zombie(&mut world, 23, Vec3::new(0.5, FEET_Y, 3.5));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(21),
            packed_items: azalea_entity::EntityMetadataItems(vec![azalea_entity::EntityDataItem {
                index: 8,
                value: azalea_entity::EntityDataValue::ItemStack(ItemStack::new(
                    ItemKind::CraftingTable,
                    1,
                )),
            }]),
        }),
    );
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let facts = p
        .entities_near(Caller::Other, &world, 8, EntityFilter::Any)
        .value()
        .cloned()
        .expect("the entities are in sight");
    let item_of = |id: i32| facts.iter().find(|fact| fact.id == id).expect("listed").item;
    assert_eq!(item_of(21), Some(ItemKind::CraftingTable));
    assert_eq!(item_of(22), None, "an item entity whose stack is not sent yet");
    assert_eq!(item_of(23), None, "a mob holds no stack");
}

#[test]
fn the_eye_block_is_the_live_cell_and_costs_no_ray() {
    let mut world = world_with(&[]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let eye_cell = p.eye_cell(&world).expect("the eye is known");
    let state = p.eye_block(&world).value().copied().expect("air at the eye");
    assert!(state.is_air());
    set_block(&mut world, eye_cell, find_block("water", &[("level", "0")]));
    p.begin_tick(&world);
    let answer = p.eye_block(&world);
    assert_eq!(
        answer.value().copied(),
        Some(find_block("water", &[("level", "0")])),
        "a block placed at the eye is read at once, not from memory"
    );
    assert_eq!(answer.provenance(), Some(Provenance::Sight));
    assert_eq!(p.rays_used_this_tick(), 0, "an own-state fact costs no ray");
    let unseen = Perception::new(Config::default());
    assert_eq!(unseen.eye_block(&world).reason(), Some(Reason::NeverSeen));
}
