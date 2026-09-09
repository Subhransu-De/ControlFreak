#![deny(unsafe_code)]

use std::{
    ffi::c_void,
    io::{Read, Write},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::OcrHelperCommand;
use controlfreak_core::{
    ActionObservation, BackendIdentity, BackendMetadata, Capability, CapabilityDescriptor,
    CapabilityState, CaptureDisplayRequest, CaptureRegionRequest, CaptureWindowRequest,
    ClickTextRequest, ClickTextResult, DisplayBackend, DisplayBounds, DisplayInfo,
    DisplayScreenshot, DisplayServer, FindTextRequest, FindTextResult, FocusWindowRequest, Key,
    KeyChordRequest, KeyboardActionResult, KeyboardBackend, MouseButton, MouseClickRequest,
    MouseDragRequest, MouseMoveRequest, MousePosition, MouseScrollRequest, MutationControl,
    ObservationMode, ObservationOptions, OcrBackend, OcrLine, OcrRegionRequest, OcrResult, OcrWord,
    PermissionDescriptor, PermissionId, PermissionState, Platform, PlatformError,
    PointerActionResult, PointerBackend, SecurityContext, TextInputRequest, TextMatch,
    VirtualDesktopDirection, VirtualDesktopInfo, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForVisualChangeRequest, WaitForWindowRequest, WindowBackend,
    WindowBounds, WindowFocusResult, WindowInfo, WindowScreenshot, WindowWaitResult,
};
use windows::Win32::{
    Foundation::{E_ACCESSDENIED, HWND, LPARAM, POINT, RECT, RPC_E_CHANGED_MODE},
    Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC,
        GetDIBits, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST,
        MONITORINFOEXW, MonitorFromWindow, ReleaseDC, SRCCOPY, SelectObject,
    },
    System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    },
    UI::{
        HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
            MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
            MOUSEINPUT, SendInput, VIRTUAL_KEY,
        },
        Shell::{IVirtualDesktopManager, VirtualDesktopManager},
        WindowsAndMessaging::{
            BringWindowToTop, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetCursorPos,
            GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
            GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
            MONITORINFOF_PRIMARY, SW_RESTORE, SetCursorPos, SetForegroundWindow, ShowWindowAsync,
            WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        },
    },
};
use windows::core::BOOL;
mod arbitration;
mod capture;
mod desktop;
mod environment;
mod glow;
mod input;
mod privilege;
mod process;

pub use arbitration::DesktopArbitrator;
pub use process::ManagedChild;

pub(crate) fn spawn_helper_process(
    program: &std::path::Path,
    arguments: &[std::ffi::OsString],
) -> std::io::Result<ManagedChild> {
    process::spawn_managed_child(program, arguments)
}

pub(crate) fn serve_desktop_glow_helper(arguments: &[String]) -> std::io::Result<Option<()>> {
    let Some(config) = glow::HelperConfig::parse(arguments)? else {
        return Ok(None);
    };
    glow::run(config)?;
    Ok(Some(()))
}

pub(crate) fn server_security_context(
    elevated_operation_allowed: bool,
) -> Result<SecurityContext, PlatformError> {
    privilege::current_security_context(elevated_operation_allowed)
}
use capture::{
    baseline_process_nonce, capture_bounds, capture_comparison_frame, capture_display_bounds,
    capture_display_image, display_bounds_from_window, display_region_bounds,
    foreground_window_info, frame_fingerprint, observe_display, observe_foreground,
    recognize_text as recognize_text_in_process, validate_max_width, validate_visual_wait,
    validate_wait_values, wait_for_change,
};
use desktop::{
    ComApartment, activate_window, bring_window_to_top, current_cursor_position,
    current_virtual_desktop_id, ensure_dpi_awareness, enumerate_displays, enumerate_windows,
    find_display, find_display_at_point, find_window, foreground_window_handle, parse_window_id,
    restore_window, validate_window_wait, virtual_desktop_manager, window_desktop_id, window_info,
    window_is_on_current_desktop, window_matches,
};
use environment::ensure_interactive_input_desktop;
use input::{
    move_cursor, post_action_error, send_click, send_drag_press, send_drag_release, send_key_chord,
    send_scroll, send_unicode_text, validate_click_request, validate_key_chord, validate_modifiers,
};

use windows::{
    Globalization::Language,
    Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap},
    Media::Ocr::OcrEngine,
    Storage::Streams::DataWriter,
    core::HSTRING,
};

const MAX_CAPTURE_BYTES: usize = 512 * 1024 * 1024;
const MAX_MOVE_DURATION_MS: u32 = 10_000;
const MAX_SCROLL_DELTA: u32 = 12_000;
const MOVE_FRAME_MS: u64 = 16;
const POST_INPUT_SETTLE_MS: u64 = 75;
const POST_FOCUS_SETTLE_MS: u64 = 150;
const MAX_ACTION_IMAGE_WIDTH: u32 = 1_280;
const MAX_TEXT_UTF16_UNITS: usize = 4_000;
const MAX_CHORD_KEYS: usize = 8;
const MAX_WINDOWS: usize = 200;
const MAX_WINDOW_TITLE_UTF16_UNITS: usize = 512;
const MAX_CLICK_COUNT: u8 = 3;
const MAX_CAPTURE_WIDTH: u32 = 7_680;
const MAX_WAIT_MS: u32 = 30_000;
const VISUAL_POLL_MS: u64 = 100;
const VISUAL_COMPARE_WIDTH: u32 = 320;
const MAX_DESKTOP_SWITCH_STEPS: u8 = 10;
const DESKTOP_SWITCH_SETTLE_MS: u64 = 300;
const VISUAL_BASELINE_TTL_MS: u32 = 300_000;
const MAX_VISUAL_BASELINES: usize = 16;
const MAX_OCR_RESULTS: u32 = 100;
const OCR_HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const OCR_HELPER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone)]
struct StoredVisualBaseline {
    id: String,
    display: DisplayInfo,
    bounds: DisplayBounds,
    frame: Vec<u8>,
    created_at: Instant,
}

#[derive(Clone, Copy)]
struct PointerActionSpec<'a> {
    display_id: &'a str,
    x: u32,
    y: u32,
    duration_ms: u32,
    observation: &'a ObservationOptions,
}

pub struct Backend {
    input_lock: Mutex<()>,
    visual_baselines: Mutex<Vec<StoredVisualBaseline>>,
    next_baseline_id: AtomicU64,
    ocr_helper: Option<OcrHelperCommand>,
    security_context: SecurityContext,
}

impl Backend {
    pub fn new() -> Self {
        let security_context = privilege::current_security_context(false).unwrap_or_default();
        Self {
            input_lock: Mutex::new(()),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: None,
            security_context,
        }
    }

