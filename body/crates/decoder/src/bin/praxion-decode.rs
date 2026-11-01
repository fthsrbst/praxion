//! `praxion-decode <file.prxcap> [--follow-conn N] [--block X Y Z]...
//! [--summary-json out.json]`
//!
//! Decode a capture, apply the followed connection to a [`World`], and print
//! packet counts, desyncs, the final player state, the queried blocks and the
//! decode rate. Exit code 0 when nothing desynchronised and no packet failed,
//! 1 when something did, 2 on bad usage or an unreadable file.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::process::ExitCode;
use std::time::Instant;

use praxion_decoder::{Decoder, Dir, Event, Packet, State};
use praxion_tee::Reader;
use praxion_world::{BlockPos, ItemStack, World};

const USAGE: &str = "usage: praxion-decode <file.prxcap> [--follow-conn N] \
                     [--block X Y Z]... [--summary-json out.json]";

struct Args {
    capture: String,
    follow_conn: Option<u32>,
    blocks: Vec<BlockPos>,
    summary_json: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut capture = None;
    let mut follow_conn = None;
    let mut blocks = Vec::new();
    let mut summary_json = None;
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--follow-conn" => {
                let value = args.next().ok_or("--follow-conn needs a number")?;
                follow_conn = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--follow-conn: '{value}' is not a u32"))?,
                );
            }
            "--block" => {
                let mut coordinates = [0i32; 3];
                for slot in &mut coordinates {
                    let value = args.next().ok_or("--block needs three integers (X Y Z)")?;
                    *slot = value
                        .parse()
                        .map_err(|_| format!("--block: '{value}' is not an i32"))?;
                }
                blocks.push(BlockPos::new(coordinates[0], coordinates[1], coordinates[2]));
            }
            "--summary-json" => {
                summary_json = Some(args.next().ok_or("--summary-json needs a path")?.to_owned());
            }
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'\n{USAGE}"));
            }
            other => {
                if capture.is_some() {
                    return Err(format!("more than one capture file given\n{USAGE}"));
                }
                capture = Some(other.to_owned());
            }
        }
    }

    Ok(Args {
        capture: capture.ok_or_else(|| USAGE.to_owned())?,
        follow_conn,
        blocks,
        summary_json,
    })
}

#[derive(Default)]
struct Run {
    packets: u64,
    by_group: BTreeMap<(State, Dir), u64>,
    desyncs: Vec<String>,
    failures: u64,
    decode_ns: u128,
    version: u8,
    records: u64,
}

