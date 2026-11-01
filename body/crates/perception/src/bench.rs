//! Release-only benchmark against a real capture.
//!
//! Run from `body/`:
//!
//! ```text
//! cargo test -p praxion-perception --release -- --ignored bench_real_corpus --nocapture
//! ```
//!
//! It decodes `spikes/captures/corpus4.prxcap` into a [`World`], then measures
//! a 64-block `blocks_in` query (rays cast per millisecond),
//! `nearest_block(oak_log, 32)` cold and warm, `nearest_block(stone, 32)` cold
//! and warm, and the cross-tick cache (a repeated query spends no ray).
//!
//! Each timing runs three times with a fresh [`Perception`]; the printed value
//! is the fastest of the three, because the first one in a process also pays
//! for the sight table of every block state it touches.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use azalea_registry::builtin::BlockKind;
use praxion_decoder::{Decoder, Event, Packet};
use praxion_tee::Reader;
use praxion_world::World;

use super::*;

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

fn decode_corpus(name: &str) -> World {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spikes/captures")
        .join(name);
    let file = File::open(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
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
    assert!(world.status().is_valid(), "corpus desynced: {:?}", world.status());
    world
}

/// The fastest of three runs of `query`, each with a fresh [`Perception`].
fn fastest(config: &Config, world: &World, query: impl Fn(&mut Perception) -> String) -> f64 {
    let mut best = f64::INFINITY;
    for run in 1..=3 {
        let mut perception = Perception::new(config.clone());
        perception.begin_tick(world);
        let started = Instant::now();
        let detail = query(&mut perception);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        println!("  run {run}: {detail} in {elapsed:.3} ms");
        best = best.min(elapsed);
    }
    best
}

#[test]
#[ignore = "release-only benchmark, see the module doc for the command"]
fn bench_real_corpus() {
    let world = decode_corpus("corpus4.prxcap");
    let pos = world.player_position().expect("the corpus has no player position");
    println!(
        "corpus: {} chunks, {} entities, player {:?}, dimension {:?}",
        world.chunk_count(),
        world.entities().count(),
        pos,
        world.dimension().map(|d| d.dimension()),
    );

    let config = Config {
        max_region_blocks: 4_000_000,
        max_list_results: 8_000,
        ..Config::default()
    };

    // Rays per millisecond: a 64-block cube of stone queries, stopped by the
    // 3,000-ray budget that `Caller::Other` may spend.
    println!("blocks_in(radius 64, stone):");
    let best = fastest(&config, &world, |perception| {
        let eye_cell = perception.eye_cell(&world).expect("eye");
        let region = BlockRegion::around(eye_cell, 64);
        let answer = perception.blocks_in(
            Caller::Other,
            &world,
            region,
            BlockFilter::Kind(BlockKind::Stone),
        );
        let rays = perception.rays_used_this_tick();
        format!("{:?}, {rays} rays", answer.reason())
    });
    let rays = 3_000.0;
    println!("  best: 3000 rays in {best:.3} ms = {:.0} rays/ms", rays / best);

    // The nearest-block searches: cold (first query) and warm (the same query
    // again in the same tick, which the cache answers without a ray).
    for kind in [BlockKind::OakLog, BlockKind::Stone] {
        println!("nearest_block({kind:?}, 32):");
        let best = fastest(&config, &world, |perception| {
            let cold = perception.nearest_block(Caller::Other, &world, kind, 32);
            let cold_rays = perception.rays_used_this_tick();
            let started = Instant::now();
            let warm = perception.nearest_block(Caller::Other, &world, kind, 32);
            let warm_ms = started.elapsed().as_secs_f64() * 1000.0;
            let warm_rays = perception.rays_used_this_tick() - cold_rays;
            assert_eq!(
                cold.value().map(|fact| fact.pos),
                warm.value().map(|fact| fact.pos),
                "the warm answer must match the cold one"
            );
            format!(
                "cold {:?} ({cold_rays} rays), warm {warm_ms:.3} ms ({warm_rays} rays)",
                cold.reason()
            )
        });
        println!("  best cold+warm in {best:.3} ms");
    }

    // The block cache survives a tick: the second tick answers from it and
    // spends no ray.
    println!("cache across ticks (nearest_block(stone, 16)):");
    let mut perception = Perception::new(Config::default());
    perception.begin_tick(&world);
    let first = perception.nearest_block(Caller::Reflex, &world, BlockKind::Stone, 16);
    let first_rays = perception.rays_used_this_tick();
    let mut repeated = 0;
    let mut repeated_rays = 0;
    for _ in 0..3 {
        perception.begin_tick(&world);
        let again = perception.nearest_block(Caller::Reflex, &world, BlockKind::Stone, 16);
        repeated_rays += perception.rays_used_this_tick();
        assert_eq!(
            first.value().map(|fact| fact.pos),
            again.value().map(|fact| fact.pos),
            "the cached answer must match the first one"
        );
        repeated += 1;
    }
    println!(
        "  first tick {first_rays} rays, {repeated} later ticks {repeated_rays} rays in total"
    );
}
