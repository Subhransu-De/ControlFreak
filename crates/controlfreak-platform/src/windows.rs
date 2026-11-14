#![deny(unsafe_code)]

use std::{
    ffi::c_void,
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
    PointerActionResult, PointerBackend, SecurityContext, TextInputRequest, VirtualDesktopInfo,
    VirtualDesktopList, VirtualDesktopSwitchRequest, VirtualDesktopSwitchResult, VisualBaseline,
    VisualBaselineRequest, VisualChangeResult, WaitForChangeSinceRequest, WaitForTextRequest,
    WaitForTextResult, WaitForVisualChangeRequest, WaitForWindowRequest, WindowBackend,
    WindowBounds, WindowFocusResult, WindowInfo, WindowList, WindowScreenshot, WindowWaitResult,
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT},
    Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
        CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits,
        GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
        MonitorFromWindow, ReleaseDC, SRCCOPY, SelectObject,
    },
    System::Com::{CLSCTX_ALL, CoCreateInstance},
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
            MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
            MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
            MOUSEINPUT, SendInput, VIRTUAL_KEY,
        },
        Shell::{IVirtualDesktopManager, VirtualDesktopManager},
        WindowsAndMessaging::{
            EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetCursorPos, GetForegroundWindow,
            GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
            GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MONITORINFOF_PRIMARY,
            SW_RESTORE, SetCursorPos, SetForegroundWindow, ShowWindowAsync, WS_EX_TOOLWINDOW,
        },
    },
};
use windows::core::BOOL;
mod actions;
mod activation;
mod ocr;
mod target;
pub(crate) use ocr::serve_ocr_helper;
use ocr::{OcrHelperPool, recognize_text_with_helper_fingerprinted};
mod arbitration;
mod capture;
mod desktop;
mod environment;
mod glow;
mod indicator;
mod input;
mod privilege;
mod process;
mod shell;
mod stop;
mod win32;
pub use stop::{SessionSignals, UserStop};

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
    baseline_process_nonce, capture_comparison_frame, capture_display_bounds,
    capture_display_image, capture_window_bounds, display_bounds_from_window,
    display_region_bounds, frame_fingerprint, observe_display, observe_foreground,
    recognize_text_fingerprinted, validate_max_width, validate_visual_wait, validate_wait_values,
    wait_for_change,
};
use desktop::{
    activate_window, current_cursor_position, current_virtual_desktop_id, ensure_dpi_awareness,
    enumerate_displays, enumerate_window_candidates, enumerate_windows, find_display,
    find_display_at_point, find_window, foreground_window_handle, initialize_com, issue_candidate,
    resolve_window_reference, restore_window, validate_window_wait, virtual_desktop_manager,
    window_desktop_id, window_info, window_is_on_current_desktop, window_matches,
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

use controlfreak_core::limits::{
    MAX_CAPTURE_WIDTH, MAX_MOVE_DURATION_MS, MAX_SCROLL_DELTA, MAX_TEXT_UTF16_UNITS, MAX_WAIT_MS,
};

const MAX_CAPTURE_BYTES: usize = 512 * 1024 * 1024;
const MOVE_FRAME_MS: u64 = 16;
const POST_INPUT_SETTLE_MS: u64 = 75;
const POST_FOCUS_SETTLE_MS: u64 = 150;
const MAX_ACTION_IMAGE_WIDTH: u32 = 1_280;
const MAX_CHORD_KEYS: usize = 8;
const MAX_WINDOWS: usize = 200;
const MAX_WINDOW_TITLE_UTF16_UNITS: usize = 512;
const MAX_CLICK_COUNT: u8 = 3;
const VISUAL_POLL_MS: u64 = 100;
const VISUAL_COMPARE_WIDTH: u32 = 320;
const MAX_DESKTOP_SWITCH_STEPS: u8 = 10;
const DESKTOP_SWITCH_SETTLE_MS: u64 = 300;
const VISUAL_BASELINE_TTL_MS: u32 = 300_000;
const MAX_VISUAL_BASELINES: usize = 16;
#[cfg(test)]
const MAX_OCR_RESULTS: u32 = 100;

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
    /// Refuse dispatch if any other input action completed since this generation was read.
    input_generation: Option<u64>,
}

pub struct Backend {
    input_lock: Mutex<()>,
    /// Counts completed input-lock holders, so slow observations can detect intervening input.
    input_generation: AtomicU64,
    visual_baselines: Mutex<Vec<StoredVisualBaseline>>,
    next_baseline_id: AtomicU64,
    ocr_helper: Option<OcrHelperCommand>,
    ocr_helpers: OcrHelperPool,
    security_context: SecurityContext,
}

struct NativeActivation<'a> {
    /// The session's approved target. The window being activated is `window_id`, which is the
    /// target itself or a window of the same process (a dialog of the approved application).
    approved_id: &'a str,
    window_id: &'a str,
    hwnd: HWND,
    /// The window in front when activation began, kept as a plain handle: that window may be one
    /// that cannot be given a target reference, and the refusal below needs only its identity.
    initial_foreground: HWND,
    minimized: bool,
    control: &'a MutationControl,
    started: Instant,
    attempts: u32,
}

