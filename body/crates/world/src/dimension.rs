//! Dimension identity and world height, resolved from configuration registry
//! data with the spike's vanilla fallback.

use azalea_core::registry_holder::RegistryHolder;
use azalea_protocol::packets::common::CommonPlayerSpawnInfo;

/// Where the height and minimum y came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionSource {
    Registry,
    Fallback,
}

/// The dimension of the followed connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dimension {
    dimension: String,
    dimension_type: String,
    height: u32,
    min_y: i32,
    source: DimensionSource,
}

impl Dimension {
    /// Registry name of the dimension, e.g. `minecraft:overworld`.
    pub fn dimension(&self) -> &str {
        &self.dimension
    }

    /// Registry name of the dimension type.
    pub fn dimension_type(&self) -> &str {
        &self.dimension_type
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    pub fn source(&self) -> DimensionSource {
        self.source
    }
}

/// Resolve the dimension from `login`/`respawn`. Vanilla sends the dimension
/// type in the configuration registry; known server packs can omit it, so the
/// three vanilla kinds fall back to their fixed heights (session 2 research).
pub(crate) fn resolve(common: &CommonPlayerSpawnInfo, registries: &RegistryHolder) -> Dimension {
    if let Some((identifier, data)) = common.dimension_type(registries) {
        return Dimension {
            dimension: common.dimension.to_string(),
            dimension_type: identifier.to_string(),
            height: data.height,
            min_y: data.min_y,
            source: DimensionSource::Registry,
        };
    }

    let path = common.dimension.path().to_owned();
    let (height, min_y) = if path.contains("nether") || path.contains("end") {
        (256, 0)
    } else {
        (384, -64)
    };
    Dimension {
        dimension: common.dimension.to_string(),
        dimension_type: format!(
            "dimension_type#{}",
            azalea_registry::DataRegistry::protocol_id(&common.dimension_type)
        ),
        height,
        min_y,
        source: DimensionSource::Fallback,
    }
}
