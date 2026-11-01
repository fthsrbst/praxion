//! The configuration state: known packs, registries, keep-alive, then game.
//!
//! This state is entered twice in a session's life: after login, and whenever
//! the server sends `start_configuration` from play (a dimension change on some
//! servers). Both entries send the client information packet first, as vanilla
//! does (`azalea-client/src/plugins/client_information.rs:20-44`).

use azalea_entity::HumanoidArm;
use azalea_protocol::common::client_information::ClientInformation;
use azalea_protocol::packets::config::{
    ClientboundConfigPacket, ServerboundClientInformation, ServerboundCookieResponse,
    ServerboundFinishConfiguration, ServerboundKeepAlive, ServerboundPong,
    ServerboundSelectKnownPacks,
};

use crate::error::Error;
use crate::login::{self, ConfigConn, GameConn};

/// Send the client information and run the configuration state.
pub(crate) async fn enter(mut conn: ConfigConn, trace: bool) -> Result<GameConn, Error> {
    login::send(
        &mut conn.writer,
        ServerboundClientInformation {
            information: client_information(),
        },
        trace,
    )
    .await?;
    run(conn, trace).await
}

/// What the brief asks for: English, view distance 8, main hand right; the rest
/// (full chat, all particles, all skin parts) is vanilla's default.
fn client_information() -> ClientInformation {
    ClientInformation {
        language: "en_us".to_owned(),
        view_distance: 8,
        main_hand: HumanoidArm::Right,
        ..ClientInformation::default()
    }
}

async fn run(mut conn: ConfigConn, trace: bool) -> Result<GameConn, Error> {
    loop {
        match login::read(&mut conn, trace).await? {
            ClientboundConfigPacket::SelectKnownPacks(_) => {
                // `azalea-client` answers with an empty list: it implements no
                // resource packs, so it knows none (`.../packet/config/mod.rs:215-227`).
                login::send(
                    &mut conn.writer,
                    ServerboundSelectKnownPacks {
                        known_packs: vec![],
                    },
                    trace,
                )
                .await?;
            }
            ClientboundConfigPacket::RegistryData(_) => {
                // The world registries belong to the decoder's world model, not to
                // the body. The body only answers.
            }
            ClientboundConfigPacket::KeepAlive(p) => {
                login::send(&mut conn.writer, ServerboundKeepAlive { id: p.id }, trace).await?;
            }
            ClientboundConfigPacket::Ping(p) => {
                login::send(&mut conn.writer, ServerboundPong { id: p.id }, trace).await?;
            }
            ClientboundConfigPacket::CookieRequest(p) => {
                login::send(
                    &mut conn.writer,
                    ServerboundCookieResponse {
                        key: p.key,
                        payload: None,
                    },
                    trace,
                )
                .await?;
            }
            ClientboundConfigPacket::FinishConfiguration(_) => {
                login::send(&mut conn.writer, ServerboundFinishConfiguration, trace).await?;
                return Ok(conn.game());
            }
            ClientboundConfigPacket::Disconnect(p) => {
                return Err(Error::Disconnected(p.reason.to_string()));
            }
            _ => {}
        }
    }
}
