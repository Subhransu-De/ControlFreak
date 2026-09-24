#![forbid(unsafe_code)]

#[cfg(test)]
mod cleanup_tests;
mod diagnostics;
mod handlers;
mod requests;
mod session;
mod stop;
#[cfg(test)]
mod stop_tests;

use diagnostics::Diagnostics;
use requests::{
    BeginControlSessionInput, CaptureDisplayInput, CaptureRegionInput, CaptureWindowInput,
    ClickMouseInput, ClickTextInput, DragMouseInput, FindTextInput, FocusWindowInput,
    MoveMouseInput, NoArguments, OcrRegionInput, PressKeysInput, ScrollMouseInput,
    SwitchVirtualDesktopInput, TypeTextInput, VisualBaselineInput, WaitForChangeSinceInput,
    WaitForVisualChangeInput, WaitForWindowInput, parse_arguments,
};
pub use session::{
    ActivityArbitrator, ActivityIndicator, ArbitrationBusy, IndicatorHealth, IndicatorLevel,
    SafetyIndicator,
};
#[cfg(test)]
use session::{GlowTiming, IdleCommand, IdleWorker};
use session::{
    IndicatorRuntime, LocalArbitrator, OperationLease, glow_timing_from_environment,
    indicator_unavailable,
};
#[cfg(test)]
mod outcome_tests;
#[cfg(test)]
mod recipe_tests;
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
pub use schema::tools as tool_definitions;
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
    ActionObservation, CaptureDisplayRequest, CaptureRegionRequest, CaptureWindowRequest,
    ClickTextRequest, ClickTextResult, DisplayScreenshot, FocusWindowRequest, KeyChordRequest,
    KeyboardActionResult, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MouseScrollRequest, MutationControl, OcrRegionRequest, PlatformBackend, PlatformError,
    PointerActionResult, TextInputRequest, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForVisualChangeRequest, WaitForWindowRequest, WindowFocusResult,
    WindowScreenshot, WindowWaitResult,
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

const LIST_DISPLAYS: &str = "list_displays";
const GET_SERVER_STATUS: &str = "get_server_status";
const BEGIN_CONTROL_SESSION: &str = "begin_control_session";
#[derive(Debug)]
enum BeginSessionError {
    Target(PlatformError),
    Indicator(String),
}
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
    stop: controlfreak_core::StopController,
}

impl ControlFreakServer {
    fn with_indicator(
        backend: Arc<dyn PlatformBackend>,
        safety_indicator: SafetyIndicator,
        indicator_runtime: Option<Arc<IndicatorRuntime>>,
    ) -> Self {
        let stop = indicator_runtime
            .as_ref()
            .map_or_else(controlfreak_core::StopController::default, |runtime| {
                runtime.stop.clone()
            });
        Self {
            stop,
            backend,
            diagnostics: Arc::new(Diagnostics::new()),
            safety_indicator,
            indicator_runtime,
        }
    }
}

