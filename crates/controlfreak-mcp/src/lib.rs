#![forbid(unsafe_code)]

mod arguments;
#[cfg(test)]
mod begin_cancel_tests;
#[cfg(test)]
mod bounds_tests;
mod catalog;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod cleanup_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod desktop_recipe_tests;
mod diagnostics;
#[cfg(test)]
mod encoding_tests;
#[cfg(test)]
mod fake_desktop;
mod handlers;
mod observations;
mod profile;
#[cfg(test)]
mod profile_tests;
mod requests;
mod session;
mod stop;
#[cfg(test)]
mod stop_recovery_tests;
#[cfg(test)]
mod stop_tests;

use diagnostics::Diagnostics;
use requests::{
    BeginControlSessionInput, CaptureDisplayInput, CaptureRegionInput, CaptureWindowInput,
    ClickMouseInput, ClickTextInput, DragMouseInput, FindTextInput, FocusWindowInput,
    MoveMouseInput, NoArguments, OcrRegionInput, PressKeysInput, ScrollMouseInput,
    SwitchVirtualDesktopInput, TypeTextInput, VisualBaselineInput, WaitForChangeSinceInput,
    WaitForTextInput, WaitForVisualChangeInput, WaitForWindowInput, parse_arguments,
};
pub use session::{
    ActivityArbitrator, ActivityIndicator, ArbitrationBusy, ArbitrationError, IndicatorHealth,
    IndicatorLevel, SafetyIndicator,
};
#[cfg(test)]
use session::{GlowTiming, IdleCommand, IdleWorker, LocalArbitrator};
use session::{
    OperationLease, SessionError, SessionRuntime, glow_timing_from_environment,
    indicator_unavailable,
};
#[cfg(test)]
mod outcome_tests;
#[cfg(test)]
mod output_schema_tests;
#[cfg(test)]
mod prepare_tests;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod recipe_tests;
mod results;
#[cfg(test)]
mod run_actions_tests;
mod schema;
#[cfg(test)]
mod session_error_tests;

use catalog::{Route, ToolSpec};
#[cfg(test)]
use controlfreak_core::{CaptureWindowRequest, WaitForChangeSinceRequest, WaitForWindowRequest};
use handlers::call_get_server_status;
pub use profile::{Profile, ProfileError};
use results::{
    CarriesPng, Encoded, click_text_result, keyboard_result, pointer_result, read_text_result,
    screenshot_result, tool_error, tool_execution_error, virtual_desktop_list_result,
    virtual_desktop_switch_result, visual_baseline_result, visual_change_result,
    window_focus_result, window_screenshot_result, window_wait_result,
};
pub use schema::full_tools as full_tool_definitions;
#[cfg(test)]
use schema::tools;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use controlfreak_core::{
    ActionObservation, BuildInfo, CaptureDisplayRequest, CaptureRegionRequest, ClickTextRequest,
    ClickTextResult, DisplayScreenshot, FocusWindowRequest, ForegroundWindow, KeyChordRequest,
    KeyboardActionResult, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MouseScrollRequest, MutationControl, OcrRegionRequest, PlatformBackend, PlatformError,
    PointerActionResult, TextInputRequest, UNKNOWN_REVISION, VirtualDesktopList,
    VirtualDesktopSwitchRequest, VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest,
    VisualChangeResult, WaitForVisualChangeRequest, WindowFocusResult, WindowScreenshot,
    WindowWaitResult,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorCode,
        Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::io::stdio,
};
use serde_json::{Value, json};

const STOP_DESKTOP_WORK: &str = "stop_desktop_work";
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
const WAIT_FOR_TEXT: &str = "wait_for_text";
const RUN_ACTIONS: &str = "run_actions";
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

const SERVER_INSTRUCTIONS: &str = "ControlFreak drives a Windows desktop. Workflow: list_windows (or any screenshot) gives a target_ref; begin_control_session(target_ref) once; focus_window; input tools; end_control_session when done. Input works only while the approved window, or a dialog or menu of its application, is in the foreground; another process needs a new session. x and y are display-local physical pixels, or pass a screenshot's observation_id with image_x and image_y. Each action returns a 1280 px screenshot taken once the window stops changing (see observation.settle); reuse it instead of capturing. Check isError and status; never repeat an action when retry_action=false. stop_desktop_work is an emergency stop that closes input permanently. stop_reason=user_stop or a session_stopped notification means the user ended control: ask before resuming; only the user can reopen input (see recovery).";

