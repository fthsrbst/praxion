//! The shadow tick rule against a hand-built mirror.

use std::path::Path;
use std::sync::Mutex;

use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::game_type::{GameMode, OptionalGameType};
use azalea_entity::LookDirection;
use azalea_protocol::common::movements::{MoveFlags, PositionMoveRotation, RelativeMovements};
use azalea_protocol::packets::common::CommonPlayerSpawnInfo;
use azalea_protocol::packets::game::{
    ClientboundGamePacket, ClientboundLogin, ClientboundPlayerPosition, ClientboundRespawn,
    ClientboundSetHealth, ServerboundClientTickEnd, ServerboundGamePacket, ServerboundMovePlayerPos,
};
use azalea_registry::DataRegistry;
use azalea_registry::data::DimensionKind;
use azalea_registry::identifier::Identifier;
use praxion_decoder::Packet;

use super::*;
use crate::driver::Driver;
use crate::frame::{Click, MenuAction};
use crate::interact::AppliedReport;

/// What a [`Fake`] driver saw.
#[derive(Default)]
struct Calls {
    /// The tick of every frame call, in order.
    ticks: Vec<u64>,
    /// How many packets each frame call got.
    packets: Vec<usize>,
    /// `applied` calls: shadow mode must never make one.
    applied: u64,
}

/// A driver that records its calls and asks for the same held key every tick.
struct Fake {
    calls: Arc<Mutex<Calls>>,
    held: Held,
}

impl Driver for Fake {
    fn frame(&mut self, view: &TickView<'_>) -> ControlFrame {
        let mut calls = self.calls.lock().unwrap();
        calls.ticks.push(view.tick);
        calls.packets.push(view.packets.len());
        drop(calls);
        ControlFrame::new(view.tick, view.body.generation(), self.held)
    }

    fn applied(&mut self, _tick: u64, _report: &AppliedReport) {
        self.calls.lock().unwrap().applied += 1;
    }
}

fn fake(held: Held) -> (DriverHandle, Arc<Mutex<Calls>>) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let driver: DriverHandle = Arc::new(Mutex::new(Box::new(Fake {
        calls: Arc::clone(&calls),
        held,
    })));
    (driver, calls)
}

fn login() -> ClientboundLogin {
    ClientboundLogin {
        player_id: MinecraftEntityId(7),
        hardcore: false,
        levels: Vec::new(),
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: CommonPlayerSpawnInfo {
            dimension_type: DimensionKind::new_raw(0),
            dimension: Identifier::new("minecraft:overworld"),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
        },
        online_mode: false,
        enforces_secure_chat: false,
    }
}

/// A world mirror that follows connection 0 and has passed the Play login.
fn mirror() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world
}

/// The client's own movement packet: this is where the mirror's pose comes from.
fn move_to(world: &mut World, position: Vec3, on_ground: bool) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: position,
            flags: MoveFlags {
                on_ground,
                horizontal_collision: false,
            },
        }),
    );
}

/// A server teleport: it gives the mirror a position without the client saying
/// anything about its feet.
fn teleport_to(world: &mut World, position: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::PlayerPosition(ClientboundPlayerPosition {
            id: 1,
            change: PositionMoveRotation {
                pos: position,
                delta: Vec3::ZERO,
                look_direction: LookDirection::new(0.0, 0.0),
            },
            relative: RelativeMovements::default(),
        }),
    );
}

fn set_health(world: &mut World, health: f32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health,
            food: 20,
            saturation: 5.0,
        }),
    );
}

/// The server builds a new player: the player's position is cleared until the
/// new life's first movement packet.
fn respawn(world: &mut World) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: login().common,
            data_to_keep: 0,
        }),
    );
}

/// The packet that marks one client tick in 26.3.
fn client_tick(world: &mut World) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
    );
}

/// A mirror that has seen one movement packet and `ticks` client ticks.
fn mirror_at(position: Vec3, on_ground: bool, ticks: u32) -> World {
    let mut world = mirror();
    move_to(&mut world, position, on_ground);
    for _ in 0..ticks {
        client_tick(&mut world);
    }
    world
}

