use std::sync::{Arc, Mutex};

use controlfreak_core::{
    MutationControl, PlatformError, StopController, StopRegistration, timing::StageTiming,
};

tokio::task_local! {
    pub(super) static WORK: Work;
}

#[derive(Clone, Default)]
pub(super) struct Work {
    pub(super) control: MutationControl,
    registration: Option<Arc<StopRegistration>>,
    /// Stage timings reported by blocking workers that ran for this request.
    stages: Arc<Mutex<Vec<StageTiming>>>,
}

impl Work {
    pub(super) fn new(stop: &StopController) -> Result<Self, PlatformError> {
        let control = MutationControl::default();
        Ok(Self {
            registration: Some(Arc::new(stop.register(control.clone())?)),
            control,
            stages: Arc::default(),
        })
    }

    /// Reports this request's worker stage timings into a slot owned by the caller.
    pub(super) fn with_stages(self, stages: Arc<Mutex<Vec<StageTiming>>>) -> Self {
        Self { stages, ..self }
    }

    /// Adds stage timings measured on a worker thread to this request's totals.
    pub(super) fn record_stages(&self, measured: Vec<StageTiming>) {
        let mut stages = self
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for stage in measured {
            if let Some(existing) = stages.iter_mut().find(|entry| entry.stage == stage.stage) {
                existing.micros = existing.micros.saturating_add(stage.micros);
                existing.calls = existing.calls.saturating_add(stage.calls);
            } else {
                stages.push(stage);
            }
        }
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
