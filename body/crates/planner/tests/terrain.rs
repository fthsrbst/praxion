//! Real-terrain planning and path following.
//!
//! `corpus4.prxcap` and `corpus5.prxcap` are decoded with the real decoder
//! into [`World`]s. Fifty random goals within 40 blocks are planned per world
//! through the perception API (`Known` only), the four statuses are counted,
//! and p50/p99 planning times are printed. Every returned path is validated
//! against the world, applying the path's own mine/place effects in order.
//!
//! Then the follower drives `praxion_headless::physics::Body` over up to 20
//! planned paths that only walk, jump or fall, and checks that the body reaches
//! the goal and stays inside the path corridor.
//!
//! The captures are machine-local and ignored by git; without them this test
//! prints `SKIP` and passes.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use azalea_block::BlockState;
use praxion_decoder::{Decoder, Event, Packet};
use praxion_headless::Body;
use praxion_perception::{Config, Perception};
use praxion_planner::blocks::{is_passable, is_solid, is_water};
use praxion_planner::{
    Budget, DirectBlocks, Follower, Goal, Inventory, LookStyle, PerceptionBlocks, Plan, PlanStatus,
    Planner, PlanningState, place_block_kind,
};
use praxion_tee::Reader;
use praxion_world::{BlockPos, Vec3, World};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("body/crates/planner is three levels below the root")
        .to_path_buf()
}

fn decode_corpus(name: &str) -> Option<World> {
    let path = repo_root().join("spikes/captures").join(name);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("SKIP real terrain: cannot read {}: {error}", path.display());
            return None;
        }
    };
    let reader = Reader::new(BufReader::new(file)).expect("tee header");
    let mut decoder = Decoder::new();
    decoder.set_reorder_window(60_000_000_000);
    let mut world = World::new();
    let mut followed = false;
    for record in reader {
        let record = record.expect("tee record");
        for event in decoder.feed(&record) {
            apply(&mut world, &mut followed, &event);
        }
    }
    for event in decoder.finish() {
        apply(&mut world, &mut followed, &event);
    }
    assert!(
        world.status().is_valid(),
        "{name} desynced: {:?}",
        world.status()
    );
    Some(world)
}

fn apply(world: &mut World, followed: &mut bool, event: &Event) {
    match event {
        Event::Opened { conn_id, .. } => world.opened(*conn_id),
        Event::Closed { conn_id, .. } => world.closed(*conn_id),
        Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
        Event::Decoded(decoded) => match &decoded.packet {
            Packet::ConfigurationClientbound(packet) => {
                world.apply_clientbound_config(decoded.conn_id, packet);
            }
            Packet::PlayClientbound(packet) => {
                if !*followed && decoded.packet.name() == "login" {
                    world.follow(decoded.conn_id);
                    *followed = true;
                }
                world.apply_clientbound_play(decoded.conn_id, packet);
            }
            Packet::PlayServerbound(packet) => {
                world.apply_serverbound_play(decoded.conn_id, packet);
            }
            _ => {}
        },
    }
}

/// A small deterministic xorshift so the 50 goals are the same on every run.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform-ish in `-range..=range`.
    fn offset(&mut self, range: i32) -> i32 {
        let span = u64::from(range as u32) * 2 + 1;
        (self.next() % span) as i32 - range
    }
}

fn standable(world: &World, pos: BlockPos) -> bool {
    let Some(feet) = world.block_state(pos) else {
        return false;
    };
    let Some(head) = world.block_state(pos.up(1)) else {
        return false;
    };
    let Some(floor) = world.block_state(pos.down(1)) else {
        return false;
    };
    is_passable(feet) && is_passable(head) && is_solid(floor)
}

fn random_goal(world: &World, start: BlockPos, rng: &mut Rng) -> Option<BlockPos> {
    for _ in 0..64 {
        let dx = rng.offset(40);
        let dz = rng.offset(40);
        if dx == 0 && dz == 0 {
            continue;
        }
        let x = start.x + dx;
        let z = start.z + dz;
        for dy in (-8..=8).rev() {
            let pos = BlockPos::new(x, start.y + dy, z);
            if standable(world, pos) {
                return Some(pos);
            }
        }
    }
    None
}

