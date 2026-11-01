//! How fast we tell the server to send chunks.
//!
//! Vanilla answers every `chunk_batch_finished` with `chunk_batch_received`,
//! carrying the number of chunks per tick it wants next. The value is derived
//! from how long the last batches took to arrive, clamped so one slow batch
//! cannot collapse it. This is `azalea-client`'s `ChunkBatchInfo`
//! (`azalea-client/src/plugins/chunks.rs:108-129`) ported without the ECS.

use std::time::{Duration, Instant};

/// Vanilla's target: this many nanoseconds of chunk work per tick.
const NANOS_PER_CHUNK_BUDGET: f64 = 7_000_000.0;
/// The duration per chunk we assume before the first batch arrives.
const INITIAL_DURATION_PER_CHUNK: Duration = Duration::from_millis(2);
/// How many past batches the average keeps before new samples weigh more.
const MAX_OLD_SAMPLES_WEIGHT: u32 = 49;

/// Arrival-time accounting for chunk batches.
#[derive(Debug)]
pub(crate) struct ChunkBatchInfo {
    start: Instant,
    aggregated_duration_per_chunk: Duration,
    old_samples_weight: u32,
}

impl Default for ChunkBatchInfo {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            aggregated_duration_per_chunk: INITIAL_DURATION_PER_CHUNK,
            old_samples_weight: 1,
        }
    }
}

impl ChunkBatchInfo {
    /// A `chunk_batch_start` packet arrived.
    pub(crate) fn start(&mut self) {
        self.start = Instant::now();
    }

    /// A `chunk_batch_finished` packet arrived; returns the desired chunks per tick.
    pub(crate) fn finished(&mut self, batch_size: u32) -> f32 {
        if batch_size != 0 {
            let duration_per_chunk = self.start.elapsed() / batch_size;
            let clamped = duration_per_chunk.clamp(
                self.aggregated_duration_per_chunk / 3,
                self.aggregated_duration_per_chunk * 3,
            );
            self.aggregated_duration_per_chunk = (self.aggregated_duration_per_chunk
                * self.old_samples_weight
                + clamped)
                / (self.old_samples_weight + 1);
            self.old_samples_weight = u32::min(MAX_OLD_SAMPLES_WEIGHT, self.old_samples_weight + 1);
        }
        self.desired_chunks_per_tick()
    }

    fn desired_chunks_per_tick(&self) -> f32 {
        (NANOS_PER_CHUNK_BUDGET / self.aggregated_duration_per_chunk.as_nanos() as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_any_batch_it_asks_for_the_vanilla_default() {
        let info = ChunkBatchInfo::default();
        // 7_000_000 ns of budget over the 2 ms first guess.
        assert_eq!(info.desired_chunks_per_tick(), 3.5);
    }

    #[test]
    fn an_empty_batch_keeps_the_estimate() {
        let mut info = ChunkBatchInfo::default();
        assert_eq!(info.finished(0), 3.5);
    }

    #[test]
    fn the_estimate_stays_inside_the_clamp() {
        let mut info = ChunkBatchInfo::default();
        info.start();
        // 20 ms for one chunk is clamped to three times the 2 ms estimate, so the
        // average of 2 ms and 6 ms is 4 ms: 7_000_000 ns / 4_000_000 ns = 1.75.
        std::thread::sleep(Duration::from_millis(20));
        let desired = info.finished(1);
        assert!((desired - 1.75).abs() < 0.01, "desired {desired}");
    }
}
