//! Movement style: the one producer of look output (§5.2).
//!
//! Callers request an absolute target (`Look::Aim`) or nothing (`Look::None`).
//! [`Gaze`] turns that request into the quantised `Look::Delta` actually sent:
//! a human reaction delay, a minimum-jerk gesture, a biased landing and a
//! corrective gesture. No other layer emits a look.

use azalea_entity::LookDirection;
use praxion_headless::Look;
use praxion_world::Vec3;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

/// The rotation grid of a client at the default sensitivity, in degrees.
pub const STEP: f32 = 0.15;

/// The walk tolerance: an aim this close counts as aligned for walking.
pub const WALK_TOLERANCE_DEG: f32 = 4.0;

/// Ticks per second of the body.
const TICKS_PER_SECOND: f32 = 20.0;
/// The time constant of the walking drift, in seconds.
const DRIFT_TAU: f32 = 2.5;
/// The walking drift's noise scale, in degrees.
const DRIFT_SIGMA: f32 = 2.0;
/// The largest walking drift offset, in degrees.
const DRIFT_MAX_DEG: f32 = 6.0;
/// The drift's integration step, in seconds (one tick).
const DRIFT_DT: f32 = 0.05;
/// The pitch error a walking drift must exceed before it starts a gesture
/// (research §4.5).
const PITCH_DEADBAND_DEG: f32 = 1.5;
/// A gesture's travel may miss by up to 6 % (half of this).
const LANDING_BIAS: f32 = 0.12;
/// A request that moves by more than this restarts the running gesture.
const RETARGET_DEG: f32 = 1.0;
/// A request that appears after this many empty ticks is a new stimulus.
const REACTION_NONE_TICKS: u64 = 10;
/// The pause between a gesture and its correction, in ticks.
const CORRECTION_PAUSE_TICKS: u64 = 2;
/// A body look this far from the emitted one means the server moved us.
const RESYNC_DEG: f32 = 2.0;

/// The traits drawn once from the run seed. Runs are reproducible: the same
/// seed gives the same personality and the same gestures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Personality {
    pub turn_time: f32,
    pub step_deg: f32,
    /// How far along the route the walk looks, in blocks.
    pub look_ahead: f32,
    /// The yaw change under which a steering gesture is not started, in
    /// degrees.
    pub deadband_deg: f32,
    /// Whether this body sprints on long flat runs.
    pub sprints: bool,
    /// Ticks after forward starts before sprint may begin.
    pub sprint_delay_ticks: u32,
    /// The walking pitch, in degrees down (Minecraft's positive pitch).
    pub walk_pitch: f32,
}

impl Personality {
    fn draw(rng: &mut StdRng) -> Self {
        let mut personality = Self {
            turn_time: rng.gen_range(0.8..1.2),
            step_deg: STEP,
            look_ahead: rng.gen_range(1.8..3.0),
            deadband_deg: rng.gen_range(4.0..12.0),
            sprints: rng.gen_bool(0.8),
            sprint_delay_ticks: if rng.gen_bool(0.25) {
                0
            } else {
                (20.0_f32 * rng.gen_range(0.1_f32..0.8)).round() as u32
            },
            walk_pitch: 0.0,
        };
        // Drawn after the older traits so their values never change (§4.6).
        let gauss: f32 = Normal::new(0.0, 1.0)
            .expect("the unit normal is a valid distribution")
            .sample(rng);
        personality.walk_pitch = (6.0 + 5.0 * gauss).clamp(-5.0, 16.0);
        personality
    }

    /// One compact line for the event log.
    pub fn describe(&self) -> String {
        format!(
            "gaze turn_time={:.3} step={:.2} look_ahead={:.2} deadband={:.1} \
             sprints={} sprint_delay={} walk_pitch={:.1}",
            self.turn_time,
            self.step_deg,
            self.look_ahead,
            self.deadband_deg,
            self.sprints,
            self.sprint_delay_ticks,
            self.walk_pitch
        )
    }
}

impl Default for Personality {
    /// Mid values for callers without a seed (reflexes, tests).
    fn default() -> Self {
        Self {
            turn_time: 1.0,
            step_deg: STEP,
            look_ahead: 2.4,
            deadband_deg: 8.0,
            sprints: true,
            sprint_delay_ticks: 0,
            walk_pitch: 8.0,
        }
    }
}

/// The yaw and pitch (degrees) that look from `eye` at `target`.
pub fn aim(eye: Vec3, target: Vec3) -> (f32, f32) {
    let dx = target.x - eye.x;
    let dy = target.y - eye.y;
    let dz = target.z - eye.z;
    let horizontal = (dx * dx + dz * dz).sqrt();
    // Azalea's `input_vector`: forward at yaw 0 is +z, and +x is yaw -90.
    let yaw = f64::atan2(-dx, dz).to_degrees() as f32;
    let pitch = f64::atan2(-dy, horizontal).to_degrees() as f32;
    (yaw, pitch)
}

/// The shortest signed difference `to - from`, in degrees.
pub fn wrap_degrees(degrees: f32) -> f32 {
    let mut value = degrees % 360.;
    if value > 180. {
        value -= 360.;
    } else if value < -180. {
        value += 360.;
    }
    value
}

