//! Independent tests and oracle for the planner and the known map (t8a review).
//!
//! A path-validity oracle over random hand-built worlds, an optimality sanity
//! check against an independent Dijkstra, frontier/budget checks, a follower
//! simulation in `praxion_headless::physics`, and panic/extreme-coordinate
//! probes. Touches only the public API of `praxion-planner` (plus headless for
//! the follower).

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use azalea_block::BlockState;
use azalea_registry::builtin::BlockKind;
use praxion_headless::{Body, Look};
use praxion_planner::blocks;
use praxion_planner::*;
use praxion_world::{BlockPos, Vec3};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}
fn water() -> BlockState {
    BlockState::from(BlockKind::Water)
}
fn lava() -> BlockState {
    BlockState::from(BlockKind::Lava)
}

#[derive(Clone)]
struct World {
    blocks: HashMap<BlockPos, BlockState>,
    unknown: HashSet<BlockPos>,
}

impl World {
    fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            unknown: HashSet::new(),
        }
    }
    fn set(&mut self, x: i32, y: i32, z: i32, s: BlockState) {
        self.blocks.insert(BlockPos::new(x, y, z), s);
    }
    fn set_unknown(&mut self, x: i32, y: i32, z: i32) {
        self.unknown.insert(BlockPos::new(x, y, z));
    }
    fn is_unknown(&self, pos: BlockPos) -> bool {
        self.unknown.contains(&pos)
    }
    fn known_state(&self, pos: BlockPos) -> BlockState {
        if self.unknown.contains(&pos) {
            BlockState::AIR
        } else {
            self.blocks.get(&pos).copied().unwrap_or(BlockState::AIR)
        }
    }
    fn flat(y: i32, radius: i32) -> Self {
        let mut w = Self::new();
        for x in -radius..=radius {
            for z in -radius..=radius {
                for yy in (y - 8)..=y {
                    w.set(x, yy, z, stone());
                }
            }
        }
        w
    }
}

impl KnownBlocks for World {
    fn block(&mut self, pos: BlockPos) -> Block {
        if self.unknown.contains(&pos) {
            Block::Unknown
        } else {
            Block::Known {
                state: self.blocks.get(&pos).copied().unwrap_or(BlockState::AIR),
                age_ticks: 0,
            }
        }
    }
}

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }
    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
    fn usize(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() as usize) % n
        }
    }
    fn i32(&mut self, lo: i32, hi: i32) -> i32 {
        if hi < lo {
            return lo;
        }
        lo + (self.next() as i32).rem_euclid(hi - lo + 1)
    }
    fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn neighbors6(pos: BlockPos) -> [BlockPos; 6] {
    [
        pos.up(1),
        pos.down(1),
        pos.north(1),
        pos.south(1),
        pos.east(1),
        pos.west(1),
    ]
}

// ---------------------------------------------------------------------------
// World generation
// ---------------------------------------------------------------------------

fn random_world(rng: &mut Rng, _id: usize) -> (World, PlanningState) {
    let mut w = World::new();
    let radius = 7_i32;
    // base terrain
    for x in -radius..=radius {
        for z in -radius..=radius {
            let h = 60 + rng.i32(0, 4); // top stone y
            for yy in 52..=h {
                w.set(x, yy, z, stone());
            }
        }
    }
    // pits: dig down a few columns
    for _ in 0..rng.usize(12) {
        let x = rng.i32(-radius, radius);
        let z = rng.i32(-radius, radius);
        for yy in 58..=66 {
            w.blocks.remove(&BlockPos::new(x, yy, z));
        }
    }
    // gaps: remove a vertical slice to create 1-wide holes
    for _ in 0..rng.usize(8) {
        let x = rng.i32(-radius, radius);
        let z = rng.i32(-radius, radius);
        for yy in 52..=66 {
            w.blocks.remove(&BlockPos::new(x, yy, z));
        }
    }
    // water / lava pools
    for _ in 0..rng.usize(6) {
        let x = rng.i32(-radius, radius);
        let z = rng.i32(-radius, radius);
        let y = 60 + rng.i32(0, 2);
        if rng.bool() {
            w.set(x, y, z, water());
        } else {
            w.set(x, y, z, lava());
        }
    }
    // unknown cells
    for _ in 0..rng.usize(25) {
        let x = rng.i32(-radius, radius);
        let z = rng.i32(-radius, radius);
        let y = rng.i32(58, 66);
        w.set_unknown(x, y, z);
    }
    // protected blocks
    let mut state = PlanningState::new(Inventory::with(&[("cobblestone", 64), ("stone_pickaxe", 1)]));
    for _ in 0..rng.usize(10) {
        let x = rng.i32(-radius, radius);
        let z = rng.i32(-radius, radius);
        let y = 63;
        state.protect(BlockPos::new(x, y, z));
    }
    (w, state)
}

