//! Praxion's headless Minecraft body: log in, answer the protocol, and play one
//! local player with physics lifted from `azalea-physics`.
//!
//! ```text
//! Session::connect_with
//!   -> std thread + single-threaded Tokio runtime
//!        TCP -> handshake -> login -> configuration -> play
//!        play loop: apply a ControlFrame, step physics, send the tick's packets
//!   -> Event::Joined / Died / Disconnected
//! Session::send(ServerboundGamePacket) / disconnect()  ->  command channel
//! ```
//!
//! The session keeps the player's physics state and the last movement packet it
//! sent. The world it collides with comes from the raw store: in live play the
//! body runs the tee subscriber -> decoder -> world pipeline, and here
//! [`WorldFeed`] does that in-process for the `--tee` mode.
//!
//! Digging, inventory and menus are later steps. What this step implements and
//! skips from a vanilla client's play behaviour is listed in the step report
//! (`logs/s5b/report.md` in the repository's scratch area).

mod chunks;
mod config;
mod driver;
mod error;
mod events;
mod frame;
mod interact;
mod login;
mod movement;
mod physics;
mod play;
mod script;
mod session;
mod win;
mod worldfeed;

pub use azalea_protocol::packets::game::ServerboundGamePacket;
pub use driver::{Driver, DriverHandle, TickView};
pub use error::Error;
pub use events::Event;
pub use frame::{Click, ControlFrame, FrameError, Held, Look, MenuAction, MenuMode};
pub use interact::{ActuatorState, AppliedReport, Outgoing, Pick, pick};
pub use login::protocol_version;
pub use physics::Body;
pub use script::{Script, ScriptLook, Step};
pub use session::{ConnectOptions, Session, SessionStats};
pub use worldfeed::{ObservedPacket, WorldFeed};