/// Check that every step of `plan` is executable in `world`: apply its mine and
/// place effects in order, then require the feet and head of the target cell to
/// be passable and its floor solid.
fn validate_path(world: &World, plan: &Plan) -> Result<(), String> {
    let mut overlay: HashMap<BlockPos, BlockState> = HashMap::new();
    let state_at = |overlay: &HashMap<BlockPos, BlockState>, pos: BlockPos| {
        overlay
            .get(&pos)
            .copied()
            .or_else(|| world.block_state(pos))
    };

    for step in &plan.steps {
        for call in &step.primitives {
            match call.name {
                "mine" => {
                    overlay.insert(call.target, BlockState::AIR);
                }
                "place" => {
                    let item = call
                        .args
                        .first()
                        .and_then(|arg| arg.name())
                        .ok_or_else(|| format!("place without an item: {call:?}"))?;
                    let kind = place_block_kind(item)
                        .ok_or_else(|| format!("unknown place item `{item}`"))?;
                    overlay.insert(call.target, BlockState::from(kind));
                }
                other => return Err(format!("unknown primitive `{other}` in a path")),
            }
        }
        let feet = state_at(&overlay, step.to)
            .ok_or_else(|| format!("step target {:?} is unloaded", step.to))?;
        let head = state_at(&overlay, step.to.up(1))
            .ok_or_else(|| format!("head of {:?} is unloaded", step.to))?;
        let floor = state_at(&overlay, step.to.down(1))
            .ok_or_else(|| format!("floor of {:?} is unloaded", step.to))?;
        let swimming = matches!(
            step.kind,
            praxion_planner::MoveKind::EnterWater
                | praxion_planner::MoveKind::Swim
                | praxion_planner::MoveKind::SwimUp
        );
        if !(is_passable(feet) || swimming && is_water(feet)) {
            return Err(format!("feet cell {:?} is not passable or water", step.to));
        }
        if !(is_passable(head) || swimming && is_water(head)) {
            return Err(format!(
                "head cell {:?} is not passable or water",
                step.to.up(1)
            ));
        }
        if !swimming && !is_solid(floor) {
            return Err(format!("floor cell {:?} is not solid", step.to.down(1)));
        }

        // The cells a move passes through, not just its target node.
        match step.kind {
            praxion_planner::MoveKind::Descend { drop } => {
                for up in 1..=drop {
                    let cell = BlockPos::new(step.to.x, step.to.y + up, step.to.z);
                    let state = state_at(&overlay, cell)
                        .ok_or_else(|| format!("fall cell {cell:?} is unloaded"))?;
                    if !is_passable(state) {
                        return Err(format!("fall cell {cell:?} is not passable"));
                    }
                }
            }
            praxion_planner::MoveKind::Bridge { over, .. } => {
                let state = state_at(&overlay, over)
                    .ok_or_else(|| format!("gap cell {over:?} is unloaded"))?;
                if !is_passable(state) {
                    return Err(format!("gap cell {over:?} is not passable"));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

#[derive(Default)]
struct Counts {
    success: usize,
    partial: usize,
    timeout: usize,
    no_path: usize,
}

impl Counts {
    fn add(&mut self, status: PlanStatus) {
        match status {
            PlanStatus::Success => self.success += 1,
            PlanStatus::Partial { .. } => self.partial += 1,
            PlanStatus::Timeout => self.timeout += 1,
            PlanStatus::NoPath { .. } => self.no_path += 1,
        }
    }

    fn total(&self) -> usize {
        self.success + self.partial + self.timeout + self.no_path
    }
}

/// A corridor around the path: every cell within one block horizontally and a
/// few blocks vertically of a path cell. A follower may overshoot a node or
/// fall through an intermediate cell, but not wander off the route.
fn corridor(plan: &Plan) -> HashSet<BlockPos> {
    let mut cells = HashSet::new();
    for cell in plan.path_cells() {
        for dx in -1..=1 {
            for dz in -1..=1 {
                for dy in -2..=3 {
                    cells.insert(BlockPos::new(cell.x + dx, cell.y + dy, cell.z + dz));
                }
            }
        }
    }
    cells
}

/// Drive the follower over one path and check it reaches the goal in the
/// corridor within `max_ticks`.
fn drive(world: &World, plan: &Plan, goal: BlockPos) -> Result<u64, String> {
    let start = plan
        .steps
        .first()
        .map(|step| step.from)
        .ok_or_else(|| "an empty path".to_owned())?;
    let mut body = Body::new(Vec3::new(
        f64::from(start.x) + 0.5,
        f64::from(start.y),
        f64::from(start.z) + 0.5,
    ));
    let mut follower = Follower::new(plan.steps.clone(), LookStyle::default());
    let corridor = corridor(plan);
    let max_ticks = (plan.steps.len() as u64) * 40 + 100;

    for tick in 0..max_ticks {
        let frame = follower.step(tick, body.generation(), &body);
        frame
            .apply(&mut body, tick)
            .map_err(|error| format!("frame at tick {tick}: {error}"))?;
        body.step(&frame.held, world);

        let feet = BlockPos::from(body.position());
        if !corridor.contains(&feet) {
            return Err(format!("left the corridor at {feet:?} (tick {tick})"));
        }
        if follower.is_stalled() {
            return Err(format!("stalled at tick {tick}: {:?}", follower.progress(&body)));
        }
        if follower.is_done() {
            let feet = BlockPos::from(body.position());
            if Goal::Block(goal).success(feet) {
                return Ok(tick + 1);
            }
            return Err(format!("finished at {feet:?}, goal {goal:?}"));
        }
    }
    Err(format!(
        "did not finish in {max_ticks} ticks: {:?} at {:?}",
        follower.progress(&body),
        body.position()
    ))
}

/// Ticks of sweeping before the goals are planned. At 20 ticks per second this
/// is 5 seconds, the top of the 2 to 5 second window; the sweep spends at most
/// `ray_budget - reflex_reserve` rays per tick (3,000 by default), so about
/// 300,000 rays per capture.
const SWEEP_TICKS: usize = 100;

#[test]
fn real_terrain_plans_and_follower() {
    let captures = ["corpus4.prxcap", "corpus5.prxcap"];
    let mut total = Counts::default();
    let mut times: Vec<f64> = Vec::new();
    let mut valid_paths = 0_usize;
    let mut followers_attempted = 0_usize;
    let mut followers_reached = 0_usize;
    let mut followers_from_perception = 0_usize;
    let mut follower_ticks: Vec<u64> = Vec::new();
    let mut partial_frontier = 0_usize;
    let mut partial_other = 0_usize;
    let mut sweep_rays = 0_u64;
    let mut decoded_any = false;

    for (capture_index, name) in captures.iter().enumerate() {
        let Some(world) = decode_corpus(name) else {
            continue;
        };
        decoded_any = true;
        let Some(position) = world.player_position() else {
            eprintln!("SKIP {name}: no player position");
            continue;
        };
        let start = BlockPos::from(position);
        println!(
            "{name}: {} chunks, player at {start:?}, start standable: {}",
            world.chunk_count(),
            standable(&world, start)
        );

        let mut planner = Planner::with_budget(Budget::new(Duration::from_millis(100), 50_000));
        planner.set_max_drop(3);
        let state = PlanningState::new(Inventory::with(&[
            ("iron_pickaxe", 1),
            ("cobblestone", 64),
        ]));
        let mut perception = Perception::new(Config::default());
        let mut rng = Rng::new(0x51A2_0000 + capture_index as u64);
        let mut counts = Counts::default();
        let mut capture_frontier = 0_usize;
        let mut capture_other = 0_usize;

        // Let the sweep look around from the start position before any plan.
        let mut capture_rays = 0_u64;
        for _ in 0..SWEEP_TICKS {
            perception.begin_tick(&world);
            capture_rays += u64::from(perception.sweep(&world).rays);
        }
        sweep_rays += capture_rays;
        println!(
            "{name}: sweep {SWEEP_TICKS} ticks, {capture_rays} rays, {} known cells, {} sections",
            perception.memory_len(),
            perception.known_sections()
        );

        for _ in 0..50 {
            let Some(goal) = random_goal(&world, start, &mut rng) else {
                continue;
            };
            let goal_known = perception.remembered(goal).is_some();
            // The planner spends no ray; a tick boundary only refreshes ages.
            perception.begin_tick(&world);
            let mut source = PerceptionBlocks {
                perception: &mut perception,
                world: &world,
            };
            let plan = planner.plan(&mut source, &state, start, Goal::Block(goal));
            times.push(plan.elapsed.as_secs_f64() * 1000.);
            counts.add(plan.status);
            total.add(plan.status);
            if matches!(plan.status, PlanStatus::Partial { .. }) {
                if goal_known {
                    capture_other += 1;
                    partial_other += 1;
                } else {
                    capture_frontier += 1;
                    partial_frontier += 1;
                }
            }

            if !plan.steps.is_empty() {
                validate_path(&world, &plan)
                    .unwrap_or_else(|error| panic!("{name} goal {goal:?}: {error}"));
                valid_paths += 1;
            }

            // Follow readable paths: no mining, placing, bridging or pillaring.
            if followers_attempted < 20
                && plan.status == PlanStatus::Success
                && plan.steps.iter().all(|step| !step.kind.changes_world())
            {
                followers_attempted += 1;
                followers_from_perception += 1;
                match drive(&world, &plan, goal) {
                    Ok(ticks) => {
                        followers_reached += 1;
                        follower_ticks.push(ticks);
                    }
                    Err(error) => {
                        panic!("{name} follower to {goal:?} failed: {error}");
                    }
                }
            }
        }
        println!(
            "{name}: {} goals, success {}, partial {}, timeout {}, no_path {}; \
             partials: {capture_frontier} true frontier, {capture_other} other",
            counts.total(),
            counts.success,
            counts.partial,
            counts.timeout,
            counts.no_path
        );

        // Fill the follower sample with full-knowledge, no-world-change paths
        // if perception did not yield 20 readable ones.
        let mut attempts = 0;
        while followers_attempted < 20 && attempts < 200 {
            attempts += 1;
            let Some(goal) = random_goal(&world, start, &mut rng) else {
                continue;
            };
            let mut planner = Planner::with_budget(Budget::new(
                Duration::from_millis(100),
                50_000,
            ));
            planner.set_max_drop(3);
            planner.set_allow_world_changes(false);
            let mut source = DirectBlocks { world: &world };
            let plan = planner.plan(&mut source, &state, start, Goal::Block(goal));
            if plan.status != PlanStatus::Success {
                continue;
            }
            validate_path(&world, &plan)
                .unwrap_or_else(|error| panic!("{name} direct goal {goal:?}: {error}"));
            followers_attempted += 1;
            match drive(&world, &plan, goal) {
                Ok(ticks) => {
                    followers_reached += 1;
                    follower_ticks.push(ticks);
                }
                Err(error) => panic!("{name} direct follower to {goal:?} failed: {error}"),
            }
        }
    }

    if !decoded_any {
        eprintln!("SKIP real terrain: no corpus capture is present");
        return;
    }

    times.sort_by(f64::total_cmp);
    println!(
        "planning: {} goals, success {}, partial {}, timeout {}, no_path {}; \
         partials: {partial_frontier} true frontier, {partial_other} other; \
         p50 {:.3} ms, p99 {:.3} ms",
        total.total(),
        total.success,
        total.partial,
        total.timeout,
        total.no_path,
        percentile(&times, 0.50),
        percentile(&times, 0.99)
    );
    println!(
        "sweep: K = {SWEEP_TICKS} ticks per capture, {sweep_rays} rays in total, \
         5 seconds at 20 ticks/s"
    );
    println!(
        "paths: {valid_paths} non-empty, all valid; follower: {followers_reached}/{followers_attempted} reached ({followers_from_perception} from the perception run), ticks min {:?} max {:?}",
        follower_ticks.iter().min(),
        follower_ticks.iter().max()
    );

    assert!(valid_paths > 0, "no path was produced at all");
    assert!(total.success > 0, "no goal succeeded");
    assert!(
        partial_frontier > partial_other,
        "most partials must be true frontiers (goal unseen), got \
         {partial_frontier} frontier vs {partial_other} other"
    );
    assert_eq!(
        followers_reached, followers_attempted,
        "every driven path must reach its goal"
    );
    assert!(
        followers_attempted >= 20,
        "wanted 20 follower paths, got {followers_attempted}"
    );
}
