//! The polling loop of `wait_for_visual_change` and `wait_for_change_since`.
//!
//! The loop reads frames through `ComparisonFrames` and time through `PollClock`, so tests drive it
//! with synthetic frame sequences and a controlled clock instead of the screen. A wait samples the
//! region every `VISUAL_POLL_MS`, so a change that comes and goes between two polls is not seen.

use std::time::{Duration, Instant};

use controlfreak_core::{MutationControl, PlatformError};

use super::VISUAL_POLL_MS;
use super::capture::{image_difference, incompatible_frames, validate_wait_values};

const OPERATION: &str = "wait_for_change";

/// The comparison frames of one region, read one poll at a time.
pub(super) trait ComparisonFrames {
    /// The length in bytes of every frame `read` produces.
    fn frame_len(&self) -> usize;

    /// Replaces `frame` with the region's current comparison frame, reusing its allocation.
    fn read(&mut self, frame: &mut Vec<u8>) -> Result<(), PlatformError>;
}

/// The time a wait runs on.
pub(super) trait PollClock {
    fn now(&self) -> Instant;

    /// Waits for `duration`, or returns the cancellation error once `control` is cancelled.
    fn pause(&self, control: &MutationControl, duration: Duration) -> Result<(), PlatformError>;
}

pub(super) struct SystemPollClock;

impl PollClock for SystemPollClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn pause(&self, control: &MutationControl, duration: Duration) -> Result<(), PlatformError> {
        control.wait(OPERATION, duration)
    }
}

/// What the polls saw. `difference` and `currently_changed` describe the last polled frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ChangeOutcome {
    pub(super) changed_once: bool,
    pub(super) currently_changed: bool,
    pub(super) settled: bool,
    pub(super) difference: f64,
}

