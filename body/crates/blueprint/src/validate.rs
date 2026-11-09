//! The intrinsic rules: everything a design must satisfy before the body
//! reads the world. Pure functions of the parsed blueprint (plus the inventory
//! for the material check), so a bad design costs no ticks.

use std::collections::{BTreeMap, VecDeque};

use azalea_core::position::BlockPos;

use crate::{
    Blueprint, CellKind, DesignError, ErrorKind, GRAVITY_BLOCKS, Shortfall, Size, Warning,
};

/// How large a blueprint may be and how many cells it may ask the body to
/// place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_size: Size,
    pub max_placements: usize,
}

impl Limits {
    /// The first build slice's bound.
    ///
    /// 8x8x4 is the size bound the critique asks for (finding 3): the largest
    /// plain box it admits is a 28-cell ring per layer (84) plus a 64-cell
    /// roof, 148 cells, which fits the 150-placement budget the design gives
    /// for `values.speed = 150`, so the budget does not bind before the
    /// geometry. A 9x9 box would be 177 cells and could never pass.
    pub const SLICE0: Limits = Limits {
        max_size: Size { x: 8, z: 8, y: 4 },
        max_placements: 150,
    };
}

/// A shelter interior must be at least this many cells high, or the body
/// cannot stand in it.
const MIN_INTERIOR_HEIGHT: usize = 2;

/// Rule 4's torch radius. Deliberately strict: torch light is 14 and loses one
/// per step, with extra attenuation next to a floor, so six blocks of
/// Euclidean distance never overstates what the server will light. The real
/// acceptance is the referee's structural check; this is the offline proxy,
/// because perception's `light_level` is always `Unknown`.
const TORCH_REACH: f64 = 6.0;

/// Vanilla block interaction range, measured from the eye: `place` uses the
/// same eye-to-cell-centre distance (`primitives/place.rs`).
const REACH: f64 = 4.5;

/// The player's eye height above the feet (`headless::physics` uses the
/// dimension's eye height; 1.62 for a player).
const EYE_HEIGHT: f64 = 1.62;

/// Cells that are face-adjacent to a cell.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (-1, 0, 0),
    (1, 0, 0),
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
];

/// `[layer][row][column]`, only for cells inside the blueprint.
type Grid = Vec<Vec<Vec<bool>>>;

/// Run every intrinsic rule, in the documented order, and return the warnings
/// when the design passes. `inventory` is the short-id to count map the
/// caller measured; without it the material rule is skipped and only the
/// materials list is reported by the caller.
pub fn validate(
    bp: &Blueprint,
    limits: &Limits,
    inventory: Option<&BTreeMap<String, u32>>,
) -> Result<Vec<Warning>, DesignError> {
    check_bounds(bp, limits)?;
    check_keep(bp)?;
    check_support(bp)?;
    let interior = check_enclosure(bp)?;
    check_light(bp, &interior)?;
    if let Some(inventory) = inventory {
        check_materials(bp, inventory)?;
    }
    check_reach(bp)?;
    Ok(bp.warnings())
}

fn check_bounds(bp: &Blueprint, limits: &Limits) -> Result<(), DesignError> {
    let size = bp.size();
    if size.x > limits.max_size.x || size.z > limits.max_size.z || size.y > limits.max_size.y {
        return Err(DesignError::new(
            ErrorKind::TooBig,
            format!(
                "size {}x{}x{} exceeds the build bound {}x{}x{}",
                size.x, size.z, size.y, limits.max_size.x, limits.max_size.z, limits.max_size.y
            ),
        ));
    }
    let placements: u32 = bp.materials().values().sum();
    if placements as usize > limits.max_placements {
        return Err(DesignError::new(
            ErrorKind::TooBig,
            format!(
                "{placements} placements exceed the budget of {}",
                limits.max_placements
            ),
        ));
    }
    Ok(())
}