const RESTRICTED_PROFILE_INSTRUCTIONS: &str = "This server runs a restricted tool profile (get_server_status reports it): only the tools in tools/list exist, so skip workflow steps whose tools are missing.";

#[derive(Debug)]
enum BeginSessionError {
    Target(PlatformError),
    Session(SessionError),
}

impl From<SessionError> for BeginSessionError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

type ToolFuture =
    std::pin::Pin<Box<dyn Future<Output = Result<CallToolResponse, ErrorData>> + Send>>;
type ToolHandler =
    fn(Arc<dyn PlatformBackend>, Option<JsonObject>, Option<OperationLease>) -> ToolFuture;

/// The run phase of an input request that passed parsing and static validation. It needs the
/// backend and, when sessions are enabled, the operation lease.
type InputRun =
    Box<dyn FnOnce(Arc<dyn PlatformBackend>, Option<OperationLease>) -> ToolFuture + Send>;
/// Parses an input request and runs its static checks, with no backend and no lease. `Err` is the
/// refusal to return as is: `invalid_arguments` or `invalid_argument`, with `not_started`
/// progress. Live checks (target, display, observation lookup) stay in the run, after the lease.
type PreparedInput = Result<InputRun, Box<CallToolResponse>>;
/// The profile is for `run_actions`, which refuses a step whose tool the server does not serve.
type InputPrepare = fn(Option<JsonObject>, &Profile) -> PreparedInput;

fn is_mutating_tool(tool: &str) -> bool {
    catalog::spec(tool).is_some_and(ToolSpec::is_mutating)
}

#[derive(Clone)]
struct ControlFreakServer {
    backend: Arc<dyn PlatformBackend>,
    diagnostics: Arc<Diagnostics>,
    safety_indicator: SafetyIndicator,
    session_runtime: Option<Arc<SessionRuntime>>,
    stop: controlfreak_core::StopController,
    build: Arc<BuildInfo>,
    /// The tools this server serves, fixed for its lifetime.
    profile: Arc<Profile>,
}

impl ControlFreakServer {
    fn with_indicator(
        backend: Arc<dyn PlatformBackend>,
        safety_indicator: SafetyIndicator,
        session_runtime: Option<Arc<SessionRuntime>>,
    ) -> Self {
        let stop = session_runtime
            .as_ref()
            .map_or_else(controlfreak_core::StopController::default, |runtime| {
                runtime.stop.clone()
            });
        Self {
            stop,
            backend,
            diagnostics: Arc::new(Diagnostics::new()),
            safety_indicator,
            session_runtime,
            // The executable that serves passes its own build; this crate cannot see it.
            build: Arc::new(BuildInfo::new(
                env!("CARGO_PKG_VERSION"),
                UNKNOWN_REVISION,
                None,
                "unknown",
            )),
            profile: Arc::new(Profile::full()),
        }
    }