// ---------------------------------------------------------------------------
// Independent validity checker
// ---------------------------------------------------------------------------

fn validate_step(
    world: &World,
    state: &PlanningState,
    step: &Step,
    places_left: &mut u16,
    max_drop: i32,
) -> Result<(), String> {
    let to = step.to;
    // unknown destination
    if world.is_unknown(to) {
        return Err("step into unknown feet".into());
    }
    if world.is_unknown(to.up(1)) {
        return Err("step into unknown head".into());
    }
    // hazard at destination
    for cell in [to, to.up(1)] {
        let st = world.known_state(cell);
        if blocks::is_hazard(st) {
            return Err(format!("step into hazard at {cell:?}"));
        }
    }
    match &step.kind {
        MoveKind::EnterWater | MoveKind::Swim | MoveKind::SwimUp => {
            let feet = world.known_state(to);
            if !blocks::is_water(feet) {
                return Err("water move into non-water".into());
            }
            let head = world.known_state(to.up(1));
            if !(blocks::is_water(head) || blocks::is_passable(head)) {
                return Err("water move bad head".into());
            }
        }
        MoveKind::ExitWater { rise } => {
            // destination is land
            for cell in [to, to.up(1)] {
                let st = world.known_state(cell);
                if !blocks::is_passable(st) {
                    return Err("exit water into non-passable".into());
                }
            }
            let below = to.down(1);
            if !blocks::is_solid(world.known_state(below)) {
                return Err("exit water no floor".into());
            }
            let _ = rise;
        }
        MoveKind::Bridge { placed, over: _ } => {
            let placed = *placed;
            if *places_left == 0 {
                return Err("bridge without placeable".into());
            }
            *places_left -= 1;
            // placed cell must be placeable + support checked by planner; here just not protected/unknown
            if state.protected.contains(&placed) || state.blacklist.contains(&placed) {
                return Err("bridge places on protected".into());
            }
            if world.is_unknown(placed) {
                return Err("bridge places on unknown".into());
            }
            for cell in [to, to.up(1)] {
                let st = world.known_state(cell);
                if !blocks::is_passable(st) {
                    return Err("bridge destination not passable".into());
                }
            }
            let below = to.down(1);
            if !blocks::is_solid(world.known_state(below)) && !state.protected.contains(&below) {
                // bridge lands on far side which has floor by planner; require solid
                if !blocks::is_solid(world.known_state(below)) {
                    return Err("bridge landing no floor".into());
                }
            }
        }
        MoveKind::Pillar { placed } => {
            let placed = *placed;
            if *places_left == 0 {
                return Err("pillar without placeable".into());
            }
            *places_left -= 1;
            if state.protected.contains(&placed) || state.blacklist.contains(&placed) {
                return Err("pillar places on protected".into());
            }
            if world.is_unknown(placed) {
                return Err("pillar places on unknown".into());
            }
            for cell in [to, to.up(1)] {
                let st = world.known_state(cell);
                if !blocks::is_passable(st) {
                    return Err("pillar destination not passable".into());
                }
            }
        }
        MoveKind::Dig { broken } => {
            for cell in broken {
                let cell = *cell;
                if state.protected.contains(&cell) || state.blacklist.contains(&cell) {
                    return Err("dig protected/blacklisted".into());
                }
                if world.is_unknown(cell) {
                    return Err("dig unknown".into());
                }
                if cell.x == step.from.x && cell.z == step.from.z && cell.y < step.from.y {
                    return Err("dig straight down".into());
                }
                let st = world.known_state(cell);
                if !blocks::is_breakable(st) {
                    return Err("dig unbreakable".into());
                }
                if blocks::fluid(st).is_some() {
                    return Err("dig fluid".into());
                }
                for nb in neighbors6(cell) {
                    if world.is_unknown(nb) {
                        return Err("dig neighbor unknown".into());
                    }
                    let nst = world.known_state(nb);
                    if blocks::fluid(nst).is_some() {
                        return Err("dig neighbor fluid".into());
                    }
                }
            }
            // destination passable after dig: planner checks floor_free
            let below = to.down(1);
            if !blocks::is_solid(world.known_state(below)) {
                // allow water? dig lands on solid usually
                if !blocks::is_water(world.known_state(below)) {
                    return Err("dig landing no floor".into());
                }
            }
            for cell in [to, to.up(1)] {
                // after dig, destination becomes air; world still has block but planner assumes broken; treat passable or breakable
                let st = world.known_state(cell);
                if !(blocks::is_passable(st) || blocks::is_breakable(st)) {
                    return Err("dig destination not passable/breakable".into());
                }
            }
        }
        MoveKind::Descend { drop } => {
            let drop = *drop;
            if drop < 1 || drop > max_drop {
                return Err(format!("bad drop {drop}"));
            }
            if step.from.y - step.to.y != drop {
                return Err("drop height mismatch".into());
            }
            for cell in [to, to.up(1)] {
                let st = world.known_state(cell);
                if !blocks::is_passable(st) {
                    return Err("descend destination not passable".into());
                }
            }
            let below = to.down(1);
            if !blocks::is_solid(world.known_state(below)) {
                return Err("descend no floor".into());
            }
        }
        MoveKind::StepUp => {
            if step.to.y - step.from.y != 1 {
                return Err("stepup height mismatch".into());
            }
            for cell in [to, to.up(1)] {
                let st = world.known_state(cell);
                if !blocks::is_passable(st) {
                    return Err("stepup destination not passable".into());
                }
            }
            let below = to.down(1);
            if !blocks::is_solid(world.known_state(below)) {
                return Err("stepup no floor".into());
            }
        }
        _ => {
            // Walk / Diagonal / SwimUp / others: land passable + solid floor
            if !matches!(step.kind, MoveKind::SwimUp) {
                for cell in [to, to.up(1)] {
                    let st = world.known_state(cell);
                    if !blocks::is_passable(st) {
                        return Err(format!("{:?} destination not passable", step.kind));
                    }
                }
                let below = to.down(1);
                if !blocks::is_solid(world.known_state(below)) {
                    return Err(format!("{:?} no floor", step.kind));
                }
            } else {
                // SwimUp: vertical in water
                let st = world.known_state(to);
                if !(blocks::is_water(st) || blocks::is_passable(st)) {
                    return Err("swimup bad cell".into());
                }
            }
        }
    }

    // horizontal reach sanity
    let dx = (to.x - step.from.x).abs();
    let dz = (to.z - step.from.z).abs();
    let dy = (to.y - step.from.y).abs();
    if matches!(step.kind, MoveKind::Bridge { .. }) {
        if dx.max(dz) != 2 || dy != 0 {
            return Err("bridge geometry".into());
        }
    } else if matches!(step.kind, MoveKind::Diagonal) {
        if dx != 1 || dz != 1 || dy != 0 {
            return Err("diagonal geometry".into());
        }
    } else if !matches!(step.kind, MoveKind::Pillar { .. } | MoveKind::SwimUp) {
        if dx.max(dz) > 1 || dy > 3 {
            return Err("move too far".into());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Test 1: oracle
// ---------------------------------------------------------------------------

#[test]
fn independent_oracle_path_validity() {
    let mut rng = Rng::new(0x1234_5678);
    let mut violations: Vec<String> = Vec::new();
    let mut plans = 0usize;
    let mut paths = 0usize;
    for w in 0..30usize {
        let (world, state) = random_world(&mut rng, w);
        for g in 0..67usize {
            let sx = rng.i32(-6, 6);
            let sz = rng.i32(-6, 6);
            let gx = rng.i32(-6, 6);
            let gz = rng.i32(-6, 6);
            let start = BlockPos::new(sx, 64, sz);
            let goal = Goal::Block(BlockPos::new(gx, 64, gz));
            plans += 1;
            let planner = Planner::with_budget(Budget::new(Duration::from_millis(20), 5000));
            let mut w2 = world.clone();
            let s2 = state.clone();
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                planner.plan(&mut w2, &s2, start, goal)
            }));
            match outcome {
                Ok(plan) => {
                    let mut places = s2.inventory.placements();
                    let mut cur = start;
                    for (i, step) in plan.steps.iter().enumerate() {
                        if step.from != cur {
                            violations.push(format!("w{w} g{g} step{i}: from mismatch"));
                        }
                        if let Err(e) = validate_step(&world, &state, step, &mut places, planner.max_drop()) {
                            violations.push(format!("w{w} g{g} step{i}: {e}"));
                        }
                        cur = step.to;
                    }
                    if !plan.steps.is_empty() {
                        paths += 1;
                    }
                    // partial cell should be unknown when present
                    if let PlanStatus::Partial { cell } = plan.status {
                        if !world.is_unknown(cell) {
                            violations.push(format!("w{w} g{g}: partial cell not unknown {cell:?}"));
                        }
                    }
                }
                Err(_) => violations.push(format!("w{w} g{g}: PANIC")),
            }
        }
    }
    // write scratch report
    let _ = std::fs::create_dir_all("logs/t8a");
    let report = format!(
        "plans={plans} paths={paths} violations={}\n{}",
        violations.len(),
        violations.iter().take(50).cloned().collect::<Vec<_>>().join("\n")
    );
    let _ = std::fs::write("logs/t8a/oracle.txt", report);
    assert!(
        violations.is_empty(),
        "oracle violations ({}): {:?}",
        violations.len(),
        &violations[..violations.len().min(10)]
    );
}

