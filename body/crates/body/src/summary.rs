//! The run's summary: counters the driver and the process keep, and
//! `summary.json` at the end (Demo-0 step 10a).
//!
//! The summary is written twice: once when the goal settles (so the harness can
//! read the referee while the body is still connected and lingers) and once at
//! exit with the final session counters.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

/// One tick's wall time, in nanoseconds: the work inside the body's tick and
/// the period between two tick starts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TickTimes {
    pub samples: u64,
    pub work_p50_ms: f64,
    pub work_p99_ms: f64,
    pub period_p50_ms: f64,
    pub period_p99_ms: f64,
    /// Ticks whose work was over 25 ms. The period is ~50 ms by design at
    /// 20 Hz, so it is reported but never counted here.
    pub over_25ms: u64,
}

/// One stage's wall time over the run, in milliseconds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StageTimes {
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// The counters shared by the driver (in the session thread) and the process
/// (the event loop). A mutex, not atomics: the driver writes every tick and the
/// process reads a few times a second.
#[derive(Debug)]
pub struct Shared {
    pub run_id: String,
    pub skill: String,
    pub args: Vec<String>,
    /// `None` while the goal is still running.
    pub outcome: Option<String>,
    /// Client ticks observed, as `tick + 1`.
    pub ticks: u64,
    pub work_ns: Vec<u64>,
    pub period_ns: Vec<u64>,
    /// Per-stage samples, so a slow tick can be attributed without a profiler.
    pub stage_ns: BTreeMap<String, Vec<u64>>,
    pub over_25ms: u64,
    pub receipts: BTreeMap<String, u64>,
    pub reflex_activations: BTreeMap<String, u64>,
    pub deaths: u64,
    pub warnings: BTreeMap<String, u64>,
    pub session: Option<SessionSummary>,
}

impl Shared {
    pub fn new(run_id: impl Into<String>, skill: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            run_id: run_id.into(),
            skill: skill.into(),
            args,
            outcome: None,
            ticks: 0,
            work_ns: Vec::new(),
            period_ns: Vec::new(),
            stage_ns: BTreeMap::new(),
            over_25ms: 0,
            receipts: BTreeMap::new(),
            reflex_activations: BTreeMap::new(),
            deaths: 0,
            warnings: BTreeMap::new(),
            session: None,
        }
    }

    pub fn record_work(&mut self, work_ns: u64) {
        self.work_ns.push(work_ns);
        if work_ns > 25_000_000 {
            self.over_25ms = self.over_25ms.saturating_add(1);
        }
    }

    pub fn record_period(&mut self, period_ns: u64) {
        self.period_ns.push(period_ns);
    }

    pub fn record_stage(&mut self, name: &str, stage_ns: u64) {
        self.stage_ns
            .entry(name.to_owned())
            .or_default()
            .push(stage_ns);
    }
}

/// The session's own counters, copied at exit.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SessionSummary {
    pub ticks_ran: u64,
    pub physics_ticks: u64,
    pub frames: u64,
    pub frames_dropped: u64,
    pub corrections: u64,
    pub attack_clicks: u64,
    pub use_clicks: u64,
    pub world: Option<String>,
}

/// `summary.json`.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub run_id: String,
    pub goal: Goal,
    pub ticks: u64,
    pub tick_time: TickTimes,
    /// Per-stage wall time, by stage name.
    pub tick_stages: BTreeMap<String, StageTimes>,
    pub receipts: BTreeMap<String, u64>,
    pub reflex_activations: BTreeMap<String, u64>,
    pub deaths: u64,
    pub warnings: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSummary>,
}

#[derive(Debug, Serialize)]
pub struct Goal {
    pub skill: String,
    pub args: Vec<String>,
    pub outcome: String,
}

/// Build the summary from the shared counters.
pub fn summary(shared: &Shared) -> Summary {
    let mut work = shared.work_ns.clone();
    let mut period = shared.period_ns.clone();
    work.sort_unstable();
    period.sort_unstable();
    Summary {
        run_id: shared.run_id.clone(),
        goal: Goal {
            skill: shared.skill.clone(),
            args: shared.args.clone(),
            outcome: shared.outcome.clone().unwrap_or_else(|| "running".to_owned()),
        },
        ticks: shared.ticks,
        tick_time: TickTimes {
            samples: work.len() as u64,
            work_p50_ms: percentile_ms(&work, 50.0),
            work_p99_ms: percentile_ms(&work, 99.0),
            period_p50_ms: percentile_ms(&period, 50.0),
            period_p99_ms: percentile_ms(&period, 99.0),
            over_25ms: shared.over_25ms,
        },
        tick_stages: shared
            .stage_ns
            .iter()
            .map(|(name, samples)| {
                let mut sorted = samples.clone();
                sorted.sort_unstable();
                (
                    name.clone(),
                    StageTimes {
                        p50_ms: percentile_ms(&sorted, 50.0),
                        p99_ms: percentile_ms(&sorted, 99.0),
                        max_ms: sorted.last().map_or(0.0, |ns| *ns as f64 / 1_000_000.0),
                    },
                )
            })
            .collect(),
        receipts: shared.receipts.clone(),
        reflex_activations: shared.reflex_activations.clone(),
        deaths: shared.deaths,
        warnings: shared.warnings.clone(),
        session: shared.session.clone(),
    }
}

/// Write `<run_dir>/summary.json`.
pub fn write_summary(run_dir: &Path, shared: &Shared) -> io::Result<()> {
    fs::create_dir_all(run_dir)?;
    let text = serde_json::to_string_pretty(&summary(shared)).map_err(io::Error::other)?;
    fs::write(run_dir.join("summary.json"), text)
}

/// The p-th percentile of an ascending sample, in milliseconds. Nearest-rank:
/// the smallest value at or above the p-th percent of the samples.
pub fn percentile_ms(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[index] as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_nearest_rank() {
        let values = [1_000_000, 2_000_000, 3_000_000, 4_000_000];
        assert_eq!(percentile_ms(&values, 50.0), 2.0);
        assert_eq!(percentile_ms(&values, 99.0), 4.0);
        assert_eq!(percentile_ms(&[], 50.0), 0.0);
        assert_eq!(percentile_ms(&[5_000_000], 99.0), 5.0);
    }

    #[test]
    fn stage_times_are_reported_per_stage() {
        let mut shared = Shared::new("run", "goal", vec![]);
        shared.record_stage("arbiter", 2_000_000);
        shared.record_stage("arbiter", 4_000_000);
        shared.record_stage("sweep", 1_000_000);
        let stages = summary(&shared).tick_stages;
        assert_eq!(stages["arbiter"].p50_ms, 2.0);
        assert_eq!(stages["arbiter"].p99_ms, 4.0);
        assert_eq!(stages["arbiter"].max_ms, 4.0);
        assert_eq!(stages["sweep"].p99_ms, 1.0);
    }

    #[test]
    fn slow_ticks_count_work_not_the_50ms_period() {
        let mut shared = Shared::new("run", "goal", vec![]);
        shared.record_work(1_000_000);
        shared.record_period(50_000_000);
        shared.record_work(26_000_000);
        shared.record_period(49_000_000);
        assert_eq!(shared.over_25ms, 1, "a 20 Hz period is never a slow tick");
        assert_eq!(summary(&shared).goal.outcome, "running");
        assert_eq!(summary(&shared).tick_time.period_p50_ms, 49.0);
    }
}
