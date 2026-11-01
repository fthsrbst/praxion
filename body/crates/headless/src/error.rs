//! Everything that can end a headless [`crate::Session`].

use std::error::Error as StdError;
use std::fmt;
use std::io;

use azalea_protocol::read::ReadPacketError;

/// Why a session ended, or why it never started.
#[derive(Debug)]
pub enum Error {
    /// The TCP connection failed, or a packet could not be sent or received.
    Io(io::Error),
    /// Azalea could not read a packet the server sent.
    Protocol(Box<ReadPacketError>),
    /// The server asked for encryption. Demo-0 logs in offline only.
    OnlineMode,
    /// The server ended the session and said why.
    Disconnected(String),
    /// The caller asked to disconnect, or dropped the [`crate::Session`].
    LocalDisconnect,
    /// The session thread ended before the login finished.
    ThreadEnded,
    /// The session could not be set up (for example, the tee is unreachable).
    Setup(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "I/O error: {error}"),
            Error::Protocol(error) => write!(f, "packet error: {error}"),
            Error::OnlineMode => {
                write!(f, "the server is in online mode; Demo-0 logs in offline only")
            }
            Error::Disconnected(reason) => write!(f, "disconnected by the server: {reason}"),
            Error::LocalDisconnect => write!(f, "disconnected locally"),
            Error::ThreadEnded => write!(f, "the session thread ended before login finished"),
            Error::Setup(message) => write!(f, "{message}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Protocol(error) => Some(error),
            Error::OnlineMode
            | Error::Disconnected(_)
            | Error::LocalDisconnect
            | Error::ThreadEnded
            | Error::Setup(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<Box<ReadPacketError>> for Error {
    fn from(error: Box<ReadPacketError>) -> Self {
        Error::Protocol(error)
    }
}