/// The absolute request toward `target` and whether the body's current look is
/// already within `tolerance_deg` on both axes.
///
/// The request is always an `Aim`, even when aligned: it keeps the request
/// alive so the [`Gaze`] does not see a pause between two path nodes.
pub fn look_towards(
    current: LookDirection,
    eye: Vec3,
    target: Vec3,
    tolerance_deg: f32,
) -> (Look, bool) {
    let (yaw, pitch) = aim(eye, target);
    let aligned = wrap_degrees(yaw - current.y_rot()).abs() <= tolerance_deg
        && (pitch - current.x_rot()).abs() <= tolerance_deg;
    (Look::Aim { yaw, pitch }, aligned)
}

/// One axis of a minimum-jerk gesture: research §4.2's
/// `e(tau) = (1-tau)^3 (q0 + q1 tau + q2 tau^2)`, `x = x1 + e`.
#[derive(Clone, Debug)]
struct Axis {
    x1: f32,
    q0: f32,
    q1: f32,
    q2: f32,
}

impl Axis {
    /// The quintic with `p(0)=e0`, `p'(0)=v0 T`, acceleration 0, and position,
    /// velocity and acceleration all zero at `tau=1`.
    fn new(x0: f32, v0: f32, x1: f32, duration: f32) -> Self {
        let q0 = x0 - x1;
        Self {
            x1,
            q0,
            q1: v0 * duration + 3.0 * q0,
            q2: 6.0 * q0 + 3.0 * v0 * duration,
        }
    }

    /// `(position, velocity)` at `tau`.
    fn sample(&self, tau: f32, duration: f32) -> (f32, f32) {
        let omt = 1.0 - tau;
        let a = self.q0 + tau * (self.q1 + tau * self.q2);
        let b = self.q1 + 2.0 * self.q2 * tau;
        let e = omt * omt * omt * a;
        let de = -3.0 * omt * omt * a + omt * omt * omt * b;
        (self.x1 + e, de / duration)
    }
}

#[derive(Clone, Debug)]
struct Gesture {
    start_tick: u64,
    duration_ticks: f32,
    yaw: Axis,
    pitch: Axis,
}

impl Gesture {
    fn new(
        start_tick: u64,
        duration_ticks: f32,
        from: (f32, f32),
        velocity: (f32, f32),
        to: (f32, f32),
    ) -> Self {
        Self {
            start_tick,
            duration_ticks,
            yaw: Axis::new(from.0, velocity.0, to.0, duration_ticks),
            pitch: Axis::new(from.1, velocity.1, to.1, duration_ticks),
        }
    }

    /// `(position, velocity)` at `tick`, in degrees and degrees per tick. The
    /// first sample is one tick after the start, so an urgent request moves on
    /// the tick it arrives.
    fn sample(&self, tick: u64) -> (f32, f32, f32, f32) {
        let elapsed = (tick + 1 - self.start_tick) as f32;
        let tau = (elapsed / self.duration_ticks).clamp(0.0, 1.0);
        let (yaw, v_yaw) = self.yaw.sample(tau, self.duration_ticks);
        let (pitch, v_pitch) = self.pitch.sample(tau, self.duration_ticks);
        (yaw, pitch, v_yaw, v_pitch)
    }
}

/// Why this tick's look request is there, as the driver knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// The same source as last tick: a moving target or the next path step.
    Continue,
    /// A new source (a new primitive or reflex): wait a human reaction time.
    Stimulus,
    /// A lethal-fast reflex or a safety guard: move at once.
    Urgent,
}

/// What the body is doing, from the driver's frame: a walking gaze drifts its
/// pitch, an idle one looks around, and a still one (a menu) does neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GazeMode {
    Walking,
    Idle,
    Still,
}

/// The body's one camera controller.
pub struct Gaze {
    personality: Personality,
    rng: StdRng,
    emitted_yaw: f32,
    emitted_pitch: f32,
    /// The drift-free camera on the rotation grid, for gesture decisions.
    base_yaw: f32,
    base_pitch: f32,
    ideal_yaw: f32,
    ideal_pitch: f32,
    velocity_yaw: f32,
    velocity_pitch: f32,
    gesture: Option<Gesture>,
    target_yaw: f32,
    target_pitch: f32,
    /// The last request seen, unwrapped near the current yaw.
    last_request: Option<(f32, f32)>,
    /// The last requested yaw and pitch, for the idle gaze's anchor.
    anchor: Option<(f32, f32)>,
    /// Consecutive ticks with no request.
    none_ticks: u64,
    /// Do not start a gesture before this tick.
    delay_until: u64,
    /// A correction waits until this tick, after a gesture that landed off.
    correction_at: Option<u64>,
    /// The walking pitch drift, in degrees (research §4.6).
    drift_pitch: f32,
    /// The next idle-gaze gesture, after the no-request pause.
    idle_next: Option<u64>,
    initialized: bool,
}