/// Rule 1's `keep` clause: "leave this cell as it is" is a statement about the
/// site, and slice 0 has no site rules, so the checker cannot verify it. A
/// design that asks for one is rejected rather than quietly treated as air (a
/// hole) or as solid (a block nobody places).
fn check_keep(bp: &Blueprint) -> Result<(), DesignError> {
    let size = bp.size();
    let mut cells = Vec::new();
    for k in 0..size.y {
        for j in 0..size.z {
            for i in 0..size.x {
                if bp.cell(i, k, j).kind == CellKind::Keep {
                    cells.push(BlockPos::new(i as i32, k as i32, j as i32));
                }
            }
        }
    }
    if !cells.is_empty() {
        return Err(DesignError::at(
            ErrorKind::BadFormat,
            "`keep` needs site rules, which this slice does not have: build the \
             cell or leave it out",
            cells,
        ));
    }
    Ok(())
}

/// Rule 2: a solid cell must reach the floor plane through face-adjacent solid
/// cells. The plane `y0 - 1` is the world's floor and is not in the blueprint,
/// so every solid cell at layer 0 starts reached. A torch is checked here too,
/// for the same reason it is not solid: a wall or ceiling torch is a different
/// block id and its placement receipt would never settle.
///
/// A glass pane needs no rule of its own: the walk reaches a pane only through
/// a face-adjacent solid cell, which is the face it attaches to, and a layer-0
/// pane stands on the floor plane. A pane with no solid face is therefore
/// already one of the unreached cells below.
// The three counters are the grid's three axes; an iterator chain would hide
// which cell each index names, and the rule reads better in coordinates.
#[allow(clippy::needless_range_loop)]
fn check_support(bp: &Blueprint) -> Result<(), DesignError> {
    let size = bp.size();
    let mut reached: Grid = vec![vec![vec![false; size.x]; size.z]; size.y];
    let mut queue = VecDeque::new();
    for j in 0..size.z {
        for i in 0..size.x {
            if bp.solid(i as i32, 0, j as i32) {
                reached[0][j][i] = true;
                queue.push_back((i, 0, j));
            }
        }
    }
    while let Some((i, k, j)) = queue.pop_front() {
        for (di, dk, dj) in NEIGHBOURS {
            let (ni, nk, nj) = (i as i32 + di, k as i32 + dk, j as i32 + dj);
            if !bp.solid(ni, nk, nj) {
                continue;
            }
            let (ni, nk, nj) = (ni as usize, nk as usize, nj as usize);
            if !reached[nk][nj][ni] {
                reached[nk][nj][ni] = true;
                queue.push_back((ni, nk, nj));
            }
        }
    }

    let mut floating = Vec::new();
    let mut falling = Vec::new();
    let mut floating_torches = Vec::new();
    for k in 0..size.y {
        for j in 0..size.z {
            for i in 0..size.x {
                let cell = bp.cell(i, k, j);
                let pos = BlockPos::new(i as i32, k as i32, j as i32);
                // Layer 0 stands on the floor plane, which is not part of the
                // blueprint but is solid by construction.
                let below = k == 0 || bp.solid(i as i32, k as i32 - 1, j as i32);
                match cell.kind {
                    CellKind::Solid | CellKind::Entry => {
                        if !reached[k][j][i] {
                            floating.push(pos);
                        }
                        if cell
                            .block
                            .is_some_and(|block| GRAVITY_BLOCKS.contains(&block))
                            && !below
                        {
                            falling.push(pos);
                        }
                    }
                    // A torch is the one placement whose block id changes with
                    // its support: on a solid cell below it stays
                    // `minecraft:torch`, on a wall or ceiling it becomes
                    // `minecraft:wall_torch` and the place receipt (which wants
                    // exactly the named id) would never settle. Layer 0 stands
                    // on the world's floor plane, so it counts as solid.
                    CellKind::Torch if !below => floating_torches.push(pos),
                    _ => {}
                }
            }
        }
    }
    if !floating.is_empty() {
        return Err(DesignError::at(
            ErrorKind::Unsupported,
            "these solid cells never reach the floor plane through face-adjacent \
             solid cells, so nothing can support their placement",
            floating,
        ));
    }
    if !falling.is_empty() {
        return Err(DesignError::at(
            ErrorKind::Unsupported,
            "sand would fall: the cell below it is not solid",
            falling,
        ));
    }
    if !floating_torches.is_empty() {
        return Err(DesignError::at(
            ErrorKind::TorchFloating,
            "a torch needs a solid cell below it: placed on a wall or ceiling it \
             arrives as minecraft:wall_torch and the place receipt never settles",
            floating_torches,
        ));
    }
    Ok(())
}

