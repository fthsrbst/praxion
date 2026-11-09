//! The privileged column read.
//!
//! A caller that wants to judge a column (explore asks whether a frontier cell
//! is open to the sky) cannot read the server's `WorldSurface` heightmap or the
//! blocks of the column itself: the body may never have seen them. Architecture
//! §5.1 allows that read only through this crate, as a privileged query whose
//! calls are ledgered.
//!
//! This module returns the raw column — the surface and the states above it —
//! and nothing else. The judgement ("is this column open") is the caller's
//! decision logic and stays in the caller; a copy of it here would be a second
//! hazard list and a second source for block tags.

use praxion_world::{BlockPos, BlockState, World};

use crate::Perception;

/// The most block states one privileged column read can return.
///
/// A read is bounded so a caller cannot ask for a whole dimension's height in
/// one call; the surface is always reported, so a caller that wants more than
/// this is looking at a roof, not a column.
pub const COLUMN_BLOCKS: usize = 48;

/// The raw contents of one block column above a body cell.
///
/// [`states`](Self::states) holds the states from [`first_y`](Self::first_y) up
/// to but excluding [`surface`](Self::surface), and is empty when the read
/// window (the caller's `max_blocks`) does not reach the surface. A missing
/// state is not a fact about the column: [`Perception::privileged_column`]
/// returns `None` only when the heightmap itself is unknown.
#[derive(Clone, Copy, Debug)]
pub struct PrivilegedColumn {
    /// The column's `WorldSurface`: the first y above its highest non-air
    /// block. The top block of the column sits at `surface - 1`.
    pub surface: i32,
    /// The first y [`states`](Self::states) describes.
    first_y: i32,
    /// How many of `blocks` are filled.
    len: usize,
    blocks: [BlockState; COLUMN_BLOCKS],
}

impl PrivilegedColumn {
    /// The first y [`states`](Self::states) describes.
    pub fn first_y(&self) -> i32 {
        self.first_y
    }

    /// The states from [`first_y`](Self::first_y) up to the surface.
    pub fn states(&self) -> &[BlockState] {
        &self.blocks[..self.len]
    }

    /// The state at world `y`, `None` outside the read window.
    ///
    /// A cell above [`states`](Self::states) is unread: the read stopped there
    /// or the surface is farther than the caller's `max_blocks`.
    pub fn state_at(&self, y: i32) -> Option<BlockState> {
        let index = usize::try_from(y.checked_sub(self.first_y)?).ok()?;
        (index < self.len).then(|| self.blocks[index])
    }
}

