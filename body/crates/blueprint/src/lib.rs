//! Blueprint v1: a layered ASCII plan, its bounds and the rules a design must
//! pass before the body spends a tick placing it.
//!
//! Decision: `docs/decisions.md` 2026-11-09 "Five next areas" (building,
//! Demo-3). The design is `logs/mac1109/design/build.out.md` §3.2-3.3 with the
//! critique's corrections in `build.crit.out.md` findings 1, 3, 4, 5 and 6:
//! the palette names concrete block ids (no role is left for the body to
//! resolve), slice 0 is capped at 8x8x4, one count is canonical, a torch needs
//! a solid cell below it (a wall torch arrives as `minecraft:wall_torch`, so
//! the place receipt would never settle), and the plan can be emitted as JSON
//! so the Python referee never re-parses the format.
//!
//! The body re-runs [`validate`] on load, so the executor trusts the code that
//! accepted the design instead of the brain's word. Nothing here reads the
//! world: every rule is a pure function of the file (plus, for materials, the
//! inventory the caller hands in). The build primitive and the site rules are
//! later slices.
//!
//! # Format
//!
//! ```text
//! blueprint v1
//! size: <X> <Z> <Y>
//! axes: x west->east, z north->south, y up; layer y0 = the feet level
//! palette:
//!   <char>: <air|keep|block-id>[ entry]
//! layer y0:
//! <Z rows of exactly X characters>
//! ...
//! layer y<Y-1>:
//! <Z rows of exactly X characters>
//! ```
//!
//! * `//` starts a comment only as the first non-space text of a line; a
//!   character used in the layers is always read as a palette character, so
//!   `#` may be a wall.
//! * A palette entry is `air`, `keep`, a concrete block id from
//!   [`ALLOWED_BLOCKS`], or a block id followed by `entry`. `entry` is not a
//!   material role: it marks a shell cell the body seals **last**, from inside.
//!   `keep` marks a cell the blueprint does not touch; that is a site
//!   instruction, so a design that uses one is rejected until the slice with
//!   site rules (a warning for the *unused* declaration is enough here).
//! * Every layer `y0..y<Y-1>` must appear exactly once, in order, with `Z`
//!   rows of exactly `X` characters. A size that disagrees with the rows is a
//!   format error, never a padded guess.
//!
//! # Coordinates
//!
//! Layer `y0` is the layer the body's feet occupy; the floor is `y0 - 1` and
//! is not part of the blueprint. Cell `(i, k, j)` — column `i` west to east,
//! layer `k`, row `j` north to south — is the world block
//! `(x0 + i, y0 + k, z0 + j)`; [`Blueprint::placements`] needs the anchor.
//!
//! # Rules
//!
//! [`validate`] runs in this order and reports the first failure with its
//! cells (blueprint-local positions; the caller adds the anchor):
//!
//! 1. bounds: `size` within [`Limits`], placements within the budget —
//!    `too_big`; a used `keep` cell is `bad_format`, because leaving an
//!    existing block alone needs site rules this slice does not have;
//! 2. support: every solid cell reaches the floor plane through face-adjacent
//!    solid cells, sand has ground under it and a torch has a solid cell below
//!    it (a floor torch, so its id stays `minecraft:torch`) — `unsupported`,
//!    `torch_floating`;
//! 3. enclosure: a flood fill from outside the blueprint reaches no `air` cell,
//!    and every `entry` cell borders both the interior and the exterior —
//!    `not_enclosed`, `entry_inside`;
//! 4. light by geometry: an interior at least two cells high, at least one
//!    torch, and every interior floor cell within 6 blocks of a torch —
//!    `dark_cell`;
//! 5. materials: every block the plan places is in the inventory — `short_materials`;
//! 6. reach: every placement cell is within 4.5 of a standing cell's eye —
//!    `unreachable`.
//!
//! Rule 4 is a proxy for the game's light engine, not the acceptance: the
//! referee decides "closed and lit" from the server. It is deliberately
//! strict (radius 6 covers torch light 14 minus one per step, with ground
//! attenuation), and it is the only check available because perception's
//! `light_level` is always `Unknown`.
//!
//! Rules 2, 3 and 6 together are what makes a support-first build order
//! meaningful rather than a hope. Rule 6 is conservative in the same way: the
//! runtime check is `place`'s own support and body-overlap search, and a cell
//! that fails there is a per-cell retry, not a whole-call failure.

mod parse;
#[cfg(test)]
mod tests;
mod validate;

use std::collections::{BTreeMap, BTreeSet};

use azalea_core::position::BlockPos;

pub use parse::{AXES_LINE, PARSER_MAX, parse};
pub use validate::{Limits, validate};

