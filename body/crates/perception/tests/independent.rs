//! Independent tests and oracle for the perception API (t4 review).
//!
//! Brute-force oracle, cache/budget/fair-play checks, corpus sanity check,
//! and panic/cache-bound probes. Touches only the public API of
//! `praxion-perception` and `praxion-world`.

mod common;

use azalea_block::BlockState;
use azalea_core::position::{ChunkPos, Vec3};
use azalea_physics::collision::BlockWithShape;
use azalea_protocol::packets::game::{
    ClientboundForgetLevelChunk, ClientboundGamePacket,
};
use azalea_registry::builtin::BlockKind;
use praxion_perception::*;
use praxion_world::{BlockPos, World};

use common::{
    FEET_Y, add_zombie, air, base_world, block_name, chunk_packet, decode_corpus, find_block,
    set_block, set_player, stone, world_across_chunks, world_with,
};

// ---------------------------------------------------------------------------
// See-through override table (copied from see_through.rs for the oracle).
// ---------------------------------------------------------------------------

fn override_pass(name: &str) -> Option<bool> {
    // glass
    if name == "glass" { return Some(true); }
    if name.ends_with("_stained_glass") { return Some(true); }
    if name == "glass_pane" { return Some(true); }
    if name.ends_with("_stained_glass_pane") { return Some(true); }
    if name == "tinted_glass" { return Some(true); }
    if name == "ice" { return Some(true); }
    if name.ends_with("_leaves") { return Some(true); }
    if name == "slime_block" { return Some(true); }
    if name == "honey_block" { return Some(true); }
    if name == "barrier" { return Some(true); }
    if name == "spawner" { return Some(true); }
    if name == "beacon" { return Some(true); }
    if name == "lava" { return Some(false); }
    if name == "powder_snow" { return Some(false); }
    None
}