impl activation::Activation for NativeActivation<'_> {
    fn validate(&mut self) -> Result<bool, PlatformError> {
        self.control
            .activation_progress(self.attempts, self.elapsed_ms());
        self.control.check_target()?;
        self.control.check("focus_window")?;
        find_window(self.window_id).map_err(|_| {
            if self.window_id == self.approved_id {
                PlatformError::TargetInvalidated {
                    reason: "the approved window no longer exists".into(),
                }
            } else {
                PlatformError::InvalidArgument {
                    argument: "window_id".into(),
                    reason: "the window no longer exists".into(),
                }
            }
        })?;
        Backend::ensure_window_input_target("focus_window", self.hwnd)?;
        let foreground = foreground_window_handle();
        if foreground.is_invalid() {
            // Windows briefly has no foreground window while it hands activation over.
            return Ok(false);
        }
        Backend::ensure_window_input_target("focus_window", foreground)?;
        if foreground == self.hwnd {
            return Ok(true);
        }
        // Another window of the approved application (it opened a dialog, or Windows handed it
        // activation) is not a change of owner; only another process taking the foreground is.
        if foreground != self.initial_foreground
            && !target::window_serves_target(self.approved_id, foreground)
        {
            return Err(PlatformError::refused(
                controlfreak_core::RefusalCause::ForegroundChanged,
                "another process took the foreground during activation; try again, or ask the user to click the target window",
            ));
        }
        Ok(false)
    }

    fn activate(&mut self) -> Result<bool, PlatformError> {
        if self.validate()? {
            return Ok(true);
        }
        self.attempts += 1;
        self.control
            .activation_progress(self.attempts, self.elapsed_ms());
        if self.minimized {
            let previous = self.control.dispatch_started();
            if !restore_window(self.hwnd) {
                self.control.dispatch_rejected(previous);
                return Ok(false);
            }
            self.control.dispatch_accepted(0);
            self.minimized = false;
        }
        // Revalidate after restoration, which can itself change foreground ownership.
        if self.validate()? {
            return Ok(true);
        }
        let previous = self.control.dispatch_started();
        let accepted = activate_window(self.hwnd);
        if accepted {
            self.control.dispatch_accepted(0);
            target::arm_if_foreground(self.approved_id)?;
        } else {
            self.control.dispatch_rejected(previous);
        }
        Ok(accepted)
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn wait(&mut self, milliseconds: u64) {
        thread::sleep(Duration::from_millis(milliseconds));
    }
}

impl Backend {
    pub fn new() -> Self {
        let security_context = privilege::current_security_context(false).unwrap_or_default();
        Self {
            input_lock: Mutex::new(()),
            input_generation: AtomicU64::new(0),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: None,
            ocr_helpers: OcrHelperPool::default(),
            security_context,
        }
    }

    pub fn with_security(security_context: SecurityContext) -> Self {
        Self {
            input_lock: Mutex::new(()),
            input_generation: AtomicU64::new(0),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: None,
            ocr_helpers: OcrHelperPool::default(),
            security_context,
        }
    }

    pub fn with_ocr_helper_and_security(
        command: OcrHelperCommand,
        security_context: SecurityContext,
    ) -> Self {
        Self {
            input_lock: Mutex::new(()),
            input_generation: AtomicU64::new(0),
            visual_baselines: Mutex::new(Vec::new()),
            next_baseline_id: AtomicU64::new(1),
            ocr_helper: Some(command),
            ocr_helpers: OcrHelperPool::default(),
            security_context,
        }
    }

    fn recognize_text(
        &self,
        request: &OcrRegionRequest,
        control: &MutationControl,
    ) -> Result<OcrResult, PlatformError> {
        self.recognize_text_fingerprinted(request, control)
            .map(|(result, _)| result)
    }

    /// Recognizes text and returns the fingerprint of the exact frame that was recognized, if
    /// the recognizer reported one.
    fn recognize_text_fingerprinted(
        &self,
        request: &OcrRegionRequest,
        control: &MutationControl,
    ) -> Result<(OcrResult, Option<u64>), PlatformError> {
        let target_before = target::observe();
        let (mut result, fingerprint) = self.ocr_helper.as_ref().map_or_else(
            || {
                recognize_text_fingerprinted(request, control)
                    .map(|(result, fingerprint)| (result, Some(fingerprint)))
            },
            |command| {
                recognize_text_with_helper_fingerprinted(
                    command,
                    &self.ocr_helpers,
                    request,
                    control,
                )
            },
        )?;
        result.target_ref = target::finish_observation(target_before);
        Ok((result, fingerprint))
    }