    pub fn with_security(security_context: SecurityContext) -> Self {
        Self {
            input_lock: Mutex::new(()),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: None,
            security_context,
        }
    }

    pub fn with_ocr_helper_and_security(
        command: OcrHelperCommand,
        security_context: SecurityContext,
    ) -> Self {
        Self {
            input_lock: Mutex::new(()),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: Some(command),
            security_context,
        }
    }

    fn recognize_text(&self, request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        self.ocr_helper.as_ref().map_or_else(
            || recognize_text_in_process(request),
            |command| recognize_text_with_helper(command, request),
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OcrHelperResponse {
    Success { result: OcrResult },
    Error { error: PlatformError },
}

fn recognize_text_with_helper(
    command: &OcrHelperCommand,
    request: &OcrRegionRequest,
) -> Result<OcrResult, PlatformError> {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let request_json =
        serde_json::to_vec(request).map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("could not encode the OCR request: {error}"),
        })?;
    let mut child = Command::new(command.program())
        .args(command.arguments())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("could not start the isolated OCR process: {error}"),
        })?;

    let send_error = match child.stdin.take() {
        Some(mut stdin) => stdin
            .write_all(&request_json)
            .err()
            .map(|error| format!("could not send the OCR request: {error}")),
        None => Some("the isolated OCR process did not expose stdin".to_owned()),
    };
    if let Some(reason) = send_error {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("{reason}{cleanup}"),
        });
    }

    let output = wait_for_ocr_helper(child, OCR_HELPER_TIMEOUT)?;
    if !output.status.success() {
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: ocr_helper_failure(output.status, &output.stderr),
        });
    }

    let response: OcrHelperResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process returned invalid output: {error}"),
        })?;
    match response {
        OcrHelperResponse::Success { result } => Ok(result),
        OcrHelperResponse::Error { error } => Err(error),
    }
}

fn wait_for_ocr_helper(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, PlatformError> {
    let Some(stdout) = child.stdout.take() else {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process did not expose stdout{cleanup}"),
        });
    };
    let Some(stderr) = child.stderr.take() else {
        let cleanup = terminate_ocr_helper(&mut child);
        return Err(PlatformError::OperationFailed {
            operation: "windows_ocr_helper".to_owned(),
            reason: format!("the isolated OCR process did not expose stderr{cleanup}"),
        });
    };
    let stdout_reader = match thread::Builder::new()
        .name("controlfreak-ocr-stdout".to_owned())
        .spawn(move || read_ocr_pipe(stdout))
    {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = terminate_ocr_helper(&mut child);
            return Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason: format!("could not start the OCR stdout reader: {error}{cleanup}"),
            });
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("controlfreak-ocr-stderr".to_owned())
        .spawn(move || read_ocr_pipe(stderr))
    {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = terminate_ocr_helper(&mut child);
            let _ = stdout_reader.join();
            return Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason: format!("could not start the OCR stderr reader: {error}{cleanup}"),
            });
        }
    };
    let started = Instant::now();
    let process_result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(
                    OCR_HELPER_POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())),
                );
            }
            Ok(None) => {
                let cleanup = terminate_ocr_helper(&mut child);
                break Err(format!(
                    "the isolated OCR process timed out after {} seconds{cleanup}",
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let cleanup = terminate_ocr_helper(&mut child);
                break Err(format!(
                    "could not poll the isolated OCR process: {error}{cleanup}"
                ));
            }
        }
    };

    let stdout = join_ocr_pipe(stdout_reader, "stdout");
    let stderr = join_ocr_pipe(stderr_reader, "stderr");
    match (process_result, stdout, stderr) {
        (Ok(status), Ok(stdout), Ok(stderr)) => Ok(std::process::Output {
            status,
            stdout,
            stderr,
        }),
        (Err(reason), _, _) | (_, Err(reason), _) | (_, _, Err(reason)) => {
            Err(PlatformError::OperationFailed {
                operation: "windows_ocr_helper".to_owned(),
                reason,
            })
        }
    }
}

fn read_ocr_pipe(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_ocr_pipe(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("the OCR {stream} reader panicked"))?
        .map_err(|error| format!("could not read OCR {stream}: {error}"))
}

fn terminate_ocr_helper(child: &mut std::process::Child) -> String {
    let kill_error = child.kill().err();
    let wait_error = child.wait().err();
    match (kill_error, wait_error) {
        (None, None) => String::new(),
        (Some(kill), None) => format!("; cleanup kill reported: {kill}"),
        (None, Some(wait)) => format!("; cleanup wait failed: {wait}"),
        (Some(kill), Some(wait)) => {
            format!("; cleanup kill reported: {kill}; cleanup wait failed: {wait}")
        }
    }
}

pub(crate) fn serve_ocr_helper(
    mut input: impl Read,
    output: impl Write,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut request_json = Vec::new();
    input.read_to_end(&mut request_json)?;
    let request: OcrRegionRequest = serde_json::from_slice(&request_json)?;
    let response = match recognize_text_in_process(&request) {
        Ok(result) => OcrHelperResponse::Success { result },
        Err(error) => OcrHelperResponse::Error { error },
    };
    serde_json::to_writer(output, &response)?;
    Ok(())
}

fn ocr_helper_failure(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let status = status.code().map_or_else(
        || "without an exit code".to_owned(),
        |code| {
            let unsigned = u32::from_ne_bytes(code.to_ne_bytes());
            format!("with exit code 0x{unsigned:08x}")
        },
    );
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("the isolated OCR process exited {status}")
    } else {
        let tail_start = stderr
            .char_indices()
            .rev()
            .nth(1_999)
            .map_or(0, |(index, _)| index);
        format!(
            "the isolated OCR process exited {status}: {}",
            &stderr[tail_start..]
        )
    }
}

impl BackendMetadata for Backend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Windows,
            display_server: DisplayServer::Dwm,
            backend_id: "windows-win32".to_owned(),
        }
    }

    fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        Capability::ALL
            .into_iter()
            .map(|capability| {
                let state = match capability {
                    Capability::ScreenCapture
                    | Capability::WindowEnumeration
                    | Capability::PointerMovement
                    | Capability::InputInjection => CapabilityState::Supported,
                    Capability::AccessibilityTree | Capability::Elevation => {
                        CapabilityState::Unsupported {
                            reason: "native implementation is not available yet".to_owned(),
                        }
                    }
                };

                CapabilityDescriptor {
                    capability,
                    state,
                    required_permissions: Vec::new(),
                }
            })
            .collect()
    }

    fn permissions(&self) -> Vec<PermissionDescriptor> {
        vec![PermissionDescriptor {
            permission: PermissionId::UiAccess,
            state: PermissionState::Unknown,
            purpose: "Future interaction with higher-integrity desktop applications".to_owned(),
        }]
    }

    fn security_context(&self) -> SecurityContext {
        self.security_context
    }
}

