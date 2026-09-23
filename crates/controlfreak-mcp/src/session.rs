use super::{
    Arc, AtomicBool, AtomicU8, Duration, ErrorData, Instant, MutationControl, Mutex, Ordering,
    PlatformBackend, Value, VecDeque, json, mpsc, thread,
};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;

const INDICATOR_DORMANT: u8 = 0;
const INDICATOR_STARTING: u8 = 1;
const INDICATOR_VISIBLE: u8 = 2;
const INDICATOR_IDLE_PENDING: u8 = 3;
const INDICATOR_HIDDEN: u8 = 4;
const INDICATOR_FAILED: u8 = 5;
const INDICATOR_STOPPING: u8 = 6;
const DEFAULT_SHORT_HOLD: Duration = Duration::from_secs(8);
const DEFAULT_SESSION_HOLD: Duration = Duration::from_secs(45);
const DEFAULT_MAX_HOLD: Duration = Duration::from_mins(2);
const MIN_SESSION_HOLD: Duration = Duration::from_secs(20);
const MAX_INDICATOR_RESTARTS: u8 = 2;

#[derive(Clone)]
pub struct SafetyIndicator {
    pub(super) state: Arc<AtomicU8>,
    failure_reason: Arc<Mutex<Option<String>>>,
}

impl SafetyIndicator {
    pub fn dormant() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(INDICATOR_DORMANT)),
            failure_reason: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn starting() -> Self {
        let indicator = Self::dormant();
        indicator.mark_starting();
        indicator
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn ready() -> Self {
        let indicator = Self::starting();
        indicator.mark_visible();
        indicator
    }

    pub(super) fn mark_visible(&self) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.state.store(INDICATOR_VISIBLE, Ordering::Release);
    }

    pub(super) fn mark_starting(&self) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.state.store(INDICATOR_STARTING, Ordering::Release);
    }

    pub(super) fn mark_failed(&self, reason: impl Into<String>) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.into());
        self.state.store(INDICATOR_FAILED, Ordering::Release);
    }

    pub(super) fn mark_idle_pending(&self) {
        self.state.store(INDICATOR_IDLE_PENDING, Ordering::Release);
    }

    pub(super) fn mark_hidden(&self) {
        self.state.store(INDICATOR_HIDDEN, Ordering::Release);
    }

    pub(super) fn mark_stopping(&self) {
        self.state.store(INDICATOR_STOPPING, Ordering::Release);
    }

    pub(super) fn status(&self) -> &'static str {
        match self.state.load(Ordering::Acquire) {
            INDICATOR_DORMANT => "dormant",
            INDICATOR_STARTING => "starting",
            INDICATOR_VISIBLE => "visible",
            INDICATOR_IDLE_PENDING => "idle_pending",
            INDICATOR_HIDDEN => "hidden",
            INDICATOR_FAILED => "failed",
            _ => "stopping",
        }
    }

    pub(super) fn is_failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == INDICATOR_FAILED
    }

    pub(super) fn failure_reason(&self) -> Option<String> {
        self.failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorLevel {
    Armed,
    Acting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorHealth {
    Healthy,
    Unhealthy(String),
    Dead(String),
}

pub trait ActivityIndicator: Send {
    fn set_level(&mut self, level: IndicatorLevel) -> Result<(), String>;
    fn hide(&mut self) -> Result<(), String>;
    fn shutdown(&mut self) -> Result<(), String>;

    fn health(&self) -> IndicatorHealth {
        IndicatorHealth::Healthy
    }

    fn mutation_control(&self) -> MutationControl {
        MutationControl::default()
    }

    fn containment(&self) -> (&'static str, Option<String>) {
        (
            "unavailable",
            Some("helper containment is not reported".to_owned()),
        )
    }
}

type IndicatorControl = Box<dyn ActivityIndicator>;
type IndicatorStarter = Arc<dyn Fn() -> Result<IndicatorControl, String> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ArbitrationBusy {
    pub owner_instance_id: Option<String>,
    pub retry_after_ms: u64,
}

pub trait ActivityArbitrator: Send + Sync {
    fn try_acquire(&self) -> Result<(), ArbitrationBusy>;
    fn release(&self);
}

pub(super) struct LocalArbitrator;

impl ActivityArbitrator for LocalArbitrator {
    fn try_acquire(&self) -> Result<(), ArbitrationBusy> {
        Ok(())
    }

    fn release(&self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlSessionState {
    Dormant,
    Armed,
    Acting,
    Closing,
}

impl ControlSessionState {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Dormant => "dormant",
            Self::Armed => "armed",
            Self::Acting => "acting",
            Self::Closing => "closing",
        }
    }
}

pub(super) struct ControlSession {
    pub(super) state: ControlSessionState,
    owns_arbitration: bool,
    explicitly_begun: bool,
    call_count: u64,
    pub(super) hold: Duration,
    pub(super) close_deadline: Option<Instant>,
    pub(super) last_mutation_finished: Option<Instant>,
    gaps: VecDeque<Duration>,
    restart_count: u8,
}

impl ControlSession {
    pub(super) fn dormant(short_hold: Duration) -> Self {
        Self {
            state: ControlSessionState::Dormant,
            owns_arbitration: false,
            explicitly_begun: false,
            call_count: 0,
            hold: short_hold,
            close_deadline: None,
            last_mutation_finished: None,
            gaps: VecDeque::with_capacity(16),
            restart_count: 0,
        }
    }
}

pub(super) struct IndicatorRuntimeState {
    control: Option<IndicatorControl>,
    active_mutations: usize,
    pub(super) generation: u64,
    last_cleanup_reason: Option<String>,
    cancellation: MutationControl,
    pub(super) session: ControlSession,
}

pub(super) struct IndicatorRuntime {
    pub(super) state: Mutex<IndicatorRuntimeState>,
    lifecycle: Mutex<()>,
    pub(super) idle_worker: Mutex<Option<IdleWorker>>,
    pub(super) safety_indicator: SafetyIndicator,
    pub(super) starter: IndicatorStarter,
    pub(super) environment: Option<Arc<dyn PlatformBackend>>,
    pub(super) terminated: AtomicBool,
    pub(super) arbitrator: Arc<dyn ActivityArbitrator>,
    pub(super) short_hold: Duration,
    pub(super) session_hold: Duration,
    pub(super) max_hold: Duration,
    #[cfg(test)]
    idle_worker_starts: AtomicUsize,
}

pub(super) enum IdleCommand {
    Schedule { generation: u64, deadline: Instant },
    Shutdown,
}

pub(super) struct IdleWorker {
    pub(super) sender: mpsc::Sender<IdleCommand>,
    pub(super) thread: Option<thread::JoinHandle<()>>,
}

impl IndicatorRuntime {
    #[cfg(test)]
    pub(super) fn new_with_idle_delay<F, G>(
        safety_indicator: SafetyIndicator,
        start_indicator: F,
        idle_delay: Duration,
    ) -> Self
    where
        F: Fn() -> Result<G, String> + Send + Sync + 'static,
        G: ActivityIndicator + 'static,
    {
        Self::new_with_timing(
            safety_indicator,
            start_indicator,
            Arc::new(LocalArbitrator),
            GlowTiming {
                short_hold: idle_delay,
                session_hold: idle_delay,
                max_hold: idle_delay,
            },
        )
    }

    pub(super) fn new_with_timing<F, G>(
        safety_indicator: SafetyIndicator,
        start_indicator: F,
        arbitrator: Arc<dyn ActivityArbitrator>,
        timing: GlowTiming,
    ) -> Self
    where
        F: Fn() -> Result<G, String> + Send + Sync + 'static,
        G: ActivityIndicator + 'static,
    {
        Self {
            state: Mutex::new(IndicatorRuntimeState {
                control: None,
                active_mutations: 0,
                generation: 0,
                last_cleanup_reason: None,
                cancellation: MutationControl::default(),
                session: ControlSession::dormant(timing.short_hold),
            }),
            lifecycle: Mutex::new(()),
            idle_worker: Mutex::new(None),
            safety_indicator,
            starter: Arc::new(move || {
                start_indicator().map(|control| Box::new(control) as IndicatorControl)
            }),
            arbitrator,
            environment: None,
            terminated: AtomicBool::new(false),
            short_hold: timing.short_hold,
            session_hold: timing.session_hold,
            max_hold: timing.max_hold,
            #[cfg(test)]
            idle_worker_starts: AtomicUsize::new(0),
        }
    }

    pub(super) async fn acquire_mutation(self: &Arc<Self>) -> Result<OperationLease, ErrorData> {
        let runtime = Arc::clone(self);
        let result = tokio::task::spawn_blocking(move || runtime.acquire_mutation_blocking())
            .await
            .map_err(|error| {
                ErrorData::internal_error(
                    "ControlFreak safety indicator worker failed",
                    Some(json!({ "reason": error.to_string() })),
                )
            })?;
        result.map_err(|reason| indicator_unavailable(&self.safety_indicator, &reason))
    }

    #[cfg(test)]
    pub(super) async fn acquire(self: &Arc<Self>) -> Result<OperationLease, ErrorData> {
        self.acquire_mutation().await
    }

    pub(super) fn acquire_mutation_blocking(self: &Arc<Self>) -> Result<OperationLease, String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_environment()?;
        self.ensure_session_owned()?;
        if let Err(reason) = self.schedule_close(0, self.max_hold) {
            let _ = self.close_session_locked("admission_failure", false);
            return Err(reason);
        }
        self.ensure_indicator_level(IndicatorLevel::Acting)?;
        self.check_environment()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if let Some(previous) = state.session.last_mutation_finished {
            let gap = now.saturating_duration_since(previous);
            if state.session.gaps.len() == 16 {
                state.session.gaps.pop_front();
            }
            state.session.gaps.push_back(gap);
        }
        state.active_mutations += 1;
        state.generation = state.generation.wrapping_add(1);
        state.session.call_count = state.session.call_count.saturating_add(1);
        state.session.state = ControlSessionState::Acting;
        state.session.close_deadline = None;
        let control = state
            .control
            .as_ref()
            .map_or_else(MutationControl::default, |indicator| {
                indicator.mutation_control()
            });
        self.safety_indicator.mark_visible();
        Ok(OperationLease {
            runtime: Arc::clone(self),
            control: control.with_cancellation(state.cancellation.clone()),
        })
    }

    pub(super) fn begin_session(
        self: &Arc<Self>,
        expected_seconds: Option<u64>,
    ) -> Result<(), String> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check_environment()?;
        self.ensure_session_owned()?;
        if let Err(reason) = self.schedule_close(0, self.max_hold) {
            let _ = self.close_session_locked("admission_failure", false);
            return Err(reason);
        }
        let active = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_mutations
            != 0;
        let level = if active {
            IndicatorLevel::Acting
        } else {
            IndicatorLevel::Armed
        };
        self.ensure_indicator_level(level)?;
        self.check_environment()?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = expected_seconds.map_or(self.session_hold, Duration::from_secs);
        state.session.explicitly_begun = true;
        state.session.hold = requested.min(self.max_hold);
        state.session.state = if active {
            ControlSessionState::Acting
        } else {
            ControlSessionState::Armed
        };
        state.session.close_deadline = (!active).then(|| Instant::now() + state.session.hold);
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let delay = state.session.hold;
        self.safety_indicator.mark_visible();
        drop(state);
        if active {
            Ok(())
        } else {
            self.schedule_close(generation, delay).inspect_err(|_| {
                let _ = self.close_session_locked("admission_failure", false);
            })
        }
    }

    pub(super) fn end_session(&self) -> Result<(), String> {
        self.close_session("explicit_end", false)
    }

    pub(super) fn close_session(&self, reason: &str, terminate: bool) -> Result<(), String> {
        if terminate {
            self.terminated.store(true, Ordering::Release);
        }
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.close_session_locked(reason, terminate)
    }

    // The lifecycle lock serializes admission and teardown. The last mutation
    // guard finishes closure only after the backend's compensating input releases.
    fn close_session_locked(&self, reason: &str, terminate: bool) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if terminate {
            self.terminated.store(true, Ordering::Release);
        }
        if !state.session.owns_arbitration {
            if terminate && state.last_cleanup_reason.is_none() {
                state.last_cleanup_reason = Some(reason.to_owned());
            }
            return Ok(());
        }
        if state.session.state != ControlSessionState::Closing {
            state.last_cleanup_reason = Some(reason.to_owned());
            state.session.state = ControlSessionState::Closing;
            state.session.close_deadline = None;
            state.generation = state.generation.wrapping_add(1);
        }
        if reason != "explicit_end" {
            state.cancellation.cancel();
        }
        if state.active_mutations != 0 {
            return Ok(());
        }
        drop(state);
        self.finish_session_close()
    }

    fn check_environment(&self) -> Result<(), String> {
        if self.terminated.load(Ordering::Acquire) {
            return Err("the server is shutting down".to_owned());
        }
        if let Some(backend) = &self.environment
            && let Err(error) = backend.check_control_environment()
        {
            let reason = error.to_string();
            let _ = self.close_session_locked(&reason, false);
            return Err(reason);
        }
        Ok(())
    }

    pub(super) fn check_lifecycle(&self) {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session
            .owns_arbitration
        {
            return;
        }
        if !self.terminated.load(Ordering::Acquire) && self.check_environment().is_err() {
            return;
        }
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let closing = state.session.state == ControlSessionState::Closing;
        let failure = state
            .control
            .as_ref()
            .and_then(|control| indicator_failure_reason(control.as_ref()));
        drop(state);
        if closing {
            let _ = self.finish_session_close();
        } else if failure.is_some() {
            let _ = self.close_session_locked("indicator_failure", false);
        }
    }

    fn release_mutation(self: &Arc<Self>) {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_mutations = state.active_mutations.saturating_sub(1);
        state.session.last_mutation_finished = Some(Instant::now());
        if state.active_mutations != 0 {
            return;
        }
        if state.session.state == ControlSessionState::Closing {
            drop(state);
            let _ = self.finish_session_close();
            return;
        }
        let failure = state
            .control
            .as_ref()
            .and_then(|control| indicator_failure_reason(control.as_ref()));
        let armed = failure.map_or_else(
            || {
                state
                    .control
                    .as_mut()
                    .map_or(Ok(()), |control| control.set_level(IndicatorLevel::Armed))
            },
            Err,
        );
        if let Err(reason) = armed {
            self.safety_indicator.mark_failed(reason);
            drop(state);
            let _ = self.close_session_locked("indicator_failure", false);
            return;
        }
        let hold = if state.session.explicitly_begun {
            state.session.hold
        } else {
            self.hold_for_session(&state.session)
        };
        state.session.state = ControlSessionState::Armed;
        state.session.hold = hold;
        state.session.close_deadline = Some(Instant::now() + hold);
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        self.safety_indicator.mark_idle_pending();
        drop(state);
        if let Err(reason) = self.schedule_close(generation, hold) {
            self.safety_indicator.mark_failed(reason);
            let _ = self.close_session_locked("admission_failure", false);
        }
    }

    fn ensure_session_owned(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.terminated.load(Ordering::Acquire) {
            return Err("the server is shutting down".to_owned());
        }
        if state.session.owns_arbitration && state.session.state != ControlSessionState::Closing {
            return Ok(());
        }
        if state.session.state == ControlSessionState::Closing {
            return Err("the prior control session is still closing".to_owned());
        }
        self.arbitrator.try_acquire().map_err(|busy| {
            serde_json::to_string(&json!({
                "code": "desktop_in_use",
                "retryable": true,
                "owner_instance_id": busy.owner_instance_id,
                "session_state": "dormant",
                "retry_after_ms": busy.retry_after_ms,
                "remedy": "wait, or ask the owning client to call end_control_session",
            }))
            .unwrap_or_else(|_| "another ControlFreak server owns the desktop".to_owned())
        })?;
        state.session = ControlSession::dormant(self.short_hold);
        state.session.owns_arbitration = true;
        state.cancellation = MutationControl::default();
        Ok(())
    }

    fn ensure_control(&self) -> Result<(), String> {
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .control
            .is_some()
        {
            return Ok(());
        }
        self.safety_indicator.mark_starting();
        let control = (self.starter)()?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .control = Some(control);
        Ok(())
    }

    fn ensure_indicator_level(&self, level: IndicatorLevel) -> Result<(), String> {
        for attempt in 0..=MAX_INDICATOR_RESTARTS {
            let result = self.ensure_control().and_then(|()| {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let health = state.control.as_ref().map_or(
                    IndicatorHealth::Dead("helper is unavailable".to_owned()),
                    |control| control.health(),
                );
                match health {
                    IndicatorHealth::Healthy => state
                        .control
                        .as_mut()
                        .ok_or_else(|| "desktop glow control was unavailable".to_owned())?
                        .set_level(level),
                    IndicatorHealth::Unhealthy(reason) | IndicatorHealth::Dead(reason) => {
                        Err(reason)
                    }
                }
            });
            match result {
                Ok(()) => return Ok(()),
                Err(reason) => {
                    let (retired, can_restart) = self.retire_failed_control(&reason);
                    if let Some(mut retired) = retired
                        && let Err(error) = retired.shutdown()
                    {
                        self.state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .control = Some(retired);
                        let _ = self.close_session_locked("admission_failure", false);
                        return Err(error);
                    }
                    if !can_restart || attempt == MAX_INDICATOR_RESTARTS {
                        if can_restart {
                            let _ = self.close_session_locked("admission_failure", false);
                        }
                        self.safety_indicator.mark_failed(reason.clone());
                        return Err(reason);
                    }
                }
            }
        }
        unreachable!()
    }

    fn retire_failed_control(&self, reason: &str) -> (Option<IndicatorControl>, bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.session.restart_count = state.session.restart_count.saturating_add(1);
        self.safety_indicator.mark_failed(reason.to_owned());
        if state.active_mutations != 0 {
            if let Some(control) = state.control.as_ref() {
                control.mutation_control().cancel();
            }
            drop(state);
            let _ = self.close_session_locked("admission_failure", false);
            (None, false)
        } else {
            (state.control.take(), true)
        }
    }

    fn close_owned_session(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.session.owns_arbitration {
            self.arbitrator.release();
        }
        state.session = ControlSession::dormant(self.short_hold);
    }

    fn hold_for_session(&self, session: &ControlSession) -> Duration {
        if session.call_count <= 1 {
            return self.short_hold;
        }
        if session.call_count == 2 || session.gaps.is_empty() {
            return MIN_SESSION_HOLD.min(self.max_hold);
        }
        let mut gaps: Vec<Duration> = session.gaps.iter().copied().collect();
        gaps.sort_unstable();
        let median = gaps[gaps.len() / 2];
        median
            .saturating_mul(4)
            .max(MIN_SESSION_HOLD)
            .min(self.max_hold)
            .max(self.session_hold.min(self.max_hold))
    }

    fn schedule_close(self: &Arc<Self>, generation: u64, delay: Duration) -> Result<(), String> {
        let mut idle_worker = self
            .idle_worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if idle_worker.is_none() {
            let (sender, receiver) = mpsc::channel();
            let runtime = Arc::downgrade(self);
            let worker = thread::Builder::new()
                .name("controlfreak-glow-session".to_owned())
                .spawn(move || idle_worker_loop(&runtime, &receiver))
                .map_err(|error| format!("desktop glow session worker could not start: {error}"))?;
            #[cfg(test)]
            self.idle_worker_starts.fetch_add(1, Ordering::Relaxed);
            *idle_worker = Some(IdleWorker {
                sender,
                thread: Some(worker),
            });
        }
        idle_worker
            .as_ref()
            .ok_or_else(|| "desktop glow session worker was unavailable".to_owned())?
            .sender
            .send(IdleCommand::Schedule {
                generation,
                deadline: Instant::now() + delay,
            })
            .map_err(|error| format!("desktop glow session worker stopped: {error}"))
    }

    pub(super) fn close_if_idle(&self, generation: u64) {
        let _lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_close = state.active_mutations == 0 && state.generation == generation;
        drop(state);
        if should_close {
            let _ = self.close_session_locked("timeout", false);
        }
    }

    fn finish_session_close(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active_mutations != 0 {
            return Ok(());
        }
        let mut control = state.control.take();
        let terminated = self.terminated.load(Ordering::Acquire);
        drop(state);
        let failure = control
            .as_ref()
            .and_then(|control| indicator_failure_reason(control.as_ref()));
        let unhealthy = failure.is_some();
        let result = if let Some(reason) = failure {
            self.safety_indicator.mark_failed(reason);
            Ok(())
        } else {
            control.as_mut().map_or(Ok(()), |control| control.hide())
        };
        if unhealthy || result.is_err() || terminated {
            if let Some(indicator) = control.as_mut()
                && let Err(reason) = indicator.shutdown()
            {
                self.safety_indicator.mark_failed(reason.clone());
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .control = control;
                return Err(reason);
            }
            control = None;
        }
        if let Err(reason) = &result
            && !self.safety_indicator.is_failed()
        {
            self.safety_indicator.mark_failed(reason.clone());
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .control = control;
        self.close_owned_session();
        if !self.safety_indicator.is_failed() {
            self.safety_indicator.mark_hidden();
        }
        result
    }

    pub(super) fn status(&self) -> Value {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remaining_ms = state.session.close_deadline.map(|deadline| {
            u64::try_from(
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis(),
            )
            .unwrap_or(u64::MAX)
        });
        let (containment, containment_reason) = state.control.as_ref().map_or(
            ("unavailable", Some("helper has not started".to_owned())),
            |control| control.containment(),
        );
        json!({
            "state": state.session.state.as_str(),
            "last_cleanup_reason": state.last_cleanup_reason,
            "draining": state.session.state == ControlSessionState::Closing,
            "call_count": state.session.call_count,
            "active_mutations": state.active_mutations,
            "hold_ms": u64::try_from(state.session.hold.as_millis()).unwrap_or(u64::MAX),
            "time_until_close_ms": remaining_ms,
            "owns_arbitration": state.session.owns_arbitration,
            "restart_count": state.session.restart_count,
            "containment": containment,
            "containment_reason": containment_reason,
        })
    }

    #[cfg(test)]
    pub(super) fn idle_worker_start_count(&self) -> usize {
        self.idle_worker_starts.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
#[allow(clippy::struct_field_names)]
pub(super) struct GlowTiming {
    pub(super) short_hold: Duration,
    pub(super) session_hold: Duration,
    pub(super) max_hold: Duration,
}

pub(super) fn glow_timing_from_environment() -> GlowTiming {
    let max_hold = duration_from_env("CONTROLFREAK_GLOW_MAX_HOLD_MS").unwrap_or(DEFAULT_MAX_HOLD);
    let configured = duration_from_env("CONTROLFREAK_GLOW_HOLD_MS");
    GlowTiming {
        short_hold: configured.unwrap_or(DEFAULT_SHORT_HOLD).min(max_hold),
        session_hold: configured.unwrap_or(DEFAULT_SESSION_HOLD).min(max_hold),
        max_hold,
    }
}

fn duration_from_env(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|milliseconds| *milliseconds > 0)
        .map(Duration::from_millis)
}

fn indicator_failure_reason(control: &dyn ActivityIndicator) -> Option<String> {
    match control.health() {
        IndicatorHealth::Healthy if control.mutation_control().is_cancelled() => {
            Some("desktop glow helper cancelled the active mutation".to_owned())
        }
        IndicatorHealth::Healthy => None,
        IndicatorHealth::Unhealthy(reason) | IndicatorHealth::Dead(reason) => Some(reason),
    }
}

fn idle_worker_loop(
    runtime: &std::sync::Weak<IndicatorRuntime>,
    receiver: &mpsc::Receiver<IdleCommand>,
) {
    let mut pending: Option<(u64, Instant)> = None;
    loop {
        let delay = pending.map_or(Duration::from_millis(50), |(_, deadline)| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50))
        });
        match receiver.recv_timeout(delay) {
            Ok(IdleCommand::Schedule {
                generation,
                deadline,
            }) => {
                pending = Some((generation, deadline));
            }
            Ok(IdleCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                runtime.check_lifecycle();
                if let Some((generation, deadline)) = pending
                    && Instant::now() >= deadline
                {
                    runtime.close_if_idle(generation);
                    pending = None;
                }
            }
        }
    }
}

