//! The search direction `explore` keeps across calls.
//!
//! A person who looks for trees in open country picks a direction and keeps
//! walking until the ground stops them, then turns. The old ranking forgot its
//! direction on every call (retries rotated the candidate list, the outward
//! phase restarted after eight goals), so six 1200-tick calls on seed 606 never
//! got more than 90 blocks from the spawn.
//!
//! The compass is split into [`SECTORS`] 45-degree sectors. The search:
//! 1. picks the sector with the longest unseen boundary (the number of its
//!    best-tier frontier cells; see `FrontierScan::sector_weights`), skipping
//!    sectors it already gave up on;
//! 2. ranks frontiers by how far forward along that heading they lie;
//! 3. after each frontier arrival measures the gain along the heading since the
//!    previous arrival; when the last [`MAX_STALLS`] arrivals gained less than
//!    [`MIN_GAIN`] blocks together (water, a cliff, a dead end), the heading is
//!    given up and marked avoided, so the next pick turns to another sector and
//!    never swings straight back. When every sector has been given up the
//!    memory restarts; when only given-up sectors have ground, the one given up
//!    longest ago is reopened, so a dead end is never re-entered straight away.

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_4};

use azalea_core::position::BlockPos;

/// Compass sectors; sector `k` points at `k * 45` degrees from +x towards +z.
pub(crate) const SECTORS: usize = 8;

/// A unit direction `(x, z)`.
pub(crate) type Dir = (f64, f64);