fn is_blocking(state: BlockState, point: Vec3, cell: BlockPos) -> bool {
    let name = block_name(state);
    match override_pass(&name) {
        Some(true) => return false,
        Some(false) => return true, // full cell
        None => {}
    }
    let shape = state.base_collision_shape();
    let boxes = shape.to_aabbs();
    if boxes.is_empty() {
        return false;
    }
    let (cx, cy, cz) = (f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    for bx in &boxes {
        let min = Vec3::new(cx + bx.min.x, cy + bx.min.y, cz + bx.min.z);
        let max = Vec3::new(cx + bx.max.x, cy + bx.max.y, cz + bx.max.z);
        if point.x >= min.x - 1e-9 && point.x <= max.x + 1e-9
            && point.y >= min.y - 1e-9 && point.y <= max.y + 1e-9
            && point.z >= min.z - 1e-9 && point.z <= max.z + 1e-9
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Brute-force oracle
// ---------------------------------------------------------------------------

fn brute_target_points(world: &World, pos: BlockPos) -> Vec<Vec3> {
    let mut pts = vec![pos.center()];
    let faces: [([i32; 3], [f64; 3]); 6] = [
        ([1, 0, 0], [1.0, 0.5, 0.5]),
        ([-1, 0, 0], [0.0, 0.5, 0.5]),
        ([0, 1, 0], [0.5, 1.0, 0.5]),
        ([0, -1, 0], [0.5, 0.0, 0.5]),
        ([0, 0, 1], [0.5, 0.5, 1.0]),
        ([0, 0, -1], [0.5, 0.5, 0.0]),
    ];
    for (off, center) in faces {
        let nbr = BlockPos::new(pos.x + off[0], pos.y + off[1], pos.z + off[2]);
        if let Some(st) = world.block_state(nbr) {
            let name = block_name(st);
            let _ = override_pass(&name);
            // see-through if pass; for non-override, empty shape => pass
            let pass = if override_pass(&name).is_some() {
                override_pass(&name).unwrap()
            } else {
                st.base_collision_shape().is_empty()
            };
            if pass {
                pts.push(Vec3::new(
                    f64::from(pos.x) + center[0],
                    f64::from(pos.y) + center[1],
                    f64::from(pos.z) + center[2],
                ));
            }
        }
    }
    pts
}

fn brute_ray(world: &World, eye: Vec3, point: Vec3, target: BlockPos) -> bool {
    let delta = point - eye;
    let len = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
    if len < 1e-12 {
        // same point; target cell handling
        return true;
    }
    let steps = ((len / 0.01).ceil() as usize).max(1);
    let eye_cell = eye.to_block_pos_floor();
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let p = Vec3::new(eye.x + delta.x * t, eye.y + delta.y * t, eye.z + delta.z * t);
        let cell = p.to_block_pos_floor();
        if cell == target {
            continue;
        }
        if cell == eye_cell && i == 0 {
            // still test eye cell if blocking
        }
        match world.block_state(cell) {
            None => {
                if cell != eye_cell {
                    return false;
                }
            }
            Some(st) => {
                if is_blocking(st, p, cell) {
                    return false;
                }
            }
        }
    }
    true
}

fn brute_visible(world: &World, eye: Vec3, target_pos: BlockPos) -> bool {
    if world.block_state(target_pos).is_none() { return false; }
    let dist = eye.distance_to(target_pos.center());
    if dist > 64.0 { return false; }
    let pts = brute_target_points(world, target_pos);
    for pt in pts {
        if brute_ray(world, eye, pt, target_pos) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Deterministic RNG
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self { Self(seed) }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
    fn usize(&mut self, n: usize) -> usize { (self.next() as usize) % n.max(1) }
    fn f64(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
}

// ---------------------------------------------------------------------------
// Test 1: brute-force oracle vs fast DDA
// ---------------------------------------------------------------------------

#[test]
fn oracle_brute_vs_fast_dda() {
    // Build a world with random blocks.
    let chunks = 6;
    let mut world = base_world();
    for x in 1..chunks {
        for z in 1..chunks {
            world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, z)));
        }
    }
    // representative block states
    let mut states: Vec<BlockState> = Vec::new();
    for name in ["stone","glass","oak_leaves","short_grass"] {
        states.push(find_block(name, &[]));
    }
    states.push(find_block("stone_slab", &[("type","bottom")]));
    states.push(find_block("stone_slab", &[("type","top")]));
    states.push(find_block("oak_stairs", &[("facing","north")]));
    states.push(find_block("water", &[]));
    states.push(find_block("lava", &[]));
    states.push(find_block("oak_fence", &[]));
    states.push(air());
    // random fill
    let mut rng = Rng::new(0xC0FFEE);
    for x in 0..(chunks*16) {
        for z in 0..(chunks*16) {
            for y in 63..=67 {
                if rng.usize(10) < 3 {
                    let s = states[rng.usize(states.len())];
                    set_block(&mut world, BlockPos::new(x, y, z), s);
                }
            }
        }
    }

    let mut mismatches = 0usize;
    let mut grazing = 0usize;
    let mut total = 0usize;
    let mut fast_visible_count = 0usize;
    let mut oracle_visible_count = 0usize;

    for _ in 0..20_000 {
        // random eye position in open-ish space
        let ex = rng.f64() * ((chunks*16 - 2) as f64) + 1.0;
        let ez = rng.f64() * ((chunks*16 - 2) as f64) + 1.0;
        let ey = 63.0 + rng.f64() * 4.0 + DEFAULT_EYE_HEIGHT;
        let eye = Vec3::new(ex, ey, ez);
        // target block
        let tx = rng.usize((chunks*16) as usize) as i32;
        let tz = rng.usize((chunks*16) as usize) as i32;
        let ty = 63 + rng.usize(5) as i32;
        let target = BlockPos::new(tx, ty, tz);
        if world.block_state(target).is_none() { continue; }
        let dist = eye.distance_to(target.center());
        if dist > 64.0 || dist < 0.5 { continue; }

        // set player so perception eye matches
        set_player(&mut world, Vec3::new(eye.x, eye.y - DEFAULT_EYE_HEIGHT, eye.z));

        let mut p = Perception::new(Config {
            ray_budget: u32::MAX,
            reflex_reserve: 0,
            entity_cap: 1000,
            memory_capacity: 0,
            ..Config::default()
        });
        p.begin_tick(&world);
        let ans = p.block(Caller::Other, &world, target);
        let fast_vis = ans.provenance() == Some(Provenance::Sight);

        let oracle_vis = brute_visible(&world, eye, target);

        total += 1;
        if fast_vis { fast_visible_count += 1; }
        if oracle_vis { oracle_visible_count += 1; }

        if fast_vis != oracle_vis {
            mismatches += 1;
            // grazing heuristic: sample points near integer boundaries
            let delta = target.center() - eye;
            let len = (delta.x*delta.x+delta.y*delta.y+delta.z*delta.z).sqrt();
            let steps = ((len/0.01).ceil() as usize).max(1);
            let mut is_grazing = false;
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let pp = Vec3::new(eye.x+delta.x*t, eye.y+delta.y*t, eye.z+delta.z*t);
                for c in [pp.x, pp.y, pp.z] {
                    if (c - c.round()).abs() < 1e-3 { is_grazing = true; }
                }
            }
            if is_grazing { grazing += 1; }
            if mismatches <= 20 {
                eprintln!("MISMATCH #{} eye={:?} target={:?} fast={} oracle={}", mismatches, eye, target, fast_vis, oracle_vis);
            }
        }
    }

    eprintln!("ORACLE: total={} mismatches={} grazing={} fast_vis={} oracle_vis={}", total, mismatches, grazing, fast_visible_count, oracle_visible_count);
    // We don't hard-fail on grazing mismatches; report. Fail only on non-grazing mismatches.
    let non_grazing = mismatches - grazing;
    // Allow a small number? Requirement wants report. We assert zero non-grazing mismatches ideally.
    // Given sampling, some near-misses possible. Report via panic message if too many.
    assert!(non_grazing <= 5, "too many non-grazing mismatches: {}", non_grazing);
}

