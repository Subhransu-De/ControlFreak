#![forbid(unsafe_code)]

#[cfg(test)]
mod cleanup_tests;
mod handlers;
mod results;
mod schema;

use handlers::{
    call_capture_display, call_capture_region, call_capture_visual_baseline, call_capture_window,
    call_click_mouse, call_click_text, call_drag_mouse, call_find_text_on_screen,
    call_focus_window, call_get_server_status, call_list_displays, call_list_virtual_desktops,
    call_list_windows, call_move_mouse, call_press_keys, call_read_text_in_region,
    call_scroll_mouse, call_switch_virtual_desktop, call_type_text, call_wait_for_change_since,
    call_wait_for_visual_change, call_wait_for_window,
};
use results::{
    click_text_result, keyboard_result, pointer_result, screenshot_result, tool_error,
    tool_execution_error, virtual_desktop_list_result, virtual_desktop_switch_result,
    visual_baseline_result, visual_change_result, window_focus_result, window_screenshot_result,
    window_wait_result,
};
use schema::tools;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use controlfreak_core::{
    ActionObservation, CaptureDisplayRequest, CaptureRegionRequest, CaptureWindowRequest,
    ClickTextRequest, ClickTextResult, DisplayScreenshot, FindTextRequest, FocusWindowRequest, Key,
    KeyChordRequest, KeyboardActionResult, MouseButton, MouseClickRequest, MouseDragRequest,
    MouseMoveRequest, MouseScrollRequest, MutationControl, ObservationMode, ObservationOptions,
    ObservationRegion, OcrRegionRequest, PlatformBackend, PlatformError, PointerActionResult,
    TextInputRequest, VirtualDesktopDirection, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForVisualChangeRequest, WaitForWindowRequest, WindowFocusResult,
    WindowScreenshot, WindowWaitResult,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
        Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        ServerInfo, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::io::stdio,
};
use serde::Deserialize;
use serde_json::{Value, json};

const LIST_DISPLAYS: &str = "list_displays";
const GET_SERVER_STATUS: &str = "get_server_status";
const BEGIN_CONTROL_SESSION: &str = "begin_control_session";
const END_CONTROL_SESSION: &str = "end_control_session";
const CAPTURE_DISPLAY: &str = "capture_display";
const CAPTURE_REGION: &str = "capture_region";
const WAIT_FOR_VISUAL_CHANGE: &str = "wait_for_visual_change";
const CAPTURE_VISUAL_BASELINE: &str = "capture_visual_baseline";
const WAIT_FOR_CHANGE_SINCE: &str = "wait_for_change_since";
const READ_TEXT_IN_REGION: &str = "read_text_in_region";
const FIND_TEXT_ON_SCREEN: &str = "find_text_on_screen";
const CLICK_TEXT: &str = "click_text";
const MOVE_MOUSE: &str = "move_mouse";
const CLICK_MOUSE: &str = "click_mouse";
const DRAG_MOUSE: &str = "drag_mouse";
const SCROLL_MOUSE: &str = "scroll_mouse";
const LIST_WINDOWS: &str = "list_windows";
const LIST_VIRTUAL_DESKTOPS: &str = "list_virtual_desktops";
const SWITCH_VIRTUAL_DESKTOP: &str = "switch_virtual_desktop";
const FOCUS_WINDOW: &str = "focus_window";
const CAPTURE_WINDOW: &str = "capture_window";
const WAIT_FOR_WINDOW: &str = "wait_for_window";
const PRESS_KEYS: &str = "press_keys";
const TYPE_TEXT: &str = "type_text";
const MAX_MOVE_DURATION_MS: u32 = 10_000;
const MAX_SCROLL_DELTA: i32 = 12_000;
const MAX_TEXT_UTF16_UNITS: usize = 4_000;
const MAX_CAPTURE_WIDTH: u32 = 7_680;
const MAX_WAIT_MS: u32 = 30_000;

#[derive(Clone)]
struct ControlFreakServer {
    backend: Arc<dyn PlatformBackend>,
    diagnostics: Arc<Diagnostics>,
    safety_indicator: SafetyIndicator,
    indicator_runtime: Option<Arc<IndicatorRuntime>>,
}

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
    state: Arc<AtomicU8>,
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
    fn starting() -> Self {
        let indicator = Self::dormant();
        indicator.mark_starting();
        indicator
    }

    #[cfg(not(target_os = "windows"))]
    fn ready() -> Self {
        let indicator = Self::starting();
        indicator.mark_visible();
        indicator
    }

    fn mark_visible(&self) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.state.store(INDICATOR_VISIBLE, Ordering::Release);
    }

    fn mark_starting(&self) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.state.store(INDICATOR_STARTING, Ordering::Release);
    }

    fn mark_failed(&self, reason: impl Into<String>) {
        *self
            .failure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.into());
        self.state.store(INDICATOR_FAILED, Ordering::Release);
    }

    fn mark_idle_pending(&self) {
        self.state.store(INDICATOR_IDLE_PENDING, Ordering::Release);
    }

    fn mark_hidden(&self) {
        self.state.store(INDICATOR_HIDDEN, Ordering::Release);
    }

    fn mark_stopping(&self) {
        self.state.store(INDICATOR_STOPPING, Ordering::Release);
    }

    fn status(&self) -> &'static str {
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

    fn is_failed(&self) -> bool {
        self.state.load(Ordering::Acquire) == INDICATOR_FAILED
    }

    fn failure_reason(&self) -> Option<String> {
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

struct LocalArbitrator;

impl ActivityArbitrator for LocalArbitrator {
    fn try_acquire(&self) -> Result<(), ArbitrationBusy> {
        Ok(())
    }

    fn release(&self) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlSessionState {
    Dormant,
    Armed,
    Acting,
    Closing,
}

impl ControlSessionState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dormant => "dormant",
            Self::Armed => "armed",
            Self::Acting => "acting",
            Self::Closing => "closing",
        }
    }
}

struct ControlSession {
    state: ControlSessionState,
    owns_arbitration: bool,
    explicitly_begun: bool,
    call_count: u64,
    hold: Duration,
    close_deadline: Option<Instant>,
    last_mutation_finished: Option<Instant>,
    gaps: VecDeque<Duration>,
    restart_count: u8,
}