impl ServerHandler for ControlFreakServer {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        capabilities.experimental = Some(std::collections::BTreeMap::from([(
            "controlfreak/user-stop".to_owned(),
            serde_json::Map::from_iter([(
                "notification".to_owned(),
                json!("notifications/controlfreak/session_stopped"),
            )]),
        )]));
        ServerConfig::new(capabilities)
            .with_server_info(Implementation::new(
                "controlfreak",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "ControlFreak is a computer-use MCP server. For tasks with multiple input actions, call begin_control_session first and end_control_session when finished. Actions return screenshots by default. Reuse them; capture again only when needed. Choose a server-issued target_ref from current window or screenshot observations. Pass it to begin_control_session and every input action; focus_window uses window_id. End the session before changing targets. Prefer click_text for unique visible labels. Check isError, status, and error text. Input dispatch and observation do not verify application effects. Observe again before recovering from partial or unknown delivery. Read structuredContent without serializing image data. Never repeat an action when retry_action=false. A notifications/controlfreak/session_stopped event or stop_reason=user_stop means the user ended control. Do not resume desktop actions or restart the server without the user's permission.",
            )
    }

    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        let stop = self.stop.clone();
        tokio::spawn(async move {
            while !context.peer.is_transport_closed() {
                if stop.user_stopped() {
                    let notification = rmcp::model::CustomNotification::new(
                        "notifications/controlfreak/session_stopped",
                        Some(json!({
                            "event": "user_stopped_session",
                            "reason": "user_stop",
                            "message": "The user stopped the ControlFreak session. Do not resume desktop actions or restart the server without the user's permission.",
                            "stop_state": stop.status(),
                            "retry_action": false,
                        })),
                    );
                    let _ = context
                        .peer
                        .send_notification(rmcp::model::ServerNotification::CustomNotification(
                            notification,
                        ))
                        .await;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
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
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let stop = self.stop.clone();
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
            let reporting = Arc::clone(&diagnostics);
            let report_name = tool_name.clone();
            let request_future = async move {
                if request.name == "stop_desktop_work" {
                    parse_arguments::<NoArguments>(request.arguments)?;
                    stop.stop();
                    return Ok(CallToolResult::structured(json!({"status": stop.status()})).into());
                }
                if request.name == GET_SERVER_STATUS {
                    let result = call_get_server_status(
                        &backend,
                        &diagnostics,
                        &safety_indicator,
                        indicator_runtime.as_ref(),
                        request.arguments,
                    )?;
                    if let CallToolResponse::Complete(result) = result
                        && let Some(mut content) = result.structured_content
                    {
                        content["stop_state"] = json!(stop.status());
                        if stop.user_stopped() {
                            content["stop_reason"] = json!("user_stop");
                        }
                        return Ok(CallToolResult::structured(content).into());
                    }
                    return Err(ErrorData::internal_error("missing server status", None));
                }
                if request.name == END_CONTROL_SESSION {
                    parse_arguments::<NoArguments>(request.arguments)?;
                    if let Some(runtime) = indicator_runtime {
                        let worker = runtime.clone();
                        tokio::task::spawn_blocking(move || worker.end_session())
                            .await
                            .map_err(|error| join_error(&error))?
                            .map_err(|reason| indicator_unavailable(&safety_indicator, &reason))?;
                        return Ok(CallToolResult::structured(
                            json!({"status": "closing", "session": runtime.status()}),
                        )
                        .into());
                    }
                    return Err(indicator_unavailable(
                        &safety_indicator,
                        "desktop glow sessions are unavailable",
                    ));
                }
                let admission = if matches!(
                    request.name.as_ref(),
                    LIST_DISPLAYS
                        | LIST_WINDOWS
                        | LIST_VIRTUAL_DESKTOPS
                        | CAPTURE_DISPLAY
                        | CAPTURE_REGION
                        | CAPTURE_WINDOW
                        | CAPTURE_VISUAL_BASELINE
                ) {
                    // Observations remain available after stop, but retain request cancellation.
                    Ok(stop::Work::default())
                } else {
                    stop::Work::new(&stop)
                };
                let work = match admission {
                    Ok(work) => work,
                    Err(error) => {
                        let response = tool_error(&error).into();
                        return Ok(if is_mutating_tool(&request.name) {
                            results::with_progress(
                                response,
                                controlfreak_core::MutationProgress::default(),
                            )
                        } else {
                            response
                        });
                    }
                };
                let cancellation = stop::CancelOnDrop(work.control.clone());
                let request_control = work.control.clone();
                let operation = stop::WORK.scope(work, async move {
                    if tool_name == BEGIN_CONTROL_SESSION {
                        match indicator_runtime.as_ref() {
                            Some(runtime) => {
                                let input =
                                    parse_arguments::<BeginControlSessionInput>(request.arguments)?;
                                let runtime_for_worker = Arc::clone(runtime);
                                let work = stop::current();
                                let backend = Arc::clone(&backend);
                                match tokio::task::spawn_blocking(move || {
                                    if let Some(work) = &work {
                                        work.control
                                            .check("begin_control_session")
                                            .map_err(BeginSessionError::Target)?;
                                    }
                                    backend
                                        .validate_target_reference(&input.target_ref)
                                        .map_err(BeginSessionError::Target)?;
                                    runtime_for_worker
                                        .begin_session(input.expected_seconds, &input.target_ref)?;
                                    if work
                                        .as_ref()
                                        .is_some_and(|work| work.control.is_cancelled())
                                    {
                                        runtime_for_worker
                                            .end_session()
                                            .map_err(BeginSessionError::Indicator)?;
                                        return Err(BeginSessionError::Indicator(
                                            "session request was cancelled".to_owned(),
                                        ));
                                    }
                                    Ok(())
                                })
                                .await
                                {
                                    Ok(Ok(())) => Ok(CallToolResult::structured(json!({
                                        "status": "armed",
                                        "session": runtime.status(),
                                    }))
                                    .into()),
                                    Ok(Err(BeginSessionError::Target(error))) => {
                                        Ok(tool_error(&error).into())
                                    }
                                    Ok(Err(BeginSessionError::Indicator(reason))) => {
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
                    }
                });
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
            };
            let outcome = match request_future.await {
                Ok(response) => Ok(response),
                Err(error) => {
                    let response = tool_execution_error(&error).into();
                    Ok(if is_mutating_tool(&report_name) {
                        results::with_progress(
                            response,
                            controlfreak_core::MutationProgress::default(),
                        )
                    } else {
                        response
                    })
                }
            };
            let elapsed_ms =
                reporting.complete(&report_name, operation_id, started, gap_ms, &outcome);
            outcome.map(|mut response| {
                if let CallToolResponse::Complete(result) = &mut response {
                    let mut meta = result.meta.take().unwrap_or_default();
                    meta.insert(
                        "controlfreak".to_owned(),
                        json!({
                            "instance_id": reporting.instance_id,
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
        controlfreak_core::StopController::default(),
    )
    .await
}

pub async fn serve_stdio_with_indicator_and_arbitrator<F, G>(
    backend: Box<dyn PlatformBackend>,
    safety_indicator: SafetyIndicator,
    start_indicator: F,
    arbitrator: Arc<dyn ActivityArbitrator>,
    stop: controlfreak_core::StopController,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    F: Fn() -> Result<G, String> + Send + Sync + 'static,
    G: ActivityIndicator + 'static,
{
    let mut runtime = IndicatorRuntime::new_with_timing(
        safety_indicator.clone(),
        start_indicator,
        arbitrator,
        glow_timing_from_environment(),
    );
    runtime.stop = stop;
    let indicator_runtime = Arc::new(runtime);
    serve_stdio_inner(backend, safety_indicator, Some(indicator_runtime)).await
}

// rmcp drains responses after EOF. Close admission at EOF itself, before that drain.
struct DisconnectReader<R> {
    inner: R,
    runtime: Option<Arc<IndicatorRuntime>>,
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
            runtime.stop.stop();
            runtime.terminated.store(true, Ordering::Release);
            tokio::task::spawn_blocking(move || runtime.close_session("disconnect", true));
        }
        result
    }
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

async fn wait_for_cleanup(runtime: &IndicatorRuntime, timeout: Duration) -> std::io::Result<()> {
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
