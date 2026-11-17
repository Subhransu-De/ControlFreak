//! When an input action stops waiting for the approved window and reads its evidence.
//!
//! The decision is kept apart from the screen reads so it can be tested with synthetic frame
//! timelines; `capture::SettleWatch` feeds it real reads.

use std::time::Duration;

use controlfreak_core::{
    ObservationMode, ObservationOptions, SettleOutcome,
    limits::{DEFAULT_QUIET_SETTLE_MS, DEFAULT_SETTLE_MS},
};

/// How long the window must stay identical to count as settled. Applications often repaint in
/// phases, for example a pressed state, a busy state and then the result, and menus fade in over
/// several frames; live tests saw gaps of 65 ms between phases, and a frame taken in such a gap
/// was the stale screenshot clients then captured again.
const STABLE: Duration = Duration::from_millis(150);
/// How long an action that returns a screenshot waits for the window to start responding. A
/// click that opens a menu or starts navigation can take a few hundred milliseconds to paint.
const SCREENSHOT_RESPONSE: Duration = Duration::from_millis(500);
/// How long an action without a screenshot, such as a `run_actions` step before the last,
/// waits for the window to start responding.
const QUIET_RESPONSE: Duration = Duration::from_millis(150);
/// Reads are frequent at first, where most responses land, then slower to avoid busy work.
const FAST_POLL: Duration = Duration::from_millis(4);
const FAST_POLL_SPAN: Duration = Duration::from_millis(150);
const SLOW_POLL: Duration = Duration::from_millis(33);

/// Limits of one settle wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SettlePolicy {
    /// No visible change by then means the window did not respond.
    pub(super) response: Duration,
    pub(super) stable: Duration,
    /// The wait never runs longer than this.
    pub(super) budget: Duration,
}

impl SettlePolicy {
    /// The policy for an action's observation options, or `None` when `settle_ms` is 0 and the
    /// action keeps its short fixed wait.
    pub(super) fn for_observation(options: &ObservationOptions) -> Option<Self> {
        let (response, default_budget) = if options.mode == ObservationMode::Screenshot {
            (SCREENSHOT_RESPONSE, DEFAULT_SETTLE_MS)
        } else {
            (QUIET_RESPONSE, DEFAULT_QUIET_SETTLE_MS)
        };
        let budget = Duration::from_millis(u64::from(options.settle_ms.unwrap_or(default_budget)));
        (!budget.is_zero()).then_some(Self {
            response: response.min(budget),
            stable: STABLE,
            budget,
        })
    }

    /// The pause before the next read, `elapsed` after the input.
    pub(super) fn poll_interval(&self, elapsed: Duration) -> Duration {
        let interval = if elapsed < FAST_POLL_SPAN {
            FAST_POLL
        } else {
            SLOW_POLL
        };
        interval.min(self.budget.saturating_sub(elapsed))
    }
}

/// Follows one settle wait read by read.
#[derive(Debug)]
pub(super) struct Settler {
    policy: SettlePolicy,
    /// When the latest change was read, once the window has responded.
    changed_at: Option<Duration>,
}

impl Settler {
    pub(super) const fn new(policy: SettlePolicy) -> Self {
        Self {
            policy,
            changed_at: None,
        }
    }

    /// Whether the window has responded, which decides what the next read is compared with:
    /// before a response, the frame before the input; after it, the latest changed frame.
    pub(super) const fn responded(&self) -> bool {
        self.changed_at.is_some()
    }

    /// Takes a read made `elapsed` after the input and whether it differs from the frame it is
    /// compared with. Returns the outcome once the wait is over.
    pub(super) fn read(&mut self, elapsed: Duration, changed: bool) -> Option<SettleOutcome> {
        if changed {
            self.changed_at = Some(elapsed);
        } else {
            match self.changed_at {
                None if elapsed >= self.policy.response => return Some(SettleOutcome::Unchanged),
                Some(since) if elapsed.saturating_sub(since) >= self.policy.stable => {
                    return Some(SettleOutcome::Settled);
                }
                _ => {}
            }
        }
        self.out_of_time(elapsed)
    }