// ---------------------------------------------------------------------------
// Test 2: optimality (land, walk-only)
// ---------------------------------------------------------------------------

const SPRINT: f32 = 20. / 5.612;
const WALK: f32 = 20. / 4.317;
const WALK_OFF: f32 = WALK * 0.8;
const CENTER_AFTER_FALL: f32 = WALK - WALK_OFF;
const JUMP_PENALTY: f32 = 2.;
const JUMP_ONE: f32 = 3.163;
const FALL: [f32; 4] = [0., 5.6147, 7.7881, 9.4687];

fn land_passable(w: &World, pos: BlockPos) -> bool {
    !w.is_unknown(pos) && blocks::is_passable(w.known_state(pos))
}
fn land_floor(w: &World, pos: BlockPos) -> bool {
    !w.is_unknown(pos.down(1)) && blocks::is_solid(w.known_state(pos.down(1)))
}

fn land_successors(w: &World, pos: BlockPos, max_drop: i32) -> Vec<(BlockPos, f32)> {
    let mut out = Vec::new();
    let dirs: [(i32, i32); 4] = [(0, 1), (0, -1), (1, 0), (-1, 0)];
    for (dx, dz) in dirs {
        let target = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
        if land_passable(w, target) && land_passable(w, target.up(1)) && land_floor(w, target) {
            out.push((target, SPRINT));
        }
    }
    for (dx, dz) in dirs {
        let side_x = BlockPos::new(pos.x + dx, pos.y, pos.z);
        let side_z = BlockPos::new(pos.x, pos.y, pos.z + dz);
        let target = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
        if land_passable(w, side_x)
            && land_passable(w, side_x.up(1))
            && land_passable(w, side_z)
            && land_passable(w, side_z.up(1))
            && land_passable(w, target)
            && land_passable(w, target.up(1))
            && land_floor(w, target)
        {
            out.push((target, SPRINT * 2_f32.sqrt() + 0.001));
        }
    }
    if land_passable(w, pos.up(1)) && land_passable(w, pos.up(2)) {
        for (dx, dz) in dirs {
            let target = BlockPos::new(pos.x + dx, pos.y + 1, pos.z + dz);
            if land_passable(w, target) && land_passable(w, target.up(1)) && land_floor(w, target) {
                let cost = WALK.max(JUMP_ONE) + JUMP_PENALTY;
                out.push((target, cost));
            }
        }
    }
    for (dx, dz) in dirs {
        let horiz = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
        if !land_passable(w, horiz) || !land_passable(w, horiz.up(1)) {
            continue;
        }
        for drop in 1..=max_drop {
            let target = horiz.down(drop as i32);
            let mut clear = true;
            for k in 0..drop {
                let cell = horiz.down(k as i32);
                if !land_passable(w, cell) || !land_passable(w, cell.up(1)) {
                    clear = false;
                    break;
                }
            }
            if !clear {
                continue;
            }
            if !land_passable(w, target) || !land_passable(w, target.up(1)) {
                continue;
            }
            if !land_floor(w, target) {
                continue;
            }
            let fall = FALL[drop as usize];
            let cost = WALK_OFF + fall.max(CENTER_AFTER_FALL);
            out.push((target, cost));
        }
    }
    out
}