impl DisplayBackend for Backend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        ensure_dpi_awareness()?;
        enumerate_displays()
    }

    fn capture_display(
        &self,
        request: &CaptureDisplayRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        ensure_dpi_awareness()?;
        validate_max_width(request.max_width)?;
        let display = find_display(&request.display_id)?;
        capture_display_image(display, request.max_width, request.include_cursor)
    }

    fn capture_region(
        &self,
        request: &CaptureRegionRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        ensure_dpi_awareness()?;
        validate_max_width(request.max_width)?;
        let display = find_display(&request.display_id)?;
        let bounds = display_region_bounds(
            &display,
            request.x,
            request.y,
            request.width,
            request.height,
        )?;
        capture_display_bounds(display, bounds, request.max_width, request.include_cursor)
    }

    fn wait_for_visual_change(
        &self,
        request: &WaitForVisualChangeRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_visual_wait(request)?;
        let display = find_display(&request.display_id)?;
        let bounds = display_region_bounds(
            &display,
            request.x,
            request.y,
            request.width,
            request.height,
        )?;
        let baseline = capture_comparison_frame(bounds)?;
        wait_for_change(
            display,
            bounds,
            &baseline,
            request.timeout_ms,
            request.stable_ms,
            request.difference_threshold,
        )
    }

    fn capture_visual_baseline(
        &self,
        request: &VisualBaselineRequest,
    ) -> Result<VisualBaseline, PlatformError> {
        ensure_dpi_awareness()?;
        let display = find_display(&request.display_id)?;
        let bounds = display_region_bounds(
            &display,
            request.x,
            request.y,
            request.width,
            request.height,
        )?;
        let frame = capture_comparison_frame(bounds)?;
        let id = format!(
            "vb-{:032x}-{:016x}",
            baseline_process_nonce(),
            self.next_baseline_id.fetch_add(1, Ordering::Relaxed)
        );
        let fingerprint = frame_fingerprint(&frame);
        let created_at = Instant::now();
        let mut baselines = self
            .visual_baselines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        baselines.retain(|baseline| {
            created_at.saturating_duration_since(baseline.created_at)
                < Duration::from_millis(u64::from(VISUAL_BASELINE_TTL_MS))
        });
        if baselines.len() >= MAX_VISUAL_BASELINES {
            baselines.remove(0);
        }
        baselines.push(StoredVisualBaseline {
            id: id.clone(),
            display: display.clone(),
            bounds,
            frame,
            created_at,
        });
        Ok(VisualBaseline {
            baseline_id: id,
            display,
            source_bounds: bounds,
            fingerprint,
            expires_in_ms: VISUAL_BASELINE_TTL_MS,
        })
    }

    fn wait_for_change_since(
        &self,
        request: &WaitForChangeSinceRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        validate_wait_values(
            request.timeout_ms,
            request.stable_ms,
            request.difference_threshold,
        )?;
        let now = Instant::now();
        let baseline = {
            let mut baselines = self
                .visual_baselines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            baselines.retain(|baseline| {
                now.saturating_duration_since(baseline.created_at)
                    < Duration::from_millis(u64::from(VISUAL_BASELINE_TTL_MS))
            });
            baselines
                .iter()
                .find(|baseline| baseline.id == request.baseline_id)
                .cloned()
        }
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "baseline_id".to_owned(),
            reason: "baseline was not found or has expired; capture a new baseline".to_owned(),
        })?;
        wait_for_change(
            baseline.display,
            baseline.bounds,
            &baseline.frame,
            request.timeout_ms,
            request.stable_ms,
            request.difference_threshold,
        )
    }
}

impl OcrBackend for Backend {
    fn read_text_in_region(&self, request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        self.recognize_text(request)
    }

    fn find_text_on_screen(
        &self,
        request: &FindTextRequest,
    ) -> Result<FindTextResult, PlatformError> {
        if request.query.trim().is_empty() {
            return Err(PlatformError::InvalidArgument {
                argument: "query".to_owned(),
                reason: "must not be empty".to_owned(),
            });
        }
        if request.max_results == 0 || request.max_results > MAX_OCR_RESULTS {
            return Err(PlatformError::InvalidArgument {
                argument: "max_results".to_owned(),
                reason: format!("must be between 1 and {MAX_OCR_RESULTS}"),
            });
        }
        let ocr = self.recognize_text(&request.region)?;
        let needle = if request.case_sensitive {
            request.query.clone()
        } else {
            request.query.to_lowercase()
        };
        let matches = ocr
            .lines
            .iter()
            .filter(|line| {
                let haystack = if request.case_sensitive {
                    line.text.clone()
                } else {
                    line.text.to_lowercase()
                };
                haystack.contains(&needle)
            })
            .take(request.max_results as usize)
            .map(|line| TextMatch {
                text: line.text.clone(),
                x: line.x,
                y: line.y,
                width: line.width,
                height: line.height,
            })
            .collect();
        Ok(FindTextResult {
            query: request.query.clone(),
            display: ocr.display,
            source_bounds: ocr.source_bounds,
            language: ocr.language,
            matches,
        })
    }

    fn click_text(&self, request: &ClickTextRequest) -> Result<ClickTextResult, PlatformError> {
        self.click_text_controlled(request, &MutationControl::default())
    }

    fn click_text_controlled(
        &self,
        request: &ClickTextRequest,
        control: &MutationControl,
    ) -> Result<ClickTextResult, PlatformError> {
        let ocr_matches = self.find_text_on_screen(&FindTextRequest {
            region: request.region.clone(),
            query: request.query.clone(),
            case_sensitive: request.case_sensitive,
            max_results: MAX_OCR_RESULTS,
        })?;
        let candidate = unique_text_candidate(
            ocr_matches.matches,
            &request.query,
            request.case_sensitive,
            request.exact_match,
        )?;
        let x = candidate.x.saturating_add(candidate.width / 2);
        let y = candidate.y.saturating_add(candidate.height / 2);
        control.check("click_text")?;
        let result = self.click_mouse_controlled(
            &MouseClickRequest {
                display_id: request.region.display_id.clone(),
                x,
                y,
                button: request.button,
                click_count: request.click_count,
                modifiers: request.modifiers.clone(),
                duration_ms: request.duration_ms,
                observation: request.observation.clone(),
            },
            control,
        )?;
        Ok(ClickTextResult {
            matched_text: candidate,
            position: result.position,
            observation: result.observation,
        })
    }
}