/// Rule 3: flood the outside in, then check that the interior stayed dry (no
/// air cell is reachable) and that each entry cell faces both sides.
// The three counters are the grid's three axes; see `check_support`.
#[allow(clippy::needless_range_loop)]
fn check_enclosure(bp: &Blueprint) -> Result<Grid, DesignError> {
    let size = bp.size();
    let outside = flood_outside(bp);
    let mut interior: Grid = vec![vec![vec![false; size.x]; size.z]; size.y];
    let mut leaked = Vec::new();
    let mut entries = Vec::new();
    for k in 0..size.y {
        for j in 0..size.z {
            for i in 0..size.x {
                match bp.cell(i, k, j).kind {
                    CellKind::Air => {
                        let pos = BlockPos::new(i as i32, k as i32, j as i32);
                        if outside.get(i as i32, k as i32, j as i32) {
                            leaked.push(pos);
                        } else {
                            interior[k][j][i] = true;
                        }
                    }
                    CellKind::Entry => entries.push((i, k, j)),
                    _ => {}
                }
            }
        }
    }
    if !leaked.is_empty() {
        return Err(DesignError::at(
            ErrorKind::NotEnclosed,
            "the room leaks: these air cells are reachable from outside the blueprint",
            leaked,
        ));
    }
    if entries.is_empty() {
        return Err(DesignError::new(
            ErrorKind::EntryInside,
            "a shelter needs at least one `entry` cell: the body seals it last, from inside",
        ));
    }
    let mut misplaced = Vec::new();
    for (i, k, j) in entries {
        let (i, k, j) = (i as i32, k as i32, j as i32);
        let inside = neighbours(i, k, j).any(|(ni, nk, nj)| inside(&interior, size, ni, nk, nj));
        let outward = neighbours(i, k, j).any(|(ni, nk, nj)| outside.get(ni, nk, nj));
        if !(inside && outward) {
            misplaced.push(BlockPos::new(i, k, j));
        }
    }
    if !misplaced.is_empty() {
        return Err(DesignError::at(
            ErrorKind::EntryInside,
            "these entry cells do not border both the interior and the exterior",
            misplaced,
        ));
    }
    Ok(interior)
}

