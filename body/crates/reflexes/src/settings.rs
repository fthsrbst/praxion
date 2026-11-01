//! The typed reflex settings (`body/config/reflexes.toml`).
//!
//! The file is the user's: every value here is a plain typed field with a
//! documented default, loaded once and validated before any reflex runs. A
//! reflex never waits for the network, so these numbers are the whole tuning
//! surface of the survival policy (session 7).
//!
//! Loading fails with a message naming the field for a missing field, a wrong
//! type, an unknown key, or a value outside its range. The TOML file carries
//! the same defaults in comments, so a user can copy a line without opening the
//! code.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

/// How far a reflex may push before the arbiter gives the body back. Also the
/// bound the engine enforces on every reflex routine.
pub const MAX_REFLEX_TICKS_LIMIT: u32 = 1_200;

/// One tick is 50 ms; three ticks are the human reaction floor (architecture
/// §3). Kept here so the latency gate and its tests name the same number.
pub const HUMAN_FLOOR_TICKS: u64 = 3;

/// The typed reflex policy.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Health at or below which the arbiter wakes the brain.
    pub critical_health: f32,
    /// The melee reflex fights only above this health; at or below it flees.
    pub fight_min_health: f32,
    /// A zombie, skeleton or spider this close triggers the melee reflex.
    pub melee_range: u32,
    /// A creeper this close triggers the creeper reflex.
    pub creeper_flee_range: u32,
    /// Air ticks left at which the drowning reflex surfaces. Vanilla air is
    /// 300 ticks; 120 is six seconds of warning.
    pub air_surface_threshold: u32,
    /// Food at or below which maintenance eats at the command's next safe
    /// point.
    pub eat_soft_threshold: u32,
    /// Food at or below which the starvation reflex preempts (survival tier).
    pub eat_hard_threshold: u32,
    /// How far away from a threat the flee routine tries to get, in blocks.
    pub flee_distance: f32,
    /// Health at or below which sheltering outranks a project.
    pub shelter_health: f32,
    /// A reflex gives the body back after this many ticks even if its end
    /// condition never became true.
    pub max_reflex_ticks: u32,
    /// `k` in "gain > k x loss" for a same-tier switch, both in expected ticks.
    pub switch_gain_factor: f32,
    /// A reflex that ran longer than this wakes the brain (Demo-0 logs it).
    /// At least 1 tick.
    pub wake_after_ticks: u32,
    /// A same-tier owner may not switch again before this many ticks, so an
    /// alternating need cannot produce A-B-A within five seconds. At least 1
    /// tick: `0` would disable the dwell altogether.
    pub min_switch_ticks: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            critical_health: 6.0,
            fight_min_health: 8.0,
            melee_range: 4,
            creeper_flee_range: 8,
            air_surface_threshold: 120,
            eat_soft_threshold: 16,
            eat_hard_threshold: 6,
            flee_distance: 8.0,
            shelter_health: 10.0,
            max_reflex_ticks: 200,
            switch_gain_factor: 2.0,
            wake_after_ticks: 40,
            min_switch_ticks: 100,
        }
    }
}

/// Why a settings file did not load.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsError {
    /// The file could not be read.
    Io(String),
    /// TOML syntax, a missing field, a wrong type or an unknown key.
    Parse(String),
    /// A typed value outside its allowed range.
    Invalid {
        field: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::Io(message) => write!(f, "cannot read the reflex settings: {message}"),
            SettingsError::Parse(message) => write!(f, "invalid reflex settings: {message}"),
            SettingsError::Invalid {
                field,
                value,
                expected,
            } => write!(
                f,
                "invalid reflex setting `{field}`: {value} is out of range ({expected})"
            ),
        }
    }
}

impl std::error::Error for SettingsError {}

