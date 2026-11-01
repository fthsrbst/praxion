//! The external decision layer's hook into the play loop.
//!
//! A [`Driver`] is called once per client tick, before physics, with the body,
//! the raw store and the packets the tee saw since the last tick. It returns one
//! [`ControlFrame`]; the session applies it and calls [`Driver::applied`] with
//! the prediction sequence number or menu state id the actuator used, so the
//! driver can attribute its receipts.
//!
//! This keeps the actuator below the decision layers: primitives and the
//! interpreter live in their own crate and never touch the socket.

use std::sync::{Arc, Mutex};

use crate::frame::ControlFrame;
use crate::interact::AppliedReport;
use crate::physics::Body;
use crate::worldfeed::ObservedPacket;

/// What the driver sees for one tick.
pub struct TickView<'a> {
    pub tick: u64,
    pub body: &'a Body,
    /// The raw store, absent when the session has no `--tee`.
    pub world: Option<&'a praxion_world::World>,
    /// Packets the tee saw since the previous tick, oldest first.
    pub packets: &'a [ObservedPacket],
}

/// A decision layer that drives the body from outside the play loop.
pub trait Driver: Send {
    /// The frame for this tick.
    fn frame(&mut self, view: &TickView<'_>) -> ControlFrame;
    /// What the actuator did with the frame, for receipt attribution.
    fn applied(&mut self, tick: u64, report: &AppliedReport);
    /// Whether the driver has finished and the session may stop.
    fn done(&self) -> bool {
        false
    }

    /// A JSON summary for the owner's log, if the driver has one.
    fn summary_json(&self) -> Option<String> {
        None
    }
}

/// The handle the owner and the play loop share.
pub type DriverHandle = Arc<Mutex<Box<dyn Driver>>>;