    fn out_of_time(&self, elapsed: Duration) -> Option<SettleOutcome> {
        (elapsed >= self.policy.budget).then_some(if self.responded() {
            SettleOutcome::StillChanging
        } else {
            SettleOutcome::Unchanged
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn options(mode: ObservationMode, settle_ms: Option<u32>) -> ObservationOptions {
        ObservationOptions {
            mode,
            settle_ms,
            ..ObservationOptions::default()
        }
    }

    fn screenshot_policy() -> SettlePolicy {
        SettlePolicy::for_observation(&ObservationOptions::default()).unwrap()
    }

    /// Feeds reads every `step` ms, where `changed(t)` says whether the read at `t` differs from
    /// its comparison frame, and returns the outcome and when it was reached.
    fn run(
        policy: SettlePolicy,
        step: u64,
        changed: impl Fn(u64) -> bool,
    ) -> (SettleOutcome, Duration) {
        let mut settler = Settler::new(policy);
        let mut elapsed = 0;
        loop {
            elapsed += step;
            if let Some(outcome) = settler.read(ms(elapsed), changed(elapsed)) {
                return (outcome, ms(elapsed));
            }
            assert!(elapsed < 60_000, "the wait never ended");
        }
    }

    #[test]
    fn the_policy_follows_the_observation_mode_and_settle_ms() {
        assert_eq!(
            screenshot_policy(),
            SettlePolicy {
                response: SCREENSHOT_RESPONSE,
                stable: STABLE,
                budget: ms(u64::from(DEFAULT_SETTLE_MS)),
            }
        );
        for mode in [ObservationMode::None, ObservationMode::Metadata] {
            assert_eq!(
                SettlePolicy::for_observation(&options(mode, None)),
                Some(SettlePolicy {
                    response: QUIET_RESPONSE,
                    stable: STABLE,
                    budget: ms(u64::from(DEFAULT_QUIET_SETTLE_MS)),
                })
            );
        }
        let short = SettlePolicy::for_observation(&options(ObservationMode::Screenshot, Some(300)))
            .unwrap();
        assert_eq!((short.response, short.budget), (ms(300), ms(300)));
        let long =
            SettlePolicy::for_observation(&options(ObservationMode::None, Some(5_000))).unwrap();
        assert_eq!((long.response, long.budget), (QUIET_RESPONSE, ms(5_000)));
        assert_eq!(
            SettlePolicy::for_observation(&options(ObservationMode::Screenshot, Some(0))),
            None
        );
    }

    #[test]
    fn a_response_that_holds_still_settles_after_the_stillness_window() {
        // The window repaints at 40 ms and then stays the same.
        let (outcome, at) = run(screenshot_policy(), 10, |t| t == 40);
        assert_eq!(outcome, SettleOutcome::Settled);
        assert_eq!(at, ms(40) + STABLE);
    }

    #[test]
    fn a_second_phase_inside_the_stillness_window_extends_the_wait() {
        // A pressed state at 20 ms, then a menu painted at 120 ms.
        let (outcome, at) = run(screenshot_policy(), 10, |t| t == 20 || t == 120);
        assert_eq!(outcome, SettleOutcome::Settled);
        assert_eq!(at, ms(120) + STABLE);
    }

    #[test]
    fn a_window_that_never_responds_is_reported_unchanged_at_the_response_limit() {
        // A caret blink stays below the changed-pixel threshold, so reads report no change.
        let (outcome, at) = run(screenshot_policy(), 4, |_| false);
        assert_eq!(
            (outcome, at),
            (SettleOutcome::Unchanged, SCREENSHOT_RESPONSE)
        );
        let quiet = SettlePolicy::for_observation(&options(ObservationMode::None, None)).unwrap();
        let (outcome, at) = run(quiet, 2, |_| false);
        assert_eq!((outcome, at), (SettleOutcome::Unchanged, QUIET_RESPONSE));
    }

    #[test]
    fn a_response_after_the_budget_is_still_changing() {
        let policy =
            SettlePolicy::for_observation(&options(ObservationMode::None, Some(100))).unwrap();
        let (outcome, at) = run(policy, 4, |t| t == 100);
        assert_eq!((outcome, at), (SettleOutcome::StillChanging, ms(100)));
    }

    #[test]
    fn a_window_that_keeps_changing_is_reported_still_changing_at_the_budget() {
        let (outcome, at) = run(screenshot_policy(), 10, |t| t >= 100);
        assert_eq!(outcome, SettleOutcome::StillChanging);
        assert_eq!(at, ms(u64::from(DEFAULT_SETTLE_MS)));
    }

    #[test]
    fn a_late_response_still_settles_within_the_budget() {
        // Navigation that paints at 450 ms, just inside the response limit.
        let (outcome, at) = run(screenshot_policy(), 10, |t| t == 450);
        assert_eq!((outcome, at), (SettleOutcome::Settled, ms(450) + STABLE));
    }

    #[test]
    fn polling_slows_after_the_first_responses_and_never_overruns_the_budget() {
        let policy = screenshot_policy();
        assert_eq!(policy.poll_interval(ms(0)), FAST_POLL);
        assert_eq!(policy.poll_interval(ms(149)), FAST_POLL);
        assert_eq!(policy.poll_interval(ms(150)), SLOW_POLL);
        assert_eq!(policy.poll_interval(ms(1_990)), ms(10));
        assert_eq!(policy.poll_interval(ms(2_500)), Duration::ZERO);
    }
}