impl ControlSession {
    fn dormant(short_hold: Duration) -> Self {
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

struct IndicatorRuntimeState {
    control: Option<IndicatorControl>,
    active_mutations: usize,
    generation: u64,
    last_cleanup_reason: Option<String>,
    terminated: bool,
    cancellation: MutationControl,
    session: ControlSession,
}

struct IndicatorRuntime {
    state: Mutex<IndicatorRuntimeState>,
    lifecycle: Mutex<()>,
    idle_worker: Mutex<Option<IdleWorker>>,
    safety_indicator: SafetyIndicator,
    starter: IndicatorStarter,
    environment: Option<Arc<dyn PlatformBackend>>,
    arbitrator: Arc<dyn ActivityArbitrator>,
    short_hold: Duration,
    session_hold: Duration,
    max_hold: Duration,
    #[cfg(test)]
    idle_worker_starts: AtomicUsize,
}

enum IdleCommand {
    Schedule { generation: u64, deadline: Instant },
    Shutdown,
}

struct IdleWorker {
    sender: mpsc::Sender<IdleCommand>,
    thread: Option<thread::JoinHandle<()>>,
}

impl IndicatorRuntime {
    #[cfg(test)]
    fn new_with_idle_delay<F, G>(
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

    fn new_with_timing<F, G>(
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
                terminated: false,
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
            short_hold: timing.short_hold,
            session_hold: timing.session_hold,
            max_hold: timing.max_hold,
            #[cfg(test)]
            idle_worker_starts: AtomicUsize::new(0),
        }
    }

    async fn acquire_mutation(self: &Arc<Self>) -> Result<OperationLease, ErrorData> {
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
    async fn acquire(self: &Arc<Self>) -> Result<OperationLease, ErrorData> {
        self.acquire_mutation().await
    }

    fn acquire_mutation_blocking(self: &Arc<Self>) -> Result<OperationLease, String> {
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

    fn begin_session(self: &Arc<Self>, expected_seconds: Option<u64>) -> Result<(), String> {
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

    fn end_session(&self) -> Result<(), String> {
        self.close_session("explicit_end", false)
    }

    fn close_session(&self, reason: &str, terminate: bool) -> Result<(), String> {
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
        state.terminated |= terminate;
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
        if let Some(backend) = &self.environment
            && let Err(error) = backend.check_control_environment()
        {
            let reason = error.to_string();
            let _ = self.close_session_locked(&reason, false);
            return Err(reason);
        }
        Ok(())
    }

    fn check_lifecycle(&self) {
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
        if self.check_environment().is_err() {
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
        if state.terminated {
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

    fn close_if_idle(&self, generation: u64) {
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
        let terminated = state.terminated;
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

    fn status(&self) -> Value {
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
    fn idle_worker_start_count(&self) -> usize {
        self.idle_worker_starts.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
#[allow(clippy::struct_field_names)]
struct GlowTiming {
    short_hold: Duration,
    session_hold: Duration,
    max_hold: Duration,
}

fn glow_timing_from_environment() -> GlowTiming {
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

struct OperationLease {
    runtime: Arc<IndicatorRuntime>,
    control: MutationControl,
}

impl OperationLease {
    fn mutation_control(&self) -> MutationControl {
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
        if let Some(mut control) = state.control.take() {
            // Ownership cannot outlive the input/indicator cleanup barrier.
            while control.shutdown().is_err() && state.session.owns_arbitration {
                thread::sleep(Duration::from_millis(50));
            }
        }
        self.safety_indicator.mark_stopping();
        if state.session.owns_arbitration {
            self.arbitrator.release();
        }
    }
}

fn indicator_unavailable(safety_indicator: &SafetyIndicator, reason: &str) -> ErrorData {
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

struct Diagnostics {
    instance_id: String,
    started: Instant,
    next_operation_id: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    last_completed: Mutex<Option<Instant>>,
    recent_operations: Mutex<VecDeque<OperationRecord>>,
}

struct OperationRecord {
    operation_id: u64,
    tool: String,
    failed: bool,
    elapsed_ms: u64,
    gap_ms: Option<u64>,
}

impl Diagnostics {
    fn new() -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        Self {
            instance_id: format!("cf-{}-{timestamp:x}", std::process::id()),
            started: Instant::now(),
            next_operation_id: AtomicU64::new(1),
            completed: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            last_completed: Mutex::new(None),
            recent_operations: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    fn record(&self, record: OperationRecord) {
        let mut recent = self
            .recent_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if recent.len() == 32 {
            recent.pop_front();
        }
        recent.push_back(record);
    }
}

impl ControlFreakServer {
    fn with_indicator(
        backend: Arc<dyn PlatformBackend>,
        safety_indicator: SafetyIndicator,
        indicator_runtime: Option<Arc<IndicatorRuntime>>,
    ) -> Self {
        Self {
            backend,
            diagnostics: Arc::new(Diagnostics::new()),
            safety_indicator,
            indicator_runtime,
        }
    }
}

impl ServerHandler for ControlFreakServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "controlfreak",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "ControlFreak is a computer-use MCP server. For tasks with multiple input actions, call begin_control_session first and end_control_session when finished. Actions return screenshots by default. Reuse them; capture again only when needed. Choose targets from current observations. Prefer click_text for unique visible labels. Check isError and error text. Read structuredContent without serializing image data. Never repeat an action when retry_action=false.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    #[allow(clippy::too_many_lines)]
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let backend = Arc::clone(&self.backend);
        let diagnostics = Arc::clone(&self.diagnostics);
        let safety_indicator = self.safety_indicator.clone();
        let indicator_runtime = self.indicator_runtime.clone();
        async move {
            let tool_name = request.name.to_string();
            let operation_id = diagnostics
                .next_operation_id
                .fetch_add(1, Ordering::Relaxed);
            let started = Instant::now();
            let gap_ms = diagnostics
                .last_completed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .map(|completed| {
                    u64::try_from(started.saturating_duration_since(completed).as_millis())
                        .unwrap_or(u64::MAX)
                });
            let outcome = if tool_name == BEGIN_CONTROL_SESSION {
                match indicator_runtime.as_ref() {
                    Some(runtime) => {
                        let input = parse_arguments::<BeginControlSessionInput>(request.arguments)?;
                        let runtime_for_worker = Arc::clone(runtime);
                        match tokio::task::spawn_blocking(move || {
                            runtime_for_worker.begin_session(input.expected_seconds)
                        })
                        .await
                        {
                            Ok(Ok(())) => Ok(CallToolResult::structured(json!({
                                "status": "armed",
                                "session": runtime.status(),
                            }))
                            .into()),
                            Ok(Err(reason)) => {
                                Err(indicator_unavailable(&safety_indicator, &reason))
                            }
                            Err(error) => Err(join_error(&error)),
                        }
                    }
                    None => Err(indicator_unavailable(
                        &safety_indicator,
                        "desktop glow sessions are unavailable",
                    )),
                }
            } else if tool_name == END_CONTROL_SESSION {
                parse_arguments::<NoArguments>(request.arguments)?;
                match indicator_runtime.as_ref() {
                    Some(runtime) => {
                        let runtime_for_worker = Arc::clone(runtime);
                        match tokio::task::spawn_blocking(move || runtime_for_worker.end_session())
                            .await
                        {
                            Ok(Ok(())) => Ok(CallToolResult::structured(json!({
                                "status": "closing",
                                "session": runtime.status(),
                            }))
                            .into()),
                            Ok(Err(reason)) => {
                                Err(indicator_unavailable(&safety_indicator, &reason))
                            }
                            Err(error) => Err(join_error(&error)),
                        }
                    }
                    None => Err(indicator_unavailable(
                        &safety_indicator,
                        "desktop glow sessions are unavailable",
                    )),
                }
            } else if is_mutating_tool(&tool_name) {
                match indicator_runtime.as_ref() {
                    Some(runtime) => match runtime.acquire_mutation().await {
                        Ok(operation_lease) => {
                            dispatch_tool(
                                backend,
                                &diagnostics,
                                &safety_indicator,
                                indicator_runtime.as_ref(),
                                request,
                                Some(operation_lease),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    },
                    None => {
                        dispatch_tool(
                            backend,
                            &diagnostics,
                            &safety_indicator,
                            indicator_runtime.as_ref(),
                            request,
                            None,
                        )
                        .await
                    }
                }
            } else {
                dispatch_tool(
                    backend,
                    &diagnostics,
                    &safety_indicator,
                    indicator_runtime.as_ref(),
                    request,
                    None,
                )
                .await
            };
            let outcome = match outcome {
                Ok(response) => Ok(response),
                Err(error) => Ok(tool_execution_error(&error).into()),
            };
            let failed = match &outcome {
                Err(_) => true,
                Ok(CallToolResponse::Complete(result)) => result.is_error == Some(true),
                Ok(_) => false,
            };
            if failed {
                diagnostics.failed.fetch_add(1, Ordering::Relaxed);
            } else {
                diagnostics.completed.fetch_add(1, Ordering::Relaxed);
            }
            let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            *diagnostics
                .last_completed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
            diagnostics.record(OperationRecord {
                operation_id,
                tool: tool_name.clone(),
                failed,
                elapsed_ms,
                gap_ms,
            });
            eprintln!(
                "{}",
                json!({
                    "event": "tool_completed",
                    "instance_id": diagnostics.instance_id,
                    "operation_id": operation_id,
                    "tool": tool_name,
                    "status": if failed { "failed" } else { "completed" },
                    "elapsed_ms": elapsed_ms,
                    "gap_ms": gap_ms,
                })
            );
            outcome.map(|mut response| {
                if let CallToolResponse::Complete(result) = &mut response {
                    let mut meta = result.meta.take().unwrap_or_default();
                    meta.insert(
                        "controlfreak".to_owned(),
                        json!({
                            "instance_id": diagnostics.instance_id,
                            "operation_id": operation_id,
                            "elapsed_ms": elapsed_ms,
                        }),
                    );
                    result.meta = Some(meta);
                }
                response
            })
        }
    }
}

async fn dispatch_tool(
    backend: Arc<dyn PlatformBackend>,
    diagnostics: &Diagnostics,
    safety_indicator: &SafetyIndicator,
    indicator_runtime: Option<&Arc<IndicatorRuntime>>,
    request: CallToolRequestParams,
    operation_lease: Option<OperationLease>,
) -> Result<CallToolResponse, ErrorData> {
    match request.name.as_ref() {
        GET_SERVER_STATUS => call_get_server_status(
            &backend,
            diagnostics,
            safety_indicator,
            indicator_runtime,
            request.arguments,
        ),
        LIST_DISPLAYS => call_list_displays(backend, request.arguments, operation_lease).await,
        CAPTURE_DISPLAY => call_capture_display(backend, request.arguments, operation_lease).await,
        CAPTURE_REGION => call_capture_region(backend, request.arguments, operation_lease).await,
        WAIT_FOR_VISUAL_CHANGE => {
            call_wait_for_visual_change(backend, request.arguments, operation_lease).await
        }
        CAPTURE_VISUAL_BASELINE => {
            call_capture_visual_baseline(backend, request.arguments, operation_lease).await
        }
        WAIT_FOR_CHANGE_SINCE => {
            call_wait_for_change_since(backend, request.arguments, operation_lease).await
        }
        READ_TEXT_IN_REGION => {
            call_read_text_in_region(backend, request.arguments, operation_lease).await
        }
        FIND_TEXT_ON_SCREEN => {
            call_find_text_on_screen(backend, request.arguments, operation_lease).await
        }
        CLICK_TEXT => call_click_text(backend, request.arguments, operation_lease).await,
        MOVE_MOUSE => call_move_mouse(backend, request.arguments, operation_lease).await,
        CLICK_MOUSE => call_click_mouse(backend, request.arguments, operation_lease).await,
        DRAG_MOUSE => call_drag_mouse(backend, request.arguments, operation_lease).await,
        SCROLL_MOUSE => call_scroll_mouse(backend, request.arguments, operation_lease).await,
        LIST_WINDOWS => call_list_windows(backend, request.arguments, operation_lease).await,
        LIST_VIRTUAL_DESKTOPS => {
            call_list_virtual_desktops(backend, request.arguments, operation_lease).await
        }
        SWITCH_VIRTUAL_DESKTOP => {
            call_switch_virtual_desktop(backend, request.arguments, operation_lease).await
        }
        FOCUS_WINDOW => call_focus_window(backend, request.arguments, operation_lease).await,
        CAPTURE_WINDOW => call_capture_window(backend, request.arguments, operation_lease).await,
        WAIT_FOR_WINDOW => call_wait_for_window(backend, request.arguments, operation_lease).await,
        PRESS_KEYS => call_press_keys(backend, request.arguments, operation_lease).await,
        TYPE_TEXT => call_type_text(backend, request.arguments, operation_lease).await,
        unknown => Err(ErrorData::invalid_params(
            format!("unknown ControlFreak tool '{unknown}'"),
            Some(json!({ "tool": unknown })),
        )),
    }
}

fn is_mutating_tool(tool: &str) -> bool {
    matches!(
        tool,
        CLICK_TEXT
            | MOVE_MOUSE
            | CLICK_MOUSE
            | DRAG_MOUSE
            | SCROLL_MOUSE
            | SWITCH_VIRTUAL_DESKTOP
            | FOCUS_WINDOW
            | PRESS_KEYS
            | TYPE_TEXT
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginControlSessionInput {
    expected_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureDisplayInput {
    display_id: String,
    max_width: Option<u32>,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRegionInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    max_width: Option<u32>,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForVisualChangeInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
    #[serde(default = "default_stable_ms")]
    stable_ms: u32,
    #[serde(default = "default_difference_threshold")]
    difference_threshold: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VisualBaselineInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForChangeSinceInput {
    baseline_id: String,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
    #[serde(default = "default_stable_ms")]
    stable_ms: u32,
    #[serde(default = "default_difference_threshold")]
    difference_threshold: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OcrRegionInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
}

impl OcrRegionInput {
    fn into_request(self) -> OcrRegionRequest {
        OcrRegionRequest {
            display_id: self.display_id,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            language: self.language,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindTextInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
    query: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default = "default_max_ocr_results")]
    max_results: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickTextInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    language: Option<String>,
    query: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default = "default_true")]
    exact_match: bool,
    #[serde(default = "default_left_button")]
    button: MouseButton,
    #[serde(default = "default_click_count")]
    click_count: u8,
    #[serde(default)]
    modifiers: Vec<Key>,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    observation: ObservationInput,
}

impl FindTextInput {
    fn into_request(self) -> FindTextRequest {
        FindTextRequest {
            region: OcrRegionRequest {
                display_id: self.display_id,
                x: self.x,
                y: self.y,
                width: self.width,
                height: self.height,
                language: self.language,
            },
            query: self.query,
            case_sensitive: self.case_sensitive,
            max_results: self.max_results,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationInput {
    #[serde(default)]
    mode: ObservationMode,
    max_width: Option<u32>,
    #[serde(default = "default_true")]
    include_cursor: bool,
    region: Option<ObservationRegionInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservationRegionInput {
    display_id: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Default for ObservationInput {
    fn default() -> Self {
        Self {
            mode: ObservationMode::Screenshot,
            max_width: None,
            include_cursor: true,
            region: None,
        }
    }
}

impl From<ObservationInput> for ObservationOptions {
    fn from(input: ObservationInput) -> Self {
        Self {
            mode: input.mode,
            max_width: input.max_width,
            include_cursor: input.include_cursor,
            region: input.region.map(|region| ObservationRegion {
                display_id: region.display_id,
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveMouseInput {
    display_id: String,
    x: u32,
    y: u32,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickMouseInput {
    display_id: String,
    x: u32,
    y: u32,
    button: MouseButton,
    #[serde(default = "default_click_count")]
    click_count: u8,
    #[serde(default)]
    modifiers: Vec<Key>,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DragMouseInput {
    start_display_id: String,
    start_x: u32,
    start_y: u32,
    end_display_id: String,
    end_x: u32,
    end_y: u32,
    button: MouseButton,
    #[serde(default)]
    modifiers: Vec<Key>,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollMouseInput {
    display_id: String,
    x: u32,
    y: u32,
    #[serde(default)]
    delta_x: i32,
    delta_y: i32,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FocusWindowInput {
    window_id: String,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SwitchVirtualDesktopInput {
    direction: VirtualDesktopDirection,
    #[serde(default = "default_desktop_steps")]
    steps: u8,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureWindowInput {
    window_id: String,
    max_width: Option<u32>,
    #[serde(default = "default_true")]
    include_cursor: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForWindowInput {
    window_id: Option<String>,
    title_contains: Option<String>,
    class_name: Option<String>,
    process_id: Option<u32>,
    is_foreground: Option<bool>,
    #[serde(default = "default_wait_timeout_ms")]
    timeout_ms: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PressKeysInput {
    keys: Vec<Key>,
    #[serde(default)]
    observation: ObservationInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeTextInput {
    text: String,
    #[serde(default)]
    observation: ObservationInput,
}

const fn default_true() -> bool {
    true
}

const fn default_click_count() -> u8 {
    1
}

const fn default_left_button() -> MouseButton {
    MouseButton::Left
}

const fn default_desktop_steps() -> u8 {
    1
}

const fn default_wait_timeout_ms() -> u32 {
    5_000
}

const fn default_stable_ms() -> u32 {
    250
}

const fn default_difference_threshold() -> f64 {
    0.01
}

const fn default_max_ocr_results() -> u32 {
    20
}

fn parse_arguments<T>(arguments: Option<JsonObject>) -> Result<T, ErrorData>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|error| {
        ErrorData::invalid_params(
            format!("invalid tool arguments: {error}"),
            Some(json!({ "reason": error.to_string() })),
        )
    })
}

fn join_error(error: &tokio::task::JoinError) -> ErrorData {
    ErrorData::internal_error(
        "ControlFreak platform worker failed",
        Some(json!({ "reason": error.to_string() })),
    )
}

#[cfg(not(target_os = "windows"))]
pub async fn serve_stdio(
    backend: Box<dyn PlatformBackend>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    serve_stdio_inner(backend, SafetyIndicator::ready(), None).await
}

pub async fn serve_stdio_with_indicator<F, G>(
    backend: Box<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    start_indicator: F,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    F: Fn() -> Result<G, String> + Send + Sync + 'static,
    G: ActivityIndicator + 'static,
{
    serve_stdio_with_indicator_and_arbitrator(
        backend,
        safety_indicator,
        start_indicator,
        Arc::new(LocalArbitrator),
    )
    .await
}

pub async fn serve_stdio_with_indicator_and_arbitrator<F, G>(
    backend: Box<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    start_indicator: F,
    arbitrator: Arc<dyn ActivityArbitrator>,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    F: Fn() -> Result<G, String> + Send + Sync + 'static,
    G: ActivityIndicator + 'static,
{
    let indicator_runtime = Arc::new(IndicatorRuntime::new_with_timing(
        safety_indicator.clone(),
        start_indicator,
        arbitrator,
        glow_timing_from_environment(),
    ));
    serve_stdio_inner(backend, safety_indicator, Some(indicator_runtime)).await
}

struct ServerCleanup(Option<Arc<IndicatorRuntime>>);

impl ServerCleanup {
    fn close(&self, reason: &str) {
        if let Some(runtime) = &self.0 {
            let _ = runtime.close_session(reason, true);
        }
    }
}

impl Drop for ServerCleanup {
    fn drop(&mut self) {
        self.close("shutdown");
    }
}

async fn serve_stdio_inner(
    backend: Box<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    indicator_runtime: Option<Arc<IndicatorRuntime>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let backend: Arc<dyn PlatformBackend> = Arc::from(backend);
    let mut indicator_runtime = indicator_runtime;
    if let Some(runtime) = indicator_runtime.as_mut() {
        Arc::get_mut(runtime)
            .expect("runtime is not shared before serving")
            .environment = Some(Arc::clone(&backend));
    }
    let cleanup = ServerCleanup(indicator_runtime.clone());
    let server = ControlFreakServer::with_indicator(backend, safety_indicator, indicator_runtime);
    let instance_id = server.diagnostics.instance_id.clone();
    eprintln!(
        "{}",
        json!({
            "event": "server_started",
            "instance_id": instance_id,
            "process_id": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
        })
    );
    let service = server.serve(stdio()).await?;
    let cancellation = service.cancellation_token();
    let waiting = service.waiting();
    tokio::pin!(waiting);
    let result = tokio::select! {
        result = &mut waiting => {
            cleanup.close("disconnect");
            result
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            cleanup.close("shutdown");
            cancellation.cancel();
            waiting.await
        }
    };
    if let Some(runtime) = &cleanup.0 {
        while runtime.status()["draining"] == true {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    match result {
        Ok(reason) => {
            eprintln!(
                "{}",
                json!({
                    "event": "server_stopped",
                    "instance_id": instance_id,
                    "reason": format!("{reason:?}"),
                })
            );
            Ok(())
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({
                    "event": "server_failed",
                    "instance_id": instance_id,
                    "error": error.to_string(),
                })
            );
            Err(Box::new(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use controlfreak_core::{
        ActionObservation, DisplayBounds, DisplayInfo, DisplayScreenshot, MousePosition,
        PointerActionResult,
    };

    use super::{
        ActivityArbitrator, ActivityIndicator, ArbitrationBusy, CaptureDisplayInput,
        FocusWindowInput, GlowTiming, IndicatorHealth, IndicatorRuntime, PressKeysInput,
        SafetyIndicator, handlers::run_platform_operation, parse_arguments, pointer_result,
        schema::json_object, tool_error, tool_execution_error, tools,
    };

    fn wait_for_status(indicator: &SafetyIndicator, expected: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while indicator.status() != expected && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(indicator.status(), expected);
    }

    struct RecordingIndicator {
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_hide: bool,
    }

    struct FailingSecondShowIndicator {
        shows: usize,
    }

    struct HealthControlledIndicator {
        health: Arc<Mutex<IndicatorHealth>>,
    }

    struct ExclusiveArbitrator {
        owned: Arc<AtomicBool>,
    }

    impl ActivityArbitrator for ExclusiveArbitrator {
        fn try_acquire(&self) -> Result<(), ArbitrationBusy> {
            self.owned
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .map(|_| ())
                .map_err(|_| ArbitrationBusy {
                    owner_instance_id: Some("other-test-server".to_owned()),
                    retry_after_ms: 25,
                })
        }

        fn release(&self) {
            self.owned.store(false, Ordering::Release);
        }
    }

    impl ActivityIndicator for FailingSecondShowIndicator {
        fn set_level(&mut self, _level: super::IndicatorLevel) -> Result<(), String> {
            self.shows += 1;
            if self.shows == 2 {
                Err("helper exited".to_owned())
            } else {
                Ok(())
            }
        }

        fn hide(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn shutdown(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    impl ActivityIndicator for HealthControlledIndicator {
        fn set_level(&mut self, _level: super::IndicatorLevel) -> Result<(), String> {
            Ok(())
        }

        fn hide(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn shutdown(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn health(&self) -> IndicatorHealth {
            self.health.lock().unwrap().clone()
        }
    }

    impl ActivityIndicator for RecordingIndicator {
        fn set_level(&mut self, level: super::IndicatorLevel) -> Result<(), String> {
            self.events.lock().unwrap().push(match level {
                super::IndicatorLevel::Armed => "armed",
                super::IndicatorLevel::Acting => "acting",
            });
            Ok(())
        }

        fn hide(&mut self) -> Result<(), String> {
            self.events.lock().unwrap().push("hide");
            if self.fail_hide {
                Err("hide failed".to_owned())
            } else {
                Ok(())
            }
        }

        fn shutdown(&mut self) -> Result<(), String> {
            self.events.lock().unwrap().push("shutdown");
            Ok(())
        }
    }

    #[test]
    fn safety_indicator_transitions_are_explicit() {
        let indicator = SafetyIndicator::dormant();
        assert_eq!(indicator.status(), "dormant");

        indicator.mark_starting();
        assert_eq!(indicator.status(), "starting");

        indicator.mark_failed("glow failed");
        assert_eq!(indicator.status(), "failed");
        assert_eq!(indicator.failure_reason().as_deref(), Some("glow failed"));

        indicator.mark_visible();
        assert_eq!(indicator.status(), "visible");
        assert_eq!(indicator.failure_reason(), None);

        indicator.mark_idle_pending();
        assert_eq!(indicator.status(), "idle_pending");
        indicator.mark_hidden();
        assert_eq!(indicator.status(), "hidden");
        indicator.mark_stopping();
        assert_eq!(indicator.status(), "stopping");
    }

    #[tokio::test]
    async fn operation_leases_start_once_and_hide_after_the_last_call() {
        let starts = Arc::new(AtomicUsize::new(0));
        let starts_for_runtime = Arc::clone(&starts);
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                starts_for_runtime.fetch_add(1, Ordering::Relaxed);
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(20),
        ));

        assert_eq!(starts.load(Ordering::Relaxed), 0);
        assert_eq!(indicator.status(), "dormant");

        let first_lease = runtime.acquire().await.unwrap();
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(indicator.status(), "visible");
        assert_eq!(*events.lock().unwrap(), ["acting"]);

        let second_lease = runtime.acquire().await.unwrap();
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

        drop(first_lease);
        thread::sleep(Duration::from_millis(40));
        assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

        drop(second_lease);
        assert_eq!(indicator.status(), "idle_pending");
        wait_for_status(&indicator, "hidden");
        assert_eq!(
            *events.lock().unwrap(),
            ["acting", "acting", "armed", "hide"]
        );
    }

    #[tokio::test]
    async fn final_mutation_preserves_the_helper_failure_reason() {
        let health = Arc::new(Mutex::new(IndicatorHealth::Healthy));
        let health_for_runtime = Arc::clone(&health);
        let indicator = SafetyIndicator::dormant();
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(HealthControlledIndicator {
                    health: Arc::clone(&health_for_runtime),
                })
            },
            Duration::from_millis(20),
        ));

        let lease = runtime.acquire().await.unwrap();
        *health.lock().unwrap() = IndicatorHealth::Dead("heartbeat was lost".to_owned());
        drop(lease);

        assert_eq!(indicator.status(), "failed");
        assert_eq!(
            indicator.failure_reason().as_deref(),
            Some("heartbeat was lost")
        );
        assert_eq!(runtime.status()["state"], "dormant");
    }

    #[tokio::test]
    async fn a_new_operation_cancels_the_pending_hide() {
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(40),
        ));

        let first_lease = runtime.acquire().await.unwrap();
        drop(first_lease);
        thread::sleep(Duration::from_millis(10));

        let second_lease = runtime.acquire().await.unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(indicator.status(), "visible");
        assert_eq!(*events.lock().unwrap(), ["acting", "armed", "acting"]);

        drop(second_lease);
        wait_for_status(&indicator, "hidden");
        assert_eq!(
            *events.lock().unwrap(),
            ["acting", "armed", "acting", "armed", "hide"]
        );
    }

    #[tokio::test]
    async fn sequential_operations_reuse_one_idle_worker() {
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(20),
        ));

        for _ in 0..20 {
            drop(runtime.acquire().await.unwrap());
        }
        assert_eq!(runtime.idle_worker_start_count(), 1);
        wait_for_status(&indicator, "hidden");
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == "hide")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_hide_failure_shuts_down_the_helper_and_degrades_status() {
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: true,
                })
            },
            Duration::from_millis(10),
        ));

        let lease = runtime.acquire().await.unwrap();
        drop(lease);
        wait_for_status(&indicator, "failed");
        assert_eq!(indicator.failure_reason().as_deref(), Some("hide failed"));
        assert_eq!(
            *events.lock().unwrap(),
            ["acting", "armed", "hide", "shutdown"]
        );
    }

    #[tokio::test]
    async fn overlapping_operations_revalidate_helper_visibility() {
        let indicator = SafetyIndicator::dormant();
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            || Ok(FailingSecondShowIndicator { shows: 0 }),
            Duration::from_millis(10),
        ));

        let first_lease = runtime.acquire().await.unwrap();
        let second = runtime.acquire().await;

        assert!(second.is_err());
        assert_eq!(indicator.status(), "failed");
        assert_eq!(indicator.failure_reason().as_deref(), Some("helper exited"));
        drop(first_lease);
    }

    #[tokio::test]
    async fn a_hide_failure_restarts_the_helper_on_the_next_mutation() {
        let starts = Arc::new(AtomicUsize::new(0));
        let starts_for_runtime = Arc::clone(&starts);
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let indicator = SafetyIndicator::dormant();
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: starts_for_runtime.fetch_add(1, Ordering::Relaxed) == 0,
                })
            },
            Duration::from_millis(10),
        ));

        drop(runtime.acquire().await.unwrap());
        wait_for_status(&indicator, "failed");

        drop(runtime.acquire().await.unwrap());
        thread::sleep(Duration::from_millis(30));
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        assert_eq!(indicator.status(), "hidden");
    }

    #[test]
    fn begin_clamps_expected_seconds_and_end_hides_immediately() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Arc::new(super::LocalArbitrator),
            GlowTiming {
                short_hold: Duration::from_millis(10),
                session_hold: Duration::from_secs(1),
                max_hold: Duration::from_secs(2),
            },
        ));

        runtime.begin_session(Some(99)).unwrap();
        assert_eq!(runtime.status()["hold_ms"], 2_000);
        assert_eq!(runtime.status()["state"], "armed");
        runtime.end_session().unwrap();

        runtime.begin_session(Some(1)).unwrap();
        assert_eq!(runtime.status()["hold_ms"], 1_000);
        runtime.end_session().unwrap();
        assert_eq!(*events.lock().unwrap(), ["armed", "hide", "armed", "hide"]);
    }

    #[tokio::test]
    async fn begin_keeps_acting_while_a_mutation_is_in_flight() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            SafetyIndicator::dormant(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(20),
        ));

        let lease = runtime.acquire().await.unwrap();
        runtime.begin_session(None).unwrap();

        assert_eq!(runtime.status()["state"], "acting");
        assert_eq!(runtime.status()["active_mutations"], 1);
        assert!(runtime.status()["time_until_close_ms"].is_null());
        assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

        drop(lease);
        runtime.end_session().unwrap();
    }

    #[tokio::test]
    async fn explicit_session_hold_survives_the_first_mutation() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Arc::new(super::LocalArbitrator),
            GlowTiming {
                short_hold: Duration::from_millis(10),
                session_hold: Duration::from_secs(1),
                max_hold: Duration::from_secs(2),
            },
        ));

        runtime.begin_session(Some(2)).unwrap();
        drop(runtime.acquire().await.unwrap());

        assert_eq!(runtime.status()["state"], "armed");
        assert_eq!(runtime.status()["hold_ms"], 2_000);
        assert!(runtime.status()["time_until_close_ms"].as_u64().unwrap() > 1_500);

        runtime.end_session().unwrap();
    }

    #[test]
    fn refused_server_stays_dark_until_the_owner_ends() {
        let owned = Arc::new(AtomicBool::new(false));
        let first_events = Arc::new(Mutex::new(Vec::new()));
        let second_events = Arc::new(Mutex::new(Vec::new()));
        let second_starts = Arc::new(AtomicUsize::new(0));
        let first = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let events = Arc::clone(&first_events);
                move || {
                    Ok(RecordingIndicator {
                        events: Arc::clone(&events),
                        fail_hide: false,
                    })
                }
            },
            Arc::new(ExclusiveArbitrator {
                owned: Arc::clone(&owned),
            }),
            GlowTiming {
                short_hold: Duration::from_millis(20),
                session_hold: Duration::from_millis(20),
                max_hold: Duration::from_secs(1),
            },
        ));
        let second = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let events = Arc::clone(&second_events);
                let starts = Arc::clone(&second_starts);
                move || {
                    starts.fetch_add(1, Ordering::Relaxed);
                    Ok(RecordingIndicator {
                        events: Arc::clone(&events),
                        fail_hide: false,
                    })
                }
            },
            Arc::new(ExclusiveArbitrator {
                owned: Arc::clone(&owned),
            }),
            GlowTiming {
                short_hold: Duration::from_millis(20),
                session_hold: Duration::from_millis(20),
                max_hold: Duration::from_secs(1),
            },
        ));

        first.begin_session(None).unwrap();
        assert!(second.begin_session(None).is_err());
        assert_eq!(second_starts.load(Ordering::Relaxed), 0);
        assert!(second_events.lock().unwrap().is_empty());

        first.end_session().unwrap();
        second.begin_session(None).unwrap();
        assert_eq!(*second_events.lock().unwrap(), ["armed"]);
        second.end_session().unwrap();
    }

    #[test]
    fn failed_helper_start_releases_arbitration_for_the_next_server() {
        let owned = Arc::new(AtomicBool::new(false));
        let failed_starts = Arc::new(AtomicUsize::new(0));
        let first = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let failed_starts = Arc::clone(&failed_starts);
                move || {
                    failed_starts.fetch_add(1, Ordering::Relaxed);
                    Err::<RecordingIndicator, _>(
                        "native indicator initialization failed".to_owned(),
                    )
                }
            },
            Arc::new(ExclusiveArbitrator {
                owned: Arc::clone(&owned),
            }),
            GlowTiming {
                short_hold: Duration::from_millis(20),
                session_hold: Duration::from_millis(20),
                max_hold: Duration::from_secs(1),
            },
        ));
        let events = Arc::new(Mutex::new(Vec::new()));
        let second = Arc::new(IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let events = Arc::clone(&events);
                move || {
                    Ok(RecordingIndicator {
                        events: Arc::clone(&events),
                        fail_hide: false,
                    })
                }
            },
            Arc::new(ExclusiveArbitrator {
                owned: Arc::clone(&owned),
            }),
            GlowTiming {
                short_hold: Duration::from_millis(20),
                session_hold: Duration::from_millis(20),
                max_hold: Duration::from_secs(1),
            },
        ));

        assert_eq!(
            first.begin_session(None).unwrap_err(),
            "native indicator initialization failed"
        );
        assert_eq!(failed_starts.load(Ordering::Relaxed), 3);
        assert_eq!(first.status()["owns_arbitration"], false);
        assert_eq!(first.status()["last_cleanup_reason"], "admission_failure");

        second.begin_session(None).unwrap();
        second.end_session().unwrap();
        assert_eq!(*events.lock().unwrap(), ["armed", "hide"]);
    }

    #[tokio::test]
    async fn end_waits_for_the_outstanding_mutation_guard() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            SafetyIndicator::dormant(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_secs(1),
        ));

        runtime.begin_session(None).unwrap();
        let lease = runtime.acquire().await.unwrap();
        runtime.end_session().unwrap();
        assert_eq!(*events.lock().unwrap(), ["armed", "acting"]);

        drop(lease);
        assert_eq!(*events.lock().unwrap(), ["armed", "acting", "hide"]);
        assert_eq!(runtime.status()["state"], "dormant");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_acquisition_releases_the_completed_worker_lease() {
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (continue_tx, continue_rx) = mpsc::sync_channel(1);
        let continue_rx = Arc::new(Mutex::new(continue_rx));
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                started_tx.send(()).unwrap();
                continue_rx.lock().unwrap().recv().unwrap();
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(10),
        ));

        let runtime_for_acquisition = Arc::clone(&runtime);
        let acquisition = tokio::spawn(async move { runtime_for_acquisition.acquire().await });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        acquisition.abort();
        continue_tx.send(()).unwrap();
        let _ = acquisition.await;
        wait_for_status(&indicator, "hidden");
        assert_eq!(*events.lock().unwrap(), ["acting", "armed", "hide"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_a_request_keeps_the_glow_until_blocking_work_ends() {
        let indicator = SafetyIndicator::dormant();
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_runtime = Arc::clone(&events);
        let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
            indicator.clone(),
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events_for_runtime),
                    fail_hide: false,
                })
            },
            Duration::from_millis(10),
        ));
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (finish_tx, finish_rx) = mpsc::sync_channel(1);

        let lease = runtime.acquire().await.unwrap();
        let request = tokio::spawn(run_platform_operation(Some(lease), move || {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
        }));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(runtime);
        request.abort();
        let _ = request.await;
        thread::sleep(Duration::from_millis(40));

        assert_eq!(indicator.status(), "visible");
        assert_eq!(*events.lock().unwrap(), ["acting"]);

        finish_tx.send(()).unwrap();
        wait_for_status(&indicator, "stopping");
        assert_eq!(*events.lock().unwrap(), ["acting", "armed", "shutdown"]);
    }

