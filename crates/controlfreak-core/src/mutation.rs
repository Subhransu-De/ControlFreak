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
}

impl MutationControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self, operation: &str) -> Result<(), PlatformError> {
        if self.is_cancelled() {
            Err(PlatformError::OperationFailed {
                operation: operation.to_owned(),
                reason: "the desktop mutation was cancelled because the safety indicator became unhealthy"
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
