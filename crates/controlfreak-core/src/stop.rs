use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use crate::{CleanupStatus, MutationControl, PlatformError};

/// Called after the latch changes. Returns `false` to unsubscribe, so a listener that holds only
/// a weak reference to its owner disappears with it.
type Listener = Box<dyn Fn() -> bool + Send + Sync>;

/// A lifetime-latched stop. No client operation can reopen admission.
#[derive(Clone, Default)]
pub struct StopController(Arc<Inner>);

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    listeners: Mutex<Vec<Listener>>,
    observations: AtomicU64,
    esc_stop: AtomicBool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    User,
    Other,
}

#[derive(Default)]
struct State {
    stop_reason: Option<StopReason>,
    /// Why the server closed admission itself, such as a failed indicator. Kept for diagnosis.
    failure: Option<String>,
    session_active: bool,
    cleanup_failed: bool,
    next_id: u64,
    active: Vec<(u64, MutationControl)>,
}

impl StopController {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers a callback run after every change of session ownership, stop state or
    /// observation count, so waiters can sleep until something happens instead of polling. The
    /// callback runs on the thread that made the change and must be short and non-blocking.
    pub fn subscribe(&self, listener: impl Fn() -> bool + Send + Sync + 'static) {
        self.0
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Box::new(listener));
    }

    fn notify(&self) {
        let mut listeners = self
            .0
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        listeners.retain(|listener| listener());
    }

    /// Desktop ownership includes draining cleanup, not observation-only requests.
    pub fn set_session_active(&self, active: bool) {
        self.state().session_active = active;
        self.notify();
    }

    pub fn session_active(&self) -> bool {
        self.state().session_active
    }

    /// Records that an observation tool read the screen. It never claims a session: the count
    /// only lets an indicator show a brief "reading" state.
    pub fn note_observation(&self) {
        self.0.observations.fetch_add(1, Ordering::AcqRel);
        self.notify();
    }

    pub fn observation_count(&self) -> u64 {
        self.0.observations.load(Ordering::Acquire)
    }

    /// Whether a physical Esc is currently being watched for. It is `true` only while a session
    /// is owned and the keyboard hook is confirmed installed.
    pub fn set_esc_stop_available(&self, available: bool) {
        self.0.esc_stop.store(available, Ordering::Release);
        self.notify();
    }

    pub fn esc_stop_available(&self) -> bool {
        self.0.esc_stop.load(Ordering::Acquire)
    }

    /// Latch a human stop only while this server owns a control session.
    pub fn stop_by_user(&self) -> bool {
        let latched = {
            let mut state = self.state();
            if !state.session_active || state.stop_reason.is_some() {
                false
            } else {
                state.stop_reason = Some(StopReason::User);
                for (_, control) in &state.active {
                    control.cancel();
                }
                true
            }
        };
        if latched {
            self.notify();
        }
        latched
    }

    pub fn user_stopped(&self) -> bool {
        self.state().stop_reason == Some(StopReason::User)
    }

    /// Close admission and cancel all registered work without waiting for providers.
    pub fn stop(&self) {
        {
            let mut state = self.state();
            state.stop_reason.get_or_insert(StopReason::Other);
            for (_, control) in &state.active {
                control.cancel();
            }
        }
        self.notify();
    }

    /// Close admission because the server itself failed, keeping `reason` for diagnosis. The
    /// first reason wins; it is shown in refusals and `failure_reason`.
    pub fn stop_with_failure(&self, reason: impl Into<String>) {
        self.state().failure.get_or_insert_with(|| reason.into());
        self.stop();
    }

    /// Why the server closed admission itself, if it did.
    pub fn failure_reason(&self) -> Option<String> {
        self.state().failure.clone()
    }

    fn cleanup_failed(&self) {
        self.stop();
        self.state().cleanup_failed = true;
        self.notify();
    }

    /// Why admission is closed, or `None` while work is still admitted.
    pub fn stop_message(&self) -> Option<&'static str> {
        stop_message(self.state().stop_reason)
    }

    /// The refusal text, including the recorded failure reason when there is one.
    pub fn refusal_message(&self) -> Option<String> {
        let state = self.state();
        refusal(state.stop_reason, state.failure.as_deref())
    }

    pub fn is_stopped(&self) -> bool {
        self.state().stop_reason.is_some()
    }

    pub fn status(&self) -> &'static str {
        let state = self.state();
        if state.cleanup_failed {
            "cleanup_failed"
        } else if state.stop_reason.is_some() && !state.active.is_empty() {
            "draining"
        } else if state.stop_reason.is_some() {
            "stopped"
        } else {
            "ready"
        }
    }

    /// Register before queuing work. The guard stays with the blocking worker through cleanup.
    pub fn register(&self, control: MutationControl) -> Result<StopRegistration, PlatformError> {
        let mut state = self.state();
        if let Some(reason) = refusal(state.stop_reason, state.failure.as_deref()) {
            return Err(PlatformError::OperationFailed {
                operation: "admission".to_owned(),
                reason,
            });
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.active.push((id, control.clone()));
        Ok(StopRegistration {
            controller: self.clone(),
            id,
            control,
        })
    }
}

