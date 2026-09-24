use std::sync::{Arc, Mutex};

use crate::{CleanupStatus, MutationControl, PlatformError};

/// A lifetime-latched stop. No client operation can reopen admission.
#[derive(Clone, Default)]
pub struct StopController(Arc<Mutex<State>>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopReason {
    User,
    Other,
}

#[derive(Default)]
struct State {
    stop_reason: Option<StopReason>,
    session_active: bool,
    cleanup_failed: bool,
    next_id: u64,
    active: Vec<(u64, MutationControl)>,
}

impl StopController {
    /// Desktop ownership includes draining cleanup, not observation-only requests.
    pub fn set_session_active(&self, active: bool) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_active = active;
    }

    pub fn session_active(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_active
    }

    /// Latch a human stop only while this server owns a control session.
    pub fn stop_by_user(&self) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.session_active || state.stop_reason.is_some() {
            return false;
        }
        state.stop_reason = Some(StopReason::User);
        for (_, control) in &state.active {
            control.cancel();
        }
        true
    }

    pub fn user_stopped(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop_reason
            == Some(StopReason::User)
    }

    /// Close admission and cancel all registered work without waiting for providers.
    pub fn stop(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stop_reason.get_or_insert(StopReason::Other);
        for (_, control) in &state.active {
            control.cancel();
        }
    }

    fn cleanup_failed(&self) {
        self.stop();
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cleanup_failed = true;
    }

    pub fn is_stopped(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop_reason
            .is_some()
    }

    pub fn status(&self) -> &'static str {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stop_reason.is_some() {
            return Err(PlatformError::OperationFailed {
                operation: "admission".to_owned(),
                reason: if state.stop_reason == Some(StopReason::User) {
                    "the user stopped the control session; do not resume or restart without the user's permission"
                } else {
                    "desktop work is stopped; only the user may restart the server"
                }.to_owned(),
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
        let mut state = self
            .controller
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.retain(|(id, _)| *id != self.id);
        if self.control.progress().cleanup == CleanupStatus::Unknown {
            state.cleanup_failed = true;
            state.stop_reason.get_or_insert(StopReason::Other);
            for (_, control) in &state.active {
                control.cancel();
            }
        }
    }
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
}