impl Perception {
    /// The raw column at `x, z` from `from_y` up to the heightmap surface.
    ///
    /// This is a privileged read (architecture §5.1): it reads the server's
    /// `WorldSurface` heightmap and blocks the body may never have seen, so
    /// every caller must ledger the read through
    /// [`ledger_privileged`](Self::ledger_privileged).
    ///
    /// `None` means the chunk or its heightmap is not loaded: that is not a
    /// fact about the column, and the caller decides what an unknown column
    /// means for it. Otherwise `surface` is the heightmap surface and `states`
    /// holds at most `max_blocks` states from `from_y` up to but excluding it.
    /// A surface farther than `max_blocks` above `from_y` returns the surface
    /// with no states: the caller must compare the surface itself, not the
    /// states, and needs no scan. `max_blocks` is clamped to
    /// [`COLUMN_BLOCKS`].
    pub fn privileged_column(
        &self,
        world: &World,
        x: i32,
        z: i32,
        from_y: i32,
        max_blocks: i32,
    ) -> Option<PrivilegedColumn> {
        let surface = world.surface_y(x, z)?;
        let mut column = PrivilegedColumn {
            surface,
            first_y: from_y,
            len: 0,
            blocks: [BlockState::AIR; COLUMN_BLOCKS],
        };
        let above = surface.saturating_sub(from_y);
        if above <= 0 || above > max_blocks.clamp(0, COLUMN_BLOCKS as i32) {
            return Some(column);
        }
        for y in from_y..from_y.saturating_add(above) {
            // A cell the store cannot answer for ends the read: `state_at`
            // returns `None` from there up, and the caller treats it as
            // covered exactly as it would a failed raw lookup.
            let Some(state) = world.block_state(BlockPos::new(x, y, z)) else {
                break;
            };
            column.blocks[column.len] = state;
            column.len += 1;
        }
        Some(column)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use azalea_block::BlockState;
    use azalea_buf::AzBuf;
    use azalea_core::game_type::{GameMode, OptionalGameType};
    use azalea_core::heightmap_kind::HeightmapKind;
    use azalea_protocol::packets::{
        common::CommonPlayerSpawnInfo,
        game::{
            ClientboundBlockUpdate, ClientboundGamePacket, ClientboundLevelChunkWithLight,
            ClientboundLogin, c_level_chunk_with_light::ClientboundLevelChunkPacketData,
        },
    };
    use azalea_registry::{
        DataRegistry,
        builtin::BlockKind,
        data::DimensionKind,
        identifier::Identifier,
    };
    use azalea_world::Chunk;
    use azalea_world::heightmap::Heightmap;
    use praxion_world::{BlockPos, World};

    use crate::{Config, FactKind, Perception, QueryKind};

    use super::*;

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
            player_id: azalea_core::entity_id::MinecraftEntityId(7),
            hardcore: false,
            levels: Vec::new(),
            max_players: 20,
            chunk_radius: 8,
            simulation_distance: 8,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            online_mode: false,
            common: spawn_info(),
            enforces_secure_chat: false,
        }
    }

    fn chunk_buffer(chunk: &Chunk) -> Arc<Box<[u8]>> {
        let mut buffer = Vec::new();
        for section in &chunk.sections {
            section.azalea_write(&mut buffer).expect("write section");
        }
        Arc::new(buffer.into_boxed_slice())
    }

    fn base_world() -> World {
        let mut world = World::new();
        world.follow(0);
        world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
        let chunk = Chunk::default();
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::LevelChunkWithLight(ClientboundLevelChunkWithLight {
                x: 0,
                z: 0,
                chunk_data: ClientboundLevelChunkPacketData {
                    heightmaps: Vec::new(),
                    buffer: chunk_buffer(&chunk),
                    block_entities: Vec::new(),
                },
                light_data: Default::default(),
            }),
        );
        world
    }

    /// A `WorldSurface` heightmap with every column at the world's minimum y.
    ///
    /// 256 columns at ceil(log2(384 + 1)) = 9 bits each pack into 36 u64 words.
    fn surface_heightmap() -> Heightmap {
        Heightmap::new(
            HeightmapKind::WorldSurface,
            384,
            -64,
            vec![0u64; 36].into_boxed_slice(),
        )
    }

    /// A world whose chunk packet carries a `WorldSurface` heightmap, so a
    /// `set_block` raises the column's surface like a live server's packet does.
    fn surface_world() -> World {
        let mut world = World::new();
        world.follow(0);
        world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
        let chunk = Chunk::default();
        let heightmap = surface_heightmap();
        let data = heightmap.data.data.clone();
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::LevelChunkWithLight(ClientboundLevelChunkWithLight {
                x: 0,
                z: 0,
                chunk_data: ClientboundLevelChunkPacketData {
                    heightmaps: vec![(HeightmapKind::WorldSurface, data)],
                    buffer: chunk_buffer(&chunk),
                    block_entities: Vec::new(),
                },
                light_data: Default::default(),
            }),
        );
        world
    }

    fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate { pos, block_state: state }),
        );
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn a_privileged_read_returns_the_surface_and_the_blocks_above_the_head() {
        let mut world = surface_world();
        let perception = Perception::new(Config::default());
        // Explore reads from the head cell: one above the feet at y = 64.
        let head = 65;

        // An untouched column: the heightmap says its first free y is min_y,
        // so nothing stands above the head and the read holds no state.
        assert_eq!(world.surface_y(0, 0), Some(-64));
        let column = perception
            .privileged_column(&world, 0, 0, head, COLUMN_BLOCKS as i32)
            .expect("the heightmap is there");
        assert_eq!(column.surface, -64);
        assert_eq!(column.first_y(), head);
        assert!(column.states().is_empty());

        // A stone roof raises the heightmap; the read returns every state from
        // the head up to the surface, air included, and judges none of them.
        set_block(&mut world, at(1, 70, 0), BlockState::from(BlockKind::Stone));
        assert_eq!(
            world.surface_y(1, 0),
            Some(71),
            "the block raised the surface"
        );
        let column = perception
            .privileged_column(&world, 1, 0, head, COLUMN_BLOCKS as i32)
            .expect("the heightmap is there");
        assert_eq!(column.surface, 71);
        assert_eq!(column.states().len(), 6, "y = 65..71");
        assert!(
            column.states()[..5].iter().all(|state| state.is_air()),
            "the five cells below the roof are air"
        );
        assert_eq!(
            column.state_at(70),
            Some(BlockState::from(BlockKind::Stone)),
            "the roof is the last state before the surface"
        );
        assert_eq!(column.state_at(71), None, "the surface cell is not read");
        assert_eq!(column.state_at(64), None, "the read starts at the head");

        // Leaves are opaque to the heightmap too, so the read reports them as
        // they are: whether they leave a column open is the caller's call.
        set_block(
            &mut world,
            at(2, 70, 0),
            BlockState::from(BlockKind::OakLeaves),
        );
        assert_eq!(world.surface_y(2, 0), Some(71));
        let column = perception
            .privileged_column(&world, 2, 0, head, COLUMN_BLOCKS as i32)
            .expect("the heightmap is there");
        assert_eq!(
            column.state_at(70),
            Some(BlockState::from(BlockKind::OakLeaves))
        );
    }

    #[test]
    fn a_surface_beyond_the_read_window_returns_no_states() {
        let mut world = surface_world();
        let perception = Perception::new(Config::default());
        set_block(&mut world, at(0, 120, 0), BlockState::from(BlockKind::Stone));
        assert_eq!(world.surface_y(0, 0), Some(121));

        // More than the window above the head: the surface is reported so the
        // caller can see the roof is out of it, but no state is read. The
        // window is clamped to `COLUMN_BLOCKS`, however much the caller asks
        // for.
        for max_blocks in [8, 56, COLUMN_BLOCKS as i32, 100] {
            let column = perception
                .privileged_column(&world, 0, 0, 65, max_blocks)
                .expect("the heightmap is there");
            assert_eq!(column.surface, 121);
            assert!(column.states().is_empty(), "max_blocks = {max_blocks}");
            assert_eq!(column.state_at(65), None);
        }

        // A surface exactly `COLUMN_BLOCKS` above the head is read in full.
        set_block(&mut world, at(3, 112, 0), BlockState::from(BlockKind::Stone));
        assert_eq!(world.surface_y(3, 0), Some(113));
        let column = perception
            .privileged_column(&world, 3, 0, 65, COLUMN_BLOCKS as i32)
            .expect("the heightmap is there");
        assert_eq!(column.states().len(), COLUMN_BLOCKS);
        assert_eq!(
            column.state_at(112),
            Some(BlockState::from(BlockKind::Stone)),
            "the roof at the window's edge is read"
        );
    }

    #[test]
    fn a_missing_heightmap_is_not_a_fact_about_the_column() {
        // A chunk without a `WorldSurface` heightmap answers `None`, even with
        // a roof in it: the caller must not read a `None` as a covered column.
        let mut world = base_world();
        set_block(&mut world, at(0, 70, 0), BlockState::from(BlockKind::Stone));
        assert_eq!(world.surface_y(0, 0), None);
        let perception = Perception::new(Config::default());
        assert!(
            perception
                .privileged_column(&world, 0, 0, 65, COLUMN_BLOCKS as i32)
                .is_none()
        );
    }

    #[test]
    fn a_privileged_read_batch_is_ledgered_once_per_tick_with_a_running_counter() {
        assert_eq!(QueryKind::SkyColumn.as_str(), "sky_column");
        assert_eq!(FactKind::Column.as_str(), "column");

        let world = base_world();
        let mut p = Perception::new(Config::default());
        p.begin_tick(&world);
        p.ledger_privileged(QueryKind::SkyColumn, FactKind::Column, 3);
        let entry = p
            .ledger()
            .get(QueryKind::SkyColumn, FactKind::Column)
            .expect("the privileged read is ledgered");
        assert_eq!(entry.count(), 3);
        assert_eq!(p.ledger().len(), 1, "one entry per (query, fact)");

        p.ledger_privileged(QueryKind::SkyColumn, FactKind::Column, 0);
        assert_eq!(p.ledger().total(), 3, "a zero count records nothing");

        p.begin_tick(&world);
        p.ledger_privileged(QueryKind::SkyColumn, FactKind::Column, 2);
        let entry = p
            .ledger()
            .get(QueryKind::SkyColumn, FactKind::Column)
            .expect("the entry persists across ticks");
        assert_eq!(entry.count(), 5, "later reads raise the same counter");
        assert_eq!(p.ledger().len(), 1);
    }
}