    #[test]
    fn tool_surface_includes_windows_and_keyboard() {
        let names: Vec<String> = tools()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();

        assert_eq!(
            names,
            [
                "get_server_status",
                "begin_control_session",
                "end_control_session",
                "list_displays",
                "capture_display",
                "capture_region",
                "wait_for_visual_change",
                "capture_visual_baseline",
                "wait_for_change_since",
                "read_text_in_region",
                "find_text_on_screen",
                "click_text",
                "list_virtual_desktops",
                "switch_virtual_desktop",
                "list_windows",
                "focus_window",
                "capture_window",
                "wait_for_window",
                "move_mouse",
                "click_mouse",
                "drag_mouse",
                "scroll_mouse",
                "press_keys",
                "type_text",
            ]
        );
    }

    #[test]
    fn improved_arguments_accept_bounded_capture_focus_observation_and_uppercase_keys() {
        let capture =
            parse_arguments::<CaptureDisplayInput>(Some(json_object(serde_json::json!({
                "display_id": "display",
                "max_width": 1600,
                "include_cursor": false
            }))))
            .unwrap();
        assert_eq!(capture.max_width, Some(1600));
        assert!(!capture.include_cursor);

        let focus = parse_arguments::<FocusWindowInput>(Some(json_object(serde_json::json!({
            "window_id": "0X10:20",
            "observation": {
                "mode": "screenshot",
                "region": {
                    "display_id": "display",
                    "x": 10,
                    "y": 20,
                    "width": 300,
                    "height": 200
                }
            }
        }))))
        .unwrap();
        assert_eq!(focus.observation.region.unwrap().width, 300);

        let chord = parse_arguments::<PressKeysInput>(Some(json_object(serde_json::json!({
            "keys": ["CTRL", "L"]
        }))))
        .unwrap();
        assert_eq!(
            chord.keys,
            [controlfreak_core::Key::Ctrl, controlfreak_core::Key::L]
        );
    }