fn observed() -> ObservedPacket {
    ObservedPacket {
        conn_id: 0,
        packet: Packet::PlayServerbound(ServerboundGamePacket::ClientTickEnd(
            ServerboundClientTickEnd,
        )),
    }
}

/// A round that has no packet burst to hand its first frame.
fn no_packets() -> Vec<ObservedPacket> {
    Vec::new()
}

#[test]
fn one_frame_per_observed_client_tick_and_none_without_one() {
    let (driver, calls) = fake(Held::NONE);
    let stats = Arc::new(SessionStats::default());
    let mut shadow = Shadow::new(driver, Arc::clone(&stats));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 0);

    assert_eq!(shadow.observe(&world, no_packets), 0, "no client tick yet");
    assert!(calls.lock().unwrap().ticks.is_empty());

    for _ in 0..3 {
        client_tick(&mut world);
    }
    assert_eq!(
        shadow.observe(&world, || vec![observed()]),
        3,
        "a counter that advanced by 3 gives 3 frames"
    );
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.ticks, vec![0, 1, 2], "consecutive tick numbers");
        assert_eq!(
            calls.packets,
            vec![1, 0, 0],
            "only the first of the burst gets the packets"
        );
        assert_eq!(calls.applied, 0, "shadow mode never applies a frame");
    }
    assert_eq!(stats.frames.load(Ordering::Relaxed), 3);
    assert_eq!(stats.ticks_ran.load(Ordering::Relaxed), 3);
    assert_eq!(stats.shadow_wanted.load(Ordering::Relaxed), 0);
    assert_eq!(
        stats.position(),
        Some(Vec3::new(0.5, 64.0, 0.5)),
        "the port snapshot follows the observed player"
    );

    // The same mirror state is not another tick.
    assert_eq!(shadow.observe(&world, no_packets), 0);
    assert_eq!(calls.lock().unwrap().ticks.len(), 3);

    client_tick(&mut world);
    assert_eq!(shadow.observe(&world, no_packets), 1);
    assert_eq!(calls.lock().unwrap().ticks, vec![0, 1, 2, 3]);
}

#[test]
fn a_round_drives_at_most_twenty_frames_and_the_rest_stays_pending() {
    let (driver, calls) = fake(Held::NONE);
    let stats = Arc::new(SessionStats::default());
    let mut shadow = Shadow::new(driver, Arc::clone(&stats));
    let world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 25);

    assert_eq!(
        shadow.observe(&world, no_packets),
        MAX_FRAMES_PER_ROUND,
        "one round never holds the world lock across more frames"
    );
    assert_eq!(calls.lock().unwrap().ticks.len(), 20);

    // The same mirror keeps the rest pending: the loop's next round drives it
    // without waiting for a new client tick.
    assert_eq!(
        shadow.observe(&world, no_packets),
        5,
        "the backlog continues on the next round"
    );
    assert_eq!(
        calls.lock().unwrap().ticks,
        (0..25).collect::<Vec<_>>(),
        "every tick is driven exactly once"
    );
    assert_eq!(shadow.observe(&world, no_packets), 0);
    assert_eq!(stats.frames.load(Ordering::Relaxed), 25);
}

#[test]
fn a_round_with_nothing_to_drive_takes_no_packets() {
    let (driver, _calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::new(SessionStats::default()));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 0);

    let mut taken = false;
    assert_eq!(
        shadow.observe(&world, || {
            taken = true;
            vec![observed()]
        }),
        0
    );
    assert!(
        !taken,
        "a zero-advance round leaves the packets queued for the frame that consumes them"
    );

    // The tick after it drives a frame, so that frame gets the queue.
    client_tick(&mut world);
    let mut taken = false;
    assert_eq!(
        shadow.observe(&world, || {
            taken = true;
            vec![observed()]
        }),
        1
    );
    assert!(taken);
}

#[test]
fn an_unreported_ground_state_reads_as_not_on_the_ground() {
    let (driver, _calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::new(SessionStats::default()));
    let mut world = mirror();
    // A server teleport gives the mirror a position; the client sent no
    // movement packet, so its feet are unreported (`None`).
    teleport_to(&mut world, Vec3::new(0.5, 64.0, 0.5));
    client_tick(&mut world);
    assert_eq!(world.player_on_ground(), None);

    assert_eq!(shadow.observe(&world, no_packets), 1);
    assert!(
        !shadow.body.on_ground(),
        "an unreported ground state is never 'on the ground'"
    );
}

