//! Human-readable block states, used by the CLI and by the block-truth test.

use azalea_block::{BlockState, BlockTrait};

/// A block state as `minecraft:oak_stairs[facing=east,half=top]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockDescription {
    name: String,
    properties: Vec<(String, String)>,
}

impl BlockDescription {
    /// Name and properties of `state`, properties sorted by name so the
    /// description does not depend on hash-map order.
    pub fn of(state: BlockState) -> Self {
        let block = Box::<dyn BlockTrait>::from(state);
        let mut properties: Vec<(String, String)> = block
            .property_map()
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        properties.sort();
        Self { name: format!("minecraft:{}", block.id()), properties }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn properties(&self) -> &[(String, String)] {
        &self.properties
    }

    pub fn display(&self) -> String {
        if self.properties.is_empty() {
            return self.name.clone();
        }
        let properties: Vec<String> = self
            .properties
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        format!("{} [{}]", self.name, properties.join(","))
    }
}
