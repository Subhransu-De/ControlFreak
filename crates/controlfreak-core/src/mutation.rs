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
        parents.push(Arc::new(parent));
        Self {
            cancelled: Arc::clone(&self.cancelled),
            parents,
            progress: Arc::clone(&self.progress),
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
            let operation = indicator
                .with_cancellation(session.clone())
                .with_cancellation(request.clone());
            [&indicator, &session, &request][source].cancel();
            assert!(operation.is_cancelled());
        }
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