    fn find_text_fingerprinted(
        &self,
        request: &FindTextRequest,
        control: &MutationControl,
    ) -> Result<(FindTextResult, Option<u64>), PlatformError> {
        control.check("find_text_on_screen")?;
        controlfreak_core::validate_text_discovery(
            &request.query,
            request.match_mode,
            request.ocr_confusions,
            request.max_results,
        )?;
        let (ocr, fingerprint) = self.recognize_text_fingerprinted(&request.region, control)?;
        let details = controlfreak_core::discover_text(
            &ocr.lines,
            &request.region,
            &request.query,
            request.case_sensitive,
            request.match_mode,
            request.ocr_confusions,
            request.max_results,
        )?;
        let matches = details.candidates.clone();
        Ok((
            FindTextResult {
                target_ref: ocr.target_ref,
                query: request.query.clone(),
                display: ocr.display,
                source_bounds: ocr.source_bounds,
                language: ocr.language,
                matches,
                details,
            },
            fingerprint,
        ))
    }
}

impl BackendMetadata for Backend {
    fn validate_target_reference(&self, target: &str) -> Result<(), PlatformError> {
        let _timer = controlfreak_core::timing::start("approve_target");
        let (hwnd, _) = target::resolve(target)?;
        Self::ensure_window_input_target("approve_target", hwnd)?;
        target::arm_if_foreground(target)
    }
    fn record_target_evidence(&self, control: &MutationControl) {
        let _timer = controlfreak_core::timing::start("target_evidence");
        target::evidence(control);
    }

    fn check_control_environment(&self) -> Result<(), PlatformError> {
        ensure_interactive_input_desktop("control_session")
    }

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
        self.wait_for_visual_change_controlled(
            request,
            &controlfreak_core::MutationControl::default(),
        )
    }

    fn wait_for_visual_change_controlled(
        &self,
        request: &WaitForVisualChangeRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        control.check("wait_for_visual_change")?;
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
            control,
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
        self.wait_for_change_since_controlled(
            request,
            &controlfreak_core::MutationControl::default(),
        )
    }

    fn wait_for_change_since_controlled(
        &self,
        request: &WaitForChangeSinceRequest,
        control: &MutationControl,
    ) -> Result<VisualChangeResult, PlatformError> {
        control.check("wait_for_change_since")?;
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
        ensure_dpi_awareness()?;
        ensure_baseline_display(&baseline.display, enumerate_displays()?.as_slice())?;
        let result = wait_for_change(
            baseline.display.clone(),
            baseline.bounds,
            &baseline.frame,
            request.timeout_ms,
            request.stable_ms,
            request.difference_threshold,
            control,
        )?;
        // A layout change during the wait would make the comparison meaningless.
        ensure_baseline_display(&baseline.display, enumerate_displays()?.as_slice())?;
        Ok(result)
    }
}

/// The result of the previous `wait_for_text` pass, tied to the fingerprint of the exact frame
/// that recognition read and to the foreground, window set and desktop it was taken under.
struct PassCache<T> {
    last: Option<(T, u64, u64)>,
}

impl<T> Default for PassCache<T> {
    fn default() -> Self {
        Self { last: None }
    }
}

impl<T: Clone> PassCache<T> {
    /// Runs one pass. `frame` is the fingerprint of a fresh read of the region, or `None` if it
    /// could not be read. The previous result stands only if that read matches the frame the
    /// previous result was recognized from and the generation is unchanged, so nothing on
    /// screen could have changed the text. Otherwise `recognize` runs, and its result is kept
    /// only if the recognizer reported the fingerprint of the frame it read. A comparison read
    /// is never the key for a result computed from another read.
    fn pass(
        &mut self,
        frame: Option<u64>,
        generation: u64,
        recognize: impl FnOnce() -> Result<(T, Option<u64>), PlatformError>,
    ) -> Result<T, PlatformError> {
        if let (Some((result, recognized, observed)), Some(frame)) = (&self.last, frame)
            && *recognized == frame
            && *observed == generation
        {
            return Ok(result.clone());
        }
        self.last = None;
        let (result, recognized) = recognize()?;
        self.last = recognized.map(|recognized| (result.clone(), recognized, generation));
        Ok(result)
    }
}

/// Where the requested region is on screen right now, or `None` if it cannot be resolved.
fn current_region_bounds(region: &OcrRegionRequest) -> Option<controlfreak_core::DisplayBounds> {
    ensure_dpi_awareness().ok()?;
    let display = find_display(&region.display_id).ok()?;
    display_region_bounds(&display, region.x, region.y, region.width, region.height).ok()
}

/// Keeps a pixel watch and its cache tied to where the region is now. A cached result is only
/// reused while the watch still covers the region's current rectangle; when the region moved or
/// cannot be resolved (the display layout changed mid-wait) the cache is dropped and the watch
/// rebuilt, so the next pass recognizes text again and reports any layout error truthfully.
fn reconcile_watch<W, T>(
    watch: &mut Option<(W, controlfreak_core::DisplayBounds)>,
    cache: &mut PassCache<T>,
    current: Option<controlfreak_core::DisplayBounds>,
    rebuild: impl FnOnce() -> Option<(W, controlfreak_core::DisplayBounds)>,
) {
    if watch.as_ref().map(|(_, bounds)| *bounds) != current {
        *cache = PassCache::default();
        *watch = if current.is_some() { rebuild() } else { None };
    }
}