fn dijkstra(start: BlockPos, goal: BlockPos, world: &World, max_drop: i32) -> Option<f32> {
    if start == goal {
        return Some(0.);
    }
    let mut dist: HashMap<BlockPos, f32> = HashMap::new();
    dist.insert(start, 0.);
    let _pq: BinaryHeap<(i64, BlockPos)> = BinaryHeap::new();
    // use negated cost via i64 bits? Simpler: store (cost bits) and pop min manually.
    // We'll use a Vec and linear scan (small worlds).
    let mut visited: HashSet<BlockPos> = HashSet::new();
    let _current = start;
    // Dijkstra with priority queue using f32 ordering via wrapper
    #[derive(Clone, Copy)]
    struct State {
        cost: f32,
        pos: BlockPos,
    }
    impl PartialEq for State {
        fn eq(&self, other: &Self) -> bool {
            self.pos == other.pos && self.cost == other.cost
        }
    }
    impl Eq for State {}
    impl PartialOrd for State {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for State {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other
                .cost
                .partial_cmp(&self.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    }
    let mut pq: BinaryHeap<State> = BinaryHeap::new();
    pq.push(State { cost: 0., pos: start });
    while let Some(State { cost, pos }) = pq.pop() {
        if pos == goal {
            return Some(cost);
        }
        if visited.contains(&pos) {
            continue;
        }
        visited.insert(pos);
        if cost > *dist.get(&pos).unwrap_or(&f32::INFINITY) {
            continue;
        }
        for (nb, w) in land_successors(world, pos, max_drop) {
            if visited.contains(&nb) {
                continue;
            }
            let nc = cost + w;
            if nc < *dist.get(&nb).unwrap_or(&f32::INFINITY) {
                dist.insert(nb, nc);
                pq.push(State { cost: nc, pos: nb });
            }
        }
    }
    None
}

#[test]
fn independent_optimality_sanity() {
    let mut rng = Rng::new(0x9999);
    let mut violations: Vec<String> = Vec::new();
    let mut compared = 0usize;
    for w in 0..200usize {
        // small flat world with random stone obstacles
        let mut world = World::flat(63, 6);
        for _ in 0..rng.usize(15) {
            let x = rng.i32(-5, 5);
            let z = rng.i32(-5, 5);
            world.set(x, 63, z, stone());
            world.set(x, 64, z, stone());
            world.set(x, 65, z, stone());
        }
        // ensure start/goal floors
        let sx = rng.i32(-4, 4);
        let sz = rng.i32(-4, 4);
        let gx = rng.i32(-4, 4);
        let gz = rng.i32(-4, 4);
        let start = BlockPos::new(sx, 64, sz);
        let goal = BlockPos::new(gx, 64, gz);
        if start == goal {
            continue;
        }
        // clear obstacles at start/goal
        for p in [start, goal] {
            world.blocks.remove(&p);
            world.blocks.remove(&p.up(1));
            world.set(p.x, 63, p.z, stone());
        }
        let opt = match dijkstra(start, goal, &world, 3) {
            Some(c) => c,
            None => continue,
        };
        let state = PlanningState::new(Inventory::empty());
        let mut planner = Planner::new();
        planner.set_allow_world_changes(false).set_max_drop(3);
        let mut w2 = world.clone();
        let plan = planner.plan(&mut w2, &state, start, Goal::Block(goal));
        if plan.status != PlanStatus::Success {
            continue;
        }
        compared += 1;
        if plan.cost > opt * 1.05 + 1e-3 {
            violations.push(format!(
                "w{w}: planner {} > 1.05*opt {} (start {start:?} goal {goal:?})",
                plan.cost, opt
            ));
        }
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write(
        "logs/t8a/optimality.txt",
        format!("compared={compared} violations={}\n{}", violations.len(), violations.join("\n")),
    );
    assert!(
        violations.is_empty(),
        "optimality violations: {:?}",
        &violations[..violations.len().min(10)]
    );
}

// ---------------------------------------------------------------------------
// Test 3: frontier
// ---------------------------------------------------------------------------

#[test]
fn independent_frontier() {
    let mut violations = Vec::new();
    // goal behind a wall of unknown cells
    let mut world = World::flat(63, 8);
    for z in 3..=4 {
        for x in -8..=8 {
            world.set_unknown(x, 64, z);
            world.set_unknown(x, 65, z);
        }
    }
    let state = PlanningState::new(Inventory::empty());
    let planner = Planner::new();
    let mut w2 = world.clone();
    let plan = planner.plan(&mut w2, &state, BlockPos::new(0, 64, 0), Goal::Block(BlockPos::new(0, 64, 7)));
    // should be partial
    match plan.status {
        PlanStatus::Partial { cell } => {
            if !world.is_unknown(cell) {
                violations.push(format!("partial cell not unknown: {cell:?}"));
            }
        }
        other => violations.push(format!("expected Partial, got {other:?}")),
    }
    // path cells known
    for c in plan.path_cells() {
        if world.is_unknown(c) {
            violations.push(format!("path enters unknown cell {c:?}"));
        }
    }
    // validate steps
    let mut places = state.inventory.placements();
    let mut cur = BlockPos::new(0, 64, 0);
    for (i, step) in plan.steps.iter().enumerate() {
        if step.from != cur {
            violations.push(format!("step {i} from mismatch"));
        }
        if let Err(e) = validate_step(&world, &state, step, &mut places, planner.max_drop()) {
            violations.push(format!("step {i}: {e}"));
        }
        cur = step.to;
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write("logs/t8a/frontier.txt", violations.join("\n"));
    assert!(violations.is_empty(), "{:?}", violations);
}

// ---------------------------------------------------------------------------
// Test 4: budgets
// ---------------------------------------------------------------------------

#[test]
fn independent_budgets() {
    let mut violations = Vec::new();
    for (time_ms, nodes) in [(1u64, 1usize), (1, 5), (2, 10), (0u64, 1)] {
        let world = World::flat(63, 10);
        // scatter obstacles to make search nontrivial
        let state = PlanningState::new(Inventory::empty());
        let planner = Planner::with_budget(Budget::new(Duration::from_millis(time_ms.max(1)), nodes.max(1)));
        let mut w2 = world.clone();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            planner.plan(&mut w2, &state, BlockPos::new(0, 64, 0), Goal::Block(BlockPos::new(9, 64, 9)))
        }));
        match outcome {
            Ok(plan) => {
                // path so far valid
                let mut places = state.inventory.placements();
                let mut cur = BlockPos::new(0, 64, 0);
                for (i, step) in plan.steps.iter().enumerate() {
                    if step.from != cur {
                        violations.push(format!("t{time_ms} n{nodes} step{i} from mismatch"));
                    }
                    if let Err(e) = validate_step(&world, &state, step, &mut places, planner.max_drop()) {
                        violations.push(format!("t{time_ms} n{nodes} step{i}: {e}"));
                    }
                    cur = step.to;
                }
            }
            Err(_) => violations.push(format!("t{time_ms} n{nodes}: PANIC")),
        }
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write("logs/t8a/budgets.txt", violations.join("\n"));
    assert!(violations.is_empty(), "{:?}", violations);
}

// ---------------------------------------------------------------------------
// Physics world helper (flat stone floor at y=63)
// ---------------------------------------------------------------------------

fn flat_physics_world() -> praxion_world::World {
    use azalea_buf::AzBuf;
    use azalea_core::game_type::GameMode;
    use azalea_protocol::packets::common::CommonPlayerSpawnInfo;
    use azalea_protocol::packets::game::{
        ClientboundBlockUpdate, ClientboundGamePacket, ClientboundLevelChunkWithLight,
        ClientboundLogin, c_level_chunk_with_light::ClientboundLevelChunkPacketData,
    };
    use azalea_registry::builtin::BlockKind;
    use azalea_registry::DataRegistry;
    use azalea_registry::data::DimensionKind;
    use azalea_registry::identifier::Identifier;
    use azalea_world::Chunk;
    use std::sync::Arc;

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
            player_id: azalea_core::entity_id::MinecraftEntityId(7),
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

    let mut world = praxion_world::World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));

    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    for cx in -1..=1 {
        for cz in -1..=1 {
            let packet = ClientboundLevelChunkWithLight {
                x: cx,
                z: cz,
                chunk_data: ClientboundLevelChunkPacketData {
                    heightmaps: Vec::new(),
                    buffer: Arc::new(buffer.clone().into_boxed_slice()),
                    block_entities: Vec::new(),
                },
                light_data: Default::default(),
            };
            world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(packet));
        }
    }

    let stone = BlockState::from(BlockKind::Stone);
    for x in -6..=6 {
        for z in -6..=6 {
            let pos = BlockPos::new(x, 63, z);
            world.apply_clientbound_play(
                0,
                &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
                    pos,
                    block_state: stone,
                }),
            );
        }
    }
    world
}

