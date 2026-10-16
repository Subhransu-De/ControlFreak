//! Content-free per-stage timing for one backend operation.
//!
//! Backends record named stages on the worker thread that runs the operation. The protocol
//! layer resets the recorder before the operation and takes the totals afterwards. Stages can
//! nest, so a parent stage such as `observe` includes its `capture` and `encode_png` children.

use std::{cell::RefCell, time::Duration, time::Instant};

/// Bounds the distinct stage names kept for one operation.
const MAX_STAGES: usize = 32;

/// Total time and call count for one named stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTiming {
    pub stage: &'static str,
    pub micros: u64,
    pub calls: u32,
}

thread_local! {
    static STAGES: RefCell<Vec<StageTiming>> = const { RefCell::new(Vec::new()) };
}

/// Clears stages left on this worker thread by an earlier operation.
pub fn reset() {
    STAGES.with_borrow_mut(Vec::clear);
}

/// Returns and clears the stages recorded on this thread, in first-recorded order.
#[must_use]
pub fn take() -> Vec<StageTiming> {
    STAGES.with_borrow_mut(std::mem::take)
}

/// Adds one measured call to a stage total.
pub fn record(stage: &'static str, elapsed: Duration) {
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    STAGES.with_borrow_mut(|stages| {
        if let Some(existing) = stages.iter_mut().find(|entry| entry.stage == stage) {
            existing.micros = existing.micros.saturating_add(micros);
            existing.calls = existing.calls.saturating_add(1);
        } else if stages.len() < MAX_STAGES {
            stages.push(StageTiming {
                stage,
                micros,
                calls: 1,
            });
        }
    });
}

/// Runs `operation` and records its duration under `stage`, including when it fails.
pub fn time<T>(stage: &'static str, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    record(stage, started.elapsed());
    result
}

/// Records the time from creation to drop under one stage name.
#[must_use = "the stage is recorded when this timer is dropped"]
pub struct StageTimer {
    stage: &'static str,
    started: Instant,
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        record(self.stage, self.started.elapsed());
    }
}

/// Starts timing a stage that ends when the returned timer goes out of scope.
pub fn start(stage: &'static str) -> StageTimer {
    StageTimer {
        stage,
        started: Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_accumulate_per_name_and_are_taken_once() {
        reset();
        record("capture", Duration::from_micros(1_500));
        record("encode_png", Duration::from_micros(700));
        record("capture", Duration::from_micros(500));
        assert_eq!(time("settle", || 7), 7);
        let stages = take();
        assert_eq!(stages.len(), 3);
        assert_eq!(
            stages[0],
            StageTiming {
                stage: "capture",
                micros: 2_000,
                calls: 2
            }
        );
        assert_eq!(stages[1].stage, "encode_png");
        assert_eq!(stages[2].stage, "settle");
        assert!(take().is_empty());
    }

    #[test]
    fn distinct_stage_names_are_bounded() {
        const NAMES: [&str; 40] = [
            "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "s12", "s13",
            "s14", "s15", "s16", "s17", "s18", "s19", "s20", "s21", "s22", "s23", "s24", "s25",
            "s26", "s27", "s28", "s29", "s30", "s31", "s32", "s33", "s34", "s35", "s36", "s37",
            "s38", "s39",
        ];
        reset();
        for name in NAMES {
            record(name, Duration::from_micros(1));
        }
        assert_eq!(take().len(), MAX_STAGES);
    }
}