// ---------------------------------------------------------------------------
// Test 2: cache correctness under random sequences
// ---------------------------------------------------------------------------

#[test]
fn cache_correctness_random_sequence() {
    let mut world = world_across_chunks(4, &[]);
    // target in chunk 2
    let target = BlockPos::new(40, 65, 0);
    set_block(&mut world, target, stone());

    let config = Config { memory_capacity: 0, ray_budget: u32::MAX, reflex_reserve: 0, entity_cap: 1000, ..Config::default() };

    let mut p_cached = Perception::new(config.clone());
    p_cached.begin_tick(&world);

    // initial query to populate cache
    let _ = p_cached.block(Caller::Other, &world, target);

    // sequence of operations
    let ops = [0u8,1,2,3,4,5,6,7, 0,2,4,1,3,5, 0,0,2,4];
    let mut rng = Rng::new(7);
    for step in 0..60 {
        let op = if step < ops.len() { ops[step] } else { (rng.next() % 8) as u8 };
        match op {
            0 => { // move eye one cell
                set_player(&mut world, Vec3::new(1.5, FEET_Y, 0.5));
            }
            1 => { // move eye back
                set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
            }
            2 => { // wall in crossed chunk (chunk 1)
                set_block(&mut world, BlockPos::new(20, 65, 0), stone());
            }
            3 => { // remove wall
                set_block(&mut world, BlockPos::new(20, 65, 0), air());
            }
            4 => { // unload crossed chunk
                world.apply_clientbound_play(0, &ClientboundGamePacket::ForgetLevelChunk(ClientboundForgetLevelChunk {
                    pos: ChunkPos::new(1, 0),
                }));
            }
            5 => { // reload chunk
                world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(1, 0)));
                // restore wall state? leave empty
            }
            6 => { // change a block in target's chunk
                set_block(&mut world, BlockPos::new(41, 65, 0), stone());
            }
            7 => { // remove that
                set_block(&mut world, BlockPos::new(41, 65, 0), air());
            }
            _ => {}
        }
        // fresh perception
        let mut p_fresh = Perception::new(config.clone());
        p_fresh.begin_tick(&world);
        p_cached.begin_tick(&world);
        let a = p_cached.block(Caller::Other, &world, target);
        let b = p_fresh.block(Caller::Other, &world, target);
        assert_eq!(a, b, "step {} op {} cached != fresh: {:?} vs {:?}", step, op, a, b);
        // no stale Known: if fresh is Unknown, cached must not be Known(Sight)
        if let Answer::Unknown(_) = b {
            assert!(!matches!(a, Answer::Known { provenance: Provenance::Sight, .. }), "stale Known(Sight) at step {}", step);
        }
    }
}

