use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::PlatformError;

/// Cooperative cancellation shared by one admitted desktop mutation and the
/// safety-indicator health monitor.
#[derive(Clone, Default)]
pub struct MutationControl {
    cancelled: Arc<AtomicBool>,
    parent: Option<Arc<Self>>,
}

impl MutationControl {
    /// Combine indicator cancellation with the owning session cancellation.
    #[must_use]
    pub fn with_cancellation(&self, parent: Self) -> Self {
        Self {
            cancelled: Arc::clone(&self.cancelled),
            parent: Some(Arc::new(parent)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
    }

    pub fn check(&self, operation: &str) -> Result<(), PlatformError> {
        if self.is_cancelled() {
            Err(PlatformError::OperationFailed {
                operation: operation.to_owned(),
                reason: "the desktop mutation was cancelled because the control session closed or the safety indicator became unhealthy"
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