/// The block ids blueprint v1 may name.
///
/// This is a bridge, not a rule of the file format. The generated game data
/// carries no collision shape and `place`'s body-overlap check assumes a full
/// cube, so v1 admits only ids whose shape is known to be a full cube, plus
/// the two deliberate non-cube exceptions: `torch` (the light a shelter needs)
/// and `glass_pane` (a window). A block outside this list is `unknown_block`
/// even when the registry knows it, because the alternative is a design that
/// parses and then fails at the click.
pub const ALLOWED_BLOCKS: &[&str] = &[
    "birch_log",
    "birch_planks",
    "cobblestone",
    "deepslate",
    "dirt",
    "glass",
    "glass_pane",
    "grass_block",
    "oak_log",
    "oak_planks",
    "sand",
    "sandstone",
    "smooth_stone",
    "spruce_log",
    "spruce_planks",
    "stone",
    "torch",
];

/// The only v1 block that falls when the cell below it is not solid.
///
/// The draft's rule names sand, gravel and soul sand; sand is the only one v1
/// allows, so the list follows the allowlist. [`GRAVITY_BLOCKS`] is checked
/// against [`ALLOWED_BLOCKS`] by a test: a gravity block outside the allowlist
/// could never be used, and one missing from here would be validated as a
/// full cube that stays put.
pub const GRAVITY_BLOCKS: &[&str] = &["sand"];

/// Cell counts per axis: `x` columns, `z` rows, `y` layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub x: usize,
    pub z: usize,
    pub y: usize,
}

/// What one palette entry means for a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteEntry {
    /// A cell that must be free in the finished structure.
    Air,
    /// A cell the blueprint does not touch. A site instruction the validator
    /// rejects until there are site rules; see [`validate`].
    Keep,
    /// A cell the body places with the named block id.
    Solid(String),
    /// A shell cell the body seals last, from inside, with the named block id.
    Entry(String),
}

impl PaletteEntry {
    /// The kind of cell this entry produces. `torch` is the one solid id that
    /// is not a cube; every other non-cube id is rejected at parse time.
    pub fn kind(&self) -> CellKind {
        match self {
            Self::Air => CellKind::Air,
            Self::Keep => CellKind::Keep,
            Self::Entry(_) => CellKind::Entry,
            Self::Solid(id) if id == "torch" => CellKind::Torch,
            Self::Solid(_) => CellKind::Solid,
        }
    }

    /// The block id the body places, if the cell is placed at all.
    pub fn block(&self) -> Option<&str> {
        match self {
            Self::Solid(id) | Self::Entry(id) => Some(id),
            Self::Air | Self::Keep => None,
        }
    }
}

/// What a cell asks of the build. `Solid` and `Entry` are both full-cube
/// placements — they seal the room the same way — and differ only in when the
/// body places them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// Must be free: part of the interior, or a hole the design leaves.
    Air,
    /// Not placed; rejected by [`validate`] because "do not touch" needs site
    /// rules, which this slice does not have.
    Keep,
    /// A normal shell or interior block.
    Solid,
    /// A shell cell sealed last, from inside.
    Entry,
    /// A torch, which needs a solid cell below it.
    Torch,
}

impl CellKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Air => "air",
            Self::Keep => "keep",
            Self::Solid => "solid",
            Self::Entry => "entry",
            Self::Torch => "torch",
        }
    }
}

/// One resolved blueprint cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell<'a> {
    pub kind: CellKind,
    pub block: Option<&'a str>,
}

/// One cell the body has to place, at a world position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Blueprint-local cell: column `i`, layer `k`, row `j`.
    pub cell: (usize, usize, usize),
    /// `(x0 + i, y0 + k, z0 + j)`.
    pub pos: BlockPos,
    pub block: String,
    pub kind: CellKind,
}

/// A parsed blueprint. Every palette entry is a concrete block id, so there is
/// nothing left for the body to resolve against a persona or an inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blueprint {
    size: Size,
    palette: BTreeMap<char, PaletteEntry>,
    /// `layers[k][j][i]`: layer, row, column.
    layers: Vec<Vec<Vec<char>>>,
}