// ---------------------------------------------------------------------------
// Test 5: follower
// ---------------------------------------------------------------------------

#[test]
fn independent_follower() {
    let mut rng = Rng::new(0x5555);
    let style = LookStyle::default();
    let mut reached = 0usize;
    let mut total_ticks = 0usize;
    let mut max_deviation: f64 = 0.;
    let mut aim_violations = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for i in 0..50usize {
        // flat world
        let mut world = World::flat(63, 8);
        let sx = rng.i32(-5, 5);
        let sz = rng.i32(-5, 5);
        let gx = rng.i32(-5, 5);
        let gz = rng.i32(-5, 5);
        let start = BlockPos::new(sx, 64, sz);
        let goal = BlockPos::new(gx, 64, gz);
        if start == goal {
            continue;
        }
        // clear start/goal
        for p in [start, goal] {
            world.blocks.remove(&p);
            world.blocks.remove(&p.up(1));
            world.set(p.x, 63, p.z, stone());
        }

        let state = PlanningState::new(Inventory::empty());
        let mut planner = Planner::new();
        planner.set_allow_world_changes(false);
        let mut w2 = world.clone();
        let plan = planner.plan(&mut w2, &state, start, Goal::Block(goal));
        if plan.status != PlanStatus::Success || plan.steps.is_empty() {
            continue;
        }

        // build follower
        let mut follower = Follower::new(plan.steps.clone(), style);
        follower = follower.with_max_ticks_per_step(400);

        // physics body
        let spawn = Vec3::new(start.x as f64 + 0.5, 64.0, start.z as f64 + 0.5);
        let mut body = Body::new(spawn);
        body.set_look(azalea_entity::LookDirection::new(0., 0.));
        let pw = flat_physics_world();

        let mut ticks = 0usize;
        let mut max_dev = 0f64;
        let mut prev_look: Option<(f32, f32)> = Some((0., 0.));
        let max_ticks = 2000usize;
        while !follower.is_done() && ticks < max_ticks {
            let frame = follower.step(ticks as u64, body.generation(), &body);
            // aim speed check
            if let Look::Aim { yaw, pitch } = frame.look {
                if let Some((py, pp)) = prev_look {
                    let dy = (yaw - py).abs();
                    let dp = (pitch - pp).abs();
                    let dy = dy.min(360. - dy);
                    if dy > style.max_yaw_deg + 1e-3 || dp > style.max_pitch_deg + 1e-3 {
                        aim_violations += 1;
                    }
                }
                prev_look = Some((yaw, pitch));
            }
            if let Err(_) = frame.apply(&mut body, ticks as u64) {
                break;
            }
            body.step(&frame.held, &pw);
            ticks += 1;
            let prog = follower.progress(&body);
            if prog.deviation > max_dev {
                max_dev = prog.deviation;
            }
            if follower.is_stalled() {
                break;
            }
        }
        total_ticks += ticks;
        if max_dev > max_deviation {
            max_deviation = max_dev;
        }
        // reached?
        let final_pos = body.position();
        let dx = final_pos.x - (goal.x as f64 + 0.5);
        let dz = final_pos.z - (goal.z as f64 + 0.5);
        let dy = final_pos.y - (goal.y as f64);
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        if follower.is_done() && dist < 1.5 {
            reached += 1;
        } else {
            failures.push(format!(
                "path {i}: done={} dist={dist:.2} ticks={ticks} stalled={}",
                follower.is_done(),
                follower.is_stalled()
            ));
        }
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write(
        "logs/t8a/follower.txt",
        format!(
            "reached={}/50 ticks_total={} max_deviation={max_deviation:.3} aim_violations={aim_violations}\n{}",
            reached,
            total_ticks,
            failures.join("\n")
        ),
    );
    // require most reached; allow some failures but report
    assert!(aim_violations == 0, "aim speed violations: {aim_violations}");
    assert!(reached >= 40, "only {reached}/50 reached; failures: {:?}", &failures[..failures.len().min(10)]);
}

// ---------------------------------------------------------------------------
// Test 6: extreme coordinates / panics
// ---------------------------------------------------------------------------

// FINDING 1: planner panics on extreme coordinates (i32 overflow in goal.rs heuristic).
#[test]
fn independent_extreme_coordinates_no_panic() {
    let mut violations = Vec::new();
    let extremes: [i32; 5] = [i32::MAX, i32::MIN, 1_000_000, -1_000_000, 0];
    for &x in &extremes {
        for &z in &extremes {
            for &y in &[0i32, 64, i32::MAX / 2] {
                let world = World::new();
                let state = PlanningState::new(Inventory::empty());
                let planner = Planner::new();
                let mut w2 = world.clone();
                let start = BlockPos::new(0, 64, 0);
                let goal = BlockPos::new(x, y, z);
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    planner.plan(&mut w2, &state, start, Goal::Block(goal))
                }));
                if outcome.is_err() {
                    violations.push(format!("panic start(0,64,0) goal({x},{y},{z})"));
                }
                // also swapped
                let mut w3 = world.clone();
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    planner.plan(&mut w3, &state, goal, Goal::Block(start))
                }));
                if outcome.is_err() {
                    violations.push(format!("panic start({x},{y},{z}) goal(0,64,0)"));
                }
            }
        }
    }
    // protected/blacklist extremes
    let mut state = PlanningState::new(Inventory::empty());
    state.protect(BlockPos::new(i32::MAX, i32::MAX, i32::MAX));
    state.blacklist.insert(BlockPos::new(i32::MIN, i32::MIN, i32::MIN));
    let world = World::new();
    let planner = Planner::new();
    let mut w2 = world.clone();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        planner.plan(&mut w2, &state, BlockPos::new(0, 64, 0), Goal::Block(BlockPos::new(1, 64, 0)))
    }));
    if outcome.is_err() {
        violations.push("panic with extreme protected".into());
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write("logs/t8a/extreme.txt", violations.join("\n"));
    assert!(violations.is_empty(), "{:?}", violations);
}