impl Gaze {
    pub fn new(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let personality = Personality::draw(&mut rng);
        Self {
            personality,
            rng,
            emitted_yaw: 0.0,
            emitted_pitch: 0.0,
            base_yaw: 0.0,
            base_pitch: 0.0,
            ideal_yaw: 0.0,
            ideal_pitch: 0.0,
            velocity_yaw: 0.0,
            velocity_pitch: 0.0,
            gesture: None,
            target_yaw: 0.0,
            target_pitch: 0.0,
            last_request: None,
            anchor: None,
            none_ticks: 0,
            delay_until: 0,
            correction_at: None,
            drift_pitch: 0.0,
            idle_next: None,
            initialized: false,
        }
    }

    pub fn personality(&self) -> Personality {
        self.personality
    }

    /// One tick: the request goes in, the look actually sent comes out.
    pub fn step(
        &mut self,
        request: Look,
        body_look: LookDirection,
        cue: Cue,
        tick: u64,
        mode: GazeMode,
    ) -> Look {
        self.resync(body_look);
        self.absorb(request, cue, tick, mode);
        self.update_drift(mode);
        self.maybe_idle(tick, mode, request);
        self.maybe_start(tick, mode);

        let mut ended = false;
        if let Some(gesture) = &self.gesture {
            let elapsed = (tick + 1 - gesture.start_tick) as f32;
            if elapsed >= gesture.duration_ticks {
                self.ideal_yaw = gesture.yaw.x1;
                self.ideal_pitch = gesture.pitch.x1;
                self.velocity_yaw = 0.0;
                self.velocity_pitch = 0.0;
                self.gesture = None;
                ended = true;
            } else {
                let (yaw, pitch, v_yaw, v_pitch) = gesture.sample(tick);
                self.ideal_yaw = yaw;
                self.ideal_pitch = pitch;
                self.velocity_yaw = v_yaw;
                self.velocity_pitch = v_pitch;
            }
        }

        if self.gesture.is_none() && !ended {
            return Look::None;
        }
        let look = self.quantise(self.gesture.is_some() || ended);
        if ended {
            self.schedule_correction(tick, mode);
        }
        look
    }

    /// One unit Gaussian from the gaze's own stream.
    fn gauss(&mut self) -> f32 {
        Normal::new(0.0, 1.0)
            .expect("the unit normal is a valid distribution")
            .sample(&mut self.rng)
    }

    /// Walk one tick of the Ornstein-Uhlenbeck pitch drift (research §4.6).
    /// The offset only shifts the walking pitch goal; it is never added to the
    /// output. Outside `Walking` it is zero at once, so a primitive's aim is
    /// reached exactly.
    fn update_drift(&mut self, mode: GazeMode) {
        if mode == GazeMode::Walking {
            let noise = self.gauss();
            let decay = 1.0 - DRIFT_DT / DRIFT_TAU;
            let kick = DRIFT_SIGMA * DRIFT_DT.sqrt();
            self.drift_pitch =
                (self.drift_pitch * decay + kick * noise).clamp(-DRIFT_MAX_DEG, DRIFT_MAX_DEG);
        } else {
            self.drift_pitch = 0.0;
        }
    }

    /// Start an idle gaze gesture after the no-request pause, at the
    /// personality's walk pitch (research §4.6). Still mode never does.
    fn maybe_idle(&mut self, tick: u64, mode: GazeMode, request: Look) {
        let pausing = mode != GazeMode::Idle
            || !matches!(request, Look::None)
            || self.none_ticks < REACTION_NONE_TICKS;
        if pausing {
            if mode != GazeMode::Idle {
                self.idle_next = None;
            }
            return;
        }
        if self.gesture.is_some() || self.correction_at.is_some_and(|at| tick < at) {
            return;
        }
        let (anchor_yaw, anchor_pitch) = self
            .anchor
            .unwrap_or((self.ideal_yaw, self.ideal_pitch));
        if self.idle_next.is_none() {
            self.idle_next = Some(tick.saturating_add(self.idle_interval()));
        }
        if tick < self.idle_next.unwrap_or(0) {
            return;
        }
        // Mostly a new spot near the last one; sometimes back to the work point.
        let (yaw, pitch) = if self.rng.gen_bool(0.30) {
            (anchor_yaw, anchor_pitch)
        } else {
            (
                anchor_yaw + self.gauss() * 12.0,
                self.personality.walk_pitch + self.gauss() * 5.0,
            )
        };
        self.target_yaw = unwrap_near(yaw, self.ideal_yaw);
        self.target_pitch = pitch.clamp(-90.0, 90.0);
        self.begin_gesture(tick);
        self.idle_next = Some(tick.saturating_add(self.idle_interval()));
    }

    /// A uniform idle-gaze interval, in ticks: `U(1.5, 5) s`.
    fn idle_interval(&mut self) -> u64 {
        (TICKS_PER_SECOND * self.rng.gen_range(1.5..5.0))
            .round()
            .max(1.0) as u64
    }