const UNIT: [Dir; SECTORS] = [
    (1.0, 0.0),
    (FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (0.0, 1.0),
    (-FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (-1.0, 0.0),
    (-FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
    (0.0, -1.0),
    (FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
];

/// Blocks of new progress over the last [`MAX_STALLS`] arrivals that count as
/// "still moving".
const MIN_GAIN: f64 = 4.0;

/// Arrivals the stall rule looks back over: the heading is given up when its
/// last [`MAX_STALLS`] arrivals gained less than [`MIN_GAIN`] blocks together.
const MAX_STALLS: usize = 3;

/// The sector a horizontal offset points into.
pub(crate) fn sector_of(dx: i64, dz: i64) -> usize {
    let angle = (dz as f64).atan2(dx as f64);
    ((angle / FRAC_PI_4).round() as i32).rem_euclid(SECTORS as i32) as usize
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Heading {
    sector: usize,
    /// Where the heading was chosen; progress is measured from here.
    anchor: (i32, i32),
    /// Progress along the heading at the previous arrival, in blocks.
    last: f64,
    /// The gains of the last [`MAX_STALLS`] arrivals, in blocks.
    recent: [f64; MAX_STALLS],
    /// Arrivals measured so far; the window counts only once it is full.
    arrivals: usize,
}

impl Heading {
    fn progress(&self, pos: BlockPos) -> f64 {
        let (ux, uz) = UNIT[self.sector];
        f64::from(pos.x - self.anchor.0) * ux + f64::from(pos.z - self.anchor.1) * uz
    }
}

/// One run's search direction. Lives in `ExploreMemory`.
#[derive(Debug, Default)]
pub(crate) struct SearchHeading {
    current: Option<Heading>,
    /// Bit `k` is set when sector `k` was given up.
    avoided: u8,
    /// Give-up order: `given_up_at[k]` is the sequence number of the last time
    /// sector `k` was given up. The smallest one is the oldest dead end, and
    /// the one the fallback reopens first.
    given_up_at: [u64; SECTORS],
    /// The sequence number of the latest give-up.
    give_ups: u64,
}

impl SearchHeading {
    /// The current direction; with none, picks the heaviest sector of
    /// `weights` (ties go to the lowest index, so the choice is deterministic)
    /// that is not avoided. When only given-up sectors have ground, the one
    /// given up longest ago is reopened, so the search never swings straight
    /// back to the sector it just left. `None` when no sector has any ground.
    pub(crate) fn direction(&mut self, from: BlockPos, weights: &[u32; SECTORS]) -> Option<Dir> {
        if self.current.is_none() {
            let sector = match self.heaviest(weights, self.avoided) {
                Some(sector) => sector,
                None => {
                    // Every sector with ground was already given up: reopen
                    // the oldest one instead of forgetting what was tried.
                    let sector = self.oldest(weights)?;
                    self.avoided &= !(1u8 << sector);
                    sector
                }
            };
            self.current = Some(Heading {
                sector,
                anchor: (from.x, from.z),
                last: 0.0,
                recent: [0.0; MAX_STALLS],
                arrivals: 0,
            });
        }
        self.current.map(|heading| UNIT[heading.sector])
    }

    fn heaviest(&self, weights: &[u32; SECTORS], avoided: u8) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (sector, weight) in weights.iter().enumerate() {
            if avoided & (1 << sector) != 0 || *weight == 0 {
                continue;
            }
            if best.is_none_or(|current| *weight > weights[current]) {
                best = Some(sector);
            }
        }
        best
    }

    /// The given-up sector with ground that was given up longest ago.
    fn oldest(&self, weights: &[u32; SECTORS]) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (sector, weight) in weights.iter().enumerate() {
            if self.avoided & (1 << sector) == 0 || *weight == 0 {
                continue;
            }
            if best.is_none_or(|current| self.given_up_at[sector] < self.given_up_at[current]) {
                best = Some(sector);
            }
        }
        best
    }

    /// A frontier arrival at `pos`: measure the gain along the heading since
    /// the previous arrival and give the heading up when the last
    /// [`MAX_STALLS`] arrivals gained less than [`MIN_GAIN`] blocks together.
    pub(crate) fn arrived(&mut self, pos: BlockPos) {
        let Some(heading) = &mut self.current else {
            return;
        };
        let progress = heading.progress(pos);
        heading.recent[heading.arrivals % MAX_STALLS] = progress - heading.last;
        heading.last = progress;
        heading.arrivals += 1;
        let stalled =
            heading.arrivals >= MAX_STALLS && heading.recent.iter().sum::<f64>() < MIN_GAIN;
        if stalled {
            self.give_up();
        }
    }

    /// The heading led nowhere (no reachable frontier forward): turn.
    pub(crate) fn give_up(&mut self) {
        let Some(heading) = self.current.take() else {
            return;
        };
        self.give_ups = self.give_ups.wrapping_add(1);
        self.given_up_at[heading.sector] = self.give_ups;
        self.avoided |= 1 << heading.sector;
        if self.avoided == u8::MAX {
            self.avoided = 1 << heading.sector;
        }
    }

    #[cfg(test)]
    pub(crate) fn sector(&self) -> Option<usize> {
        self.current.map(|heading| heading.sector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, z: i32) -> BlockPos {
        BlockPos::new(x, 64, z)
    }

    #[test]
    fn sectors_follow_the_compass() {
        assert_eq!(sector_of(10, 0), 0);
        assert_eq!(sector_of(10, 10), 1);
        assert_eq!(sector_of(0, 10), 2);
        assert_eq!(sector_of(-10, 1), 4);
        assert_eq!(sector_of(0, -10), 6);
        assert_eq!(sector_of(10, -9), 7);
    }

    #[test]
    fn the_heaviest_sector_is_picked_and_kept() {
        let mut search = SearchHeading::default();
        let mut weights = [0; SECTORS];
        weights[2] = 5;
        weights[6] = 9;
        assert_eq!(search.direction(at(0, 0), &weights), Some((0.0, -1.0)));
        // The weights changing later does not move a held heading.
        weights[2] = 100;
        assert_eq!(search.direction(at(0, 0), &weights), Some((0.0, -1.0)));
        assert_eq!(search.direction(at(0, 0), &[0; SECTORS]), Some((0.0, -1.0)));
    }

    #[test]
    fn no_open_ground_gives_no_heading() {
        let mut search = SearchHeading::default();
        assert_eq!(search.direction(at(0, 0), &[0; SECTORS]), None);
    }

    #[test]
    fn a_stalled_heading_turns_and_does_not_swing_back() {
        let mut search = SearchHeading::default();
        let weights = [30, 20, 0, 0, 25, 0, 0, 0];
        search.direction(at(0, 0), &weights);
        assert_eq!(search.sector(), Some(0));
        // Real progress resets the stall count.
        search.arrived(at(10, 0));
        search.arrived(at(11, 0));
        search.arrived(at(12, 0));
        assert_eq!(search.sector(), Some(0), "two weak arrivals are tolerated");
        search.arrived(at(13, 0));
        assert_eq!(search.sector(), None, "three in a row give the heading up");
        // East is avoided: the next heaviest sector wins, not east again.
        search.direction(at(13, 0), &weights);
        assert_eq!(search.sector(), Some(4));
        // Giving that up too leaves only sector 1.
        search.give_up();
        search.direction(at(13, 0), &weights);
        assert_eq!(search.sector(), Some(1));
    }

    #[test]
    fn a_steady_small_gain_keeps_the_heading() {
        // Three blocks per leg is less than MIN_GAIN on its own, but the last
        // three arrivals together gain nine: the search is moving, not stuck.
        let mut search = SearchHeading::default();
        let mut weights = [0; SECTORS];
        weights[0] = 1;
        search.direction(at(0, 0), &weights);
        for leg in 1..=8 {
            search.arrived(at(3 * leg, 0));
            assert_eq!(
                search.sector(),
                Some(0),
                "leg {leg} gained three blocks: still east"
            );
        }
    }

    #[test]
    fn the_avoided_set_restarts_when_every_sector_was_tried() {
        let mut search = SearchHeading::default();
        let weights = [1; SECTORS];
        let mut seen = Vec::new();
        for _ in 0..SECTORS {
            search.direction(at(0, 0), &weights);
            seen.push(search.sector().unwrap());
            search.give_up();
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..SECTORS).collect::<Vec<_>>(), "all tried once");
        search.direction(at(0, 0), &weights);
        assert!(search.sector().is_some(), "the search continues afterwards");
    }

    #[test]
    fn avoided_sectors_are_reused_when_nothing_else_has_ground() {
        let mut search = SearchHeading::default();
        let mut weights = [0; SECTORS];
        weights[3] = 1;
        search.direction(at(0, 0), &weights);
        search.give_up();
        search.direction(at(0, 0), &weights);
        assert_eq!(search.sector(), Some(3));
    }

    #[test]
    fn two_dead_ends_alternate_without_clearing_the_memory() {
        // Ground only east and west: two dead ends. Every pick is given up at
        // once, so every pick after the first comes from the fallback. It must
        // keep both marks and alternate one dead end per cycle, never repeat
        // the sector it just gave up and never forget the other one.
        let mut search = SearchHeading::default();
        let mut weights = [0; SECTORS];
        weights[0] = 1;
        weights[4] = 1;
        let mut picks = Vec::new();
        for _ in 0..4 {
            search.direction(at(0, 0), &weights);
            picks.push(search.sector().expect("a dead end is still ground"));
            search.give_up();
        }
        assert_eq!(picks, [0, 4, 0, 4], "one alternation per cycle");
        // The fallback reopened east and cleared only its own mark: west is
        // still avoided. Clearing the whole set would lose that memory.
        search.direction(at(0, 0), &weights);
        assert_eq!(search.sector(), Some(0));
        assert_ne!(search.avoided & (1 << 4), 0, "west is still avoided");
    }

    #[test]
    fn the_fallback_reopens_the_oldest_dead_end_not_the_heaviest() {
        // East is given up first, then north, then west. Once only given-up
        // sectors have ground the fallback must return to the oldest (east),
        // not to the heaviest (north) or the one just given up (west).
        let mut search = SearchHeading::default();
        for sector in [0, 2, 4] {
            let mut only = [0; SECTORS];
            only[sector] = 1;
            search.direction(at(0, 0), &only);
            assert_eq!(search.sector(), Some(sector));
            search.give_up();
        }
        let weights = [1, 0, 9, 0, 5, 0, 0, 0];
        search.direction(at(0, 0), &weights);
        assert_eq!(search.sector(), Some(0), "the oldest dead end comes back");
        search.give_up();
        search.direction(at(0, 0), &weights);
        assert_eq!(search.sector(), Some(2), "then the rotation continues");
    }
}
