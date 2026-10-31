//! Minimal world model for an offline capture.
//!
//! Azalea's `Chunk` does the parsing (paletted containers, heightmaps, block
//! states); this module only keeps the chunks and applies the four packets a
//! client would apply to its world:
//!
//! - `level_chunk_with_light` -> `Chunk::read_with_dimension_height`
//!   (the core of `PartialChunkStorage::replace_with_packet_data`),
//! - `forget_level_chunk`      -> drop the chunk,
//! - `block_update`            -> set one block (what `azalea-client` queues and
//!   then writes through `ChunkStorage::set_block_state`),
//! - `section_blocks_update`   -> set many blocks in one section.
//!
//! Difference from `azalea-client`: a real client keeps chunks in a
//! `PartialChunkStorage` limited to its render distance and the shared
//! `ChunkStorage` only holds weak references. The decoder is a passive observer,
//! not a renderer, so it keeps every chunk the server sent. That keeps
//! `--block` answerable for the whole capture.

use std::collections::HashMap;
use std::io::Cursor;

use azalea_block::{BlockState, BlockTrait};
use azalea_core::{
    heightmap_kind::HeightmapKind,
    position::{BlockPos, ChunkBlockPos, ChunkPos},
};
use azalea_world::Chunk;

#[derive(Debug)]
pub struct WorldModel {
    pub height: u32,
    pub min_y: i32,
    chunks: HashMap<ChunkPos, Chunk>,
}

impl WorldModel {
    pub fn new(height: u32, min_y: i32) -> Self {
        Self {
            height,
            min_y,
            chunks: HashMap::new(),
        }
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn has_chunk(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// Apply `level_chunk_with_light` chunk data.
    pub fn read_chunk(
        &mut self,
        pos: ChunkPos,
        buffer: &[u8],
        heightmaps: &[(HeightmapKind, Box<[u64]>)],
    ) -> Result<(), String> {
        let chunk = Chunk::read_with_dimension_height(
            &mut Cursor::new(buffer),
            self.height,
            self.min_y,
            heightmaps,
        )
        .map_err(|e| format!("chunk {pos:?}: {e}"))?;
        self.chunks.insert(pos, chunk);
        Ok(())
    }

    pub fn forget_chunk(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
    }

    /// Set one block. Returns false when the chunk is not loaded.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState) -> bool {
        let Some(chunk) = self.chunks.get_mut(&ChunkPos::from(pos)) else {
            return false;
        };
        chunk.set_block_state(&ChunkBlockPos::from(pos), state, self.min_y);
        true
    }

    pub fn block_state(&self, pos: BlockPos) -> Option<BlockState> {
        if pos.y < self.min_y || pos.y >= self.min_y + self.height as i32 {
            return None;
        }
        let chunk = self.chunks.get(&ChunkPos::from(pos))?;
        chunk.get_block_state(&ChunkBlockPos::from(pos), self.min_y)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDescription {
    pub name: String,
    pub properties: Vec<(String, String)>,
}

impl BlockDescription {
    /// `minecraft:stone`, plus properties sorted by name.
    pub fn of(state: BlockState) -> Self {
        let block = Box::<dyn BlockTrait>::from(state);
        let mut properties: Vec<(String, String)> = block
            .property_map()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        properties.sort();
        Self {
            name: format!("minecraft:{}", block.id()),
            properties,
        }
    }

    pub fn display(&self) -> String {
        if self.properties.is_empty() {
            return self.name.clone();
        }
        let props: Vec<String> = self
            .properties
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        format!("{} [{}]", self.name, props.join(","))
    }
}