/// Owns cancellation and cleanup accounting for one queued or running operation.
pub struct StopRegistration {
    controller: StopController,
    id: u64,
    control: MutationControl,
}

impl StopRegistration {
    pub fn record_cleanup(&self) {
        if self.control.progress().cleanup == CleanupStatus::Unknown {
            self.controller.cleanup_failed();
        }
    }
}

impl Drop for StopRegistration {
    fn drop(&mut self) {
        {
            let mut state = self.controller.state();
            state.active.retain(|(id, _)| *id != self.id);
            if self.control.progress().cleanup == CleanupStatus::Unknown {
                state.cleanup_failed = true;
                state.stop_reason.get_or_insert(StopReason::Other);
                for (_, control) in &state.active {
                    control.cancel();
                }
            }
        }
        // A drained stop changes the status from draining to stopped.
        self.controller.notify();
    }
}

fn stop_message(reason: Option<StopReason>) -> Option<&'static str> {
    match reason? {
        StopReason::User => Some(
            "the user stopped the control session; do not resume or restart without the user's permission",
        ),
        StopReason::Other => Some("desktop work is stopped; only the user may restart the server"),
    }
}

/// How a teardown that failed is retried: delays that double from `first` up to `cap`, and a
/// fixed number of attempts, so a helper that never exits cannot keep a thread busy forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    pub first: Duration,
    pub cap: Duration,
    pub attempts: u32,
}

impl Backoff {
    /// About a minute in all: 50 ms doubling to 2 s, 30 attempts.
    pub const TEARDOWN: Self = Self {
        first: Duration::from_millis(50),
        cap: Duration::from_secs(2),
        attempts: 30,
    };

    /// The pause after the `failed`-th failure, counting from one.
    pub fn delay(self, failed: u32) -> Duration {
        let doublings = failed.saturating_sub(1).min(16);
        self.first.saturating_mul(1 << doublings).min(self.cap)
    }
}

/// Runs `operation` until it succeeds or `backoff.attempts` attempts have failed, calling `sleep`
/// between them. Returns the last error, so the caller can log it and give up.
pub fn retry_with_backoff<T, E>(
    backoff: Backoff,
    mut operation: impl FnMut() -> Result<T, E>,
    mut sleep: impl FnMut(Duration),
) -> Result<T, E> {
    let mut failed = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                failed += 1;
                if failed >= backoff.attempts {
                    return Err(error);
                }
                sleep(backoff.delay(failed));
            }
        }
    }
}