    #[test]
    fn argument_errors_have_text_and_structured_content() {
        let result = tool_execution_error(&rmcp::ErrorData::invalid_params(
            "bad arguments",
            Some(serde_json::json!({ "reason": "unknown field" })),
        ));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content.len(), 1);
        assert_eq!(
            result.structured_content.unwrap()["error"]["code"],
            "invalid_arguments"
        );
    }

    #[test]
    fn pointer_result_contains_post_action_png() {
        let display = DisplayInfo {
            id: r"\\.\DISPLAY1".to_owned(),
            name: r"\\.\DISPLAY1".to_owned(),
            bounds: DisplayBounds {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
            },
            is_primary: true,
        };
        let action_result = PointerActionResult {
            position: MousePosition {
                display_id: display.id.clone(),
                local_x: 0,
                local_y: 0,
                virtual_x: 0,
                virtual_y: 0,
            },
            observation: ActionObservation {
                foreground_window: None,
                screenshot: Some(DisplayScreenshot {
                    source_bounds: display.bounds,
                    display,
                    png: b"\x89PNG\r\n\x1a\n".to_vec(),
                    image_width: 1,
                    image_height: 1,
                    downscale_factor: 1,
                    cursor_marker: false,
                }),
            },
        };
        let action = serde_json::json!({ "kind": "move" });
        let result = pointer_result(&action_result, &action);
        let serialized = serde_json::to_value(result).expect("serialize tool result");
        let encoded = serialized["content"][0]["data"]
            .as_str()
            .expect("image data");

        assert_eq!(serialized["content"][0]["type"], "image");
        assert_eq!(serialized["structuredContent"]["action"]["kind"], "move");
        assert_eq!(
            serialized["structuredContent"]["observation"]["screenshot"]["downscale_factor"],
            1
        );
        assert_eq!(
            serialized["structuredContent"]["observation"]["screenshot"]["source_bounds"]["width"],
            1
        );
        assert_eq!(
            STANDARD.decode(encoded).expect("valid base64"),
            b"\x89PNG\r\n\x1a\n"
        );
    }

    #[test]
    fn pointer_result_without_screenshot_has_no_image_block() {
        let action_result = PointerActionResult {
            position: MousePosition {
                display_id: r"\\.\DISPLAY1".to_owned(),
                local_x: 10,
                local_y: 20,
                virtual_x: 10,
                virtual_y: 20,
            },
            observation: ActionObservation {
                foreground_window: None,
                screenshot: None,
            },
        };
        let result = pointer_result(&action_result, &serde_json::json!({ "kind": "move" }));
        let serialized = serde_json::to_value(result).expect("serialize tool result");

        assert!(
            serialized["content"]
                .as_array()
                .expect("content")
                .iter()
                .all(|content| content["type"] != "image")
        );
        assert_eq!(
            serialized["structuredContent"]["observation"]["screenshot"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn completed_action_with_failed_observation_is_not_retryable_error() {
        let result = tool_error(
            &controlfreak_core::PlatformError::PostActionObservationFailed {
                operation: "click_mouse".to_owned(),
                reason: "capture failed".to_owned(),
            },
        );
        let serialized = serde_json::to_value(result).expect("serialize tool result");

        assert_ne!(serialized["isError"], true);
        assert_eq!(
            serialized["structuredContent"]["status"],
            "completed_unverified"
        );
        assert_eq!(serialized["structuredContent"]["retry_action"], false);
    }

    #[test]
    fn higher_integrity_targets_have_a_dedicated_structured_code() {
        let result = tool_error(&controlfreak_core::PlatformError::HigherIntegrityTarget {
            operation: "press_keys".to_owned(),
            process_id: 42,
            server_integrity: controlfreak_core::IntegrityLevel::Medium,
            target_integrity: controlfreak_core::IntegrityLevel::High,
        });
        let serialized = serde_json::to_value(result).expect("serialize tool result");

        assert_eq!(serialized["isError"], true);
        assert_eq!(
            serialized["structuredContent"]["error"]["code"],
            "higher_integrity_target"
        );
    }
}
