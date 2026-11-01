//! Praxion's headless Minecraft body, step 5a: log in, answer what the protocol
//! requires, stay connected and idle.
//!
//! ```text
//! Session::connect_with
//!   -> std thread + single-threaded Tokio runtime
//!        TCP -> handshake -> login -> configuration -> play
//!        play loop: keep alive, ping, chunk batches, teleports, tick end, respawn
//!   -> Event::Joined / Died / Disconnected
//! Session::send(ServerboundGamePacket) / disconnect()  ->  command channel
//! ```
//!
//! The session keeps nothing about the world except what it needs to answer the
//! server (last teleport, last position sent, whether our chunk arrived). The
//! world model is the decoder's, fed by the forwarder's tee
//! (docs/decisions.md 2026-11-01, "Demo-0 data path").
//!
//! Physics, digging, inventory and the `ControlFrame` are later steps. What this
//! step implements and skips from a vanilla client's play behaviour is listed in
//! the step report (`logs/briefs/demo0/s5a-headless-login.md`).

mod chunks;
mod config;
mod error;
mod events;
mod login;
mod movement;
mod play;
mod session;
mod win;

pub use azalea_protocol::packets::game::ServerboundGamePacket;
pub use error::Error;
pub use events::Event;
pub use login::protocol_version;
pub use session::{ConnectOptions, Session};