impl PointerBackend for Backend {
    fn move_mouse(&self, request: &MouseMoveRequest) -> Result<PointerActionResult, PlatformError> {
        self.move_mouse_controlled(request, &MutationControl::default())
    }

    fn move_mouse_controlled(
        &self,
        request: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        self.perform_pointer_action(
            "move_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
            },
            control,
            |_, _| Ok(()),
        )
    }

    fn click_mouse(
        &self,
        request: &MouseClickRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        self.click_mouse_controlled(request, &MutationControl::default())
    }

    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        validate_click_request(request)?;
        self.perform_pointer_action(
            "click_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
            },
            control,
            |control, target| {
                control.check("click_mouse")?;
                send_click(
                    request.button,
                    request.click_count,
                    &request.modifiers,
                    || Self::ensure_point_input_target("click_mouse", target),
                )?;
                thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
                Ok(())
            },
        )
    }

    fn drag_mouse(&self, request: &MouseDragRequest) -> Result<PointerActionResult, PlatformError> {
        self.drag_mouse_controlled(request, &MutationControl::default())
    }

    fn drag_mouse_controlled(
        &self,
        request: &MouseDragRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_duration(request.duration_ms)?;
        validate_observation(&request.observation)?;
        validate_modifiers(&request.modifiers)?;
        let start_display = find_display(&request.start_display_id)?;
        let end_display = find_display(&request.end_display_id)?;
        let (start_x, start_y) = start_display
            .bounds
            .to_virtual(request.start_x, request.start_y)?;
        let (end_x, end_y) = end_display
            .bounds
            .to_virtual(request.end_x, request.end_y)?;
        let _guard = self.lock_input();
        let foreground_before = foreground_window_handle();
        ensure_interactive_input_desktop("drag_mouse")?;
        control.check("drag_mouse")?;
        move_cursor(
            "drag_mouse",
            current_cursor_position()?,
            POINT {
                x: start_x,
                y: start_y,
            },
            0,
            control,
            |point| Self::ensure_point_input_target("drag_mouse", point),
        )?;
        control.check("drag_mouse")?;
        send_drag_press(request.button, &request.modifiers, || {
            Self::ensure_point_input_target("drag_mouse", current_cursor_position()?)
        })?;
        let movement = move_cursor(
            "drag_mouse",
            POINT {
                x: start_x,
                y: start_y,
            },
            POINT { x: end_x, y: end_y },
            request.duration_ms,
            control,
            |point| Self::ensure_point_input_target("drag_mouse", point),
        );
        let release = send_drag_release(request.button, &request.modifiers, || {
            Self::ensure_point_input_target("drag_mouse", current_cursor_position()?)
        });
        movement?;
        release?;
        thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
        Self::pointer_action_result("drag_mouse", foreground_before, &request.observation)
    }

    fn scroll_mouse(
        &self,
        request: &MouseScrollRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        self.scroll_mouse_controlled(request, &MutationControl::default())
    }

    fn scroll_mouse_controlled(
        &self,
        request: &MouseScrollRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        if request.delta_x == 0 && request.delta_y == 0 {
            return Err(PlatformError::InvalidArgument {
                argument: "delta_x/delta_y".to_owned(),
                reason: "at least one scroll delta must be nonzero".to_owned(),
            });
        }
        if request.delta_x.unsigned_abs() > MAX_SCROLL_DELTA
            || request.delta_y.unsigned_abs() > MAX_SCROLL_DELTA
        {
            return Err(PlatformError::InvalidArgument {
                argument: "delta_x/delta_y".to_owned(),
                reason: format!(
                    "each scroll delta must be between -{MAX_SCROLL_DELTA} and {MAX_SCROLL_DELTA}"
                ),
            });
        }

        self.perform_pointer_action(
            "scroll_mouse",
            PointerActionSpec {
                display_id: &request.display_id,
                x: request.x,
                y: request.y,
                duration_ms: request.duration_ms,
                observation: &request.observation,
            },
            control,
            |control, target| {
                control.check("scroll_mouse")?;
                send_scroll(request.delta_x, request.delta_y, || {
                    Self::ensure_point_input_target("scroll_mouse", target)
                })?;
                thread::sleep(Duration::from_millis(POST_INPUT_SETTLE_MS));
                Ok(())
            },
        )
    }
}