/// Rule 4: the geometric light check and the two shelter requirements that
/// make it meaningful (an interior, at least one torch).
///
/// A torch at the feet layer is allowed and is the normal case: it stands on
/// the world's floor, which is not part of the blueprint, and the cell it
/// occupies is simply not an interior cell. The old design's "raise it to head
/// height on a wall" is what the critique rejects (finding 1), because that
/// torch is a wall torch and cannot be placed with a plain `torch` receipt.
// The three counters are the grid's three axes; see `check_support`.
#[allow(clippy::needless_range_loop)]
fn check_light(bp: &Blueprint, interior: &Grid) -> Result<(), DesignError> {
    let size = bp.size();
    let mut torches = Vec::new();
    for k in 0..size.y {
        for j in 0..size.z {
            for i in 0..size.x {
                if bp.cell(i, k, j).kind == CellKind::Torch {
                    torches.push((i, k, j));
                }
            }
        }
    }
    if torches.is_empty() {
        return Err(DesignError::new(
            ErrorKind::DarkCell,
            "a shelter needs at least one torch",
        ));
    }

    let mut floor = Vec::new();
    let mut low = Vec::new();
    for j in 0..size.z {
        for i in 0..size.x {
            if !interior[0][j][i] {
                continue;
            }
            floor.push((i, j));
            let mut height = 0;
            while height < size.y && interior[height][j][i] {
                height += 1;
            }
            if height < MIN_INTERIOR_HEIGHT {
                low.push(BlockPos::new(i as i32, 0, j as i32));
            }
        }
    }
    if floor.is_empty() {
        return Err(DesignError::new(
            ErrorKind::DarkCell,
            "a shelter needs an interior at layer y0: no cell at the feet layer is free",
        ));
    }
    if !low.is_empty() {
        return Err(DesignError::at(
            ErrorKind::DarkCell,
            format!(
                "the interior is lower than {MIN_INTERIOR_HEIGHT} cells at these \
                 cells, so the body cannot stand there"
            ),
            low,
        ));
    }

    let mut dark = Vec::new();
    for (i, j) in floor {
        let covered = torches
            .iter()
            .any(|&(ti, tk, tj)| {
                distance(centre(i, 0, j), centre(ti, tk, tj)) <= TORCH_REACH
            });
        if !covered {
            dark.push(BlockPos::new(i as i32, 0, j as i32));
        }
    }
    if !dark.is_empty() {
        return Err(DesignError::at(
            ErrorKind::DarkCell,
            format!("these interior floor cells are more than {TORCH_REACH} blocks from a torch"),
            dark,
        ));
    }
    Ok(())
}

/// Rule 5: every block the plan places must be in the inventory. Keys are
/// short block ids (`cobblestone`), the same form the palette stores.
fn check_materials(
    bp: &Blueprint,
    inventory: &BTreeMap<String, u32>,
) -> Result<(), DesignError> {
    for (block, need) in bp.materials() {
        let have = inventory.get(&block).copied().unwrap_or(0);
        if have < need {
            let cells = bp
                .placements(BlockPos::ZERO)
                .into_iter()
                .filter(|placement| placement.block == block)
                .map(|placement| placement.pos)
                .collect();
            return Err(DesignError::at(
                ErrorKind::ShortMaterials,
                format!("{block}: need {need}, have {have}"),
                cells,
            )
            .with_shortfall(Shortfall {
                block,
                need,
                have,
            }));
        }
    }
    Ok(())
}

/// Rule 6: every placement cell must have a standing cell whose eye is within
/// `REACH` of its centre. A standing cell is free, has a solid cell below it
/// and a free cell above it, and is not a torch cell (the design's stand-cell
/// chooser skips torch cells; a torch has no collision box, so excluding them
/// is conservative, not a physical limit). Slice 0 builds without a scaffold,
/// so this is also the check that the design fits the floor-up reach the 8x8x4
/// bound promises.
///
/// Cells in the one-cell margin outside the footprint count as free, which is
/// the prepared flat pad slice 0 builds on (design §3.1); the site rules of a
/// later slice are what replace that assumption.
fn check_reach(bp: &Blueprint) -> Result<(), DesignError> {
    let size = bp.size();
    let mut standing = Vec::new();
    for k in 0..size.y {
        for j in -1..=size.z as i32 {
            for i in -1..=size.x as i32 {
                if matches!(
                    bp.kind(i, k as i32, j),
                    Some(CellKind::Solid | CellKind::Entry | CellKind::Torch)
                ) {
                    continue;
                }
                let below = k == 0 || bp.solid(i, k as i32 - 1, j);
                if !below {
                    continue;
                }
                if k + 1 < size.y && bp.solid(i, k as i32 + 1, j) {
                    continue;
                }
                standing.push((i, k as i32, j));
            }
        }
    }

    let mut unreachable = Vec::new();
    for placement in bp.placements(BlockPos::ZERO) {
        let target = centre(placement.cell.0, placement.cell.1, placement.cell.2);
        let reachable = standing.iter().any(|&(i, k, j)| {
            let eye = (i as f64 + 0.5, k as f64 + EYE_HEIGHT, j as f64 + 0.5);
            distance(eye, target) <= REACH
        });
        if !reachable {
            unreachable.push(placement.pos);
        }
    }
    if !unreachable.is_empty() {
        return Err(DesignError::at(
            ErrorKind::Unreachable,
            format!(
                "no standing cell has an eye within {REACH} of these placement \
                 cells; slice 0 builds without a scaffold"
            ),
            unreachable,
        ));
    }
    Ok(())
}