#[test]
fn a_death_is_reported_once_and_a_respawn_bumps_the_generation() {
    let (driver, _calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::new(SessionStats::default()));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 1);
    assert_eq!(shadow.observe(&world, no_packets), 1);
    assert_eq!(shadow.notice_life(&world), None, "healthy and alive");
    let generation = shadow.body.generation();

    // The kill arrives as a zero health update.
    set_health(&mut world, 0.0);
    client_tick(&mut world);
    assert_eq!(
        shadow.notice_life(&world),
        Some(Event::Died { message: None }),
        "the operator's death reaches the owner once"
    );
    assert_eq!(
        shadow.notice_life(&world),
        None,
        "health staying at zero is the same death, not another"
    );

    // The client presses respawn: the server builds a new player and the
    // mirror's life number moves.
    respawn(&mut world);
    assert_eq!(
        shadow.notice_life(&world),
        None,
        "a respawn is not a death of its own"
    );
    assert_eq!(
        shadow.body.generation(),
        generation.wrapping_add(1),
        "frames built against the old life are stale"
    );
    assert!(
        !shadow.body.on_ground(),
        "the body is reset the way a respawn resets it"
    );
    // The new life's health update has not arrived yet: still one death.
    assert_eq!(
        shadow.notice_life(&world),
        None,
        "a respawned player whose health is still zero is not dead again"
    );

    // A positive health ends the death; the next kill is a new `Died`.
    set_health(&mut world, 20.0);
    assert_eq!(shadow.notice_life(&world), None);
    set_health(&mut world, 0.0);
    assert_eq!(
        shadow.notice_life(&world),
        Some(Event::Died { message: None })
    );
}

#[test]
fn only_a_frame_that_asks_for_something_is_counted_as_wanted() {
    let world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 1);

    let quiet = Arc::new(SessionStats::default());
    let (driver, _calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::clone(&quiet));
    shadow.observe(&world, no_packets);
    assert_eq!(quiet.frames.load(Ordering::Relaxed), 1);
    assert_eq!(
        quiet.shadow_wanted.load(Ordering::Relaxed),
        0,
        "an idle frame wants nothing"
    );

    let wanted = Arc::new(SessionStats::default());
    let (driver, _calls) = fake(Held {
        forward: true,
        ..Held::NONE
    });
    let mut shadow = Shadow::new(driver, Arc::clone(&wanted));
    shadow.observe(&world, no_packets);
    assert_eq!(wanted.frames.load(Ordering::Relaxed), 1);
    assert_eq!(
        wanted.shadow_wanted.load(Ordering::Relaxed),
        1,
        "a frame holding forward is wanted, even when it is dropped"
    );
}

#[test]
fn every_kind_of_control_makes_a_frame_wanted() {
    let mut frame = ControlFrame::new(0, 0, Held::NONE);
    assert!(!frame_wants(&frame), "an empty frame wants nothing");

    frame.look = Look::Delta {
        dyaw: 1.0,
        dpitch: 0.0,
    };
    assert!(frame_wants(&frame), "a look change");
    frame.look = Look::None;

    frame.clicks.push(Click::Attack);
    assert!(frame_wants(&frame), "a click");
    frame.clicks.clear();

    frame.select_slot = Some(3);
    assert!(frame_wants(&frame), "a hotbar slot");
    frame.select_slot = None;

    frame.menu = Some(MenuAction {
        window: 0,
        slot: 1,
        button: 0,
    });
    assert!(frame_wants(&frame), "a menu action");
    frame.menu = None;

    frame.chat = Some("hello".to_owned());
    assert!(frame_wants(&frame), "chat");
    frame.chat = None;
    assert!(!frame_wants(&frame));
}