impl WindowBackend for Backend {
    fn list_windows(&self) -> Result<Vec<WindowInfo>, PlatformError> {
        ensure_dpi_awareness()?;
        enumerate_windows()
    }

    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        ensure_dpi_awareness()?;
        let _apartment = ComApartment::initialize()?;
        let manager = virtual_desktop_manager()?;
        let current_id = current_virtual_desktop_id(&manager);
        let mut current_ids = std::collections::BTreeSet::<String>::new();
        let mut grouped = std::collections::BTreeMap::<String, Vec<WindowInfo>>::new();
        for window in enumerate_windows()? {
            let Ok((hwnd, _)) = parse_window_id(&window.id) else {
                continue;
            };
            let Ok(desktop_id) = window_desktop_id(&manager, hwnd) else {
                continue;
            };
            let id = format!("{desktop_id:?}");
            if window_is_on_current_desktop(&manager, hwnd) {
                current_ids.insert(id.clone());
            }
            grouped.entry(id).or_default().push(window);
        }
        if current_ids.is_empty()
            && let Some(current_id) = &current_id
        {
            grouped.entry(current_id.clone()).or_default();
        }
        let desktops = grouped
            .into_iter()
            .map(|(id, windows)| VirtualDesktopInfo {
                is_current: if current_ids.is_empty() {
                    current_id.as_ref().is_some_and(|current| current == &id)
                } else {
                    current_ids.contains(&id)
                },
                id,
                windows,
            })
            .collect();
        Ok(VirtualDesktopList {
            desktops,
            includes_empty_desktops: false,
            order_available: false,
            names_available: false,
        })
    }

    fn switch_virtual_desktop(
        &self,
        request: &VirtualDesktopSwitchRequest,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        self.switch_virtual_desktop_controlled(request, &MutationControl::default())
    }

    fn switch_virtual_desktop_controlled(
        &self,
        request: &VirtualDesktopSwitchRequest,
        control: &MutationControl,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        if request.steps == 0 || request.steps > MAX_DESKTOP_SWITCH_STEPS {
            return Err(PlatformError::InvalidArgument {
                argument: "steps".to_owned(),
                reason: format!("must be between 1 and {MAX_DESKTOP_SWITCH_STEPS}"),
            });
        }
        let _apartment = ComApartment::initialize()?;
        let manager = virtual_desktop_manager()?;
        let before = current_virtual_desktop_id(&manager);
        let arrow = match request.direction {
            VirtualDesktopDirection::Left => Key::ArrowLeft,
            VirtualDesktopDirection::Right => Key::ArrowRight,
        };
        let _guard = self.lock_input();
        ensure_interactive_input_desktop("switch_virtual_desktop")?;
        for _ in 0..request.steps {
            control.check("switch_virtual_desktop")?;
            send_key_chord(
                "switch_virtual_desktop",
                &[Key::Ctrl, Key::Win, arrow],
                control,
                || Self::ensure_foreground_input_target("switch_virtual_desktop"),
            )?;
            thread::sleep(Duration::from_millis(DESKTOP_SWITCH_SETTLE_MS));
        }
        let after = current_virtual_desktop_id(&manager);
        let observation = observe_foreground(&request.observation)
            .map_err(|error| post_action_error("switch_virtual_desktop", error.to_string()))?;
        Ok(VirtualDesktopSwitchResult {
            direction: request.direction,
            requested_steps: request.steps,
            changed: before != after,
            observation,
        })
    }

    fn focus_window(
        &self,
        request: &FocusWindowRequest,
    ) -> Result<WindowFocusResult, PlatformError> {
        self.focus_window_controlled(request, &MutationControl::default())
    }

    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        let _guard = self.lock_input();
        ensure_interactive_input_desktop("focus_window")?;
        control.check("focus_window")?;
        let window = find_window(&request.window_id)?;
        let (hwnd, _) = parse_window_id(&request.window_id)?;
        Self::ensure_window_input_target("focus_window", hwnd)?;

        if window.is_minimized {
            restore_window(hwnd);
        }
        bring_window_to_top(hwnd)?;
        if !activate_window(hwnd) {
            return Err(PlatformError::OperationFailed {
                operation: "focus_window".to_owned(),
                reason: "Windows denied foreground activation; refresh list_windows and retry only if user activity did not change the target".to_owned(),
            });
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while foreground_window_handle() != hwnd && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if foreground_window_handle() != hwnd {
            let actual = foreground_window_info();
            return Err(PlatformError::OperationFailed {
                operation: "focus_window".to_owned(),
                reason: format!(
                    "the requested window did not remain foreground after activation; actual foreground: {actual:?}"
                ),
            });
        }

        let focused = window_info(hwnd)?;
        let observation = observe_display(&focused.display_id, &request.observation)?;
        Ok(WindowFocusResult {
            window: focused,
            observation,
        })
    }

    fn capture_window(
        &self,
        request: &CaptureWindowRequest,
    ) -> Result<WindowScreenshot, PlatformError> {
        ensure_dpi_awareness()?;
        validate_max_width(request.max_width)?;
        let window = find_window(&request.window_id)?;
        if window.is_minimized {
            return Err(PlatformError::InvalidArgument {
                argument: "window_id".to_owned(),
                reason:
                    "cannot capture a minimized window; focus it first or wait until it is restored"
                        .to_owned(),
            });
        }
        let bounds = display_bounds_from_window(window.bounds);
        let (png, image_width, image_height, downscale_factor, cursor_marker) =
            capture_bounds(bounds, request.max_width, request.include_cursor)?;
        Ok(WindowScreenshot {
            source_bounds: window.bounds,
            window,
            png,
            image_width,
            image_height,
            downscale_factor,
            cursor_marker,
        })
    }

    fn wait_for_window(
        &self,
        request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_window_wait(request)?;
        let started = Instant::now();
        let deadline = started + Duration::from_millis(u64::from(request.timeout_ms));
        loop {
            if let Some(window) = enumerate_windows()?
                .into_iter()
                .find(|window| window_matches(window, request))
            {
                return Ok(WindowWaitResult {
                    matched: true,
                    timed_out: false,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    window: Some(window),
                });
            }
            if Instant::now() >= deadline {
                return Ok(WindowWaitResult {
                    matched: false,
                    timed_out: true,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    window: None,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(Duration::from_millis(VISUAL_POLL_MS)));
        }
    }
}

impl KeyboardBackend for Backend {
    fn press_keys(&self, request: &KeyChordRequest) -> Result<KeyboardActionResult, PlatformError> {
        self.press_keys_controlled(request, &MutationControl::default())
    }

    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_key_chord(request)?;
        validate_observation(&request.observation)?;
        let _guard = self.lock_input();
        ensure_interactive_input_desktop("press_keys")?;
        send_key_chord("press_keys", &request.keys, control, || {
            Self::ensure_foreground_input_target("press_keys")
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        Ok(KeyboardActionResult {
            observation: observe_foreground(&request.observation)?,
        })
    }

    fn type_text(&self, request: &TextInputRequest) -> Result<KeyboardActionResult, PlatformError> {
        self.type_text_controlled(request, &MutationControl::default())
    }

    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        let utf16: Vec<u16> = request.text.encode_utf16().collect();
        if utf16.is_empty() {
            return Err(PlatformError::InvalidArgument {
                argument: "text".to_owned(),
                reason: "must not be empty".to_owned(),
            });
        }
        if utf16.len() > MAX_TEXT_UTF16_UNITS {
            return Err(PlatformError::InvalidArgument {
                argument: "text".to_owned(),
                reason: format!("must contain at most {MAX_TEXT_UTF16_UNITS} UTF-16 code units"),
            });
        }

        let _guard = self.lock_input();
        ensure_interactive_input_desktop("type_text")?;
        send_unicode_text(&utf16, control, || {
            Self::ensure_foreground_input_target("type_text")
        })?;
        thread::sleep(Duration::from_millis(POST_FOCUS_SETTLE_MS));
        Ok(KeyboardActionResult {
            observation: observe_foreground(&request.observation)?,
        })
    }
}

fn unique_text_candidate(
    matches: Vec<TextMatch>,
    query: &str,
    case_sensitive: bool,
    exact_match: bool,
) -> Result<TextMatch, PlatformError> {
    let query = query.trim();
    let normalized_query = (!case_sensitive).then(|| query.to_lowercase());
    let mut candidates = matches.into_iter().filter(|candidate| {
        if !exact_match {
            return true;
        }
        if case_sensitive {
            candidate.text.trim() == query
        } else {
            candidate.text.trim().to_lowercase() == *normalized_query.as_ref().unwrap()
        }
    });
    let first = candidates
        .next()
        .ok_or_else(|| PlatformError::OperationFailed {
            operation: "click_text".to_owned(),
            reason: format!(
                "no unique OCR match for {query:?}; refresh the region or relax exact_match"
            ),
        })?;
    if candidates.next().is_some() {
        return Err(PlatformError::InvalidArgument {
            argument: "query".to_owned(),
            reason: format!(
                "matched more than one OCR line for {query:?}; narrow the region or use a more specific query"
            ),
        });
    }
    Ok(first)
}

impl Backend {
    fn lock_input(&self) -> std::sync::MutexGuard<'_, ()> {
        self.input_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ensure_window_input_target(operation: &str, hwnd: HWND) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop(operation)?;
        privilege::ensure_window_integrity(operation, hwnd)
    }

    fn ensure_foreground_input_target(operation: &str) -> Result<(), PlatformError> {
        Self::ensure_window_input_target(operation, foreground_window_handle())
    }

    fn ensure_point_input_target(operation: &str, point: POINT) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop(operation)?;
        privilege::ensure_point_integrity(operation, point)
    }

    fn perform_pointer_action<F>(
        &self,
        operation: &str,
        spec: PointerActionSpec<'_>,
        control: &MutationControl,
        action: F,
    ) -> Result<PointerActionResult, PlatformError>
    where
        F: FnOnce(&MutationControl, POINT) -> Result<(), PlatformError>,
    {
        ensure_dpi_awareness()?;
        validate_duration(spec.duration_ms)?;
        validate_observation(spec.observation)?;

        let display = find_display(spec.display_id)?;
        let (target_x, target_y) = display.bounds.to_virtual(spec.x, spec.y)?;
        let _guard = self.lock_input();
        ensure_interactive_input_desktop(operation)?;
        control.check(operation)?;
        let foreground_before = foreground_window_handle();

        let start = current_cursor_position()?;

        move_cursor(
            operation,
            start,
            POINT {
                x: target_x,
                y: target_y,
            },
            spec.duration_ms,
            control,
            |point| Self::ensure_point_input_target(operation, point),
        )?;
        let actual_target = current_cursor_position()?;
        action(control, actual_target)?;

        Self::pointer_action_result(operation, foreground_before, spec.observation)
    }

    fn pointer_action_result(
        operation: &str,
        foreground_before: HWND,
        options: &ObservationOptions,
    ) -> Result<PointerActionResult, PlatformError> {
        let actual = current_cursor_position()
            .map_err(|error| post_action_error(operation, error.to_string()))?;
        let actual_display = find_display_at_point(actual).ok_or_else(|| {
            post_action_error(
                operation,
                format!(
                    "Windows reported cursor position ({}, {}) outside every active display",
                    actual.x, actual.y
                ),
            )
        })?;
        let local_x = u32::try_from(actual.x - actual_display.bounds.left).unwrap_or(0);
        let local_y = u32::try_from(actual.y - actual_display.bounds.top).unwrap_or(0);
        let position = MousePosition {
            display_id: actual_display.id.clone(),
            local_x,
            local_y,
            virtual_x: actual.x,
            virtual_y: actual.y,
        };
        let foreground_after = foreground_window_handle();
        let observation_display_id =
            if foreground_after != foreground_before && !foreground_after.is_invalid() {
                window_info(foreground_after)
                    .map_or_else(|_| actual_display.id.clone(), |window| window.display_id)
            } else {
                actual_display.id.clone()
            };
        let observation = observe_display(&observation_display_id, options)
            .map_err(|error| post_action_error(operation, error.to_string()))?;

        Ok(PointerActionResult {
            position,
            observation,
        })
    }
}

