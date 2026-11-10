//! The overworld `world_clock`, mirrored the way the 26.3 client keeps it.
//!
//! 26.3 does not stream the day time. The server sends a clock's full state
//! (`total_ticks`, `partial_tick`, `rate`) only on join, on an `advance_time`
//! change and when a command moves the clock (`time set`, `time add`, pause,
//! rate). Every 20 server ticks it sends `set_time` with the game time and an
//! empty clock map. The client extrapolates in between: each level tick it adds
//! one to its game time and advances every clock by `game time delta * rate`,
//! carrying the fraction in `partial_tick` (`ClientClockManager.tick`). A
//! `set_time` first advances the clocks to the packet's game time, then
//! overwrites the clocks it carries (`ClientClockManager.handleUpdates`).
//! Paused clocks and `advance_time false` arrive as `rate == 0`.

use azalea_protocol::packets::game::c_set_time::ClockState;

/// One clock instance (`ClientClockManager.ClientClockInstance`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Clock {
    total_ticks: i64,
    partial_tick: f32,
    rate: f32,
}

impl Clock {
    pub(crate) fn from_state(state: &ClockState) -> Self {
        Self {
            total_ticks: state.total_ticks as i64,
            partial_tick: state.partial_tick,
            rate: state.rate,
        }
    }

    /// Whole ticks, never below zero.
    pub(crate) fn total_ticks(&self) -> u64 {
        self.total_ticks.max(0) as u64
    }

    /// Advance by `delta` game ticks, exactly as `ClientClockManager.tick`:
    /// `Mth.floor` returns an `int`, so the whole part saturates at the `i32`
    /// range like Java's `d2i`.
    fn advance(&mut self, delta: i64) {
        let value = f64::from(self.partial_tick) + delta as f64 * f64::from(self.rate);
        let whole = i64::from(value.floor() as i32);
        self.partial_tick = (value - whole as f64) as f32;
        self.total_ticks = self.total_ticks.wrapping_add(whole);
    }
}

/// The clock manager state the body needs: only the overworld clock.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OverworldClock {
    clock: Option<Clock>,
    last_tick_game_time: u64,
}

impl OverworldClock {
    /// `ClientClockManager.tick(gameTime)`.
    pub(crate) fn tick(&mut self, game_time: u64) {
        let delta = game_time.wrapping_sub(self.last_tick_game_time) as i64;
        self.last_tick_game_time = game_time;
        if let Some(clock) = &mut self.clock {
            clock.advance(delta);
        }
    }

    /// `ClientClockManager.handleUpdates`: advance to `game_time`, then take
    /// the server's state if the packet carries the overworld clock.
    pub(crate) fn handle_update(&mut self, game_time: u64, update: Option<&ClockState>) {
        self.tick(game_time);
        if let Some(state) = update {
            self.clock = Some(Clock::from_state(state));
        }
    }

    /// The day time: the overworld clock's whole ticks, 0 before any update.
    pub(crate) fn total_ticks(&self) -> u64 {
        self.clock.map_or(0, |clock| clock.total_ticks())
    }
}