impl Blueprint {
    pub(crate) fn new(
        size: Size,
        palette: BTreeMap<char, PaletteEntry>,
        layers: Vec<Vec<Vec<char>>>,
    ) -> Self {
        Self {
            size,
            palette,
            layers,
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn palette(&self) -> &BTreeMap<char, PaletteEntry> {
        &self.palette
    }

    /// The cell at `(i, k, j)`; panics outside the size. Parsing keeps every
    /// layer character in the palette, so the lookup cannot fail.
    pub fn cell(&self, i: usize, k: usize, j: usize) -> Cell<'_> {
        let entry = &self.palette[&self.layers[k][j][i]];
        Cell {
            kind: entry.kind(),
            block: entry.block(),
        }
    }

    /// The kind at an arbitrary position: `None` outside the blueprint, which
    /// every rule treats as air the blueprint does not own.
    pub(crate) fn kind(&self, i: i32, k: i32, j: i32) -> Option<CellKind> {
        let (i, k, j) = (
            usize::try_from(i).ok()?,
            usize::try_from(k).ok()?,
            usize::try_from(j).ok()?,
        );
        if i >= self.size.x || k >= self.size.y || j >= self.size.z {
            return None;
        }
        Some(self.cell(i, k, j).kind)
    }

    /// Whether the cell is a full-cube placement: a wall, an entry cell or a
    /// pane. A torch is not solid: it is a non-cube placement with its own
    /// support rule.
    pub(crate) fn solid(&self, i: i32, k: i32, j: i32) -> bool {
        matches!(
            self.kind(i, k, j),
            Some(CellKind::Solid | CellKind::Entry)
        )
    }

    /// Every cell the body places, in layer order (then row, then column).
    /// Layer order is not the build order; the executor orders support-first.
    pub fn placements(&self, anchor: BlockPos) -> Vec<Placement> {
        let mut out = Vec::new();
        for k in 0..self.size.y {
            for j in 0..self.size.z {
                for i in 0..self.size.x {
                    let cell = self.cell(i, k, j);
                    let Some(block) = cell.block else { continue };
                    out.push(Placement {
                        cell: (i, k, j),
                        pos: BlockPos::new(
                            anchor.x + i as i32,
                            anchor.y + k as i32,
                            anchor.z + j as i32,
                        ),
                        block: block.to_owned(),
                        kind: cell.kind,
                    });
                }
            }
        }
        out
    }

    /// How many of each block the plan places, keyed by short block id.
    pub fn materials(&self) -> BTreeMap<String, u32> {
        let mut out = BTreeMap::new();
        for k in 0..self.size.y {
            for j in 0..self.size.z {
                for i in 0..self.size.x {
                    if let Some(block) = self.cell(i, k, j).block {
                        *out.entry(block.to_owned()).or_insert(0) += 1;
                    }
                }
            }
        }
        out
    }

    /// Non-fatal findings the design should know about.
    pub fn warnings(&self) -> Vec<Warning> {
        let used: BTreeSet<char> = self
            .layers
            .iter()
            .flatten()
            .flatten()
            .copied()
            .collect();
        self.palette
            .keys()
            .filter(|ch| !used.contains(ch))
            .map(|ch| Warning {
                kind: "unused_palette_entry",
                message: format!("palette entry `{ch}` is declared but never used"),
            })
            .collect()
    }
}

/// A non-fatal finding; the repair loop may ignore it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub kind: &'static str,
    pub message: String,
}

/// Why a blueprint was rejected. The kind is the repair loop's key; the
/// message carries the numbers and the cells are blueprint-local positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    BadFormat,
    UnknownBlock,
    TooBig,
    Unsupported,
    NotEnclosed,
    EntryInside,
    DarkCell,
    TorchFloating,
    ShortMaterials,
    Unreachable,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadFormat => "bad_format",
            Self::UnknownBlock => "unknown_block",
            Self::TooBig => "too_big",
            Self::Unsupported => "unsupported",
            Self::NotEnclosed => "not_enclosed",
            Self::EntryInside => "entry_inside",
            Self::DarkCell => "dark_cell",
            Self::TorchFloating => "torch_floating",
            Self::ShortMaterials => "short_materials",
            Self::Unreachable => "unreachable",
        }
    }
}

/// One typed rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesignError {
    pub kind: ErrorKind,
    pub message: String,
    /// Blueprint-local `(i, k, j)` positions; the caller adds the anchor.
    pub cells: Vec<BlockPos>,
    /// The numbers behind a [`ErrorKind::ShortMaterials`] rejection. The brain
    /// turns them into a gather goal (design §3.4, "Material gathering"), so
    /// they must not have to be parsed back out of `message`.
    pub shortfall: Option<Shortfall>,
}

/// What the inventory is missing for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortfall {
    /// Short block id, as the palette stores it.
    pub block: String,
    pub need: u32,
    pub have: u32,
}

impl DesignError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            cells: Vec::new(),
            shortfall: None,
        }
    }

    pub fn at(kind: ErrorKind, message: impl Into<String>, cells: Vec<BlockPos>) -> Self {
        Self {
            kind,
            message: message.into(),
            cells,
            shortfall: None,
        }
    }

    pub fn with_shortfall(mut self, shortfall: Shortfall) -> Self {
        self.shortfall = Some(shortfall);
        self
    }
}