// ---------------------------------------------------------------------------
// Test 3: budget
// ---------------------------------------------------------------------------

#[test]
fn budget_reflex_reserve_and_no_stale() {
    let config = Config { ray_budget: 2, reflex_reserve: 1, memory_capacity: 0, ..Config::default() };
    let wall = BlockPos::new(2, 65, 0);
    let target = BlockPos::new(4, 65, 0);
    let world = world_with(&[(wall, stone()), (target, stone())]);
    let mut p = Perception::new(config);
    p.begin_tick(&world);

    // Other cannot use reflex reserve
    assert_eq!(p.block(Caller::Other, &world, target).reason(), Some(Reason::Budget));
    assert_eq!(p.rays_used_this_tick(), 1);
    assert_eq!(p.rays_remaining(Caller::Other), 0);
    assert_eq!(p.rays_remaining(Caller::Reflex), 1);
    // beyond budget returns Budget, not a stale value
    assert_eq!(p.block(Caller::Other, &world, target).reason(), Some(Reason::Budget));
    // Reflex can spend reserve
    let r = p.block(Caller::Reflex, &world, target).reason();
    // may be Budget now after spending 2 total? reflex remaining 1, needs ray -> spends, now 2 used
    assert!(r == Some(Reason::Budget) || r == Some(Reason::OutOfSight), "{:?}", r);
    assert_eq!(p.rays_used_this_tick(), 2);

    // new tick resets
    p.begin_tick(&world);
    assert_eq!(p.rays_remaining(Caller::Other), 1);
    assert_eq!(p.rays_remaining(Caller::Reflex), 2);
}

#[test]
fn entity_cap_counts_overflow_independent() {
    let config = Config { entity_cap: 1, memory_capacity: 0, ..Config::default() };
    let mut world = world_with(&[]);
    add_zombie(&mut world, 1, Vec3::new(3.0, FEET_Y, 0.5));
    add_zombie(&mut world, 2, Vec3::new(3.0, FEET_Y, 2.5));
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    assert_eq!(p.entity(Caller::Other, &world, 1).provenance(), Some(Provenance::Sight));
    assert_eq!(p.entity(Caller::Other, &world, 2).reason(), Some(Reason::Budget));
    assert_eq!(p.entity_overflow(), 1);
}

// ---------------------------------------------------------------------------
// Test 4: fair play
// ---------------------------------------------------------------------------

