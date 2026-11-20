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

/// Sent when the user stops, and again when the stopped work has drained.
pub(super) const STOPPED_NOTIFICATION: &str = "notifications/controlfreak/session_stopped";
/// Sent when the user allows input again from the session tab.
pub(super) const REARMED_NOTIFICATION: &str = "notifications/controlfreak/session_rearmed";

/// The user-stop notification for `recovery`, after `previous` was reported: the stop itself,
/// the end of its drain, or another change of the step, such as a client stop that makes the
/// user's stop permanent.
pub(super) fn stopped_notification(
    recovery: controlfreak_core::StopRecovery,
    previous: Option<controlfreak_core::StopRecovery>,
) -> rmcp::model::ServerNotification {
    let event = match previous {
        None => "user_stopped_session",
        Some(controlfreak_core::StopRecovery::WaitForDrain) => "stop_drained",
        Some(_) => "recovery_changed",
    };
    rmcp::model::ServerNotification::CustomNotification(rmcp::model::CustomNotification::new(
        STOPPED_NOTIFICATION,
        Some(serde_json::json!({
            "event": event,
            "reason": "user_stop",
            "message": format!("You stopped desktop input. {}", recovery.guidance()),
            "stop_state": controlfreak_core::StopRecovery::stop_state(Some(recovery)),
            "recovery": recovery_json(Some(recovery)),
            "retry_action": false,
        })),
    ))
}

/// The user pressed Allow again on the session tab. Nothing resumes: the agent asks the user
/// before it acts, and its next action begins a new session.
pub(super) fn rearmed_notification() -> rmcp::model::ServerNotification {
    rmcp::model::ServerNotification::CustomNotification(rmcp::model::CustomNotification::new(
        REARMED_NOTIFICATION,
        Some(serde_json::json!({
            "event": "user_allowed_input",
            "message": "The user allowed desktop input again. Nothing resumes on its own: ask the user before you continue, then list windows and begin a new session.",
            "stop_state": "ready",
            "input_admission": "open",
            "recovery": null,
        })),
    ))
}

/// The wire form of a recovery step: `null` while admission is open. Refusals, status, the end
/// result and the stop notification all use it, so they always agree.
pub(super) fn recovery_json(
    recovery: Option<controlfreak_core::StopRecovery>,
) -> serde_json::Value {
    recovery.map_or(serde_json::Value::Null, |recovery| {
        serde_json::json!({
            "actor": recovery.actor(),
            "action": recovery.action(),
            "message": recovery.guidance(),
        })
    })
}