#[test]
fn a_far_position_jump_bumps_the_generation() {
    let (driver, _calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::new(SessionStats::default()));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 1);
    shadow.observe(&world, no_packets);
    assert_eq!(shadow.body.generation(), 0);
    assert_eq!(shadow.body.position(), Vec3::new(0.5, 64.0, 0.5));

    // A server teleport moves the mirrored player: more than 8 blocks at once.
    move_to(&mut world, Vec3::new(120.5, 70.0, -40.5), false);
    client_tick(&mut world);
    shadow.observe(&world, no_packets);
    assert_eq!(shadow.body.generation(), 1, "a jump is a new generation");
    assert_eq!(shadow.body.position(), Vec3::new(120.5, 70.0, -40.5));
    assert!(!shadow.body.on_ground());
}

/// A fake helper for the hands path: it acks frames and takes over on `TAKE`.
#[cfg(unix)]
fn hands_script(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    const SCRIPT: &str = r#"#!/bin/sh
printf 'READY\n'
while IFS= read -r line; do
  case "$line" in
    ARM) printf 'OK 0\n' ;;
    F*) printf 'OK %s\n' "$(printf '%s' "$line" | cut -d' ' -f2)" ;;
    TAKE) printf 'TAKEOVER foreign_input\n' ;;
  esac
done
"#;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-runs")
        .join(format!("hands-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the test dir");
    let path = dir.join(name);
    std::fs::write(&path, SCRIPT).expect("the fake helper");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("executable");
    path
}

#[cfg(unix)]
fn hands_config(name: &str) -> HandsConfig {
    HandsConfig {
        helper: hands_script(name),
        client_pid: 1,
        activate: false,
        options: crate::hands::ClientOptions {
            mouse_sensitivity: 0.5,
            invert_y_mouse: false,
        },
        look_latency_ticks: 2,
    }
}

#[cfg(unix)]
fn poll_until_takeover(shadow: &mut Shadow) -> Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = shadow.pump_hands() {
            return event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fake helper did not take over"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn hands_apply_every_frame_until_a_takeover_stops_them() {
    let (driver, calls) = fake(Held {
        forward: true,
        ..Held::NONE
    });
    let stats = Arc::new(SessionStats::default());
    let mut shadow = Shadow::new(driver, Arc::clone(&stats));
    shadow.hands = Some(Hands::spawn(hands_config("take.sh")).expect("the fake helper arms"));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 1);

    assert_eq!(shadow.observe(&world, no_packets), 1);
    assert_eq!(
        calls.lock().unwrap().applied,
        1,
        "hands mode reports what the client's packets did"
    );
    assert_eq!(stats.hands_frames.load(Ordering::Relaxed), 1);

    // The operator takes the mouse: the helper says so, and the frames stop
    // being applied while the body still computes them.
    shadow
        .hands
        .as_mut()
        .unwrap()
        .link
        .send("TAKE")
        .expect("a takeover");
    assert_eq!(
        poll_until_takeover(&mut shadow),
        Event::TakenOver {
            reason: "foreign_input".to_owned()
        }
    );
    assert!(!shadow.hands.as_ref().unwrap().applying);
    assert_eq!(shadow.body.generation(), 1, "frames built for the old control are stale");
    assert_eq!(stats.hands_takeovers.load(Ordering::Relaxed), 1);

    client_tick(&mut world);
    assert_eq!(shadow.observe(&world, no_packets), 1);
    assert_eq!(
        calls.lock().unwrap().applied,
        1,
        "after the takeover, frame reports stop exactly as in S2"
    );
    assert_eq!(
        stats.hands_frames.load(Ordering::Relaxed),
        1,
        "no frame reaches the helper after the takeover"
    );
    assert_eq!(shadow.pump_hands(), None, "one takeover, one event");
}

#[test]
fn a_new_connection_starts_the_mirror_counter_over() {
    let (driver, calls) = fake(Held::NONE);
    let mut shadow = Shadow::new(driver, Arc::new(SessionStats::default()));
    let mut world = mirror_at(Vec3::new(0.5, 64.0, 0.5), true, 2);
    assert_eq!(shadow.observe(&world, no_packets), 2);

    // The operator reconnects: the followed world starts over at tick 0.
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    move_to(&mut world, Vec3::new(0.5, 64.0, 0.5), true);
    client_tick(&mut world);
    assert_eq!(world.tick(), 1);
    assert_eq!(
        shadow.observe(&world, no_packets),
        1,
        "the restarted mirror is driven again"
    );
    assert_eq!(calls.lock().unwrap().ticks, vec![0, 1, 2]);
}