    /// Serves only the tools of `profile`.
    fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = Arc::new(profile);
        self
    }

    /// Runs one request through admission and its handler.
    async fn route_request(
        &self,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
        stages: Arc<Mutex<Vec<controlfreak_core::timing::StageTiming>>>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool_name = request.name.to_string();
        // A tool the profile hides gets the same answer as a tool that does not exist, before
        // admission, arbitration or a lease.
        let Some(spec) = self.profile.spec(&tool_name) else {
            return Err(ErrorData::invalid_params(
                format!("unknown ControlFreak tool '{tool_name}'"),
                Some(json!({ "tool": tool_name })),
            ));
        };
        match spec.route {
            Route::Stop => {
                parse_arguments::<NoArguments>(request.arguments)?;
                self.stop.stop();
                Ok(CallToolResult::structured(json!({"status": self.stop.status()})).into())
            }
            Route::Status => self.server_status(request.arguments),
            Route::EndSession => self.end_session(request.arguments).await,
            Route::BeginSession | Route::Observe(_) | Route::Watch(_) | Route::Input(_) => {
                self.admitted_request(spec, request, context, stages).await
            }
        }
    }

    fn server_status(&self, arguments: Option<JsonObject>) -> Result<CallToolResponse, ErrorData> {
        let result = call_get_server_status(
            &self.backend,
            &self.diagnostics,
            &self.safety_indicator,
            self.session_runtime.as_ref(),
            arguments,
        )?;
        if let CallToolResponse::Complete(result) = result
            && let Some(mut content) = result.structured_content
        {
            // `status` describes server and indicator health; whether input is accepted is
            // reported on its own, with the step that reopens it.
            let recovery = self.stop.recovery();
            content["build"] = json!(*self.build);
            content["input_admission"] = json!(if recovery.is_some() { "closed" } else { "open" });
            content["recovery"] = stop::recovery_json(recovery);
            content["profile"] = self.profile.status_json();
            content["stop_state"] = json!(controlfreak_core::StopRecovery::stop_state(recovery));
            if self.stop.user_stopped() {
                content["stop_reason"] = json!("user_stop");
            }
            return Ok(CallToolResult::structured(content).into());
        }
        Err(ErrorData::internal_error("missing server status", None))
    }

    async fn end_session(
        &self,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResponse, ErrorData> {
        parse_arguments::<NoArguments>(arguments)?;
        let Some(runtime) = self.session_runtime.clone() else {
            return Err(indicator_unavailable(
                &self.safety_indicator,
                "desktop glow sessions are unavailable",
            ));
        };
        let worker = Arc::clone(&runtime);
        let outcome = tokio::task::spawn_blocking(move || worker.end_session())
            .await
            .map_err(|error| join_error(&error))?
            .map_err(|reason| {
                let mut error = indicator_unavailable(&self.safety_indicator, &reason);
                if let Some(data) = error.data.as_mut() {
                    data["recovery"] = stop::recovery_json(self.stop.recovery());
                }
                error
            })?;
        Ok(CallToolResult::structured(json!({
            "status": outcome.as_str(),
            "session": runtime.status(),
            "recovery": stop::recovery_json(self.stop.recovery()),
        }))
        .into())
    }

    /// Admits a request that acts on the desktop or reads it, and runs it to completion.
    async fn admitted_request(
        &self,
        spec: &'static ToolSpec,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
        stages: Arc<Mutex<Vec<controlfreak_core::timing::StageTiming>>>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool_name = request.name.to_string();
        let route = spec.route;
        if spec.reads_screen {
            // Lets the indicator show a brief "reading" state; it claims no session.
            self.stop.note_observation();
        }
        let admission = match route {
            Route::Observe(_) => Ok(stop::Work::default()),
            Route::Watch(_) => Ok(stop::Work::new(&self.stop).unwrap_or_default()),
            _ => stop::Work::new(&self.stop),
        };
        let work = match admission {
            Ok(work) => work.with_stages(stages),
            Err(error) => return Ok(self.refused(&tool_name, &error)),
        };
        let cancellation = stop::CancelOnDrop(work.control.clone());
        let request_control = work.control.clone();
        let operation = stop::WORK.scope(work, self.run_admitted(route, request));
        tokio::pin!(operation);
        let result = tokio::select! {
            biased;
            () = context.ct.cancelled() => {
                request_control.cancel();
                operation.await
            }
            result = &mut operation => result,
        };
        drop(cancellation);
        result
    }

    /// The tool result for a request that admission refused.
    fn refused(&self, tool_name: &str, error: &PlatformError) -> CallToolResponse {
        let response = if self.stop.is_stopped() {
            self.session_runtime
                .as_ref()
                .map_or_else(
                    || SessionError::stopped(&self.stop),
                    |runtime| runtime.stop_refusal(),
                )
                .into_tool_result(&self.safety_indicator)
                .into()
        } else {
            tool_error(error).into()
        };
        if is_mutating_tool(tool_name) {
            results::with_progress(response, controlfreak_core::MutationProgress::default())
        } else {
            response
        }
    }

    async fn run_admitted(
        &self,
        route: Route,
        request: CallToolRequestParams,
    ) -> Result<CallToolResponse, ErrorData> {
        let backend = Arc::clone(&self.backend);
        match route {
            Route::BeginSession => self.begin_session(request.arguments).await,
            Route::Observe(handler) | Route::Watch(handler) => {
                handler(backend, request.arguments, None).await
            }
            Route::Input(prepare) => {
                let is_switch = request.name == SWITCH_VIRTUAL_DESKTOP;
                // Parse and check the arguments before taking the desktop: a refused request must
                // not claim arbitration, arm the idle hold or show the indicator.
                let run = match prepare(request.arguments, &self.profile) {
                    Ok(run) => run,
                    Err(refusal) => return Ok(*refusal),
                };
                let Some(runtime) = self.session_runtime.as_ref() else {
                    return run(backend, None).await;
                };
                let lease = match runtime.acquire_mutation().await {
                    Ok(lease) => lease,
                    Err(error) => {
                        return Ok(results::with_progress(
                            error.into_tool_result(&self.safety_indicator).into(),
                            controlfreak_core::MutationProgress::default(),
                        ));
                    }
                };
                let response = run(backend, Some(lease)).await?;
                if is_switch && results::input_was_sent(&response) {
                    // The approved window is no longer on screen, so the session ends with the switch.
                    let runtime = Arc::clone(runtime);
                    let ended = tokio::task::spawn_blocking(move || runtime.end_session())
                        .await
                        .map_err(|error| join_error(&error))?;
                    if let Err(reason) = ended {
                        eprintln!(
                            "{}",
                            json!({"event": "session_end_after_switch_failed", "reason": reason})
                        );
                    }
                }
                Ok(response)
            }
            Route::Stop | Route::Status | Route::EndSession => {
                unreachable!("lifecycle tools are handled before admission")
            }
        }
    }

    async fn begin_session(
        &self,
        arguments: Option<JsonObject>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(runtime) = self.session_runtime.as_ref() else {
            return Err(indicator_unavailable(
                &self.safety_indicator,
                "desktop glow sessions are unavailable",
            ));
        };
        let input = parse_arguments::<BeginControlSessionInput>(arguments)?;
        let runtime_for_worker = Arc::clone(runtime);
        let work = stop::current();
        let backend = Arc::clone(&self.backend);
        let started = tokio::task::spawn_blocking(move || {
            // A stop or shutdown cancels the request; report it as such.
            let cancelled = |error| {
                if runtime_for_worker.stop.is_stopped() {
                    runtime_for_worker.stop_refusal().into()
                } else {
                    error
                }
            };
            if let Some(work) = &work {
                work.control
                    .check("begin_control_session")
                    .map_err(|error| cancelled(BeginSessionError::Target(error)))?;
            }
            backend
                .validate_target_reference(&input.target_ref)
                .map_err(BeginSessionError::Target)?;
            // The runtime checks cancellation under its lifecycle lock, so a cancelled renewal
            // keeps the session and a request cancelled by a stop reports the stop.
            runtime_for_worker.begin_session(
                input.expected_seconds,
                &input.target_ref,
                work.as_ref().map(|work| &work.control),
            )
        })
        .await;
        match started {
            Ok(Ok(())) => Ok(CallToolResult::structured(json!({
                "status": "armed",
                "session": runtime.status(),
            }))
            .into()),
            Ok(Err(BeginSessionError::Target(error))) => Ok(tool_error(&error).into()),
            Ok(Err(BeginSessionError::Session(error))) => {
                Ok(error.into_tool_result(&self.safety_indicator).into())
            }
            Err(error) => Err(join_error(&error)),
        }
    }
}

