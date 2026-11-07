//! The background sweep: spend the callers' leftover ray budget on the space
//! around the player and write every reached cell to the known map.

use praxion_world::{BlockPos, Vec3, World};

use crate::sight::CellReader;
use crate::{lower_bound_sq, section_key_of, Caller, Perception, Reason, SectionCells};

/// What one sweep spent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Sweep {
    /// Rays cast by the sweep, all of them from `Caller::Other`'s share.
    pub rays: u32,
    /// Cells whose visibility was tested.
    pub probes: usize,
}

/// Rays the background sweep may spend per tick. The configured ray budget is
/// an upper bound shared with queries; the sweep takes a slice so one tick
/// never pays the whole map fill.
const SWEEP_RAYS_PER_TICK: u32 = 900;

impl Perception {
    /// Spend the ray budget the callers left (never the reflex reserve) on the
    /// space around the player: nearest sections first, rotating inside each
    /// distance shell, and write every cell a ray reaches to the known map.
    ///
    /// Call this once per tick, after the callers have made their queries. A
    /// cell that is known and fresh is skipped; a cell whose chunk changed or
    /// whose sighting is stale is probed again. A cell no ray reaches is only
    /// remembered as a miss, never as a block fact.
    pub fn sweep(&mut self, world: &World) -> Sweep {
        let mut stats = Sweep::default();
        let Some(eye) = self.valid_eye(world) else {
            return stats;
        };
        let eye_cell = eye.to_block_pos_floor();
        let focus = section_key_of(eye_cell);
        let radius = self.config.sweep_radius.min(self.config.range);
        let rays_before = self.rays_other;
        let mut reader = CellReader::new();
        let shells = swept_sections(eye, radius, self.tick);

        'outer: for shell in shells {
            for key in shell {
                let base = BlockPos::new(key.0 * 16, key.1 * 16, key.2 * 16);
                let cells = 16 * 16 * 16;
                let start = (self.tick as usize).wrapping_add(section_seed(key)) % cells;
                for offset in 0..cells {
                    // Spend a bounded slice per tick. The sweep is background
                    // map filling, not an action: a smaller slice keeps a tick
                    // under the 25 ms budget while the same cells are covered
                    // over the next few ticks.
                    if self.remaining_rays(Caller::Other) == 0
                        || self.rays_other.saturating_sub(rays_before) >= SWEEP_RAYS_PER_TICK
                    {
                        break 'outer;
                    }
                    let index = (start + offset) % cells;
                    let pos = BlockPos::new(
                        base.x + (index & 15) as i32,
                        base.y + ((index >> 8) & 15) as i32,
                        base.z + ((index >> 4) & 15) as i32,
                    );
                    if eye.distance_to(pos.center()) > f64::from(radius) {
                        continue;
                    }
                    let Some(revision) = world.chunk_revision(praxion_world::ChunkPos::from(pos))
                    else {
                        continue;
                    };
                    if !self.known.needs_probe(pos, self.tick, revision) {
                        continue;
                    }
                    let Some(state) = world.block_state(pos) else {
                        continue;
                    };
                    stats.probes += 1;
                    match self.visible_block(world, Caller::Other, eye, pos, state, &mut reader) {
                        Ok(()) => {
                            self.known
                                .record_seen(pos, state, self.tick, revision, focus);
                        }
                        Err(Reason::OutOfSight) => {
                            self.known.record_miss(pos, self.tick, revision, focus);
                        }
                        Err(Reason::Budget) => break 'outer,
                        Err(_) => {}
                    }
                }
            }
        }
        stats.rays = self.rays_other.saturating_sub(rays_before);
        stats
    }
}

/// A per-section rotation seed, so two sections do not scan their cells in the
/// same order.
fn section_seed(key: (i32, i32, i32)) -> usize {
    let mut hash = 0x9E37_79B9_7F4A_7C15_u64;
    for word in [key.0, key.1, key.2] {
        hash = (hash ^ u64::from(word as u32)).wrapping_mul(0x1000_0000_01B3);
        hash ^= hash >> 29;
    }
    (hash >> 32) as usize
}

/// The sections the sweep walks, grouped into distance shells: nearest shell
/// first, and inside a shell a per-tick rotation so one direction cannot take
/// the whole budget every tick.
fn swept_sections(eye: Vec3, radius: u32, tick: u64) -> Vec<Vec<(i32, i32, i32)>> {
    let r = i32::try_from(radius).unwrap_or(i32::MAX);
    let eye_cell = eye.to_block_pos_floor();
    let first = (
        eye_cell.x.saturating_sub(r) >> 4,
        eye_cell.y.saturating_sub(r) >> 4,
        eye_cell.z.saturating_sub(r) >> 4,
    );
    let last = (
        eye_cell.x.saturating_add(r) >> 4,
        eye_cell.y.saturating_add(r) >> 4,
        eye_cell.z.saturating_add(r) >> 4,
    );
    let radius_sq = f64::from(radius) * f64::from(radius);
    let mut sections = Vec::new();
    for x in first.0..=last.0 {
        for y in first.1..=last.1 {
            for z in first.2..=last.2 {
                let base = BlockPos::new(
                    x.saturating_mul(16),
                    y.saturating_mul(16),
                    z.saturating_mul(16),
                );
                let cells = SectionCells {
                    base,
                    min: BlockPos::new(
                        base.x.max(eye_cell.x.saturating_sub(r)),
                        base.y.max(eye_cell.y.saturating_sub(r)),
                        base.z.max(eye_cell.z.saturating_sub(r)),
                    ),
                    max: BlockPos::new(
                        base.x.saturating_add(15).min(eye_cell.x.saturating_add(r)),
                        base.y.saturating_add(15).min(eye_cell.y.saturating_add(r)),
                        base.z.saturating_add(15).min(eye_cell.z.saturating_add(r)),
                    ),
                };
                let distance_sq = lower_bound_sq(eye, cells);
                if distance_sq <= radius_sq {
                    sections.push((distance_sq, (x, y, z)));
                }
            }
        }
    }
    sections.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut shells: Vec<Vec<(i32, i32, i32)>> = Vec::new();
    let mut current = Vec::new();
    let mut current_shell = -1_i32;
    for (distance_sq, key) in sections {
        let shell = distance_sq.sqrt().floor() as i32;
        if shell != current_shell {
            if !current.is_empty() {
                shells.push(std::mem::take(&mut current));
            }
            current_shell = shell;
        }
        current.push(key);
    }
    if !current.is_empty() {
        shells.push(current);
    }
    for shell in &mut shells {
        if !shell.is_empty() {
            let start = (tick as usize) % shell.len();
            shell.rotate_left(start);
        }
    }
    shells
}