impl Settings {
    /// Read and validate `<repo>/body/config/reflexes.toml`, or any path.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let text = std::fs::read_to_string(path.as_ref())
            .map_err(|error| SettingsError::Io(error.to_string()))?;
        Self::from_toml(&text)
    }

    /// Parse and validate a TOML document.
    pub fn from_toml(text: &str) -> Result<Self, SettingsError> {
        let settings: Settings =
            toml::from_str(text).map_err(|error| SettingsError::Parse(error.to_string()))?;
        settings.validate()?;
        Ok(settings)
    }

    /// Check every field's range and the relationships between fields.
    pub fn validate(&self) -> Result<(), SettingsError> {
        fn health(field: &'static str, value: f32) -> Result<(), SettingsError> {
            if (0.0..=20.0).contains(&value) {
                Ok(())
            } else {
                Err(SettingsError::Invalid {
                    field,
                    value: value.to_string(),
                    expected: "0 to 20 health",
                })
            }
        }
        /// A tick count: one tick is the shortest meaningful dwell, so `0` is
        /// refused rather than silently meaning "no bound" or "no dwell".
        fn ticks(field: &'static str, value: u32, max: u32) -> Result<(), SettingsError> {
            if (1..=max).contains(&value) {
                Ok(())
            } else {
                Err(SettingsError::Invalid {
                    field,
                    value: value.to_string(),
                    expected: "a tick count of 1 to 1200",
                })
            }
        }

        health("critical_health", self.critical_health)?;
        health("fight_min_health", self.fight_min_health)?;
        health("shelter_health", self.shelter_health)?;
        if !(1..=32).contains(&self.melee_range) {
            return Err(SettingsError::Invalid {
                field: "melee_range",
                value: self.melee_range.to_string(),
                expected: "1 to 32 blocks",
            });
        }
        if !(1..=32).contains(&self.creeper_flee_range) {
            return Err(SettingsError::Invalid {
                field: "creeper_flee_range",
                value: self.creeper_flee_range.to_string(),
                expected: "1 to 32 blocks",
            });
        }
        if self.air_surface_threshold > 300 {
            return Err(SettingsError::Invalid {
                field: "air_surface_threshold",
                value: self.air_surface_threshold.to_string(),
                expected: "0 to 300 air ticks",
            });
        }
        if self.eat_soft_threshold > 20 {
            return Err(SettingsError::Invalid {
                field: "eat_soft_threshold",
                value: self.eat_soft_threshold.to_string(),
                expected: "0 to 20 food points",
            });
        }
        if self.eat_hard_threshold > 20 {
            return Err(SettingsError::Invalid {
                field: "eat_hard_threshold",
                value: self.eat_hard_threshold.to_string(),
                expected: "0 to 20 food points",
            });
        }
        if self.eat_hard_threshold > self.eat_soft_threshold {
            return Err(SettingsError::Invalid {
                field: "eat_hard_threshold",
                value: self.eat_hard_threshold.to_string(),
                expected: "at most eat_soft_threshold",
            });
        }
        if !(1.0..=64.0).contains(&self.flee_distance) {
            return Err(SettingsError::Invalid {
                field: "flee_distance",
                value: self.flee_distance.to_string(),
                expected: "1 to 64 blocks",
            });
        }
        ticks("max_reflex_ticks", self.max_reflex_ticks, MAX_REFLEX_TICKS_LIMIT)?;
        if !(0.0..=100.0).contains(&self.switch_gain_factor) {
            return Err(SettingsError::Invalid {
                field: "switch_gain_factor",
                value: self.switch_gain_factor.to_string(),
                expected: "0 to 100",
            });
        }
        ticks("wake_after_ticks", self.wake_after_ticks, MAX_REFLEX_TICKS_LIMIT)?;
        ticks("min_switch_ticks", self.min_switch_ticks, MAX_REFLEX_TICKS_LIMIT)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "\
critical_health = 6.0
fight_min_health = 8.0
melee_range = 4
creeper_flee_range = 8
air_surface_threshold = 120
eat_soft_threshold = 16
eat_hard_threshold = 6
flee_distance = 8.0
shelter_health = 10.0
max_reflex_ticks = 200
switch_gain_factor = 2.0
wake_after_ticks = 40
min_switch_ticks = 100
";

    #[test]
    fn the_shipped_file_matches_the_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/reflexes.toml");
        let shipped = Settings::load(path).expect("the shipped reflexes.toml loads");
        assert_eq!(shipped, Settings::default());
    }

    #[test]
    fn a_missing_field_names_the_field() {
        let text = MINIMAL.replace("melee_range = 4\n", "");
        let error = Settings::from_toml(&text).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("melee_range"), "{message}");
    }

    #[test]
    fn a_wrong_type_names_the_field() {
        let text = MINIMAL.replace("melee_range = 4", "melee_range = \"four\"");
        let error = Settings::from_toml(&text).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("melee_range"), "{message}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let text = format!("{MINIMAL}extra_key = 1\n");
        let error = Settings::from_toml(&text).unwrap_err();
        assert!(error.to_string().contains("extra_key"), "{error}");
    }

    #[test]
    fn an_out_of_range_value_names_the_field() {
        let text = MINIMAL.replace("melee_range = 4", "melee_range = 99");
        let error = Settings::from_toml(&text).unwrap_err();
        assert_eq!(
            error,
            SettingsError::Invalid {
                field: "melee_range",
                value: "99".to_owned(),
                expected: "1 to 32 blocks",
            }
        );
        assert!(error.to_string().contains("melee_range"), "{error}");
    }

    #[test]
    fn the_hard_food_threshold_cannot_exceed_the_soft_one() {
        let text = MINIMAL.replace("eat_hard_threshold = 6", "eat_hard_threshold = 18");
        let error = Settings::from_toml(&text).unwrap_err();
        assert!(error.to_string().contains("eat_hard_threshold"), "{error}");
    }

    #[test]
    fn the_tick_settings_refuse_zero() {
        for field in ["max_reflex_ticks", "wake_after_ticks", "min_switch_ticks"] {
            let text = MINIMAL
                .lines()
                .map(|line| {
                    if line.starts_with(&format!("{field} =")) {
                        format!("{field} = 0")
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            let error = Settings::from_toml(&text).unwrap_err();
            assert!(
                error.to_string().contains(field),
                "{field}: {error}"
            );
        }
        // One tick is fine for every one of them.
        let text = MINIMAL.replace("wake_after_ticks = 40", "wake_after_ticks = 1");
        let text = text.replace("min_switch_ticks = 100", "min_switch_ticks = 1");
        assert!(Settings::from_toml(&text).is_ok(), "{text}");
    }

    #[test]
    fn a_negative_health_is_refused() {
        let text = MINIMAL.replace("critical_health = 6.0", "critical_health = -1.0");
        let error = Settings::from_toml(&text).unwrap_err();
        assert!(error.to_string().contains("critical_health"), "{error}");
    }

    #[test]
    fn the_minimal_document_loads() {
        assert_eq!(Settings::from_toml(MINIMAL).unwrap(), Settings::default());
    }
}