impl ServerHandler for ControlFreakServer {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        let notification =
            |method: &str| serde_json::Map::from_iter([("notification".to_owned(), json!(method))]);
        capabilities.experimental = Some(std::collections::BTreeMap::from([
            (
                "controlfreak/user-stop".to_owned(),
                notification(stop::STOPPED_NOTIFICATION),
            ),
            (
                "controlfreak/user-rearm".to_owned(),
                notification(stop::REARMED_NOTIFICATION),
            ),
        ]));
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(
                "controlfreak",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(if self.profile.is_full() {
                SERVER_INSTRUCTIONS.to_owned()
            } else {
                format!("{SERVER_INSTRUCTIONS} {RESTRICTED_PROFILE_INSTRUCTIONS}")
            })
    }

    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        let stop = self.stop.clone();
        tokio::spawn(async move {
            // The stop latch wakes this task; the timeout below only notices a closed transport.
            let changed = Arc::new(tokio::sync::Notify::new());
            let waker = Arc::downgrade(&changed);
            stop.subscribe(move || {
                waker
                    .upgrade()
                    .map(|changed| changed.notify_one())
                    .is_some()
            });
            // One notification when the user stops, one more when a drain ends, so the client
            // learns when the user's step applies, and one when the user allows input again.
            // A re-arm starts a new stop epoch, so a later stop is reported afresh.
            let mut epoch = stop.stop_epoch();
            let mut reported: Option<controlfreak_core::StopRecovery> = None;
            while !context.peer.is_transport_closed() {
                let now = stop.snapshot();
                if now.epoch != epoch {
                    epoch = now.epoch;
                    reported = None;
                    let _ = context
                        .peer
                        .send_notification(stop::rearmed_notification())
                        .await;
                }
                if now.user_stop
                    && let Some(current) = now.recovery
                    && reported != Some(current)
                {
                    let _ = context
                        .peer
                        .send_notification(stop::stopped_notification(current, reported))
                        .await;
                    reported = Some(current);
                }
                let _ = tokio::time::timeout(Duration::from_secs(5), changed.notified()).await;
            }
        });
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(schema::tools_for(
            &self.profile,
        ))))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let server = self.clone();
        async move {
            let diagnostics = Arc::clone(&server.diagnostics);
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
            let request_stages = Arc::new(Mutex::new(Vec::new()));
            let outcome = match server
                .route_request(request, &context, Arc::clone(&request_stages))
                .await
            {
                Ok(response) => Ok(response),
                Err(error) => {
                    let response = tool_execution_error(&error).into();
                    // A hidden tool's error stays the unknown-tool error, with no progress fields.
                    let mutating = server
                        .profile
                        .spec(&tool_name)
                        .is_some_and(ToolSpec::is_mutating);
                    Ok(if mutating {
                        results::with_progress(
                            response,
                            controlfreak_core::MutationProgress::default(),
                        )
                    } else {
                        response
                    })
                }
            };
            let stages = std::mem::take(
                &mut *request_stages
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            let stages_ms = diagnostics::stages_json(&stages);
            let elapsed_ms = diagnostics.complete(
                &tool_name,
                operation_id,
                started,
                gap_ms,
                &outcome,
                stages_ms.clone(),
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
                            "stages_ms": stages_ms,
                        }),
                    );
                    result.meta = Some(meta);
                }
                response
            })
        }
    }
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
    build: BuildInfo,
    profile: Profile,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    serve_stdio_inner(
        Arc::from(backend),
        SafetyIndicator::ready(),
        None,
        build,
        profile,
    )
    .await
}