    fn resync(&mut self, body_look: LookDirection) {
        let yaw = body_look.y_rot();
        let pitch = body_look.x_rot();
        if !self.initialized {
            self.initialized = true;
            self.emitted_yaw = yaw;
            self.emitted_pitch = pitch;
            self.base_yaw = yaw;
            self.base_pitch = pitch;
            self.ideal_yaw = yaw;
            self.ideal_pitch = pitch;
            self.target_yaw = yaw;
            self.target_pitch = pitch;
            return;
        }
        let moved = (yaw - self.emitted_yaw).abs() > RESYNC_DEG
            || (pitch - self.emitted_pitch).abs() > RESYNC_DEG;
        // The body is the truth: the session applies every frame before the
        // next tick, so a small difference is a dropped frame or a pitch clamp.
        self.emitted_yaw = yaw;
        self.emitted_pitch = pitch;
        if !moved {
            return;
        }
        // A server move (teleport, respawn) put the camera somewhere else. The
        // local trajectory is meaningless now; restart from the real look.
        self.ideal_yaw = yaw;
        self.ideal_pitch = pitch;
        self.base_yaw = yaw;
        self.base_pitch = pitch;
        self.drift_pitch = 0.0;
        self.velocity_yaw = 0.0;
        self.velocity_pitch = 0.0;
        self.gesture = None;
        self.correction_at = None;
        self.target_yaw = unwrap_near(self.target_yaw, yaw);
        self.target_pitch = self.target_pitch.clamp(-90.0, 90.0);
        if let Some((request_yaw, request_pitch)) = self.last_request {
            self.last_request = Some((unwrap_near(request_yaw, yaw), request_pitch));
        }
    }

    fn absorb(&mut self, request: Look, cue: Cue, tick: u64, mode: GazeMode) {
        let Look::Aim { yaw, pitch } = request else {
            self.last_request = None;
            self.none_ticks = self.none_ticks.saturating_add(1);
            return;
        };
        let request_yaw = unwrap_near(yaw, self.ideal_yaw);
        let request_pitch = pitch.clamp(-90.0, 90.0);
        let after_pause = self.last_request.is_none() && self.none_ticks >= REACTION_NONE_TICKS;
        let moved = wrap_degrees(request_yaw - self.target_yaw).abs() > RETARGET_DEG
            || (request_pitch - self.target_pitch).abs() > RETARGET_DEG;
        if cue == Cue::Urgent {
            // A lethal reflex waits for nothing: no reaction, no correction pause.
            self.delay_until = 0;
            self.correction_at = None;
        } else if cue == Cue::Stimulus || after_pause {
            // A new stimulus: a running gesture finishes its old aim while a
            // human reaction time passes, then the new gesture starts.
            let reaction = self.reaction_ticks();
            self.delay_until = tick.saturating_add(reaction);
        }
        self.target_yaw = request_yaw;
        self.target_pitch = request_pitch;
        self.last_request = Some((request_yaw, request_pitch));
        self.anchor = Some((request_yaw, request_pitch));
        self.idle_next = None;
        self.none_ticks = 0;
        if self.gesture.is_some() && moved && tick >= self.delay_until {
            // Keep position and velocity continuous across the new target.
            let pitch_goal = request_pitch + self.drift_goal(mode);
            self.begin_gesture_to(tick, request_yaw, pitch_goal);
        }
    }

    /// The pitch offset the walking drift adds to the goal. Outside `Walking`
    /// it is zero, so the request itself is the goal.
    fn drift_goal(&self, mode: GazeMode) -> f32 {
        if mode == GazeMode::Walking {
            self.drift_pitch
        } else {
            0.0
        }
    }

    /// One human reaction time, drawn per stimulus: about 250 ms, never under
    /// 150 ms or 3 ticks.
    fn reaction_ticks(&mut self) -> u64 {
        let gauss = self.gauss();
        let seconds = (0.25 + 0.10 * gauss).max(0.15);
        (TICKS_PER_SECOND * seconds).round().max(3.0) as u64
    }

    fn maybe_start(&mut self, tick: u64, mode: GazeMode) {
        if self.gesture.is_some() || tick < self.delay_until {
            return;
        }
        if self.correction_at.is_some_and(|at| tick < at) {
            return;
        }
        let Some((request_yaw, request_pitch)) = self.last_request else {
            return;
        };
        // While walking the drift moves the pitch goal, but only through a
        // gesture once it leaves the 1.5 deg deadband (research §4.5). Outside
        // `Walking` the goal is the request itself.
        let pitch_goal = request_pitch + self.drift_goal(mode);
        let pitch_deadband = if mode == GazeMode::Walking {
            PITCH_DEADBAND_DEG
        } else {
            self.personality.step_deg
        };
        let off = wrap_degrees(request_yaw - self.base_yaw).abs() > self.personality.step_deg
            || (pitch_goal - self.base_pitch).abs() > pitch_deadband;
        if off {
            self.begin_gesture_to(tick, request_yaw, pitch_goal);
        }
    }

    /// Start a quintic from the current ideal position and velocity toward the
    /// stored target, with the per-axis landing bias.
    fn begin_gesture(&mut self, tick: u64) {
        let (target_yaw, target_pitch) = (self.target_yaw, self.target_pitch);
        self.begin_gesture_to(tick, target_yaw, target_pitch);
    }