/// A pixel watch over an OCR region, or `None` if the region cannot be watched, in which case
/// every pass recognizes text.
fn region_change_watch(
    region: &OcrRegionRequest,
) -> Option<(capture::RegionChange, controlfreak_core::DisplayBounds)> {
    let bounds = current_region_bounds(region)?;
    capture::RegionChange::new(bounds)
        .ok()
        .map(|watch| (watch, bounds))
}

/// Refuses a stored baseline whose display moved, resized, or disappeared since capture.
fn ensure_baseline_display(
    captured: &DisplayInfo,
    current: &[DisplayInfo],
) -> Result<(), PlatformError> {
    if current
        .iter()
        .any(|display| display.id == captured.id && display.bounds == captured.bounds)
    {
        Ok(())
    } else {
        Err(PlatformError::InvalidArgument {
            argument: "baseline_id".to_owned(),
            reason: "the display layout changed since this baseline was captured; capture a new baseline".to_owned(),
        })
    }
}

impl OcrBackend for Backend {
    fn read_text_in_region(&self, request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        self.read_text_in_region_controlled(request, &controlfreak_core::MutationControl::default())
    }

    fn read_text_in_region_controlled(
        &self,
        request: &OcrRegionRequest,
        control: &MutationControl,
    ) -> Result<OcrResult, PlatformError> {
        control.check("read_text_in_region")?;
        self.recognize_text(request, control)
    }

    fn find_text_on_screen(
        &self,
        request: &FindTextRequest,
    ) -> Result<FindTextResult, PlatformError> {
        self.find_text_on_screen_controlled(request, &controlfreak_core::MutationControl::default())
    }

    fn find_text_on_screen_controlled(
        &self,
        request: &FindTextRequest,
        control: &MutationControl,
    ) -> Result<FindTextResult, PlatformError> {
        self.find_text_fingerprinted(request, control)
            .map(|(result, _)| result)
    }

    /// Waits for text with a pixel check in front of each recognition pass. If every pixel of
    /// the region matches the previous pass and the foreground has not changed, nothing on screen
    /// could have changed the text, so the previous result stands and recognition (the expensive
    /// part) is skipped. Any doubt, such as a region that cannot be read, runs recognition.
    fn wait_for_text_controlled(
        &self,
        request: &WaitForTextRequest,
        control: &MutationControl,
    ) -> Result<WaitForTextResult, PlatformError> {
        let mut watch = region_change_watch(&request.find.region);
        let mut cache = PassCache::default();
        controlfreak_core::wait_for_text_with(request, control, || {
            reconcile_watch(
                &mut watch,
                &mut cache,
                current_region_bounds(&request.find.region),
                || region_change_watch(&request.find.region),
            );
            let generation = target::observation_generation();
            let frame = watch
                .as_mut()
                .and_then(|(watch, _)| watch.fingerprint().ok());
            cache.pass(frame, generation, || {
                let (result, fingerprint) = self.find_text_fingerprinted(&request.find, control)?;
                // Recognition resolves the display again on every pass. If it read another
                // rectangle than the watch does (the layout changed mid-wait), two frames of
                // the same colour could look equal, so the result is not kept.
                let same_rectangle = watch
                    .as_ref()
                    .is_some_and(|(_, bounds)| *bounds == result.source_bounds);
                Ok((result, fingerprint.filter(|_| same_rectangle)))
            })
        })
    }

    fn click_text_controlled(
        &self,
        request: &ClickTextRequest,
        control: &MutationControl,
    ) -> Result<ClickTextResult, PlatformError> {
        if request.query.trim().is_empty() {
            return Err(PlatformError::InvalidArgument {
                argument: "query".to_owned(),
                reason: "must not be empty".to_owned(),
            });
        }
        target::validate(control, "click_text", None)?;
        // OCR runs outside the input lock; input that lands meanwhile can move the matched text.
        let input_generation = self.input_generation.load(Ordering::Acquire);
        let ocr = self.recognize_text(&request.region, control)?;
        target::validate(control, "click_text", None)?;
        // The observation must have been taken with the approved window, or a window of its own
        // process such as a dialog, in front and unchanged. If not, the text may belong to
        // something else, so this click is refused; the session itself stays valid because every
        // action re-checks the live foreground.
        target::ensure_observation_matches(
            control.approved_target().as_deref(),
            ocr.target_ref.as_deref(),
        )?;
        // Search result limits must not hide eligible lines from the uniqueness check.
        let candidate = controlfreak_core::select_text_candidate(
            &ocr.lines,
            &request.region,
            &request.query,
            request.case_sensitive,
            request.exact_match,
        )?;
        let x = candidate.x.saturating_add(candidate.width / 2);
        let y = candidate.y.saturating_add(candidate.height / 2);
        control.check("click_text")?;
        let result = self.click_mouse_at_generation(
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
            Some(input_generation),
        )?;
        Ok(ClickTextResult {
            matched_text: candidate,
            position: result.position,
            observation: result.observation,
        })
    }
}

