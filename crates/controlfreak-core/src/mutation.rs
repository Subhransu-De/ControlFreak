use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{CleanupStatus, InputOutcome, MutationProgress, PlatformError};

/// Cooperative cancellation shared by one admitted desktop mutation and the
/// safety-indicator health monitor.
#[derive(Clone, Default)]
pub struct MutationControl {
    cancelled: Arc<AtomicBool>,
    parents: Vec<Arc<Self>>,
    progress: Arc<Mutex<MutationProgress>>,
    target: Arc<Mutex<Option<String>>>,
    target_invalidated: Arc<AtomicBool>,
}

impl MutationControl {
    /// Bounded cooperative wait, also used by providers between native calls.
    pub fn wait(
        &self,
        operation: &str,
        duration: std::time::Duration,
    ) -> Result<(), PlatformError> {
        let deadline = std::time::Instant::now() + duration;
        loop {
            self.check(operation)?;
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(10)));
        }
    }

    /// Combine indicator cancellation with the owning session cancellation.
    #[must_use]
    pub fn with_cancellation(&self, parent: Self) -> Self {
        let mut parents = self.parents.clone();
        let target = Arc::clone(&parent.target);
        let target_invalidated = Arc::clone(&parent.target_invalidated);
        parents.push(Arc::new(parent));
        Self {
            target,
            target_invalidated,
            cancelled: Arc::clone(&self.cancelled),
            parents,
            progress: Arc::clone(&self.progress),
        }
    }

    /// Bind once for the entire session, including concurrent operations.
    pub fn bind_target(&self, target: &str) -> Result<(), PlatformError> {
        self.check_target()?;
        let mut approved = self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match approved.as_deref() {
            Some(current) if current != target => Err(PlatformError::TargetInvalidated {
                reason: "end the control session before approving a different target".into(),
            }),
            _ if target.is_empty() => Err(PlatformError::TargetInvalidated {
                reason: "an approved target reference is required".into(),
            }),
            _ => {
                *approved = Some(target.to_owned());
                Ok(())
            }
        }
    }

    pub fn approved_target(&self) -> Option<String> {
        self.target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn target_foreground(&self, remained: bool) {
        self.update_progress(|progress| {
            if progress.target_remained_foreground != Some(false) {
                progress.target_remained_foreground = Some(remained);
            }
        });
    }

    pub fn activation_progress(&self, attempts: u32, elapsed_ms: u64) {
        self.update_progress(|progress| {
            progress.activation_attempts = attempts;
            progress.activation_elapsed_ms = elapsed_ms;
        });
    }

    pub fn invalidate_target(&self) {
        self.target_invalidated.store(true, Ordering::Release);
    }

    pub fn check_target(&self) -> Result<(), PlatformError> {
        if self.target_invalidated.load(Ordering::Acquire) {
            Err(PlatformError::TargetInvalidated {
                reason: "the session target was invalidated; end the session and observe again"
                    .into(),
            })
        } else {
            Ok(())
        }
    }

    /// Keep cancellation links but start an independent operation's progress.
    #[must_use]
    pub fn for_operation(&self) -> Self {
        Self {
            progress: Arc::default(),
            ..self.clone()
        }
    }

    pub fn progress(&self) -> MutationProgress {
        *self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Call immediately before dispatch, after validation. A panic or provider failure
    /// before acknowledgement must leave delivery unknown.
    pub fn dispatch_started(&self) -> InputOutcome {
        let mut previous = InputOutcome::NotStarted;
        self.update_progress(|progress| {
            previous = progress.input_outcome;
            progress.input_outcome = InputOutcome::Unknown;
        });
        previous
    }

    /// Restore known progress when the provider confirms that no new events were accepted.
    pub fn dispatch_rejected(&self, previous: InputOutcome) {
        self.update_progress(|progress| progress.input_outcome = previous);
    }

    /// Record accepted events cumulatively, excluding cleanup. For cursor/window APIs,
    /// pass zero: acknowledgement still records that a mutation occurred.
    pub fn dispatch_accepted(&self, events: u64) {
        self.update_progress(|progress| {
            progress.sent_events = progress.sent_events.saturating_add(events);
            progress.input_outcome = InputOutcome::PartiallySent;
        });
    }

    /// Call once all dispatch is done, before any post-action observation.
    pub fn input_complete(&self) {
        self.update_progress(|progress| progress.input_outcome = InputOutcome::InputSent);
    }

    pub fn cleanup_status(&self, status: CleanupStatus) {
        self.update_progress(|progress| progress.cleanup = status);
    }

    fn update_progress(&self, update: impl FnOnce(&mut MutationProgress)) {
        update(
            &mut self
                .progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self.parents.iter().any(|parent| parent.is_cancelled())
    }

    pub fn check(&self, operation: &str) -> Result<(), PlatformError> {
        if self.is_cancelled() {
            Err(PlatformError::OperationFailed {
                operation: operation.to_owned(),
                reason: "the desktop operation was cancelled by a user/client stop, request cancellation, a closing control session, or because the safety indicator became unhealthy"
                    .to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for MutationControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MutationControl")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::MutationControl;

    #[test]
    fn cancellation_interrupts_provider_waits_without_waiting_for_the_deadline() {
        use std::{sync::mpsc, time::Duration};
        for operation in [
            "move_mouse",
            "type_text",
            "wait_for_change",
            "windows_ocr_helper",
        ] {
            let control = MutationControl::default();
            let worker = control.clone();
            let (sender, receiver) = mpsc::channel();
            let task = std::thread::spawn(move || {
                sender
                    .send(worker.wait(operation, Duration::from_secs(30)))
                    .unwrap();
            });
            control.cancel();
            assert!(
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .is_err()
            );
            task.join().unwrap();
        }
    }

    #[test]
    fn linking_request_cancellation_preserves_session_and_indicator_cancellation() {
        for source in 0..3 {
            let indicator = MutationControl::default();
            let session = MutationControl::default();
            let request = MutationControl::default();
            session.bind_target("approved").unwrap();
            let lease = indicator.with_cancellation(session.clone()).for_operation();
            let operation = request.with_cancellation(lease);
            assert_eq!(operation.approved_target().as_deref(), Some("approved"));
            assert!(operation.bind_target("other").is_err());
            operation.invalidate_target();
            assert!(session.check_target().is_err());
            [&indicator, &session, &request][source].cancel();
            assert!(operation.is_cancelled());
        }
    }

    #[test]
    fn session_binding_survives_operations_and_refuses_retargeting() {
        let session = MutationControl::default();
        let indicator = MutationControl::default();
        let first = indicator.with_cancellation(session.clone()).for_operation();
        first.bind_target("approved").unwrap();
        let next = indicator.with_cancellation(session.clone()).for_operation();
        assert_eq!(next.approved_target().as_deref(), Some("approved"));
        assert!(next.bind_target("different").is_err());
        next.invalidate_target();
        assert!(first.check_target().is_err());
        assert!(session.bind_target("approved").is_err());
        assert!(!indicator.is_cancelled());
        let fresh = indicator.with_cancellation(MutationControl::default());
        fresh.bind_target("different").unwrap();
        fresh.check_target().unwrap();
    }

    #[test]
    fn lost_foreground_evidence_cannot_be_overwritten_by_later_focus() {
        let control = MutationControl::default();
        control.target_foreground(false);
        control.target_foreground(true);
        assert_eq!(control.progress().target_remained_foreground, Some(false));
        assert_eq!(
            control
                .for_operation()
                .progress()
                .target_remained_foreground,
            None
        );
    }

    #[test]
    fn session_cancellation_does_not_poison_indicator_reuse() {
        let indicator = MutationControl::default();
        let session = MutationControl::default();
        let operation = indicator.with_cancellation(session.clone());
        session.cancel();
        assert!(operation.is_cancelled());
        assert!(!indicator.is_cancelled());
        let next = indicator.with_cancellation(MutationControl::default());
        assert!(!next.is_cancelled());
        indicator.cancel();
        assert!(next.is_cancelled());
    }

    #[test]
    fn cancellation_is_shared_by_all_clones() {
        let control = MutationControl::default();
        let worker = control.clone();

        assert!(worker.check("move_mouse").is_ok());
        control.cancel();

        let error = worker.check("move_mouse").unwrap_err().to_string();
        assert!(worker.is_cancelled());
        assert!(error.contains("safety indicator became unhealthy"));
    }
}