    /// Start a quintic toward an explicit goal, with the per-axis landing bias.
    fn begin_gesture_to(&mut self, tick: u64, target_yaw: f32, target_pitch: f32) {
        let x0_yaw = self.ideal_yaw;
        let x0_pitch = self.ideal_pitch;
        let v0_yaw = self.velocity_yaw;
        let v0_pitch = self.velocity_pitch;
        let x1_yaw = target_yaw
            + (self.rng.gen_range(0.0..1.0) - 0.5) * LANDING_BIAS * (target_yaw - x0_yaw);
        let x1_pitch = target_pitch
            + (self.rng.gen_range(0.0..1.0) - 0.5) * LANDING_BIAS * (target_pitch - x0_pitch);
        let distance = ((target_yaw - x0_yaw).powi(2) + (target_pitch - x0_pitch).powi(2)).sqrt();
        let seconds =
            ((0.08 + 0.30 * (distance / 90.0).sqrt()) * self.personality.turn_time).max(0.10);
        let duration_ticks = (seconds * TICKS_PER_SECOND).max(2.0);
        self.gesture = Some(Gesture::new(
            tick,
            duration_ticks,
            (x0_yaw, x0_pitch),
            (v0_yaw, v0_pitch),
            (x1_yaw, x1_pitch),
        ));
        self.correction_at = None;
    }

    fn schedule_correction(&mut self, tick: u64, mode: GazeMode) {
        let Some((request_yaw, request_pitch)) = self.last_request else {
            self.correction_at = None;
            return;
        };
        let pitch_goal = request_pitch + self.drift_goal(mode);
        let pitch_deadband = if mode == GazeMode::Walking {
            PITCH_DEADBAND_DEG
        } else {
            self.personality.step_deg
        };
        let off = wrap_degrees(request_yaw - self.base_yaw).abs() > self.personality.step_deg
            || (pitch_goal - self.base_pitch).abs() > pitch_deadband;
        self.correction_at = if off {
            Some(tick.saturating_add(CORRECTION_PAUSE_TICKS + 1))
        } else {
            None
        };
    }

    /// Move the drift-free camera and the actual output to the nearest point of
    /// the rotation grid. The remainder is never accumulated, so the output
    /// never drifts from the ideal.
    fn quantise(&mut self, base_active: bool) -> Look {
        let step = self.personality.step_deg;
        if base_active {
            self.base_yaw += ((self.ideal_yaw - self.base_yaw) / step).round() * step;
            self.base_pitch += ((self.ideal_pitch - self.base_pitch) / step).round() * step;
        }
        let dyaw = ((self.ideal_yaw - self.emitted_yaw) / step).round() * step;
        let dpitch = ((self.ideal_pitch - self.emitted_pitch) / step).round() * step;
        self.emitted_yaw += dyaw;
        self.emitted_pitch += dpitch;
        if dyaw == 0.0 && dpitch == 0.0 {
            Look::None
        } else {
            Look::Delta { dyaw, dpitch }
        }
    }
}

