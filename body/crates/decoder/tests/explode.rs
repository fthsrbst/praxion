//! Regression tests for the 26.3 `ClientboundExplode` layout (run 8).
//!
//! A creeper explosion left "explode left 1 of 83 bytes unread": 26.3 appends
//! a `playSound` bool that vendored azalea lacked, and `explosionSound` is a
//! `Holder<SoundEvent>` (registry id + 1, or 0 and a direct sound). The layout
//! below is the 26.3 client's `ClientboundExplodePacket.STREAM_CODEC`, read
//! with javap: Vec3, FLOAT, INT, optional Vec3, ParticleTypes.STREAM_CODEC,
//! SoundEvent.STREAM_CODEC (`ByteBufCodecs.holder`), WeightedList of
//! ExplosionParticleInfo (particle, FLOAT, FLOAT; VarInt weight), BOOL.

use std::io::Cursor;

use azalea_buf::{AzBuf, AzBufVar};
use azalea_protocol::packets::{
    Packet, ProtocolPacket,
    game::{ClientboundExplode, ClientboundGamePacket},
};
use azalea_registry::{
    Holder, Registry,
    builtin::{ParticleKind, SoundEvent},
};

fn var(value: u32, out: &mut Vec<u8>) {
    value.azalea_write_var(out).unwrap();
}

/// The packet body (no id) of a creeper-style explosion, byte by byte.
fn hand_built_body(play_sound: bool) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [10.5f64, 64.0, -3.25] {
        b.extend_from_slice(&v.to_be_bytes()); // center
    }
    b.extend_from_slice(&3.0f32.to_be_bytes()); // radius
    b.extend_from_slice(&7i32.to_be_bytes()); // blockCount
    b.push(1); // playerKnockback: present
    for v in [0.5f64, 0.25, -0.5] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    var(ParticleKind::ExplosionEmitter.to_u32(), &mut b); // explosionParticle
    var(SoundEvent::EntityGenericExplode.to_u32() + 1, &mut b); // Holder reference
    var(1, &mut b); // blockParticles: one entry
    var(ParticleKind::Poof.to_u32(), &mut b);
    b.extend_from_slice(&0.5f32.to_be_bytes()); // scaling
    b.extend_from_slice(&1.0f32.to_be_bytes()); // speed
    var(1, &mut b); // weight
    b.push(u8::from(play_sound));
    b
}

#[test]
fn decodes_the_26_3_layout_with_no_leftover_bytes() {
    let body = hand_built_body(true);
    let mut cursor = Cursor::new(body.as_slice());
    let packet = ClientboundExplode::azalea_read(&mut cursor).unwrap();
    assert_eq!(cursor.position(), body.len() as u64, "bytes left unread");

    assert_eq!(packet.block_count, 7);
    assert!(packet.player_knockback.is_some());
    assert_eq!(format!("{:?}", packet.explosion_particle), "ExplosionEmitter");
    assert_eq!(packet.explosion_sound, Holder::Reference(SoundEvent::EntityGenericExplode));
    assert_eq!(packet.block_particles.len(), 1);
    assert_eq!(format!("{:?}", packet.block_particles[0].value.particle), "Poof");
    assert_eq!(packet.block_particles[0].weight, 1);
    assert!(packet.play_sound);

    // The decoder's path: packet id first, then `ClientboundGamePacket::read`.
    let id = packet.clone().into_variant().id();
    let mut frame = Vec::new();
    var(id, &mut frame);
    let id_len = frame.len();
    frame.extend_from_slice(&body);
    let mut cursor = Cursor::new(frame.as_slice());
    cursor.set_position(id_len as u64);
    let read = ClientboundGamePacket::read(id, &mut cursor).unwrap();
    assert_eq!(read.name(), "explode");
    assert_eq!(cursor.position(), frame.len() as u64, "frame left bytes unread");
}

#[test]
fn the_pre_26_3_layout_without_play_sound_is_short() {
    // Run 8's failure mode, inverted: a body without the trailing bool must
    // not decode, so a layout regression cannot pass silently.
    let mut body = hand_built_body(false);
    body.pop();
    let mut cursor = Cursor::new(body.as_slice());
    assert!(ClientboundExplode::azalea_read(&mut cursor).is_err());
}

#[test]
fn round_trips_byte_for_byte() {
    for play_sound in [true, false] {
        let body = hand_built_body(play_sound);
        let packet = ClientboundExplode::azalea_read(&mut Cursor::new(body.as_slice())).unwrap();
        let mut written = Vec::new();
        packet.azalea_write(&mut written).unwrap();
        assert_eq!(written, body, "re-encoding changes the bytes");
        assert_eq!(written.last(), Some(&u8::from(play_sound)), "playSound is the last byte");
        let again = ClientboundExplode::azalea_read(&mut Cursor::new(written.as_slice())).unwrap();
        assert_eq!(again, packet);
    }
}