fn validate_duration(duration_ms: u32) -> Result<(), PlatformError> {
    if duration_ms > MAX_MOVE_DURATION_MS {
        return Err(PlatformError::InvalidArgument {
            argument: "duration_ms".to_owned(),
            reason: format!("must be at most {MAX_MOVE_DURATION_MS}"),
        });
    }
    Ok(())
}

fn validate_observation(options: &ObservationOptions) -> Result<(), PlatformError> {
    validate_max_width(options.max_width)
}

fn last_win32_error(operation: &str) -> PlatformError {
    win32_error(operation, &windows::core::Error::from_thread())
}

fn win32_error(operation: &str, error: &windows::core::Error) -> PlatformError {
    PlatformError::OperationFailed {
        operation: operation.to_owned(),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code)]

    use controlfreak_core::{
        CaptureDisplayRequest, CaptureRegionRequest, CaptureWindowRequest, DisplayBackend,
        DisplayBounds, DisplayInfo, Key, MouseButton, MouseMoveRequest, MouseScrollRequest,
        ObservationOptions, OcrWord, PlatformError, PointerBackend, WaitForWindowRequest,
        WindowBackend, WindowBounds, WindowInfo,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    };

    use super::{
        Backend,
        capture::{encode_png, image_difference, resize_rgba_box, scaled_dimensions, word_union},
        current_cursor_position, display_region_bounds, frame_fingerprint,
        input::{click_inputs, interpolate, key_chord_inputs, scroll_inputs},
        unique_text_candidate, wait_for_ocr_helper, window_matches,
    };

    #[test]
    fn isolated_ocr_process_is_killed_after_its_deadline() {
        use std::{process::Stdio, time::Duration};

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "windows::tests::ocr_timeout_test_helper",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let error = wait_for_ocr_helper(child, Duration::from_millis(100)).unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(5));
        match error {
            PlatformError::OperationFailed { operation, reason } => {
                assert_eq!(operation, "windows_ocr_helper");
                assert!(reason.contains("timed out"));
            }
            other => panic!("unexpected OCR timeout error: {other:?}"),
        }
    }

    #[test]
    fn isolated_ocr_process_drains_output_before_exit() {
        use std::{process::Stdio, time::Duration};

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "windows::tests::ocr_large_output_test_helper",
                "--nocapture",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = wait_for_ocr_helper(child, Duration::from_secs(5)).unwrap();

        assert!(output.status.success());
        assert!(output.stdout.len() >= 512 * 1024);
    }

    #[test]
    #[ignore = "spawned only by isolated_ocr_process_is_killed_after_its_deadline"]
    fn ocr_timeout_test_helper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    #[ignore = "spawned only by isolated_ocr_process_drains_output_before_exit"]
    fn ocr_large_output_test_helper() {
        use std::io::Write as _;

        std::io::stdout()
            .write_all(&vec![b'x'; 512 * 1024])
            .unwrap();
    }

    fn mouse_flags(input: &INPUT) -> MOUSE_EVENT_FLAGS {
        // SAFETY: Callers pass INPUT values built by this module with INPUT_MOUSE as their type.
        unsafe { input.Anonymous.mi.dwFlags }
    }

    fn mouse_data(input: &INPUT) -> u32 {
        // SAFETY: Callers pass INPUT values built by this module with INPUT_MOUSE as their type.
        unsafe { input.Anonymous.mi.mouseData }
    }

    fn key_code(input: &INPUT) -> u16 {
        // SAFETY: Callers pass INPUT values built by this module with INPUT_KEYBOARD as their type.
        unsafe { input.Anonymous.ki.wVk.0 }
    }

    fn key_flags(input: &INPUT) -> KEYBD_EVENT_FLAGS {
        // SAFETY: Callers pass INPUT values built by this module with INPUT_KEYBOARD as their type.
        unsafe { input.Anonymous.ki.dwFlags }
    }

    #[test]
    fn interpolation_reaches_both_positive_and_negative_targets() {
        assert_eq!(interpolate(100, -100, 1, 2), 0);
        assert_eq!(interpolate(100, -100, 2, 2), -100);
    }

    #[test]
    fn png_encoder_writes_a_valid_signature() {
        let png = encode_png(1, 1, &[255, 0, 0, 255]).expect("encode one red pixel");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn click_inputs_always_pair_down_with_up() {
        let left = click_inputs(MouseButton::Left, 1, &[]);
        let right = click_inputs(MouseButton::Right, 1, &[]);
        let middle = click_inputs(MouseButton::Middle, 1, &[]);

        assert_eq!(mouse_flags(&left[0]), MOUSEEVENTF_LEFTDOWN);
        assert_eq!(mouse_flags(&left[1]), MOUSEEVENTF_LEFTUP);
        assert_eq!(mouse_flags(&right[0]), MOUSEEVENTF_RIGHTDOWN);
        assert_eq!(mouse_flags(&right[1]), MOUSEEVENTF_RIGHTUP);
        assert_eq!(mouse_flags(&middle[0]), MOUSEEVENTF_MIDDLEDOWN);
        assert_eq!(mouse_flags(&middle[1]), MOUSEEVENTF_MIDDLEUP);
    }

    #[test]
    fn modified_double_click_is_one_balanced_sequence() {
        let inputs = click_inputs(MouseButton::Left, 2, &[Key::Ctrl, Key::Shift]);
        assert_eq!(inputs.len(), 8);
        let types: Vec<u32> = inputs.iter().map(|input| input.r#type.0).collect();
        assert_eq!(types, [1, 1, 0, 0, 0, 0, 1, 1]);
        assert_eq!(key_code(&inputs[0]), 0x11);
        assert_eq!(key_code(&inputs[1]), 0x10);
        assert_eq!(key_code(&inputs[6]), 0x10);
        assert_eq!(key_code(&inputs[7]), 0x11);
        assert_eq!(mouse_flags(&inputs[2]), MOUSEEVENTF_LEFTDOWN);
        assert_eq!(mouse_flags(&inputs[5]), MOUSEEVENTF_LEFTUP);
    }

    #[test]
    fn region_validation_handles_offsets_and_rejects_overflow() {
        let display = DisplayInfo {
            id: "display".to_owned(),
            name: "display".to_owned(),
            bounds: DisplayBounds {
                left: -1920,
                top: 10,
                width: 1920,
                height: 1080,
            },
            is_primary: false,
        };
        assert_eq!(
            display_region_bounds(&display, 20, 30, 100, 200).expect("valid region"),
            DisplayBounds {
                left: -1900,
                top: 40,
                width: 100,
                height: 200
            }
        );
        assert!(display_region_bounds(&display, 1900, 0, 100, 100).is_err());
        assert!(display_region_bounds(&display, u32::MAX, 0, 2, 1).is_err());
    }

    #[test]
    fn image_difference_is_normalized() {
        let unchanged = image_difference(&[0, 0, 0, 255], &[0, 0, 0, 255]).unwrap();
        let opposite = image_difference(&[0, 0, 0, 255], &[255, 255, 255, 255]).unwrap();
        assert!(unchanged.abs() < f64::EPSILON);
        assert!((opposite - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn visual_fingerprints_are_stable_and_content_sensitive() {
        assert_eq!(frame_fingerprint(&[1, 2, 3]), frame_fingerprint(&[1, 2, 3]));
        assert_ne!(frame_fingerprint(&[1, 2, 3]), frame_fingerprint(&[1, 2, 4]));
    }

    #[test]
    fn ocr_word_union_preserves_display_local_coordinates() {
        let words = [
            OcrWord {
                text: "hello".to_owned(),
                x: 100,
                y: 50,
                width: 40,
                height: 20,
            },
            OcrWord {
                text: "world".to_owned(),
                x: 150,
                y: 48,
                width: 50,
                height: 24,
            },
        ];

        assert_eq!(word_union(&words), Some((100, 48, 100, 24)));
    }

    #[test]
    fn semantic_click_selection_is_fail_closed_for_zero_or_ambiguous_matches() {
        let match_at = |text: &str, x| controlfreak_core::TextMatch {
            text: text.to_owned(),
            x,
            y: 20,
            width: 100,
            height: 30,
        };
        assert!(unique_text_candidate(Vec::new(), "Save", false, true).is_err());
        assert!(matches!(
            unique_text_candidate(
                vec![match_at("Save", 10), match_at("SAVE", 200)],
                "save",
                false,
                true
            ),
            Err(PlatformError::InvalidArgument { .. })
        ));
        assert_eq!(
            unique_text_candidate(vec![match_at("Save", 10)], "save", false, true)
                .unwrap()
                .x,
            10
        );
        assert!(
            unique_text_candidate(vec![match_at("Save changes", 10)], "Save", false, true).is_err()
        );
    }

    #[test]
    fn window_matching_combines_every_supplied_criterion() {
        let window = WindowInfo {
            id: "0X10:20".to_owned(),
            title: "Save report - Editor".to_owned(),
            class_name: "EditorWindow".to_owned(),
            process_id: 32,
            bounds: WindowBounds {
                left: 0,
                top: 0,
                width: 800,
                height: 600,
            },
            display_id: "display".to_owned(),
            is_foreground: true,
            is_minimized: false,
        };
        let request = WaitForWindowRequest {
            window_id: None,
            title_contains: Some("REPORT".to_owned()),
            class_name: Some("editorwindow".to_owned()),
            process_id: Some(32),
            is_foreground: Some(true),
            timeout_ms: 0,
        };
        assert!(window_matches(&window, &request));
    }

    #[test]
    fn scroll_inputs_preserve_signed_wheel_deltas() {
        let inputs = scroll_inputs(-120, 240);

        assert_eq!(inputs.len(), 2);
        assert_eq!(mouse_flags(&inputs[0]), MOUSEEVENTF_WHEEL);
        assert_eq!(
            mouse_data(&inputs[0]),
            u32::from_ne_bytes(240_i32.to_ne_bytes())
        );
        assert_eq!(mouse_flags(&inputs[1]), MOUSEEVENTF_HWHEEL);
        assert_eq!(
            mouse_data(&inputs[1]),
            u32::from_ne_bytes((-120_i32).to_ne_bytes())
        );
    }

    #[test]
    fn key_chords_release_every_key_in_reverse_order() {
        let inputs = key_chord_inputs(&[Key::Ctrl, Key::Shift, Key::T]);

        assert_eq!(inputs.len(), 6);
        let virtual_keys: Vec<u16> = inputs.iter().map(key_code).collect();
        assert_eq!(virtual_keys, [0x11, 0x10, 0x54, 0x54, 0x10, 0x11]);
        assert!(
            inputs[..3]
                .iter()
                .all(|input| key_flags(input) & KEYEVENTF_KEYUP == KEYBD_EVENT_FLAGS::default())
        );
        assert!(
            inputs[3..]
                .iter()
                .all(|input| key_flags(input) & KEYEVENTF_KEYUP == KEYEVENTF_KEYUP)
        );
    }

    #[test]
    fn box_downscale_averages_source_pixels() {
        let source = [
            0, 0, 0, 255, 100, 0, 0, 255, 0, 100, 0, 255, 100, 100, 0, 255,
        ];

        assert_eq!(resize_rgba_box(&source, 2, 2, 2), [50, 50, 0, 255]);
    }

    #[test]
    fn proportional_dimensions_use_the_requested_width_without_integer_steps() {
        assert_eq!(scaled_dimensions(1920, 1080, Some(1600)), (1600, 900, 2));
        assert_eq!(scaled_dimensions(1920, 1080, Some(1400)), (1400, 788, 2));
        assert_eq!(scaled_dimensions(1920, 1080, None), (1920, 1080, 1));
    }

    #[test]
    fn scroll_rejects_empty_or_excessive_deltas_before_injection() {
        let backend = Backend::new();
        let display = backend
            .list_displays()
            .expect("enumerate Windows displays")
            .remove(0);
        let request = |delta_x, delta_y| MouseScrollRequest {
            display_id: display.id.clone(),
            x: 0,
            y: 0,
            delta_x,
            delta_y,
            duration_ms: 0,
            observation: ObservationOptions::default(),
        };

        assert!(matches!(
            backend.scroll_mouse(&request(0, 0)),
            Err(PlatformError::InvalidArgument { .. })
        ));
        assert!(matches!(
            backend.scroll_mouse(&request(12_001, 0)),
            Err(PlatformError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn live_backend_enumerates_and_captures_primary_display() {
        let backend = Backend::new();
        let displays = backend.list_displays().expect("enumerate Windows displays");
        let primary = displays
            .iter()
            .find(|display| display.is_primary)
            .unwrap_or(&displays[0]);
        let screenshot = backend
            .capture_display(&CaptureDisplayRequest {
                display_id: primary.id.clone(),
                max_width: None,
                include_cursor: true,
            })
            .expect("capture primary display");

        assert_eq!(screenshot.display, *primary);
        assert_eq!(screenshot.source_bounds, primary.bounds);
        assert_eq!(screenshot.downscale_factor, 1);
        assert_eq!(&screenshot.png[..8], b"\x89PNG\r\n\x1a\n");

        let region = backend
            .capture_region(&CaptureRegionRequest {
                display_id: primary.id.clone(),
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                max_width: None,
                include_cursor: false,
            })
            .expect("capture one-pixel region");
        assert_eq!(region.image_width, 1);
        assert_eq!(region.image_height, 1);
    }

    #[test]
    fn live_backend_enumerates_visible_windows() {
        let backend = Backend::new();
        let windows = backend
            .list_windows()
            .expect("enumerate visible Windows windows");

        assert!(windows.iter().all(|window| !window.title.is_empty()));
        assert!(
            windows
                .iter()
                .all(|window| window.id.starts_with("0X") && window.id.contains(':'))
        );
        let desktops = backend
            .list_virtual_desktops()
            .expect("group windows by virtual desktop");
        assert!(!desktops.includes_empty_desktops);
        assert!(!desktops.order_available);
        assert!(!desktops.names_available);
        assert!(
            desktops
                .desktops
                .iter()
                .all(|desktop| !desktop.id.is_empty())
        );
        if let Some(window) = windows.iter().find(|window| !window.is_minimized) {
            let screenshot = backend
                .capture_window(&CaptureWindowRequest {
                    window_id: window.id.clone(),
                    max_width: Some(320),
                    include_cursor: false,
                })
                .expect("capture visible non-minimized window");
            assert_eq!(screenshot.window.id, window.id);
            assert_eq!(&screenshot.png[..8], b"\x89PNG\r\n\x1a\n");

            let wait = backend
                .wait_for_window(&WaitForWindowRequest {
                    window_id: Some(window.id.clone()),
                    title_contains: None,
                    class_name: None,
                    process_id: None,
                    is_foreground: None,
                    timeout_ms: 0,
                })
                .expect("match an existing window immediately");
            assert!(wait.matched);
            assert!(!wait.timed_out);
        }
    }

    #[test]
    fn live_backend_can_set_the_existing_cursor_position() {
        let backend = Backend::new();
        let displays = backend.list_displays().expect("enumerate Windows displays");
        let point = current_cursor_position().expect("read current cursor position");
        let display = displays
            .iter()
            .find(|display| {
                let right = i64::from(display.bounds.left) + i64::from(display.bounds.width);
                let bottom = i64::from(display.bounds.top) + i64::from(display.bounds.height);
                i64::from(point.x) >= i64::from(display.bounds.left)
                    && i64::from(point.x) < right
                    && i64::from(point.y) >= i64::from(display.bounds.top)
                    && i64::from(point.y) < bottom
            })
            .expect("cursor is located on an active display");
        let local_x = u32::try_from(point.x - display.bounds.left).expect("local cursor x");
        let local_y = u32::try_from(point.y - display.bounds.top).expect("local cursor y");
        let result = backend
            .move_mouse(&MouseMoveRequest {
                display_id: display.id.clone(),
                x: local_x,
                y: local_y,
                duration_ms: 0,
                observation: ObservationOptions::default(),
            })
            .expect("set current cursor position");

        assert_eq!(
            (result.position.virtual_x, result.position.virtual_y),
            (point.x, point.y)
        );
        assert_eq!(
            &result
                .observation
                .screenshot
                .as_ref()
                .expect("screenshot")
                .png[..8],
            b"\x89PNG\r\n\x1a\n"
        );
    }
}