/// The outside of the blueprint: cells in a one-cell margin around the
/// footprint, from layer 0 up to one layer above the roof, that are reachable
/// from the margin without crossing a solid cell.
struct Outside {
    x: i32,
    z: i32,
    y: i32,
    /// `[k][j + 1][i + 1]`, `k` in `0..=y`.
    cells: Vec<Vec<Vec<bool>>>,
}

impl Outside {
    fn new(size: Size) -> Self {
        Self {
            x: size.x as i32,
            z: size.z as i32,
            y: size.y as i32,
            cells: vec![vec![vec![false; size.x + 2]; size.z + 2]; size.y + 1],
        }
    }

    fn in_box(&self, i: i32, k: i32, j: i32) -> bool {
        i >= -1 && i <= self.x && k >= 0 && k <= self.y && j >= -1 && j <= self.z
    }

    fn get(&self, i: i32, k: i32, j: i32) -> bool {
        if !self.in_box(i, k, j) {
            return false;
        }
        self.cells[k as usize][(j + 1) as usize][(i + 1) as usize]
    }

    fn set(&mut self, i: i32, k: i32, j: i32) {
        self.cells[k as usize][(j + 1) as usize][(i + 1) as usize] = true;
    }
}

fn flood_outside(bp: &Blueprint) -> Outside {
    let mut outside = Outside::new(bp.size());
    let mut queue = VecDeque::new();
    for k in 0..=outside.y {
        for j in -1..=outside.z {
            for i in -1..=outside.x {
                let boundary =
                    i == -1 || i == outside.x || j == -1 || j == outside.z || k == outside.y;
                if !boundary || bp.solid(i, k, j) {
                    continue;
                }
                outside.set(i, k, j);
                queue.push_back((i, k, j));
            }
        }
    }
    while let Some((i, k, j)) = queue.pop_front() {
        for (ni, nk, nj) in neighbours(i, k, j) {
            if !outside.in_box(ni, nk, nj)
                || outside.get(ni, nk, nj)
                || bp.solid(ni, nk, nj)
            {
                continue;
            }
            outside.set(ni, nk, nj);
            queue.push_back((ni, nk, nj));
        }
    }
    outside
}

fn inside(grid: &Grid, size: Size, i: i32, k: i32, j: i32) -> bool {
    let (Ok(i), Ok(k), Ok(j)) = (
        usize::try_from(i),
        usize::try_from(k),
        usize::try_from(j),
    ) else {
        return false;
    };
    if i >= size.x || k >= size.y || j >= size.z {
        return false;
    }
    grid[k][j][i]
}

fn neighbours(i: i32, k: i32, j: i32) -> impl Iterator<Item = (i32, i32, i32)> {
    NEIGHBOURS
        .into_iter()
        .map(move |(di, dk, dj)| (i + di, k + dk, j + dj))
}

fn centre(i: usize, k: usize, j: usize) -> (f64, f64, f64) {
    (i as f64 + 0.5, k as f64 + 0.5, j as f64 + 0.5)
}

fn distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (dx, dy, dz) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (dx * dx + dy * dy + dz * dz).sqrt()
}