pub async fn serve_stdio_with_indicator_and_arbitrator<F, G>(
    backend: Box<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    start_indicator: F,
    arbitrator: Arc<dyn ActivityArbitrator>,
    stop: controlfreak_core::StopController,
    build: BuildInfo,
    profile: Profile,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    F: Fn() -> Result<G, String> + Send + Sync + 'static,
    G: ActivityIndicator + 'static,
{
    let backend: Arc<dyn PlatformBackend> = Arc::from(backend);
    let mut runtime = SessionRuntime::new_with_timing(
        safety_indicator.clone(),
        start_indicator,
        arbitrator,
        glow_timing_from_environment(),
    );
    runtime.stop = stop;
    runtime.environment = Some(Arc::clone(&backend));
    let session_runtime = Arc::new(runtime);
    serve_stdio_inner(
        backend,
        safety_indicator,
        Some(session_runtime),
        build,
        profile,
    )
    .await
}

// rmcp drains responses after EOF. Close admission at EOF itself, before that drain.
struct DisconnectReader<R> {
    inner: R,
    runtime: Option<Arc<SessionRuntime>>,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for DisconnectReader<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        let has_capacity = buffer.remaining() != 0;
        let result = std::pin::Pin::new(&mut this.inner).poll_read(context, buffer);
        if has_capacity
            && matches!(&result, std::task::Poll::Ready(result) if result.is_err() || buffer.filled().len() == before)
            && let Some(runtime) = this.runtime.take()
        {
            runtime.terminated.store(true, Ordering::Release);
            runtime.stop.stop();
            tokio::task::spawn_blocking(move || runtime.close_session("disconnect", true));
        }
        result
    }
}

struct ServerCleanup(Option<Arc<SessionRuntime>>);

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

async fn wait_for_cleanup(runtime: &SessionRuntime, timeout: Duration) -> std::io::Result<()> {
    tokio::time::timeout(timeout, async {
        while runtime.status()["draining"] == true {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "control-session cleanup is still draining",
        )
    })
}

async fn serve_stdio_inner(
    backend: Arc<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    session_runtime: Option<Arc<SessionRuntime>>,
    build: BuildInfo,
    profile: Profile,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let cleanup = ServerCleanup(session_runtime.clone());
    let profile_json = profile.status_json();
    let mut server = ControlFreakServer::with_indicator(backend, safety_indicator, session_runtime)
        .with_profile(profile);
    server.build = Arc::new(build);
    let instance_id = server.diagnostics.instance_id.clone();
    eprintln!(
        "{}",
        json!({
            "event": "server_started",
            "instance_id": instance_id,
            "process_id": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "build": *server.build,
            "profile": profile_json,
        })
    );
    let (stdin, stdout) = stdio();
    let reader = DisconnectReader {
        inner: stdin,
        runtime: cleanup.0.clone(),
    };
    let service = server.serve((reader, stdout)).await?;
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
        wait_for_cleanup(runtime, Duration::from_secs(30)).await?;
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
mod tests;