fn unwrap_near(degrees: f32, reference: f32) -> f32 {
    reference + wrap_degrees(degrees - reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rig {
        gaze: Gaze,
        yaw: f32,
        pitch: f32,
        tick: u64,
    }

    impl Rig {
        fn new(seed: u64) -> Self {
            Self {
                gaze: Gaze::new(seed),
                yaw: 0.,
                pitch: 0.,
                tick: 0,
            }
        }

        fn step(&mut self, request: Look) -> Look {
            self.step_cue(request, Cue::Continue)
        }

        fn step_mode(&mut self, request: Look, mode: GazeMode) -> Look {
            self.step_cue_mode(request, Cue::Continue, mode)
        }

        fn step_cue(&mut self, request: Look, cue: Cue) -> Look {
            self.step_cue_mode(request, cue, GazeMode::Still)
        }

        fn step_cue_mode(&mut self, request: Look, cue: Cue, mode: GazeMode) -> Look {
            let look = self.gaze.step(
                request,
                LookDirection::new(self.yaw, self.pitch),
                cue,
                self.tick,
                mode,
            );
            if let Look::Delta { dyaw, dpitch } = look {
                self.yaw += dyaw;
                self.pitch = (self.pitch + dpitch).clamp(-90., 90.);
            }
            self.tick += 1;
            look
        }
    }

    #[test]
    fn aiming_uses_minecraft_yaw_conventions() {
        let eye = Vec3::new(0.5, 65.62, 0.5);
        let (yaw, pitch) = aim(eye, Vec3::new(0.5, 65.62, 3.5));
        assert!(yaw.abs() < 1e-3, "south is yaw 0, got {yaw}");
        assert!(pitch.abs() < 1e-3);

        let (yaw, _) = aim(eye, Vec3::new(3.5, 65.62, 0.5));
        assert!((yaw + 90.).abs() < 1e-3, "east is yaw -90, got {yaw}");

        let (yaw, _) = aim(eye, Vec3::new(0.5, 65.62, -3.5));
        assert!((yaw.abs() - 180.).abs() < 1e-3, "north is yaw 180, got {yaw}");
    }

    #[test]
    fn a_gesture_lands_on_its_biased_target_with_zero_velocity() {
        for seed in 0..8 {
            let mut rig = Rig::new(seed);
            let request = Look::Aim {
                yaw: 90.,
                pitch: 0.,
            };
            let first = rig.step(request);
            let (amplitude, duration, x1_yaw, x1_pitch) = {
                let gesture = rig.gaze.gesture.as_ref().expect("the gesture started");
                (
                    (gesture.yaw.q0.powi(2) + gesture.pitch.q0.powi(2)).sqrt(),
                    gesture.duration_ticks,
                    gesture.yaw.x1,
                    gesture.pitch.x1,
                )
            };
            let expected_peak = 1.875 * amplitude / duration;
            let mut peak = match first {
                Look::Delta { dyaw, dpitch } => (dyaw * dyaw + dpitch * dpitch).sqrt(),
                _ => 0.0,
            };
            loop {
                let look = rig.step(request);
                if let Look::Delta { dyaw, dpitch } = look {
                    peak = peak.max((dyaw * dyaw + dpitch * dpitch).sqrt());
                }
                if rig.gaze.gesture.is_none() {
                    break;
                }
                assert!(rig.tick < 60, "seed {seed}: the gesture never ended");
            }
            assert!(
                peak > expected_peak * 0.85 && peak < expected_peak * 1.05,
                "seed {seed}: peak {peak:.2} deg/tick, expected {expected_peak:.2}"
            );
            assert_eq!(rig.gaze.velocity_yaw, 0.0);
            assert_eq!(rig.gaze.velocity_pitch, 0.0);
            assert!((rig.gaze.ideal_yaw - x1_yaw).abs() < 1e-4);
            assert!((rig.gaze.ideal_pitch - x1_pitch).abs() < 1e-4);
        }
    }

    #[test]
    fn emitted_deltas_sit_on_the_grid_and_track_the_ideal() {
        let mut rig = Rig::new(7);
        let request = Look::Aim {
            yaw: 137.,
            pitch: 15.,
        };
        for _ in 0..80 {
            let look = rig.step(request);
            if let Look::Delta { dyaw, dpitch } = look {
                let n_yaw = dyaw / STEP;
                let n_pitch = dpitch / STEP;
                assert!((n_yaw - n_yaw.round()).abs() < 1e-3, "dyaw {dyaw}");
                assert!((n_pitch - n_pitch.round()).abs() < 1e-3, "dpitch {dpitch}");
            }
            let off_yaw = (rig.gaze.ideal_yaw - rig.gaze.emitted_yaw).abs();
            let off_pitch = (rig.gaze.ideal_pitch - rig.gaze.emitted_pitch).abs();
            assert!(off_yaw <= STEP / 2.0 + 1e-4, "yaw off by {off_yaw}");
            assert!(off_pitch <= STEP / 2.0 + 1e-4, "pitch off by {off_pitch}");
        }
    }

    #[test]
    fn a_wrapped_request_turns_the_short_way() {
        let mut rig = Rig::new(3);
        let request = Look::Aim {
            yaw: 350.,
            pitch: 0.,
        };
        let mut total = 0.0;
        for _ in 0..60 {
            if let Look::Delta { dyaw, .. } = rig.step(request) {
                total += dyaw;
            }
        }
        assert!(total < 0.0, "350 deg from yaw 0 is a negative turn, got {total}");
        assert!((total + 10.0).abs() < 1.0, "expected about -10 deg, got {total}");
    }

    #[test]
    fn correction_converges_within_a_grid_step() {
        for seed in 0..16 {
            let mut rig = Rig::new(seed);
            let request = Look::Aim {
                yaw: 90.,
                pitch: 0.,
            };
            let mut settled = None;
            for _ in 0..40 {
                rig.step(request);
                let off_yaw = wrap_degrees(90.0 - rig.yaw).abs();
                let off_pitch = (0.0 - rig.pitch).abs();
                if off_yaw <= STEP
                    && off_pitch <= STEP
                    && rig.gaze.gesture.is_none()
                    && rig.gaze.correction_at.is_none()
                {
                    settled = Some(rig.tick);
                    break;
                }
            }
            let settled = settled.unwrap_or_else(|| panic!("seed {seed} did not converge"));
            assert!(settled < 40, "seed {seed} settled in {settled} ticks");
        }
    }

    #[test]
    fn a_new_stimulus_waits_for_the_reaction_delay() {
        let mut rig = Rig::new(5);
        for _ in 0..30 {
            rig.step(Look::Aim {
                yaw: 0.,
                pitch: 0.,
            });
        }
        let request = Look::Aim {
            yaw: 60.,
            pitch: 0.,
        };
        let start = rig.tick;
        assert_eq!(rig.step_cue(request, Cue::Stimulus), Look::None, "the stimulus tick must wait");
        let reaction = rig.gaze.delay_until - start;
        assert!(reaction >= 3);
        let mut waited = 1;
        let mut moved = false;
        for tick in 1..(reaction + 5) {
            let look = rig.step(request);
            if tick < reaction {
                assert_eq!(look, Look::None, "tick {tick} must wait");
                waited += 1;
            } else if look != Look::None {
                moved = true;
            }
        }
        assert!(waited >= 3, "waited {waited} ticks");
        assert!(moved, "the gesture must start after the delay");
    }

    #[test]
    fn a_continuing_request_does_not_wait() {
        let mut rig = Rig::new(5);
        for _ in 0..30 {
            rig.step(Look::Aim {
                yaw: 0.,
                pitch: 0.,
            });
        }
        // The next path step, 90 degrees round a corner: no reaction delay.
        let look = rig.step(Look::Aim {
            yaw: 90.,
            pitch: 0.,
        });
        assert!(matches!(look, Look::Delta { .. }), "a path step moves at once, got {look:?}");
    }

    #[test]
    fn an_urgent_request_moves_on_the_first_tick() {
        let mut rig = Rig::new(5);
        for _ in 0..30 {
            rig.step(Look::Aim {
                yaw: 0.,
                pitch: 0.,
            });
        }
        let look = rig.step_cue(
            Look::Aim {
                yaw: 60.,
                pitch: 0.,
            },
            Cue::Urgent,
        );
        assert!(matches!(look, Look::Delta { .. }), "urgent emits at once, got {look:?}");
    }

    #[test]
    fn an_urgent_request_cancels_a_pending_reaction() {
        let mut rig = Rig::new(5);
        for _ in 0..(REACTION_NONE_TICKS + 2) {
            rig.step(Look::None);
        }
        let calm = Look::Aim {
            yaw: 60.,
            pitch: 0.,
        };
        assert_eq!(rig.step(calm), Look::None);
        assert!(rig.gaze.delay_until > rig.tick, "the reaction is pending");
        let look = rig.step_cue(
            Look::Aim {
                yaw: -60.,
                pitch: 0.,
            },
            Cue::Urgent,
        );
        assert!(matches!(look, Look::Delta { .. }), "urgent emits at once, got {look:?}");
    }

    #[test]
    fn the_same_seed_gives_the_same_output() {
        let requests = [
            Look::Aim {
                yaw: 0.,
                pitch: 0.,
            },
            Look::Aim {
                yaw: 75.,
                pitch: 12.,
            },
            Look::None,
            Look::Aim {
                yaw: -40.,
                pitch: -8.,
            },
            Look::Aim {
                yaw: 350.,
                pitch: 0.,
            },
        ];
        let run = || {
            let mut rig = Rig::new(42);
            let mut output = Vec::new();
            for _ in 0..3 {
                for &request in &requests {
                    output.push(rig.step(request));
                }
            }
            output
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_request_after_a_long_pause_waits_for_the_reaction_delay() {
        let mut rig = Rig::new(1);
        for _ in 0..(REACTION_NONE_TICKS + 2) {
            rig.step(Look::None);
        }
        let request = Look::Aim {
            yaw: 45.,
            pitch: 0.,
        };
        let start = rig.tick;
        assert_eq!(rig.step(request), Look::None, "the stimulus tick must wait");
        let reaction = rig.gaze.delay_until - start;
        assert!(reaction >= 3);
        for tick in 1..reaction {
            assert_eq!(rig.step(request), Look::None, "tick {tick} must wait");
        }
    }

    #[test]
    fn no_request_and_no_gesture_emits_nothing() {
        let mut rig = Rig::new(1);
        for _ in 0..10 {
            assert_eq!(rig.step(Look::None), Look::None);
        }
    }

    #[test]
    fn yaw_wraps_the_short_way() {
        assert_eq!(wrap_degrees(350.), -10.);
        assert_eq!(wrap_degrees(-350.), 10.);
        assert_eq!(wrap_degrees(10.), 10.);
    }

    #[test]
    fn the_tolerance_reports_alignment() {
        let current = LookDirection::new(2., 1.);
        let eye = Vec3::new(0., 64., 0.);
        let target = Vec3::new(0., 64., 5.);
        let (look, aligned) = look_towards(current, eye, target, 4.);
        assert!(aligned);
        assert!(matches!(look, Look::Aim { .. }));
    }

    #[test]
    fn idle_mode_glances_after_the_pause_and_still_mode_is_silent() {
        let mut still = Rig::new(2);
        for _ in 0..2000 {
            assert_eq!(
                still.step_mode(Look::None, GazeMode::Still),
                Look::None,
                "Still must never start a look"
            );
        }

        let mut rig = Rig::new(11);
        let anchor_yaw = 0.0;
        let walk_pitch = rig.gaze.personality.walk_pitch;
        let mut gestures = 0;
        let mut was_active = false;
        for _ in 0..2000 {
            rig.step_mode(Look::None, GazeMode::Idle);
            let active = rig.gaze.gesture.is_some();
            if active && !was_active {
                gestures += 1;
            }
            was_active = active;
            assert!(
                wrap_degrees(rig.yaw - anchor_yaw).abs() < 60.0,
                "idle yaw left the N(0,12) range: {}",
                rig.yaw
            );
            assert!(
                (rig.pitch - walk_pitch).abs() < 40.0,
                "idle pitch left the walk pitch range: {}",
                rig.pitch
            );
        }
        assert!(gestures >= 5, "only {gestures} idle gestures in 2000 ticks");
    }

    #[test]
    fn walking_pitch_drift_moves_only_through_gestures() {
        let mut rig = Rig::new(9);
        let request = Look::Aim { yaw: 0., pitch: 5. };
        // Let the first pitch gesture settle.
        for _ in 0..100 {
            rig.step_mode(request, GazeMode::Walking);
        }
        let mut gestures = 0;
        let mut was_active = false;
        let mut last_pitch_only = false;
        for _ in 0..1000 {
            let look = rig.step_mode(request, GazeMode::Walking);
            let active = rig.gaze.gesture.is_some();
            // A gesture that ended this tick was running when the delta was
            // sampled, so it still counts as inside one.
            let during = was_active || active;
            if active && !was_active {
                gestures += 1;
            }
            was_active = active;
            if let Look::Delta { dyaw, dpitch } = look {
                let n_yaw = dyaw / STEP;
                let n_pitch = dpitch / STEP;
                assert!((n_yaw - n_yaw.round()).abs() < 1e-3, "dyaw {dyaw}");
                assert!((n_pitch - n_pitch.round()).abs() < 1e-3, "dpitch {dpitch}");
                let pitch_only = n_yaw == 0.0 && (1.0..=2.0).contains(&n_pitch.abs());
                if pitch_only {
                    assert!(
                        during,
                        "a pitch-only micro-delta outside a gesture: {look:?}"
                    );
                    assert!(!last_pitch_only, "two consecutive pitch-only micro-deltas");
                }
                last_pitch_only = pitch_only;
            } else {
                last_pitch_only = false;
            }
            let off_pitch = (rig.pitch - 5.).abs();
            assert!(
                off_pitch <= DRIFT_MAX_DEG + 1.0 + 1e-3,
                "pitch off by {off_pitch}"
            );
        }
        assert!(
            gestures >= 5,
            "the drift stopped moving the pitch: {gestures}"
        );
    }

    #[test]
    fn walking_never_changes_the_emitted_yaw() {
        let mut rig = Rig::new(9);
        let request = Look::Aim { yaw: 0., pitch: 5. };
        for _ in 0..1000 {
            rig.step_mode(request, GazeMode::Walking);
            assert_eq!(rig.yaw, 0.0, "the walking drift must not touch yaw");
        }
    }

    #[test]
    fn leaving_walking_drops_the_drift_without_a_tail() {
        let mut rig = Rig::new(9);
        let request = Look::Aim { yaw: 0., pitch: 5. };
        let mut drifted = false;
        for _ in 0..2000 {
            rig.step_mode(request, GazeMode::Walking);
            if (rig.pitch - 5.).abs() > 2.0 {
                drifted = true;
                break;
            }
        }
        assert!(drifted, "the walking pitch never left the request by 2 deg");

        let mut reached = None;
        for tick in 1..=20 {
            rig.step_mode(request, GazeMode::Idle);
            if wrap_degrees(rig.yaw).abs() <= 0.25 && (rig.pitch - 5.).abs() <= 0.25 {
                reached = Some(tick);
                break;
            }
        }
        assert!(
            reached.is_some(),
            "the request was not reached in 20 ticks (yaw {}, pitch {})",
            rig.yaw,
            rig.pitch
        );
    }

    #[test]
    fn idle_with_a_live_aim_never_starts_an_idle_gesture() {
        let mut rig = Rig::new(11);
        let request = Look::Aim {
            yaw: 20.,
            pitch: 5.,
        };
        for _ in 0..200 {
            rig.step_mode(request, GazeMode::Idle);
        }
        assert!(rig.gaze.gesture.is_none(), "the aim should have settled");
        assert!(rig.gaze.correction_at.is_none(), "no correction is pending");

        // Keep the aim exactly on the settled look: any gesture now would be
        // the idle gaze.
        let settled = Look::Aim {
            yaw: rig.yaw,
            pitch: rig.pitch,
        };
        for _ in 0..2000 {
            assert_eq!(
                rig.step_mode(settled, GazeMode::Idle),
                Look::None,
                "a live aim must suppress the idle gaze"
            );
        }
        assert!(
            rig.gaze.idle_next.is_none(),
            "no idle gesture may be scheduled"
        );
    }

    #[test]
    fn the_same_seed_gives_the_same_walking_and_idle_output() {
        let run = || {
            let mut rig = Rig::new(77);
            let mut output = Vec::new();
            for tick in 0..600 {
                let mode = if tick % 3 == 0 {
                    GazeMode::Walking
                } else {
                    GazeMode::Idle
                };
                let request = if tick < 100 {
                    Look::Aim {
                        yaw: 40.,
                        pitch: 3.,
                    }
                } else {
                    Look::None
                };
                output.push(rig.step_mode(request, mode));
            }
            output
        };
        assert_eq!(run(), run());
    }
}