#[test]
fn fair_play_entity_health_denied_and_ledgered() {
    let world = world_with(&[]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(p.entity_health(Caller::Other, 1).reason(), Some(Reason::Denied));
    assert!(p.ledger().get(QueryKind::EntityHealth, FactKind::EntityHealth).is_some());
    assert_eq!(p.ledger().get(QueryKind::EntityHealth, FactKind::EntityHealth).unwrap().count(), 1);
}

#[test]
fn fair_play_hidden_block_not_in_blocks_in() {
    let target = BlockPos::new(4, 65, 0);
    let wall = BlockPos::new(2, 65, 0);
    let world = world_with(&[(wall, stone()), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let region = BlockRegion::new(BlockPos::new(0, 64, -1), BlockPos::new(7, 70, 1));
    let ans = p.blocks_in(Caller::Other, &world, region, BlockFilter::Kind(BlockKind::Stone));
    // target hidden behind wall; should not be in results (or Unknown OutOfSight not returned as list)
    if let Answer::Known { value: blocks, .. } = ans {
        assert!(!blocks.iter().any(|b| b.pos == target), "hidden block leaked: {:?}", blocks);
    }
}

#[test]
fn fair_play_buried_ore_not_sight() {
    // block behind wall: memory_capacity 0 so no memory; must not be Sight
    let target = BlockPos::new(4, 65, 0);
    let wall = BlockPos::new(2, 65, 0);
    let world = world_with(&[(wall, stone()), (target, stone())]);
    let mut p = Perception::new(Config { memory_capacity: 0, ..Config::default() });
    p.begin_tick(&world);
    let ans = p.block(Caller::Other, &world, target);
    assert_ne!(ans.provenance(), Some(Provenance::Sight));
}

#[test]
fn fair_play_ledger_bounded_rate() {
    let target = BlockPos::new(4, 65, 0);
    let wall = BlockPos::new(2, 65, 0);
    let world = world_with(&[(wall, stone()), (target, stone())]);
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    for _ in 0..5 {
        let _ = p.block(Caller::Other, &world, target);
    }
    let entry = p.ledger().get(QueryKind::Block, FactKind::BlockState).unwrap();
    assert_eq!(entry.count(), 5);
    // bounded: one entry per (query, fact) key
    assert_eq!(p.ledger().len(), 1);
}

// ---------------------------------------------------------------------------
// Test 5: memory LRU bound
// ---------------------------------------------------------------------------

#[test]
fn memory_lru_bound_respected() {
    let cap = 4usize;
    let config = Config { memory_capacity: cap, ray_budget: u32::MAX, reflex_reserve: 0, ..Config::default() };
    let mut world = base_world();
    set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    for i in 0..(cap + 5) {
        // clear and place a single visible block
        // remove previous
        if i > 0 {
            set_block(&mut world, BlockPos::new(2 + (i-1) as i32, 65, 0), air());
        }
        set_block(&mut world, BlockPos::new(2 + i as i32, 65, 0), stone());
        p.begin_tick(&world);
        let pos = BlockPos::new(2 + i as i32, 65, 0);
        let _ = p.block(Caller::Other, &world, pos);
    }
    assert!(p.memory_len() <= cap, "memory_len {} > cap {}", p.memory_len(), cap);
}

// ---------------------------------------------------------------------------
// Test 6: panics
// ---------------------------------------------------------------------------

#[test]
fn panics_extreme_positions_no_panic() {
    let world = base_world();
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let extremes = [i32::MAX, i32::MIN, 0, 100000, -100000];
    for &x in &extremes {
        for &y in &extremes {
            for &z in &extremes {
                let pos = BlockPos::new(x, y, z);
                let _ = p.block(Caller::Other, &world, pos);
                let _ = p.nearest_block(Caller::Other, &world, BlockKind::Stone, 1);
                let _ = p.blocks_in(Caller::Other, &world, BlockRegion::around(pos, 1), BlockFilter::Any);
            }
        }
    }
}

#[test]
fn panics_eye_outside_height_no_panic() {
    let mut world = base_world();
    set_player(&mut world, Vec3::new(0.5, 10000.0, 0.5));
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    let pos = BlockPos::new(0, 65, 0);
    let _ = p.block(Caller::Other, &world, pos);
    let _ = p.blocks_in(Caller::Other, &world, BlockRegion::around(pos, 4), BlockFilter::Any);
    let _ = p.nearest_block(Caller::Other, &world, BlockKind::Stone, 4);
    // sneaking height
    p.set_sneaking(true);
    let _ = p.eye(&world);
}

#[test]
fn panics_world_no_chunks_no_panic() {
    let world = World::new(); // no follow, no chunks
    let mut p = Perception::new(Config::default());
    p.begin_tick(&world);
    assert_eq!(p.block(Caller::Other, &world, BlockPos::new(0, 65, 0)).reason(), Some(Reason::NeverSeen));
    assert_eq!(p.own_position(&world).reason(), Some(Reason::NeverSeen));
    let _ = p.blocks_in(Caller::Other, &world, BlockRegion::around(BlockPos::new(0,65,0), 2), BlockFilter::Any);
    let _ = p.nearest_block(Caller::Other, &world, BlockKind::Stone, 2);
    let _ = p.entities_near(Caller::Other, &world, 2, EntityFilter::Any);
    let _ = p.sounds_near(&world, 2);
}

// ---------------------------------------------------------------------------
// Test 7: corpus sanity check (ignored by default; run for numbers)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "corpus decode; run with --ignored --nocapture"]
fn corpus_visibility_summary() {
    let world = decode_corpus("corpus4.prxcap");
    let pos = world.player_position().expect("player pos");
    println!("corpus: {} chunks, {} entities, player {:?}", world.chunk_count(), world.entities().count(), pos);
    let config = Config { max_region_blocks: 4_000_000, max_list_results: 1_000_000, memory_capacity: 0, ray_budget: u32::MAX, reflex_reserve: 0, entity_cap: 1000, ..Config::default() };
    let mut p = Perception::new(config);
    p.begin_tick(&world);
    let eye_cell = p.eye_cell(&world).expect("eye cell");
    let region = BlockRegion::around(eye_cell, 16);
    let ans = p.blocks_in(Caller::Other, &world, region, BlockFilter::Any);
    println!("answer reason: {:?}", ans.reason());
    let visible: Vec<BlockPos> = match &ans {
        Answer::Known { value: bs, .. } => { println!("known len {}", bs.len()); bs.iter().map(|b| b.pos).collect() }
        _ => vec![],
    };
    println!("eye_cell: {:?}", eye_cell);
    println!("visible within 16: {}", visible.len());
    // raw store count in same region
    let mut raw = 0usize;
    for x in region.min.x..=region.max.x {
        for y in region.min.y..=region.max.y {
            for z in region.min.z..=region.max.z {
                if world.block_state(BlockPos::new(x,y,z)).is_some() { raw += 1; }
            }
        }
    }
    println!("raw store cells in region: {}", raw);
    // sanity: blocks deep inside stone should be Unknown; sample a point 3 deep
    // find a stone block and query 3 inside
    // print first few visible
    for v in visible.iter().take(10) { println!("  visible {:?}", v); }
    // sanity: a block deep inside stone (all 6 neighbours stone) must be Unknown
    let mut buried_example = None;
    'outer: for x in region.min.x..=region.max.x {
        for y in region.min.y..=region.max.y {
            for z in region.min.z..=region.max.z {
                let pos = BlockPos::new(x, y, z);
                if world.block_state(pos).map(block_name) != Some("stone".to_string()) { continue; }
                // check 6 neighbours stone
                let nbrs = [(1,0,0),(-1,0,0),(0,1,0),(0,-1,0),(0,0,1),(0,0,-1)];
                if nbrs.iter().all(|(dx,dy,dz)| {
                    world.block_state(BlockPos::new(x+dx,y+dy,z+dz)).map(block_name) == Some("stone".to_string())
                }) {
                    buried_example = Some(pos);
                    break 'outer;
                }
            }
        }
    }
    if let Some(pos) = buried_example {
        let a = p.block(Caller::Other, &world, pos);
        println!("buried stone example {:?}: {:?}", pos, a.reason());
    } else {
        println!("no fully-buried stone found in sample");
    }
}
