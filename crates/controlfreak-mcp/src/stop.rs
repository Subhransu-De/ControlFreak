use std::sync::Arc;

use controlfreak_core::{MutationControl, PlatformError, StopController, StopRegistration};

tokio::task_local! {
    pub(super) static WORK: Work;
}

#[derive(Clone, Default)]
pub(super) struct Work {
    pub(super) control: MutationControl,
    registration: Option<Arc<StopRegistration>>,
}

impl Work {
    pub(super) fn new(stop: &StopController) -> Result<Self, PlatformError> {
        let control = MutationControl::default();
        Ok(Self {
            registration: Some(Arc::new(stop.register(control.clone())?)),
            control,
        })
    }
}

pub(super) fn current() -> Option<Work> {
    WORK.try_with(Clone::clone).ok()
}

pub(super) struct CancelOnDrop(pub(super) MutationControl);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub(super) struct CleanupOnDrop(pub(super) Option<Work>);

impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        if let Some(registration) = self.0.as_ref().and_then(|work| work.registration.as_ref()) {
            registration.record_cleanup();
        }
    }
}