fn refusal(reason: Option<StopReason>, failure: Option<&str>) -> Option<String> {
    let message = stop_message(reason)?;
    Some(match failure {
        Some(failure) => format!("{message} ({failure})"),
        None => message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_stop_requires_ownership_cancels_work_and_preserves_reason_after_cleanup() {
        let stop = StopController::default();
        assert!(!stop.stop_by_user());
        assert!(!stop.is_stopped());
        let control = MutationControl::default();
        let guard = stop.register(control.clone()).unwrap();
        stop.set_session_active(true);
        assert!(stop.stop_by_user());
        assert!(!stop.stop_by_user());
        stop.stop();
        assert!(control.is_cancelled());
        assert_eq!(stop.status(), "draining");
        drop(guard);
        stop.set_session_active(false);
        assert_eq!(stop.status(), "stopped");
        assert!(stop.user_stopped());
        let error = stop.register(MutationControl::default()).err().unwrap();
        assert!(error.to_string().contains("the user stopped"));
    }

    #[test]
    fn client_stop_is_not_reported_as_a_human_stop() {
        let stop = StopController::default();
        stop.set_session_active(true);
        stop.stop();
        assert!(!stop.stop_by_user());
        assert!(!stop.user_stopped());
    }

    #[test]
    fn stop_cancels_queued_work_and_stays_latched_after_drain() {
        let stop = StopController::default();
        let control = MutationControl::default();
        let guard = stop.register(control.clone()).unwrap();
        stop.stop();
        stop.stop();
        assert!(control.is_cancelled());
        assert_eq!(stop.status(), "draining");
        assert!(stop.register(MutationControl::default()).is_err());
        drop(guard);
        assert_eq!(stop.status(), "stopped");
        assert!(stop.register(MutationControl::default()).is_err());
    }

    #[test]
    fn uncertain_cleanup_closes_admission() {
        let stop = StopController::default();
        let control = MutationControl::default();
        let guard = stop.register(control.clone()).unwrap();
        control.cleanup_status(CleanupStatus::Unknown);
        drop(guard);
        assert_eq!(stop.status(), "cleanup_failed");
        assert!(stop.is_stopped());
    }

    #[test]
    fn a_failure_reason_is_kept_and_shown_in_every_refusal() {
        let stop = StopController::default();
        assert_eq!(stop.failure_reason(), None);
        assert_eq!(stop.refusal_message(), None);
        stop.stop_with_failure("the session tab failed: no monitors");
        stop.stop_with_failure("a second failure does not replace the first");
        assert!(stop.is_stopped());
        assert!(!stop.user_stopped(), "a server failure is not a user stop");
        assert_eq!(
            stop.failure_reason().as_deref(),
            Some("the session tab failed: no monitors")
        );
        let refusal = stop.refusal_message().unwrap();
        assert!(refusal.contains("only the user may restart"));
        assert!(refusal.contains("no monitors"));
        let error = stop.register(MutationControl::default()).err().unwrap();
        assert!(error.to_string().contains("no monitors"));
    }

    #[test]
    fn listeners_hear_every_change_and_unsubscribe_by_returning_false() {
        let stop = StopController::default();
        let calls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&calls);
        stop.subscribe(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            true
        });
        let once = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&once);
        stop.subscribe(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            false
        });
        stop.set_session_active(true);
        stop.note_observation();
        stop.set_esc_stop_available(true);
        assert!(stop.stop_by_user());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(once.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn teardown_retries_back_off_and_give_up_after_a_bound() {
        let backoff = Backoff::TEARDOWN;
        let delays: Vec<u128> = (1..=8)
            .map(|failed| backoff.delay(failed).as_millis())
            .collect();
        assert_eq!(delays, [50, 100, 200, 400, 800, 1_600, 2_000, 2_000]);
        assert_eq!(backoff.delay(u32::MAX), backoff.cap, "never overflows");

        let mut tries = 0;
        let mut slept = Duration::ZERO;
        let result: Result<(), &str> = retry_with_backoff(
            backoff,
            || {
                tries += 1;
                Err("helper still running")
            },
            |delay| slept += delay,
        );
        assert_eq!(result, Err("helper still running"));
        assert_eq!(tries, backoff.attempts, "gives up");
        assert!(slept < Duration::from_secs(70), "a bounded wait: {slept:?}");

        let mut tries = 0;
        let result = retry_with_backoff(
            backoff,
            || {
                tries += 1;
                if tries == 3 {
                    Ok(tries)
                } else {
                    Err("not yet")
                }
            },
            |_| {},
        );
        assert_eq!(result, Ok(3), "stops at the first success");
    }

    #[test]
    fn observations_are_counted_without_claiming_a_session() {
        let stop = StopController::default();
        assert_eq!(stop.observation_count(), 0);
        stop.note_observation();
        stop.note_observation();
        assert_eq!(stop.observation_count(), 2);
        assert!(!stop.session_active());
        assert!(!stop.stop_by_user(), "reading the screen is not ownership");
        assert!(!stop.esc_stop_available());
    }
}