impl WindowBackend for Backend {
    fn list_windows(&self) -> Result<WindowList, PlatformError> {
        ensure_dpi_awareness()?;
        let (windows, truncated) = enumerate_windows()?;
        Ok(WindowList { windows, truncated })
    }

    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        ensure_dpi_awareness()?;
        let _apartment = initialize_com()?;
        let manager = virtual_desktop_manager()?;
        let current_id = current_virtual_desktop_id(&manager);
        let mut current_ids = std::collections::BTreeSet::<String>::new();
        let mut grouped = std::collections::BTreeMap::<String, Vec<WindowInfo>>::new();
        for window in enumerate_windows()?.0 {
            let Ok((hwnd, _)) = resolve_window_reference(&window.id) else {
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
        let _apartment = initialize_com()?;
        let manager = virtual_desktop_manager()?;
        let before = current_virtual_desktop_id(&manager);
        let _guard = self.lock_input(control)?;
        ensure_interactive_input_desktop("switch_virtual_desktop")?;
        input::send_desktop_switch_hops(
            request.direction,
            request.steps,
            control,
            |hop| {
                if hop == 0 {
                    target::validate(control, "switch_virtual_desktop", None)
                } else {
                    target::validate_after_switch(control)
                }
            },
            || {
                controlfreak_core::timing::time("settle", || {
                    thread::sleep(Duration::from_millis(DESKTOP_SWITCH_SETTLE_MS));
                });
            },
        )?;
        control.input_complete();
        target::evidence(control);
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

    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        ensure_dpi_awareness()?;
        validate_observation(&request.observation)?;
        let _guard = self.lock_input(control)?;
        ensure_interactive_input_desktop("focus_window")?;
        control.check("focus_window")?;
        let approved = control
            .approved_target()
            .ok_or_else(|| target::invalid("an approved target reference is required"))?;
        let window = find_window(&request.window_id)?;
        let (hwnd, _) = resolve_window_reference(&request.window_id)?;
        // The approved window, or a window of its own process (one of its dialogs), by the same
        // rule as the foreground and pointer checks; anything else needs a new session.
        if approved != request.window_id && !target::window_serves_target(&approved, hwnd) {
            return Err(target::invalid(
                "focus target differs from the approved session target; focus_window accepts the approved window or a dialog of the same application",
            ));
        }
        Self::ensure_window_input_target("focus_window", hwnd)?;

        let mut driver = NativeActivation {
            approved_id: &approved,
            attempts: 0,
            window_id: &request.window_id,
            hwnd,
            initial_foreground: foreground_window_handle(),
            minimized: window.is_minimized,
            control,
            started: Instant::now(),
        };
        let activation = controlfreak_core::timing::time("activate", || {
            activation::focus(&mut driver, request.timeout_ms)
        });
        // A refused or timed-out activation leaves the target as it was, and its error says
        // the request is retryable, so only a change of the target itself ends the session.
        if matches!(activation, Err(PlatformError::TargetInvalidated { .. })) {
            control.invalidate_target();
        }
        let (_, elapsed_ms) = activation?;
        let attempts = driver.attempts;
        control.activation_progress(attempts, elapsed_ms);
        target::arm_if_foreground(&approved)?;
        control.input_complete();
        target::evidence(control);

        let focused = window_info(hwnd)
            .map_err(|error| post_action_error("focus_window", error.to_string()))?;
        let observation = observe_display(&focused.display_id, &request.observation)
            .map_err(|error| post_action_error("focus_window", error.to_string()))?;
        Ok(WindowFocusResult {
            attempts,
            elapsed_ms,
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
        let (hwnd, _) = resolve_window_reference(&request.window_id)?;
        // The window renders itself, so a window covered by others is captured as itself; a
        // window that cannot is copied from the screen, and `capture_method` says which.
        let capture =
            capture_window_bounds(hwnd, bounds, request.max_width, request.include_cursor)?;
        if target::issue(hwnd)? != window.id {
            return Err(target::invalid(
                "window geometry changed during capture; observe again",
            ));
        }
        Ok(WindowScreenshot {
            source_bounds: window.bounds,
            window,
            png: capture.png,
            image_width: capture.image_width,
            image_height: capture.image_height,
            downscale_factor: capture.downscale_factor,
            cursor_marker: capture.cursor_marker,
            capture_method: capture.capture_method,
        })
    }

    fn wait_for_window(
        &self,
        request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        self.wait_for_window_controlled(request, &controlfreak_core::MutationControl::default())
    }

    fn wait_for_window_controlled(
        &self,
        request: &WaitForWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowWaitResult, PlatformError> {
        control.check("wait_for_window")?;
        ensure_dpi_awareness()?;
        validate_window_wait(request)?;
        let started = Instant::now();
        let deadline = started + Duration::from_millis(u64::from(request.timeout_ms));
        loop {
            // A window_id names one handle; matching it never depends on the list_windows cap.
            let target = match &request.window_id {
                Some(id) => match resolve_window_reference(id) {
                    Ok((hwnd, _)) => Some(hwnd),
                    Err(_) => None,
                },
                None => None,
            };
            let matched = if request.window_id.is_some() && target.is_none() {
                None
            } else {
                enumerate_window_candidates()?
                    .0
                    .into_iter()
                    .filter(|candidate| target.is_none_or(|hwnd| hwnd == candidate.hwnd))
                    .filter(|candidate| window_matches(&candidate.window, request))
                    // A match can vanish before its reference is issued; try the next one.
                    .map(issue_candidate)
                    .find_map(Result::transpose)
                    .transpose()?
            };
            if let Some(window) = matched {
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
            control.wait(
                "wait_for_window",
                remaining.min(Duration::from_millis(VISUAL_POLL_MS)),
            )?;
        }
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
        ObservationOptions, OcrLine, OcrRegionRequest, OcrWord, PlatformError, PointerBackend,
        TextMatch, WaitForWindowRequest, WindowBackend, WindowBounds, WindowInfo,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    };

    use super::{
        Backend,
        capture::{encode_png, image_difference, resize_rgba_box, scaled_dimensions, word_union},
        display_region_bounds, frame_fingerprint,
        input::{click_inputs, interpolate, key_chord_inputs, scroll_inputs},
        window_matches,
    };

    /// Drives `PassCache` with synthetic frames. Each pass reads a comparison frame, then (when it
    /// recognizes) the recognizer reads its own frame. Returns what each pass answered.
    fn run_passes(
        passes: &[(Option<u64>, u64)],
        recognizer_frames: &[Option<u64>],
    ) -> (Vec<String>, usize) {
        let mut cache = super::PassCache::<String>::default();
        let mut recognitions = 0;
        let answers = passes
            .iter()
            .map(|(frame, generation)| {
                cache
                    .pass(*frame, *generation, || {
                        let seen = recognizer_frames[recognitions];
                        recognitions += 1;
                        Ok((format!("text of frame {seen:?}"), seen))
                    })
                    .unwrap()
            })
            .collect();
        (answers, recognitions)
    }

    #[test]
    fn a_text_wait_reuses_its_previous_pass_only_for_the_frame_it_recognized() {
        let (answers, recognitions) = run_passes(
            &[(Some(1), 7), (Some(1), 7), (Some(1), 7)],
            &[Some(1), Some(1), Some(1)],
        );
        assert_eq!(recognitions, 1);
        assert!(
            answers
                .iter()
                .all(|answer| answer == "text of frame Some(1)")
        );
    }

    #[test]
    fn a_result_recognized_from_a_different_frame_than_the_comparison_is_never_reused() {
        // Comparison frames are A, A, A (1) but the first recognition read a transient frame B
        // (2). The second pass must recognize again and get the answer for A.
        let (answers, recognitions) = run_passes(
            &[(Some(1), 7), (Some(1), 7), (Some(1), 7)],
            &[Some(2), Some(1), Some(1)],
        );
        assert_eq!(recognitions, 2);
        assert_eq!(answers[0], "text of frame Some(2)");
        assert_eq!(answers[1], "text of frame Some(1)");
        assert_eq!(answers[2], "text of frame Some(1)");
    }

    #[test]
    fn a_changed_frame_a_changed_generation_or_an_unreadable_comparison_recognizes_again() {
        for passes in [
            [(Some(1), 7), (Some(2), 7)],
            [(Some(1), 7), (Some(1), 8)],
            [(Some(1), 7), (None, 7)],
        ] {
            let (_, recognitions) = run_passes(&passes, &[Some(1), Some(1)]);
            assert_eq!(recognitions, 2, "{passes:?}");
        }
        // A recognizer that cannot say which frame it read is never cached.
        let (_, recognitions) = run_passes(&[(Some(1), 7), (Some(1), 7)], &[None, None]);
        assert_eq!(recognitions, 2);
    }

    #[test]
    fn a_failed_recognition_drops_the_cached_result() {
        let mut cache = super::PassCache::<u8>::default();
        cache.pass(Some(1), 7, || Ok((1, Some(1)))).unwrap();
        let failed = cache.pass(Some(2), 7, || {
            Err(PlatformError::Unavailable {
                reason: "synthetic".into(),
            })
        });
        assert!(failed.is_err());
        let mut recognized = false;
        cache
            .pass(Some(1), 7, || {
                recognized = true;
                Ok((2, Some(1)))
            })
            .unwrap();
        assert!(
            recognized,
            "the earlier result must not survive a failed pass"
        );
    }

    #[test]
    fn a_relocated_or_unresolvable_region_drops_the_cache_and_rebuilds_the_watch() {
        use controlfreak_core::DisplayBounds;
        let at = |left| DisplayBounds {
            left,
            top: 0,
            width: 10,
            height: 10,
        };
        let mut cache = super::PassCache::<u8>::default();
        cache.pass(Some(1), 7, || Ok((1, Some(1)))).unwrap();
        let mut watch = Some(("old", at(0)));

        // Same rectangle: the watch and the cached result stand.
        super::reconcile_watch(&mut watch, &mut cache, Some(at(0)), || panic!("no rebuild"));
        assert_eq!(watch.map(|(name, _)| name), Some("old"));
        let mut recognized = false;
        cache
            .pass(Some(1), 7, || {
                recognized = true;
                Ok((2, Some(1)))
            })
            .unwrap();
        assert!(!recognized);

        // The region moved with the old pixels and generation unchanged: recognize again.
        super::reconcile_watch(&mut watch, &mut cache, Some(at(5)), || Some(("new", at(5))));
        assert_eq!(watch, Some(("new", at(5))));
        cache
            .pass(Some(1), 7, || {
                recognized = true;
                Ok((2, Some(1)))
            })
            .unwrap();
        assert!(recognized);

        // A region that cannot be resolved is not watched at all.
        super::reconcile_watch(&mut watch, &mut cache, None, || panic!("no rebuild"));
        assert!(watch.is_none());
    }

    #[test]
    fn an_unreadable_text_region_is_not_watched_so_every_pass_recognizes() {
        let region = OcrRegionRequest {
            display_id: "no-such-display-must-not-be-captured".into(),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            language: None,
        };
        assert!(super::region_change_watch(&region).is_none());
    }

    #[test]
    fn queued_input_cancels_while_another_worker_still_owns_the_input_lock() {
        use std::{
            sync::{Arc, mpsc},
            time::Duration,
        };
        let backend = Arc::new(Backend::new());
        let owner = backend.input_lock.lock().unwrap();
        let control = controlfreak_core::MutationControl::default();
        let worker_control = control.clone();
        let worker_backend = Arc::clone(&backend);
        let (done, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            done.send(worker_backend.lock_input(&worker_control).is_err())
                .unwrap();
        });
        control.cancel();
        assert!(result.recv_timeout(Duration::from_secs(2)).unwrap());
        drop(owner);
        worker.join().unwrap();
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
    fn baselines_are_refused_after_their_display_moves_resizes_or_disappears() {
        let captured = DisplayInfo {
            id: "display".to_owned(),
            name: "display".to_owned(),
            bounds: DisplayBounds {
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
            },
            is_primary: false,
        };
        assert!(super::ensure_baseline_display(&captured, std::slice::from_ref(&captured)).is_ok());
        let mut moved = captured.clone();
        moved.bounds.left = 0;
        let mut resized = captured.clone();
        resized.bounds.width = 2560;
        for current in [vec![], vec![moved], vec![resized]] {
            assert!(matches!(
                super::ensure_baseline_display(&captured, &current),
                Err(PlatformError::InvalidArgument { argument, .. }) if argument == "baseline_id"
            ));
        }
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
    fn invalid_discovery_modes_are_rejected_before_display_access() {
        use controlfreak_core::{FindTextRequest, OcrBackend, TextMatchMode};
        for mode in [TextMatchMode::Exact, TextMatchMode::Substring] {
            let error = Backend::new()
                .find_text_on_screen(&FindTextRequest {
                    region: OcrRegionRequest {
                        display_id: "invalid-display-must-not-be-accessed".into(),
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                        language: None,
                    },
                    query: "Code".into(),
                    case_sensitive: false,
                    max_results: 20,
                    match_mode: mode,
                    ocr_confusions: true,
                })
                .unwrap_err();
            assert!(
                matches!(error, PlatformError::InvalidArgument { argument, .. } if argument == "ocr_confusions")
            );
        }
    }

    fn unique_text_candidate(
        lines: &[OcrLine],
        query: &str,
        case_sensitive: bool,
        exact: bool,
    ) -> Result<TextMatch, PlatformError> {
        controlfreak_core::select_text_candidate(
            lines,
            &OcrRegionRequest {
                display_id: "synthetic".into(),
                x: 0,
                y: 0,
                width: 1000,
                height: 1000,
                language: None,
            },
            query,
            case_sensitive,
            exact,
        )
    }

    fn ocr_line(text: &str, x: u32) -> controlfreak_core::OcrLine {
        controlfreak_core::OcrLine {
            text: text.to_owned(),
            x,
            y: 20,
            width: 100,
            height: 30,
            words: Vec::new(),
        }
    }

    #[test]
    fn semantic_click_selection_is_fail_closed_for_zero_or_ambiguous_matches() {
        assert!(unique_text_candidate(&[], "Save", false, true).is_err());
        assert!(matches!(
            unique_text_candidate(
                &[ocr_line("Save", 10), ocr_line("SAVE", 200)],
                "save",
                false,
                true
            ),
            Err(PlatformError::OcrAmbiguousMatch { .. })
        ));
        assert_eq!(
            unique_text_candidate(&[ocr_line("Save", 10)], "save", false, true)
                .unwrap()
                .x,
            10
        );
        assert!(
            unique_text_candidate(&[ocr_line("Save changes", 10)], "Save", false, true).is_err()
        );
    }

    #[test]
    fn semantic_click_checks_exact_matches_beyond_the_search_limit() {
        for case_sensitive in [false, true] {
            let mut lines = vec![ocr_line("Save", 10)];
            lines.extend((1..super::MAX_OCR_RESULTS).map(|x| ocr_line("Save changes", x)));
            lines.push(ocr_line("Save", 200));
            assert!(matches!(
                unique_text_candidate(&lines, "Save", case_sensitive, true),
                Err(PlatformError::OcrAmbiguousMatch { .. })
            ));

            lines[0] = ocr_line("Save as", 10);
            let candidate = unique_text_candidate(&lines, "Save", case_sensitive, true).unwrap();
            assert_eq!(
                candidate,
                controlfreak_core::TextMatch {
                    text: "Save".to_owned(),
                    x: 200,
                    y: 20,
                    width: 100,
                    height: 30,
                }
            );
        }
    }

    #[test]
    fn semantic_click_applies_the_final_predicate_to_all_lines() {
        for (text, query, case_sensitive, exact_match, found) in [
            (" \tSave\n", " Save ", true, true, true),
            (" SAVE ", " save ", false, true, true),
            ("SAVE", "save", true, true, false),
            ("Save changes", " Save ", true, false, true),
            ("SAVE changes", " save ", false, false, true),
            ("SAVE changes", "save", true, false, false),
            ("Cancel", "Save", false, false, false),
            (" ÉDITER ", " éditer ", false, true, true),
        ] {
            let result =
                unique_text_candidate(&[ocr_line(text, 10)], query, case_sensitive, exact_match);
            assert_eq!(
                result.is_ok(),
                found,
                "{text:?}, {query:?}, {case_sensitive}, {exact_match}"
            );
        }
        assert!(matches!(
            unique_text_candidate(
                &[
                    ocr_line("Cancel", 0),
                    ocr_line("Save as", 10),
                    ocr_line("SAVE changes", 200)
                ],
                " save ",
                false,
                false,
            ),
            Err(PlatformError::OcrAmbiguousMatch { .. })
        ));
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
            owner: None,
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
            backend.scroll_mouse_controlled(
                &request(0, 0),
                &controlfreak_core::MutationControl::default()
            ),
            Err(PlatformError::InvalidArgument { .. })
        ));
        assert!(matches!(
            backend.scroll_mouse_controlled(
                &request(12_001, 0),
                &controlfreak_core::MutationControl::default()
            ),
            Err(PlatformError::InvalidArgument { .. })
        ));
    }

    #[test]
    #[ignore = "requires an explicitly approved disposable desktop"]
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
    #[ignore = "requires an explicitly approved disposable desktop"]
    fn live_backend_enumerates_visible_windows() {
        let backend = Backend::new();
        let windows = backend
            .list_windows()
            .expect("enumerate visible Windows windows")
            .windows;

        assert!(windows.iter().all(|window| !window.title.is_empty()));
        assert!(
            windows
                .iter()
                .all(|window| window.id.starts_with("target-"))
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
    fn every_input_family_refuses_an_unapproved_target() {
        use controlfreak_core::{
            KeyChordRequest, KeyboardBackend, MouseClickRequest, MouseDragRequest, ObservationMode,
            TextInputRequest,
        };
        let backend = Backend::new();
        let observation = ObservationOptions {
            mode: ObservationMode::None,
            ..ObservationOptions::default()
        };
        let requests = [
            backend
                .move_mouse_controlled(
                    &MouseMoveRequest {
                        display_id: "synthetic".into(),
                        x: 0,
                        y: 0,
                        duration_ms: 0,
                        observation: observation.clone(),
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
            backend
                .click_mouse_controlled(
                    &MouseClickRequest {
                        display_id: "synthetic".into(),
                        x: 0,
                        y: 0,
                        duration_ms: 0,
                        button: MouseButton::Left,
                        click_count: 1,
                        modifiers: vec![],
                        observation: observation.clone(),
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
            backend
                .scroll_mouse_controlled(
                    &MouseScrollRequest {
                        display_id: "synthetic".into(),
                        x: 0,
                        y: 0,
                        duration_ms: 0,
                        delta_x: 0,
                        delta_y: 120,
                        observation: observation.clone(),
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
            backend
                .drag_mouse_controlled(
                    &MouseDragRequest {
                        start_display_id: "synthetic".into(),
                        end_display_id: "synthetic".into(),
                        start_x: 0,
                        start_y: 0,
                        end_x: 1,
                        end_y: 1,
                        duration_ms: 0,
                        button: MouseButton::Left,
                        modifiers: vec![],
                        observation: observation.clone(),
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
            backend
                .press_keys_controlled(
                    &KeyChordRequest {
                        keys: vec![Key::Enter],
                        observation: observation.clone(),
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
            backend
                .type_text_controlled(
                    &TextInputRequest {
                        text: "synthetic".into(),
                        observation,
                    },
                    &controlfreak_core::MutationControl::default(),
                )
                .map(|_| ()),
        ];
        for result in requests {
            assert!(
                matches!(result, Err(PlatformError::TargetInvalidated { .. })),
                "{result:?}"
            );
        }
    }
}