/// Polls `frames` until the region differed from `baseline` at least once and consecutive frames
/// then stayed still for `stable_ms`, or until `timeout_ms` passes. Settling compares consecutive
/// frames, not the baseline, so a region that changes and changes back still settles; the outcome
/// keeps "changed at some point" (`changed_once`) apart from "differs now" (`currently_changed`).
///
/// Invalid wait values and a baseline of the wrong size are refused before the first poll. The two
/// comparison buffers are swapped between polls, so the loop allocates nothing after it starts.
pub(super) fn poll_for_change(
    frames: &mut impl ComparisonFrames,
    baseline: &[u8],
    timeout_ms: u32,
    stable_ms: u32,
    difference_threshold: f64,
    control: &MutationControl,
    clock: &impl PollClock,
) -> Result<ChangeOutcome, PlatformError> {
    validate_wait_values(timeout_ms, stable_ms, difference_threshold)?;
    if baseline.is_empty() || baseline.len() != frames.frame_len() {
        return Err(incompatible_frames());
    }
    let stable_for = Duration::from_millis(u64::from(stable_ms));
    let stable_threshold = (difference_threshold / 4.0).max(0.001);
    let deadline = clock.now() + Duration::from_millis(u64::from(timeout_ms));
    let mut previous = baseline.to_owned();
    let mut frame = Vec::with_capacity(baseline.len());
    let mut difference = 0.0_f64;
    let mut changed_once = false;
    let mut settled = false;
    let mut stable_since: Option<Instant> = None;

    while clock.now() < deadline {
        // Read the time again: a worker suspended since the check must not sleep past the deadline.
        let remaining = deadline.saturating_duration_since(clock.now());
        clock.pause(
            control,
            remaining.min(Duration::from_millis(VISUAL_POLL_MS)),
        )?;
        frames.read(&mut frame)?;
        difference = image_difference(baseline, &frame)?;
        changed_once |= difference >= difference_threshold;
        if changed_once {
            if image_difference(&previous, &frame)? < stable_threshold {
                let now = clock.now();
                let since = *stable_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= stable_for {
                    settled = true;
                    break;
                }
            } else {
                stable_since = None;
            }
        }
        std::mem::swap(&mut previous, &mut frame);
    }

    Ok(ChangeOutcome {
        changed_once,
        currently_changed: difference >= difference_threshold,
        settled,
        difference,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use controlfreak_core::FailureCause;

    use super::*;

    const LEN: usize = 16;
    const A: [u8; LEN] = [0; LEN];
    const B: [u8; LEN] = [255; LEN];
    /// Differs from `A` by about 0.8 %, below the default threshold of 1 %.
    const NEAR_A: [u8; LEN] = [2; LEN];

    /// Time that moves only when the wait pauses.
    struct FakeClock {
        now: Cell<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: Cell::new(Instant::now()),
            }
        }
    }

    impl PollClock for FakeClock {
        fn now(&self) -> Instant {
            self.now.get()
        }

        fn pause(
            &self,
            control: &MutationControl,
            duration: Duration,
        ) -> Result<(), PlatformError> {
            control.check(OPERATION)?;
            self.now.set(self.now.get() + duration);
            Ok(())
        }
    }

    /// Plays `frames` one per poll and then repeats the last one, or loops when `cycle` is set.
    /// Records the buffer each poll was read into, and can cancel `control` at one poll.
    struct Sequence<'a> {
        frames: Vec<[u8; LEN]>,
        cycle: bool,
        reads: usize,
        buffers: RefCell<Vec<*const u8>>,
        cancel_at: Option<(usize, &'a MutationControl)>,
    }

    impl<'a> Sequence<'a> {
        fn new(frames: &[[u8; LEN]]) -> Self {
            Self {
                frames: frames.to_vec(),
                cycle: false,
                reads: 0,
                buffers: RefCell::default(),
                cancel_at: None,
            }
        }

        fn cycling(frames: &[[u8; LEN]]) -> Self {
            Self {
                cycle: true,
                ..Self::new(frames)
            }
        }

        fn cancelling(frames: &[[u8; LEN]], poll: usize, control: &'a MutationControl) -> Self {
            Self {
                cancel_at: Some((poll, control)),
                ..Self::new(frames)
            }
        }
    }

    impl ComparisonFrames for Sequence<'_> {
        fn frame_len(&self) -> usize {
            LEN
        }

        fn read(&mut self, frame: &mut Vec<u8>) -> Result<(), PlatformError> {
            let index = if self.cycle {
                self.reads % self.frames.len()
            } else {
                self.reads.min(self.frames.len() - 1)
            };
            self.reads += 1;
            if let Some((poll, control)) = self.cancel_at
                && poll == self.reads
            {
                control.cancel();
            }
            frame.clear();
            frame.extend_from_slice(&self.frames[index]);
            self.buffers.borrow_mut().push(frame.as_ptr());
            Ok(())
        }
    }

    fn poll(
        frames: &mut Sequence<'_>,
        timeout_ms: u32,
        stable_ms: u32,
        clock: &FakeClock,
        control: &MutationControl,
    ) -> Result<ChangeOutcome, PlatformError> {
        poll_for_change(frames, &A, timeout_ms, stable_ms, 0.01, control, clock)
    }

    #[test]
    fn a_change_that_reverts_settles_as_changed_once_but_not_currently_changed() {
        let clock = FakeClock::new();
        let mut frames = Sequence::new(&[B, B, A]);
        let outcome = poll(&mut frames, 5_000, 200, &clock, &MutationControl::default()).unwrap();
        assert_eq!(
            outcome,
            ChangeOutcome {
                changed_once: true,
                currently_changed: false,
                settled: true,
                difference: 0.0,
            }
        );
        // B held from the first to the second poll, but the change back to A at the third
        // restarted the stable time: A is still from the fourth poll and has held 200 ms by
        // the sixth.
        assert_eq!(frames.reads, 6);
    }

    #[test]
    fn a_sustained_change_settles_as_currently_changed() {
        let clock = FakeClock::new();
        let started = clock.now();
        let mut frames = Sequence::new(&[A, B]);
        let outcome = poll(&mut frames, 5_000, 200, &clock, &MutationControl::default()).unwrap();
        assert_eq!(
            outcome,
            ChangeOutcome {
                changed_once: true,
                currently_changed: true,
                settled: true,
                difference: 1.0,
            }
        );
        // B arrives at the second poll, is still at the third and has held 200 ms by the fifth.
        assert_eq!(frames.reads, 5);
        assert_eq!(clock.now() - started, Duration::from_millis(500));
    }

    #[test]
    fn continuous_animation_reaches_the_deadline_unsettled() {
        let clock = FakeClock::new();
        let started = clock.now();
        let mut frames = Sequence::cycling(&[B, A]);
        let outcome = poll(&mut frames, 1_050, 200, &clock, &MutationControl::default()).unwrap();
        assert!(outcome.changed_once);
        assert!(!outcome.settled);
        // The last poll, at the 1050 ms deadline, is the eleventh frame: B.
        assert!(outcome.currently_changed);
        assert_eq!(frames.reads, 11);
        assert_eq!(clock.now() - started, Duration::from_millis(1_050));
    }

    #[test]
    fn a_change_below_the_threshold_reports_nothing() {
        let clock = FakeClock::new();
        let mut frames = Sequence::new(&[NEAR_A]);
        let outcome = poll(&mut frames, 500, 100, &clock, &MutationControl::default()).unwrap();
        assert!(!outcome.changed_once);
        assert!(!outcome.currently_changed);
        assert!(!outcome.settled);
        assert!(outcome.difference > 0.0 && outcome.difference < 0.01);
    }

    #[test]
    fn cancellation_during_the_wait_returns_the_cancellation_error() {
        let clock = FakeClock::new();
        let control = MutationControl::default();
        let mut frames = Sequence::cancelling(&[A, B], 2, &control);
        let error = poll(&mut frames, 5_000, 1_000, &clock, &control).unwrap_err();
        assert!(
            matches!(
                error,
                PlatformError::Failed {
                    cause: FailureCause::Cancelled,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(frames.reads, 2, "the wait stops at the next pause");
    }

    #[test]
    fn an_invalid_baseline_or_wait_value_is_refused_before_the_first_poll() {
        let clock = FakeClock::new();
        let control = MutationControl::default();
        for baseline in [&[][..], &A[..LEN - 4]] {
            let mut frames = Sequence::new(&[B]);
            let error = poll_for_change(&mut frames, baseline, 500, 100, 0.01, &control, &clock)
                .unwrap_err();
            assert_eq!(error, incompatible_frames());
            assert_eq!(frames.reads, 0);
        }
        for (timeout_ms, stable_ms, threshold) in [(0, 0, 0.01), (500, 600, 0.01), (500, 0, 0.0)] {
            let mut frames = Sequence::new(&[B]);
            let error = poll_for_change(
                &mut frames,
                &A,
                timeout_ms,
                stable_ms,
                threshold,
                &control,
                &clock,
            )
            .unwrap_err();
            assert!(
                matches!(error, PlatformError::InvalidArgument { .. }),
                "{error:?}"
            );
            assert_eq!(frames.reads, 0);
        }
    }

    #[test]
    fn every_poll_reuses_the_same_two_buffers() {
        let clock = FakeClock::new();
        let mut frames = Sequence::cycling(&[B, A, NEAR_A]);
        poll(&mut frames, 2_000, 200, &clock, &MutationControl::default()).unwrap();
        let mut buffers = frames.buffers.into_inner();
        assert_eq!(buffers.len(), 20);
        buffers.sort_unstable();
        buffers.dedup();
        assert_eq!(
            buffers.len(),
            2,
            "the loop allocated a new comparison buffer"
        );
    }
}