// ---------------------------------------------------------------------------
// Test 7: stuck guard (loop detection)
// ---------------------------------------------------------------------------

#[test]
fn independent_stuck_guard() {
    let mut violations: Vec<String> = Vec::new();
    // threshold 2
    let mut guard = StuckGuard::new(2);
    let plan_cells = vec![
        BlockPos::new(0, 64, 0),
        BlockPos::new(0, 64, 1),
        BlockPos::new(0, 64, 2),
    ];
    // build a Plan manually
    let steps: Vec<Step> = plan_cells
        .windows(2)
        .map(|p| Step {
            from: p[0],
            to: p[1],
            kind: MoveKind::Walk,
            primitives: Vec::new(),
            cost: 1.,
        })
        .collect();
    let plan = Plan {
        status: PlanStatus::NoPath {
            reason: Unreachable::NoRoute,
        },
        steps,
        cost: 0.,
        nodes_expanded: 0,
        elapsed: Duration::ZERO,
    };
    let first = guard.record_replan(&plan);
    let second = guard.record_replan(&plan);
    if first.is_some() {
        violations.push("first replan should not trip".into());
    }
    if second.is_none() {
        violations.push("second replan should trip".into());
    }
    if !guard.is_blacklisted(BlockPos::new(0, 64, 1)) {
        violations.push("cell not blacklisted".into());
    }
    // reset condition
    guard.reset(BlockPos::new(0, 64, 1), 0);
    if guard.is_blacklisted(BlockPos::new(0, 64, 1)) {
        // closer than 64? distance 0 <= 64, so should clear. If still blacklisted, violation.
        violations.push("reset did not clear".into());
    }
    let _ = std::fs::create_dir_all("logs/t8a");
    let _ = std::fs::write("logs/t8a/stuck.txt", violations.join("\n"));
    assert!(violations.is_empty(), "{:?}", violations);
}

// FINDING 1: land_successors_fixed is unused duplicate; harmless.
// FINDING 2: oracle world generation may produce worlds where the planner
// returns Timeout rarely; no correctness issue found.
// FINDING 3: follower test requires >=40/50 reached; see review.