pub(super) struct OperationLease {
    runtime: Arc<IndicatorRuntime>,
    control: MutationControl,
}

impl OperationLease {
    pub(super) fn mutation_control(&self) -> MutationControl {
        self.control.clone()
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        self.runtime.release_mutation();
    }
}

/*
 * The old per-call idle worker is intentionally replaced by the session worker
 * above. Keep all teardown after this point outside the state mutex.
 */

impl Drop for IndicatorRuntime {
    fn drop(&mut self) {
        if let Some(mut worker) = self
            .idle_worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.sender.send(IdleCommand::Shutdown);
            if let Some(handle) = worker.thread.take()
                && handle.thread().id() != thread::current().id()
            {
                let _ = handle.join();
            }
        }
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut control) = state.control.take()
            && control.shutdown().is_err()
            && state.session.owns_arbitration
        {
            // A failed shutdown must not block Drop or surrender ownership.
            // Transfer both resources together; release only after safe cleanup.
            let arbitrator = Arc::clone(&self.arbitrator);
            thread::spawn(move || {
                while control.shutdown().is_err() {
                    thread::sleep(Duration::from_millis(50));
                }
                arbitrator.release();
            });
            return;
        }
        self.safety_indicator.mark_stopping();
        if state.session.owns_arbitration {
            self.arbitrator.release();
        }
    }
}

pub(super) fn indicator_unavailable(safety_indicator: &SafetyIndicator, reason: &str) -> ErrorData {
    let mut details = serde_json::from_str::<serde_json::Map<String, Value>>(reason)
        .unwrap_or_else(|_| serde_json::Map::new());
    details
        .entry("code".to_owned())
        .or_insert_with(|| Value::String("indicator_unavailable".to_owned()));
    details.insert(
        "indicator_status".to_owned(),
        Value::String(safety_indicator.status().to_owned()),
    );
    details
        .entry("reason".to_owned())
        .or_insert_with(|| Value::String(reason.to_owned()));
    details
        .entry("retryable".to_owned())
        .or_insert_with(|| Value::Bool(matches!(safety_indicator.status(), "starting" | "failed")));
    ErrorData::internal_error(
        "ControlFreak safety indicator is not visible",
        Some(Value::Object(details)),
    )
}
