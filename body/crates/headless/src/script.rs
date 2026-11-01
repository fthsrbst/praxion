//! The scripted frame source: a JSON list of `{ticks, held, look}` steps.
//!
//! This is the binary's test driver until the decision layers exist. One step
//! lasts `ticks` client ticks and holds its controls for all of them; a step's
//! `look` is applied every tick it lasts (`delta` is per tick, `aim` is
//! absolute). `clicks` are delivered on the step's first tick only, so a long
//! step does not click once per tick.
//!
//! ```json
//! [
//!   {"ticks": 20, "held": {"forward": true}},
//!   {"ticks": 5,  "look": {"aim": [90, 0]}},
//!   {"ticks": 1,  "held": {"attack": false}, "clicks": ["attack"]}
//! ]
//! ```
//!
//! After the last step the runner keeps producing idle frames (`held` all up,
//! no look change), which releases every control.

use std::path::Path;

use serde::Deserialize;

use crate::frame::{Click, ControlFrame, Held, Look};

/// One scripted step.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// How many client ticks this step lasts; at least 1.
    pub ticks: u32,
    #[serde(default)]
    pub held: Held,
    #[serde(default)]
    pub look: Option<ScriptLook>,
    /// Clicks delivered on the step's first tick.
    #[serde(default)]
    pub clicks: Vec<Click>,
}

/// A look instruction in a script step.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLook {
    /// `[dyaw, dpitch]` in degrees, per tick.
    Delta(f32, f32),
    /// `[yaw, pitch]` in degrees, absolute.
    Aim(f32, f32),
}

impl From<ScriptLook> for Look {
    fn from(look: ScriptLook) -> Self {
        match look {
            ScriptLook::Delta(dyaw, dpitch) => Look::Delta { dyaw, dpitch },
            ScriptLook::Aim(yaw, pitch) => Look::Aim { yaw, pitch },
        }
    }
}

/// A parsed script.
#[derive(Clone, Debug)]
pub struct Script {
    steps: Vec<Step>,
    total_ticks: u64,
}

impl Script {
    /// Parse a script from its JSON text. Empty scripts and zero-tick steps are
    /// refused: both are almost certainly a mistake in the driver.
    pub fn parse(json: &str) -> Result<Self, String> {
        let steps: Vec<Step> =
            serde_json::from_str(json).map_err(|error| format!("script: {error}"))?;
        if steps.is_empty() {
            return Err("script: no steps".to_owned());
        }
        for (index, step) in steps.iter().enumerate() {
            if step.ticks == 0 {
                return Err(format!("script: step {} has 0 ticks", index + 1));
            }
        }
        let total_ticks = steps.iter().map(|step| u64::from(step.ticks)).sum();
        Ok(Self { steps, total_ticks })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let json = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        Self::parse(&json)
    }

    /// How many ticks the script lasts before it goes idle.
    pub fn total_ticks(&self) -> u64 {
        self.total_ticks
    }
}

/// Walks a [`Script`] one tick at a time.
#[derive(Clone, Debug)]
pub struct Runner {
    script: Script,
    step: usize,
    remaining: u32,
    first_tick: bool,
}

impl Runner {
    pub fn new(script: Script) -> Self {
        Self {
            script,
            step: 0,
            remaining: 0,
            first_tick: false,
        }
    }

    /// The frame for `tick`. Always returns a frame; after the script it is
    /// idle, so the body's controls are released.
    pub fn frame(&mut self, tick: u64, generation: u32) -> ControlFrame {
        if self.remaining == 0 {
            let Some(step) = self.script.steps.get(self.step) else {
                return ControlFrame::new(tick, generation, Held::NONE);
            };
            self.remaining = step.ticks;
            self.first_tick = true;
        }

        let step = &self.script.steps[self.step];
        let mut frame = ControlFrame::new(tick, generation, step.held);
        frame.look = step.look.map(Look::from).unwrap_or(Look::None);
        if self.first_tick {
            frame.clicks = step.clicks.clone();
            self.first_tick = false;
        }

        self.remaining -= 1;
        if self.remaining == 0 {
            self.step += 1;
        }

        frame
    }

    /// Whether the last step has been handed out.
    pub fn finished(&self) -> bool {
        self.step >= self.script.steps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_lasts_its_ticks_and_holds_its_controls() {
        let script = Script::parse(r#"[{"ticks": 2, "held": {"forward": true}}]"#).unwrap();
        let mut runner = Runner::new(script);
        for tick in 0..2 {
            let frame = runner.frame(tick, 7);
            assert_eq!(frame.tick, tick);
            assert_eq!(frame.generation, 7);
            assert!(frame.held.forward);
        }
        assert!(runner.finished());
    }

    #[test]
    fn clicks_arrive_once_per_step() {
        let script = Script::parse(
            r#"[{"ticks": 3, "clicks": ["attack", "use"]}, {"ticks": 1, "clicks": ["attack"]}]"#,
        )
        .unwrap();
        let mut runner = Runner::new(script);
        assert_eq!(runner.frame(0, 0).clicks.len(), 2);
        assert!(runner.frame(1, 0).clicks.is_empty());
        assert!(runner.frame(2, 0).clicks.is_empty());
        assert_eq!(runner.frame(3, 0).clicks, vec![Click::Attack]);
    }

    #[test]
    fn an_aim_and_a_delta_keep_their_meaning() {
        let script = Script::parse(
            r#"[{"ticks": 1, "look": {"aim": [90, 0]}},
                {"ticks": 1, "look": {"delta": [5, -10]}}]"#,
        )
        .unwrap();
        let mut runner = Runner::new(script);
        assert_eq!(
            runner.frame(0, 0).look,
            Look::Aim {
                yaw: 90.0,
                pitch: 0.0
            }
        );
        assert_eq!(
            runner.frame(1, 0).look,
            Look::Delta {
                dyaw: 5.0,
                dpitch: -10.0
            }
        );
    }

    #[test]
    fn after_the_last_step_the_runner_is_idle_forever() {
        let script = Script::parse(r#"[{"ticks": 1, "held": {"jump": true}}]"#).unwrap();
        let mut runner = Runner::new(script);
        assert!(runner.frame(0, 0).held.jump);
        for tick in 1..5 {
            let frame = runner.frame(tick, 0);
            assert_eq!(frame.tick, tick);
            assert_eq!(frame.held, Held::NONE);
            assert_eq!(frame.look, Look::None);
        }
    }

    #[test]
    fn an_empty_script_or_a_zero_tick_step_is_refused() {
        assert!(Script::parse("[]").is_err());
        assert!(Script::parse(r#"[{"ticks": 0}]"#).is_err());
        assert!(Script::parse(r#"[{"ticks": 1, "held": {"walk": true}}]"#).is_err());
    }
}
