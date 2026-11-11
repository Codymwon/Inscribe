//! 60Hz telemetry emission and moving-window progress tracking.

use crate::progress::{FlashPhase, FlashProgress};
use std::time::{Duration, Instant};

/// Real-time progress and speed calculator with 60Hz telemetry throttling.
pub struct ProgressTracker {
    pass_start_time: Instant,
    last_emit_time: Instant,
    last_speed_time: Instant,
    last_speed_bytes: u64,
    emit_interval: Duration,
    phase: FlashPhase,
    current_pass: u8,
    total_passes: u8,
    bytes_processed: u64,
    total_bytes: Option<u64>,
    current_speed_bps: f64,
}

impl ProgressTracker {
    /// Initialize a progress tracker configured for a specific telemetry frequency (e.g. 60Hz).
    #[must_use]
    pub fn new(total_bytes: Option<u64>, total_passes: u8, rate_hz: u32) -> Self {
        let now = Instant::now();
        let rate = rate_hz.max(1);
        let emit_interval = Duration::from_nanos(1_000_000_000 / rate as u64);

        Self {
            pass_start_time: now,
            last_emit_time: now - emit_interval, // force first emit
            last_speed_time: now,
            last_speed_bytes: 0,
            emit_interval,
            phase: FlashPhase::Initializing,
            current_pass: 1,
            total_passes,
            bytes_processed: 0,
            total_bytes,
            current_speed_bps: 0.0,
        }
    }

    /// Transition to a new execution phase and pass index, resetting pass timing.
    pub fn set_phase(&mut self, phase: FlashPhase, current_pass: u8, total_bytes: Option<u64>) {
        let now = Instant::now();
        self.phase = phase;
        self.current_pass = current_pass;
        self.total_bytes = total_bytes;
        self.bytes_processed = 0;
        self.pass_start_time = now;
        self.last_speed_time = now;
        self.last_speed_bytes = 0;
        self.current_speed_bps = 0.0;
    }

    /// Ingest processed bytes and update instantaneous throughput calculation.
    pub fn update_progress(&mut self, bytes_processed: u64) {
        self.bytes_processed = bytes_processed;
        let now = Instant::now();
        let dt = now.duration_since(self.last_speed_time).as_secs_f64();

        // Sample speed every ~50ms for smooth moving throughput
        if dt >= 0.05 {
            let delta = self.bytes_processed.saturating_sub(self.last_speed_bytes);
            let instant_speed = delta as f64 / dt;
            self.current_speed_bps = if self.current_speed_bps == 0.0 {
                instant_speed
            } else {
                0.6 * instant_speed + 0.4 * self.current_speed_bps
            };
            self.last_speed_time = now;
            self.last_speed_bytes = self.bytes_processed;
        }
    }

    /// Check whether sufficient time has elapsed since the last telemetry emission (60Hz rate).
    #[must_use]
    pub fn should_emit(&self) -> bool {
        self.last_emit_time.elapsed() >= self.emit_interval
    }

    /// Assemble instantaneous `FlashProgress` snapshot.
    #[must_use]
    pub fn current_progress(&self) -> FlashProgress {
        let fraction = self.total_bytes.map(|total| {
            if total == 0 {
                1.0
            } else {
                ((self.bytes_processed as f64 / total as f64).min(1.0)) as f32
            }
        });

        let eta_seconds = if let (Some(total), speed) = (self.total_bytes, self.current_speed_bps) {
            if speed > 1024.0 && self.bytes_processed < total {
                let remaining = total - self.bytes_processed;
                Some((remaining as f64 / speed).round() as u64)
            } else if self.bytes_processed >= total {
                Some(0)
            } else {
                None
            }
        } else {
            None
        };

        FlashProgress {
            phase: self.phase,
            current_pass: self.current_pass,
            total_passes: self.total_passes,
            bytes_processed: self.bytes_processed,
            total_bytes: self.total_bytes,
            bytes_per_second: self.current_speed_bps,
            eta_seconds,
            fraction,
        }
    }

    /// Emit progress over channel if interval elapsed or if forced.
    pub fn emit(&mut self, tx: &crossbeam_channel::Sender<FlashProgress>, force: bool) {
        if force || self.should_emit() {
            let progress = self.current_progress();
            let _ = tx.send(progress);
            self.last_emit_time = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_tracker_eta_and_fractions() {
        let mut tracker = ProgressTracker::new(Some(100_000_000), 2, 60);
        tracker.set_phase(FlashPhase::Flashing, 1, Some(100_000_000));
        tracker.update_progress(50_000_000);

        let p = tracker.current_progress();
        assert_eq!(p.phase, FlashPhase::Flashing);
        assert_eq!(p.current_pass, 1);
        assert_eq!(p.total_passes, 2);
        assert_eq!(p.bytes_processed, 50_000_000);
        assert_eq!(p.total_bytes, Some(100_000_000));
        assert_eq!(p.fraction, Some(0.5));
        assert_eq!(p.percentage(), Some(50.0));
    }

    #[test]
    fn test_progress_tracker_indeterminate_mode() {
        let mut tracker = ProgressTracker::new(None, 2, 60);
        tracker.set_phase(FlashPhase::Flashing, 1, None);
        tracker.update_progress(12345);

        let p = tracker.current_progress();
        assert_eq!(p.fraction, None);
        assert_eq!(p.percentage(), None);
        assert_eq!(p.eta_seconds, None);
        assert_eq!(p.bytes_processed, 12345);
    }
}