fn apply(world: &mut World, explicit_follow: bool, followed: &mut Option<u32>, event: &Event) {
    match event {
        Event::Opened { conn_id, .. } => world.opened(*conn_id),
        Event::Closed { conn_id, .. } => world.closed(*conn_id),
        Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
        Event::Decoded(decoded) => match &decoded.packet {
            Packet::ConfigurationClientbound(packet) => {
                world.apply_clientbound_config(decoded.conn_id, packet);
            }
            Packet::PlayClientbound(packet) => {
                if !explicit_follow
                    && decoded.packet.name() == "login"
                    && *followed != Some(decoded.conn_id)
                {
                    world.follow(decoded.conn_id);
                    *followed = Some(decoded.conn_id);
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

fn percentile(mut samples: Vec<u64>, fraction: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    let rank = (fraction * samples.len() as f64).ceil() as usize;
    Some(samples[rank.clamp(1, samples.len()) - 1])
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_option(value: Option<String>) -> String {
    value.unwrap_or_else(|| "null".to_owned())
}

/// Item counts over the populated inventory slots, keyed by `minecraft:` id.
///
/// Empty slots are skipped; every stack of the same item adds its count, so
/// the result is what an oracle compares with `data get entity ... Inventory`.
fn inventory_counts(inventory: &[ItemStack]) -> BTreeMap<String, i64> {
    let mut counts = BTreeMap::new();
    for stack in inventory {
        if stack.is_present() {
            *counts.entry(stack.kind().to_str().to_owned()).or_insert(0) +=
                i64::from(stack.count());
        }
    }
    counts
}

/// Entity counts by `minecraft:` type id, over the entities the followed
/// connection still tracks.
fn entity_type_counts(world: &World) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for entity in world.entities() {
        *counts.entry(entity.kind().to_str().to_owned()).or_insert(0) += 1;
    }
    counts
}

/// A JSON object of string keys to numeric counts, sorted by key.
fn json_counts<T: std::fmt::Display>(counts: &BTreeMap<String, T>) -> String {
    let entries: Vec<String> = counts
        .iter()
        .map(|(key, count)| format!("{}:{count}", json_string(key)))
        .collect();
    format!("{{{}}}", entries.join(","))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    let file = match File::open(&args.capture) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args.capture);
            return ExitCode::from(2);
        }
    };
    let reader = match Reader::new(BufReader::new(file)) {
        Ok(reader) => reader,
        Err(e) => {
            eprintln!("{}: {e}", args.capture);
            return ExitCode::from(2);
        }
    };

    let version = reader.version();
    let mut decoder = Decoder::with_timings();
    // A capture is replayed offline, so a wide reorder window is free. The
    // live body keeps the small default window instead.
    decoder.set_reorder_window(60_000_000_000);
    let mut world = World::new();
    let explicit_follow = args.follow_conn.is_some();
    let mut followed = args.follow_conn;
    if let Some(conn_id) = followed {
        world.follow(conn_id);
    }
    let mut run = Run { version, ..Run::default() };

    for record in reader {
        let record = match record {
            Ok(record) => record,
            Err(e) => {
                eprintln!("{}: {e}", args.capture);
                return ExitCode::from(2);
            }
        };
        run.records += 1;
        let started = Instant::now();
        let events = decoder.feed(&record);
        run.decode_ns += started.elapsed().as_nanos();
        for event in &events {
            if let Event::Decoded(decoded) = event {
                run.packets += 1;
                *run.by_group.entry((decoded.state, decoded.dir)).or_insert(0) += 1;
            }
            if let Event::Desync(desync) = event {
                run.desyncs.push(format!(
                    "connection {} at {}: {}",
                    desync.conn_id, desync.ts_ns, desync.reason
                ));
            }
            apply(&mut world, explicit_follow, &mut followed, event);
        }
    }
    for event in decoder.finish() {
        if let Event::Decoded(decoded) = &event {
            run.packets += 1;
            *run.by_group.entry((decoded.state, decoded.dir)).or_insert(0) += 1;
        }
        if let Event::Desync(desync) = &event {
            run.desyncs.push(format!(
                "connection {} at {}: {}",
                desync.conn_id, desync.ts_ns, desync.reason
            ));
        }
        apply(&mut world, explicit_follow, &mut followed, &event);
    }

    for stats in decoder.stats().values() {
        run.failures += stats.decode_failures.values().sum::<u64>();
    }
    let timings = decoder.decode_timings().unwrap_or(&[]);
    let p99 = percentile(timings.to_vec(), 0.99);
    let p50 = percentile(timings.to_vec(), 0.50);
    let packets_per_s = if run.decode_ns > 0 {
        run.packets as f64 * 1e9 / run.decode_ns as f64
    } else {
        0.0
    };

    println!("capture: {} (tee format v{})", args.capture, run.version);
    println!("records: {}  packets: {}", run.records, run.packets);
    println!("packets by state/direction:");
    for ((state, dir), count) in &run.by_group {
        println!("  {state:<14} {dir:<12} {count}");
    }
    println!("desyncs: {}", run.desyncs.len());
    for desync in &run.desyncs {
        println!("  {desync}");
    }
    println!("decode failures: {}", run.failures);

    println!(
        "world: status {:?}, followed connection {}, {} chunks",
        world.status(),
        world.followed().map_or_else(|| "-".to_owned(), |id| id.to_string()),
        world.chunk_count()
    );
    if let Some(dimension) = world.dimension() {
        println!(
            "dimension: {} ({}, height {}, min y {}, from {:?})",
            dimension.dimension(),
            dimension.dimension_type(),
            dimension.height(),
            dimension.min_y(),
            dimension.source()
        );
    }
    println!(
        "player: entity {:?}, position {:?}, rotation ({:.2}, {:.2}), game mode {:?}",
        world.own_entity_id(),
        world.player_position(),
        world.player_rotation().0,
        world.player_rotation().1,
        world.game_mode()
    );
    match world.health() {
        Some(health) => println!(
            "health: {:.2}, food {}, saturation {:.2}",
            health.health(),
            health.food(),
            health.saturation()
        ),
        None => println!("health: unknown"),
    }
    match world.experience() {
        Some(experience) => println!(
            "experience: level {}, progress {:.3}, total {}",
            experience.level(),
            experience.progress(),
            experience.total()
        ),
        None => println!("experience: unknown"),
    }
    println!(
        "hotbar slot: {:?}, inventory slots: {}, carried: {:?}",
        world.selected_hotbar_slot(),
        world.inventory().len(),
        world.carried_item()
    );
    println!("entities: {}", world.entities().count());
    println!(
        "time: game {}, day {}, tick {}, weather raining={} rain={} thunder={}",
        world.game_time(),
        world.day_time(),
        world.tick(),
        world.weather().raining(),
        world.weather().rain_level(),
        world.weather().thunder_level()
    );
    println!(
        "chunk batches: {} started, {} finished, last size {:?}; last block ack {:?}",
        world.chunk_batches_started(),
        world.chunk_batches_finished(),
        world.last_chunk_batch_size(),
        world.last_block_acked()
    );

    let mut block_lines = Vec::new();
    for pos in &args.blocks {
        let description = world.block_description(*pos);
        match &description {
            Some(description) => {
                println!("block {:?}: {}", pos, description.display());
                block_lines.push(format!(
                    "{{\"pos\":[{},{},{}],\"block\":{}}}",
                    pos.x,
                    pos.y,
                    pos.z,
                    json_string(&description.display())
                ));
            }
            None => {
                println!("block {:?}: unavailable", pos);
                block_lines.push(format!(
                    "{{\"pos\":[{},{},{}],\"block\":null}}",
                    pos.x, pos.y, pos.z
                ));
            }
        }
    }

    println!(
        "performance: {} packets in {:.3} ms, {:.0} packets/s; per-packet decode p50 {:?} us, p99 {:?} us",
        run.packets,
        run.decode_ns as f64 / 1e6,
        packets_per_s,
        p50.map(|ns| ns as f64 / 1000.0),
        p99.map(|ns| ns as f64 / 1000.0)
    );

    if let Some(path) = &args.summary_json {
        let groups: Vec<String> = run
            .by_group
            .iter()
            .map(|((state, dir), count)| {
                format!(
                    "{{\"state\":{},\"dir\":{},\"count\":{count}}}",
                    json_string(state.as_str()),
                    json_string(dir.as_str())
                )
            })
            .collect();
        let desyncs: Vec<String> = run.desyncs.iter().map(|reason| json_string(reason)).collect();
        let health = world.health().map_or_else(
            || "null".to_owned(),
            |health| {
                format!(
                    "{{\"health\":{},\"food\":{},\"saturation\":{}}}",
                    health.health(),
                    health.food(),
                    health.saturation()
                )
            },
        );
        let experience = world.experience().map_or_else(
            || "null".to_owned(),
            |experience| {
                format!(
                    "{{\"level\":{},\"progress\":{},\"total\":{}}}",
                    experience.level(),
                    experience.progress(),
                    experience.total()
                )
            },
        );
        let position = world.player_position().map_or_else(
            || "null".to_owned(),
            |pos| format!("[{},{},{}]", pos.x, pos.y, pos.z),
        );
        let dimension = world.dimension().map_or_else(
            || "null".to_owned(),
            |dimension| {
                format!(
                    "{{\"dimension\":{},\"dimension_type\":{},\"height\":{},\"min_y\":{},\"source\":\"{:?}\"}}",
                    json_string(dimension.dimension()),
                    json_string(dimension.dimension_type()),
                    dimension.height(),
                    dimension.min_y(),
                    dimension.source()
                )
            },
        );
        let status = match world.status().reason() {
            None => "{\"valid\":true}".to_owned(),
            Some(reason) => format!("{{\"valid\":false,\"reason\":{}}}", json_string(reason)),
        };
        let inventory = inventory_counts(world.inventory());
        let entity_types = entity_type_counts(&world);
        let summary = format!(
            "{{\"capture\":{},\"tee_version\":{},\"records\":{},\"packets\":{},\"groups\":[{}],\
             \"desyncs\":[{}],\"decode_failures\":{},\
             \"inventory\":{},\"entity_types\":{},\
             \"world\":{{\"status\":{status},\"dimension\":{dimension},\"followed_connection\":{},\
             \"chunks\":{},\"entities\":{},\"game_time\":{},\"day_time\":{},\"tick\":{}}},\
             \"player\":{{\"entity_id\":{},\"position\":{position},\"yaw\":{},\"pitch\":{},\
             \"health\":{health},\"experience\":{experience},\"hotbar_slot\":{},\
             \"inventory_slots\":{},\"carried_item\":{}}},\
             \"blocks\":[{}],\"performance\":{{\"decode_ms\":{:.3},\"packets_per_s\":{:.1},\
             \"p50_us\":{},\"p99_us\":{}}}}}\n",
            json_string(&args.capture),
            run.version,
            run.records,
            run.packets,
            groups.join(","),
            desyncs.join(","),
            run.failures,
            json_counts(&inventory),
            json_counts(&entity_types),
            json_option(world.followed().map(|id| id.to_string())),
            world.chunk_count(),
            world.entities().count(),
            world.game_time(),
            world.day_time(),
            world.tick(),
            json_option(world.own_entity_id().map(|id| id.to_string())),
            world.player_rotation().0,
            world.player_rotation().1,
            json_option(world.selected_hotbar_slot().map(|slot| slot.to_string())),
            world.inventory().len(),
            json_string(&format!("{:?}", world.carried_item())),
            block_lines.join(","),
            run.decode_ns as f64 / 1e6,
            packets_per_s,
            json_option(p50.map(|ns| format!("{:.3}", ns as f64 / 1000.0))),
            json_option(p99.map(|ns| format!("{:.3}", ns as f64 / 1000.0)))
        );
        if let Err(e) = std::fs::write(path, summary) {
            eprintln!("cannot write {path}: {e}");
            return ExitCode::from(2);
        }
    }

    if run.desyncs.is_empty() && run.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea_inventory::ItemStack;
    use azalea_registry::builtin::ItemKind;

    #[test]
    fn inventory_counts_sum_populated_slots_by_item() {
        let inventory = [
            ItemStack::new(ItemKind::OakLog, 5),
            ItemStack::Empty,
            ItemStack::new(ItemKind::OakLog, 3),
            ItemStack::new(ItemKind::StonePickaxe, 1),
            ItemStack::new(ItemKind::Air, 7), // air is an empty stack
        ];
        let counts = inventory_counts(&inventory);
        assert_eq!(counts.get("minecraft:oak_log"), Some(&8));
        assert_eq!(counts.get("minecraft:stone_pickaxe"), Some(&1));
        assert_eq!(counts.len(), 2);
    }

    #[test]
    fn json_counts_is_empty_for_no_counts() {
        let counts: BTreeMap<String, i64> = BTreeMap::new();
        assert_eq!(json_counts(&counts), "{}");
    }

    #[test]
    fn json_counts_sorts_keys_and_escapes_them() {
        let counts = BTreeMap::from([
            ("minecraft:stone".to_owned(), 2u64),
            ("custom:\"quoted\"".to_owned(), 1u64),
        ]);
        assert_eq!(
            json_counts(&counts),
            "{\"custom:\\\"quoted\\\"\":1,\"minecraft:stone\":2}"
        );
    }
}
