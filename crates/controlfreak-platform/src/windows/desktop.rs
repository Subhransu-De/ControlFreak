#![allow(unsafe_code)]

use super::{
    AssertUnwindSafe, BOOL, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DWMWA_CLOAKED,
    DWMWA_EXTENDED_FRAME_BOUNDS, DisplayBounds, DisplayInfo, DwmGetWindowAttribute, E_ACCESSDENIED,
    EnumDisplayMonitors, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetCursorPos,
    GetForegroundWindow, GetMonitorInfoW, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HDC, HMONITOR, HWND,
    IVirtualDesktopManager, IsIconic, IsWindow, IsWindowVisible, LPARAM, MAX_WAIT_MS,
    MAX_WINDOW_TITLE_UTF16_UNITS, MAX_WINDOWS, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
    MONITORINFOF_PRIMARY, MonitorFromWindow, OnceLock, POINT, PlatformError, RECT,
    RPC_E_CHANGED_MODE, SW_RESTORE, SetForegroundWindow, SetProcessDpiAwarenessContext,
    ShowWindowAsync, VirtualDesktopManager, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WaitForWindowRequest, WindowBounds, WindowInfo, c_void, catch_unwind, last_win32_error,
    size_of, win32_error,
};

pub(super) fn foreground_window_handle() -> HWND {
    // SAFETY: GetForegroundWindow has no pointer parameters and returns either a valid handle or
    // a null handle owned by Windows.
    unsafe { GetForegroundWindow() }
}

pub(super) fn window_desktop_id(
    manager: &IVirtualDesktopManager,
    hwnd: HWND,
) -> windows::core::Result<windows::core::GUID> {
    // SAFETY: `manager` is an initialized COM interface and `hwnd` comes from the current Windows
    // enumeration. The windows crate owns the out-parameter handling.
    unsafe { manager.GetWindowDesktopId(hwnd) }
}

pub(super) fn window_on_current_desktop(
    manager: &IVirtualDesktopManager,
    hwnd: HWND,
) -> windows::core::Result<bool> {
    // SAFETY: `manager` is an initialized COM interface and `hwnd` is a borrowed window handle.
    // The windows crate owns the result storage.
    unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }.map(BOOL::as_bool)
}

pub(super) fn window_is_on_current_desktop(manager: &IVirtualDesktopManager, hwnd: HWND) -> bool {
    // SAFETY: `manager` is an initialized COM interface and `hwnd` comes from the current Windows
    // enumeration. The windows crate owns the result storage.
    unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }.is_ok_and(BOOL::as_bool)
}

pub(super) fn restore_window(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` was validated by `find_window`; this asynchronous call does not borrow data.
    unsafe { ShowWindowAsync(hwnd, SW_RESTORE) }.as_bool()
}

pub(super) fn activate_window(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` was validated by `find_window` and remains owned by Windows.
    unsafe { SetForegroundWindow(hwnd) }.as_bool()
}

pub(super) fn current_cursor_position() -> Result<POINT, PlatformError> {
    let mut point = POINT::default();
    // SAFETY: `point` is a valid writable POINT for the duration of the synchronous call.
    unsafe { GetCursorPos(&raw mut point) }.map_err(|error| win32_error("GetCursorPos", &error))?;
    Ok(point)
}

pub(super) struct ComApartment {
    uninitialize: bool,
}

impl ComApartment {
    pub(super) fn initialize() -> Result<Self, PlatformError> {
        // SAFETY: This initializes COM only for the current thread and passes no borrowed pointers.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.is_ok() {
            Ok(Self { uninitialize: true })
        } else if result == RPC_E_CHANGED_MODE {
            // The host initialized this worker thread with another apartment
            // model. COM is still usable; its owner remains responsible for it.
            Ok(Self {
                uninitialize: false,
            })
        } else {
            Err(win32_error(
                "CoInitializeEx",
                &windows::core::Error::from_hresult(result),
            ))
        }
    }
}

impl ComApartment {
    /// Whether this thread is in the multithreaded apartment rather than a host-owned STA.
    const fn is_multithreaded(&self) -> bool {
        self.uninitialize
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            // SAFETY: `uninitialize` is true only when this instance successfully initialized COM
            // on the current thread, so this exactly balances that initialization.
            unsafe { CoUninitialize() };
        }
    }
}

pub(super) fn virtual_desktop_manager() -> Result<IVirtualDesktopManager, PlatformError> {
    // SAFETY: COM is initialized by the caller and both class/interface identifiers are provided by
    // the windows crate for IVirtualDesktopManager.
    unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }
        .map_err(|error| win32_error("CoCreateInstance(VirtualDesktopManager)", &error))
}

/// A virtual-desktop manager proxy created in the process multithreaded apartment.
struct SharedVirtualDesktopManager(IVirtualDesktopManager);

// SAFETY: The proxy is created on an MTA thread and is only used by callers that have joined the
// MTA (checked through `ComApartment::is_multithreaded`). COM permits any MTA thread to call an
// MTA proxy without marshaling, and the windows crate reference-counts it with atomic AddRef.
unsafe impl Send for SharedVirtualDesktopManager {}
// SAFETY: See `Send`; the shared slot only hands out AddRef'd clones under its mutex.
unsafe impl Sync for SharedVirtualDesktopManager {}

static SHARED_VIRTUAL_DESKTOP_MANAGER: std::sync::Mutex<Option<SharedVirtualDesktopManager>> =
    std::sync::Mutex::new(None);

/// Keeps the process MTA alive between worker threads, so the shared proxy and its connection to
/// the shell are not torn down whenever a short-lived worker thread uninitializes COM.
fn keep_multithreaded_apartment() -> Result<(), PlatformError> {
    static USAGE: OnceLock<Result<(), String>> = OnceLock::new();
    USAGE
        .get_or_init(|| {
            // SAFETY: The call has no inputs. The returned cookie is intentionally never passed to
            // CoDecrementMTAUsage, so MTA support stays active for the life of the process.
            unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() }
                .map(|_cookie| ())
                .map_err(|error| error.to_string())
        })
        .clone()
        .map_err(|reason| PlatformError::OperationFailed {
            operation: "CoIncrementMTAUsage".to_owned(),
            reason,
        })
}

fn shared_virtual_desktop_manager(fresh: bool) -> Result<IVirtualDesktopManager, PlatformError> {
    let mut slot = SHARED_VIRTUAL_DESKTOP_MANAGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if fresh {
        *slot = None;
    }
    if let Some(shared) = slot.as_ref() {
        return Ok(shared.0.clone());
    }
    let manager = virtual_desktop_manager()?;
    *slot = Some(SharedVirtualDesktopManager(manager.clone()));
    Ok(manager)
}

/// Runs a virtual-desktop query through a process-wide manager. Creating a manager is a
/// cross-process activation in the shell that costs milliseconds, and target checks need several
/// per action. A failed query is retried once with a new manager, for example after the shell
/// restarts. The outer error reports COM setup failure; the inner result is the query's own.
pub(super) fn with_virtual_desktop_manager<T>(
    query: impl Fn(&IVirtualDesktopManager) -> windows::core::Result<T>,
) -> Result<windows::core::Result<T>, PlatformError> {
    let apartment = ComApartment::initialize()?;
    if !apartment.is_multithreaded() {
        // A host-initialized single-threaded apartment cannot call an MTA proxy directly.
        return Ok(query(&virtual_desktop_manager()?));
    }
    keep_multithreaded_apartment()?;
    let manager = shared_virtual_desktop_manager(false)?;
    match query(&manager) {
        Ok(value) => Ok(Ok(value)),
        Err(_) => Ok(query(&shared_virtual_desktop_manager(true)?)),
    }
}

pub(super) fn current_virtual_desktop_id(manager: &IVirtualDesktopManager) -> Option<String> {
    let foreground = foreground_window_handle();
    if foreground.is_invalid() {
        return None;
    }
    window_desktop_id(manager, foreground)
        .ok()
        .map(|desktop_id| format!("{desktop_id:?}"))
}

pub(super) fn ensure_dpi_awareness() -> Result<(), PlatformError> {
    static DPI_RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    DPI_RESULT
        .get_or_init(|| {
            // SAFETY: This process-wide API takes a constant Windows-provided awareness context
            // and does not retain Rust references.
            let result = unsafe {
                SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            };
            match result {
                Ok(()) => Ok(()),
                Err(error) if error.code() == E_ACCESSDENIED => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
        .clone()
        .map_err(|reason| PlatformError::OperationFailed {
            operation: "SetProcessDpiAwarenessContext".to_owned(),
            reason,
        })
}

#[derive(Default)]
struct MonitorEnumeration {
    displays: Vec<DisplayInfo>,
    error: Option<String>,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _monitor_dc: HDC,
    _monitor_rect: *mut windows::Win32::Foundation::RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` was created from the unique mutable `MonitorEnumeration` reference passed to
    // synchronous EnumDisplayMonitors and remains valid for the complete callback invocation.
    let enumeration = unsafe { &mut *(data.0 as *mut MonitorEnumeration) };
    let outcome = catch_unwind(AssertUnwindSafe(|| monitor_info(monitor)));

    match outcome {
        Ok(Ok(display)) => {
            enumeration.displays.push(display);
            BOOL(1)
        }
        Ok(Err(error)) => {
            enumeration.error = Some(error.to_string());
            BOOL(0)
        }
        Err(_) => {
            enumeration.error = Some("monitor callback panicked".to_owned());
            BOOL(0)
        }
    }
}

fn monitor_info(monitor: HMONITOR) -> Result<DisplayInfo, PlatformError> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize =
        u32::try_from(size_of::<MONITORINFOEXW>()).map_err(|_| PlatformError::OperationFailed {
            operation: "GetMonitorInfoW".to_owned(),
            reason: "MONITORINFOEXW size exceeds the Win32 field width".to_owned(),
        })?;

    // SAFETY: `info.monitorInfo.cbSize` is initialized and the writable structure outlives the call.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info.monitorInfo) }.as_bool() {
        return Err(last_win32_error("GetMonitorInfoW"));
    }

    let rect = info.monitorInfo.rcMonitor;
    let width = rect
        .right
        .checked_sub(rect.left)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| PlatformError::OperationFailed {
            operation: "GetMonitorInfoW".to_owned(),
            reason: "monitor reported an invalid width".to_owned(),
        })?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| PlatformError::OperationFailed {
            operation: "GetMonitorInfoW".to_owned(),
            reason: "monitor reported an invalid height".to_owned(),
        })?;
    let name_length = info
        .szDevice
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(info.szDevice.len());
    let name = String::from_utf16_lossy(&info.szDevice[..name_length]);

    Ok(DisplayInfo {
        id: name.clone(),
        name,
        bounds: DisplayBounds {
            left: rect.left,
            top: rect.top,
            width,
            height,
        },
        is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

pub(super) fn enumerate_displays() -> Result<Vec<DisplayInfo>, PlatformError> {
    let mut enumeration = MonitorEnumeration::default();
    // SAFETY: The LPARAM points to `enumeration`, which remains uniquely borrowed for this
    // synchronous enumeration. The callback catches panics before returning across the FFI edge.
    let succeeded = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM((&raw mut enumeration).cast::<c_void>() as isize),
        )
    };

    if !succeeded.as_bool() {
        return Err(PlatformError::OperationFailed {
            operation: "EnumDisplayMonitors".to_owned(),
            reason: enumeration
                .error
                .unwrap_or_else(|| windows::core::Error::from_thread().to_string()),
        });
    }
    if enumeration.displays.is_empty() {
        return Err(PlatformError::OperationFailed {
            operation: "EnumDisplayMonitors".to_owned(),
            reason: "Windows reported no active displays".to_owned(),
        });
    }

    enumeration.displays.sort_by(|left, right| {
        right
            .is_primary
            .cmp(&left.is_primary)
            .then_with(|| left.bounds.left.cmp(&right.bounds.left))
            .then_with(|| left.bounds.top.cmp(&right.bounds.top))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(enumeration.displays)
}

pub(super) fn find_display(display_id: &str) -> Result<DisplayInfo, PlatformError> {
    enumerate_displays()?
        .into_iter()
        .find(|display| display.id.eq_ignore_ascii_case(display_id))
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "display_id".to_owned(),
            reason: format!(
                "active display '{display_id}' was not found; call list_displays again"
            ),
        })
}

pub(super) fn find_display_at_point(point: POINT) -> Option<DisplayInfo> {
    enumerate_displays().ok()?.into_iter().find(|display| {
        let right = i64::from(display.bounds.left) + i64::from(display.bounds.width);
        let bottom = i64::from(display.bounds.top) + i64::from(display.bounds.height);
        i64::from(point.x) >= i64::from(display.bounds.left)
            && i64::from(point.x) < right
            && i64::from(point.y) >= i64::from(display.bounds.top)
            && i64::from(point.y) < bottom
    })
}

// Bounds native enumeration work on desktops with unusually many top-level windows.
const MAX_WINDOW_CANDIDATES: usize = 4_096;

/// A described window that has not yet been issued a target reference.
pub(super) struct WindowCandidate {
    pub(super) hwnd: HWND,
    pub(super) window: WindowInfo,
}

#[derive(Default)]
struct WindowEnumeration {
    candidates: Vec<WindowCandidate>,
    truncated: bool,
    error: Option<PlatformError>,
}

impl WindowEnumeration {
    fn accept(&mut self, hwnd: HWND, outcome: Result<WindowInfo, PlatformError>) -> BOOL {
        match outcome {
            Ok(window) => {
                if self.candidates.len() < MAX_WINDOW_CANDIDATES {
                    self.candidates.push(WindowCandidate { hwnd, window });
                    BOOL(1)
                } else {
                    self.truncated = true;
                    BOOL(0)
                }
            }
            Err(error @ PlatformError::Unavailable { .. }) => {
                self.error = Some(error);
                BOOL(0)
            }
            // Windows can disappear or become inaccessible during enumeration.
            Err(_) => BOOL(1),
        }
    }
}

unsafe extern "system" fn window_callback(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` was created from the unique mutable `WindowEnumeration` reference passed to
    // synchronous EnumWindows and remains valid for the complete callback invocation.
    let enumeration = unsafe { &mut *(data.0 as *mut WindowEnumeration) };
    let outcome = catch_unwind(AssertUnwindSafe(|| describe_window(hwnd)));
    if let Ok(result) = outcome {
        enumeration.accept(hwnd, result)
    } else {
        enumeration.error = Some(PlatformError::OperationFailed {
            operation: "EnumWindows".into(),
            reason: "window callback panicked".into(),
        });
        BOOL(0)
    }
}

/// Describes eligible windows in foreground-then-title order without issuing references.
pub(super) fn enumerate_window_candidates() -> Result<(Vec<WindowCandidate>, bool), PlatformError> {
    let _timer = controlfreak_core::timing::start("enumerate_windows");
    super::target::ensure_available()?;
    let mut enumeration = WindowEnumeration::default();
    // SAFETY: The LPARAM points to `enumeration`, which remains uniquely borrowed for this
    // synchronous enumeration. The callback catches panics before returning across the FFI edge.
    let result = unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM((&raw mut enumeration).cast::<c_void>() as isize),
        )
    };
    if let Some(error) = enumeration.error {
        return Err(error);
    }
    // EnumWindows reports failure when the callback stops early at the candidate cap.
    if !enumeration.truncated {
        result.map_err(|error| win32_error("EnumWindows", &error))?;
    }
    sort_candidates(&mut enumeration.candidates);
    Ok((enumeration.candidates, enumeration.truncated))
}

fn sort_candidates(candidates: &mut [WindowCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .window
            .is_foreground
            .cmp(&left.window.is_foreground)
            .then_with(|| {
                left.window
                    .title
                    .to_lowercase()
                    .cmp(&right.window.title.to_lowercase())
            })
            .then_with(|| (left.hwnd.0 as usize).cmp(&(right.hwnd.0 as usize)))
    });
}

/// Issues a reference for a described window, or `None` if it vanished or became ineligible.
pub(super) fn issue_candidate(
    candidate: WindowCandidate,
) -> Result<Option<WindowInfo>, PlatformError> {
    match super::target::issue(candidate.hwnd) {
        Ok(id) => Ok(Some(WindowInfo {
            id,
            ..candidate.window
        })),
        Err(error @ PlatformError::Unavailable { .. }) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Lists at most `MAX_WINDOWS` windows; references are issued only for returned windows.
pub(super) fn enumerate_windows() -> Result<(Vec<WindowInfo>, bool), PlatformError> {
    let (candidates, mut truncated) = enumerate_window_candidates()?;
    let mut windows = Vec::with_capacity(candidates.len().min(MAX_WINDOWS));
    for candidate in candidates {
        if windows.len() == MAX_WINDOWS {
            truncated = true;
            break;
        }
        if let Some(window) = issue_candidate(candidate)? {
            windows.push(window);
        }
    }
    Ok((windows, truncated))
}

pub(super) fn window_info(hwnd: HWND) -> Result<WindowInfo, PlatformError> {
    let window = describe_window(hwnd)?;
    Ok(WindowInfo {
        id: super::target::issue(hwnd)?,
        ..window
    })
}

/// Reads window metadata with an empty `id`; callers issue a reference only when they return it.
fn describe_window(hwnd: HWND) -> Result<WindowInfo, PlatformError> {
    ensure_window_target(hwnd)?;
    let title = window_title(hwnd)?;
    let class_name = window_class_name(hwnd)?;
    let rect = visible_window_bounds(hwnd)?;
    let width = rect
        .right
        .checked_sub(rect.left)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| window_not_available("window has invalid bounds"))?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| window_not_available("window has invalid bounds"))?;

    // SAFETY: `hwnd` was validated as a visible Windows-owned handle above.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return Err(last_win32_error("MonitorFromWindow"));
    }
    let display_id = monitor_info(monitor)?.id;
    let mut process_id = 0_u32;
    // SAFETY: `process_id` is a valid writable u32 and `hwnd` remains Windows-owned.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process_id)) };
    if process_id == 0 {
        return Err(window_not_available("window process ID is unavailable"));
    }

    Ok(WindowInfo {
        id: String::new(),
        title,
        class_name,
        process_id,
        bounds: WindowBounds {
            left: rect.left,
            top: rect.top,
            width,
            height,
        },
        display_id,
        is_foreground: foreground_window_handle() == hwnd,
        // SAFETY: `hwnd` was validated as a visible Windows-owned handle above.
        is_minimized: unsafe { IsIconic(hwnd) }.as_bool(),
    })
}

fn ensure_window_target(hwnd: HWND) -> Result<(), PlatformError> {
    // SAFETY: This predicate only inspects the opaque Windows-owned handle and retains no pointers.
    let is_window = unsafe { IsWindow(Some(hwnd)) }.as_bool();
    // SAFETY: This predicate only inspects the opaque Windows-owned handle and retains no pointers.
    let is_visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    if hwnd.is_invalid() || !is_window || !is_visible {
        return Err(window_not_available(
            "window is not a visible top-level target",
        ));
    }
    let mut cloaked = 0_u32;
    // SAFETY: `cloaked` is a correctly sized writable u32 and remains alive for the call.
    let cloaked_result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            u32::try_from(size_of::<u32>()).unwrap_or(4),
        )
    };
    if cloaked_result.is_ok() && cloaked != 0 {
        return Err(window_not_available(
            "window is cloaked by the desktop compositor",
        ));
    }
    // SAFETY: `hwnd` was validated above and GWL_EXSTYLE requests a pointer-sized integer value.
    let extended_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let tool_window_mask = isize::try_from(WS_EX_TOOLWINDOW.0).unwrap_or(isize::MAX);
    let app_window_mask = isize::try_from(WS_EX_APPWINDOW.0).unwrap_or(isize::MAX);
    let is_tool_window = extended_style & tool_window_mask != 0;
    let is_app_window = extended_style & app_window_mask != 0;
    // SAFETY: `hwnd` was validated above; GW_OWNER returns another Windows-owned opaque handle.
    let has_owner = unsafe { GetWindow(hwnd, GW_OWNER) }
        .ok()
        .is_some_and(|owner| !owner.is_invalid());
    if is_tool_window || (has_owner && !is_app_window) {
        return Err(window_not_available("window is an owned or tool window"));
    }
    Ok(())
}

fn window_title(hwnd: HWND) -> Result<String, PlatformError> {
    // SAFETY: `hwnd` was validated by `ensure_window_target`; the call has no output pointer.
    let title_length = unsafe { GetWindowTextLengthW(hwnd) };
    if title_length <= 0 {
        return Err(window_not_available("window has no title"));
    }
    let buffer_length = usize::try_from(title_length)
        .ok()
        .map(|length| length.min(MAX_WINDOW_TITLE_UTF16_UNITS))
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| window_not_available("window title length is invalid"))?;
    let mut title_buffer = vec![0_u16; buffer_length];
    // SAFETY: The buffer is writable, includes space for the terminator, and outlives the call.
    let copied = unsafe { GetWindowTextW(hwnd, &mut title_buffer) };
    if copied <= 0 {
        return Err(window_not_available("window title became unavailable"));
    }
    let copied = usize::try_from(copied)
        .map_err(|_| window_not_available("window title length is invalid"))?;
    Ok(String::from_utf16_lossy(&title_buffer[..copied]))
}

fn window_class_name(hwnd: HWND) -> Result<String, PlatformError> {
    let mut class_buffer = vec![0_u16; 256];
    // SAFETY: The buffer is writable and outlives this synchronous call for a validated handle.
    let class_length = unsafe { GetClassNameW(hwnd, &mut class_buffer) };
    if class_length > 0 {
        let class_length = usize::try_from(class_length)
            .map_err(|_| window_not_available("window class length is invalid"))?;
        Ok(String::from_utf16_lossy(&class_buffer[..class_length]))
    } else {
        Ok(String::new())
    }
}

fn visible_window_bounds(hwnd: HWND) -> Result<RECT, PlatformError> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a correctly sized writable RECT and remains alive for the call.
    let frame_result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast::<c_void>(),
            u32::try_from(size_of::<RECT>()).unwrap_or(16),
        )
    };
    if frame_result.is_err() {
        // SAFETY: `rect` is a valid writable RECT and `hwnd` was validated by the caller.
        unsafe { GetWindowRect(hwnd, &raw mut rect) }
            .map_err(|error| win32_error("GetWindowRect", &error))?;
    }
    Ok(rect)
}

pub(super) fn find_window(window_id: &str) -> Result<WindowInfo, PlatformError> {
    let (hwnd, expected_process_id) = parse_window_id(window_id)?;
    let window = window_info(hwnd).map_err(|_| PlatformError::InvalidArgument {
        argument: "window_id".to_owned(),
        reason: format!("visible window '{window_id}' was not found; call list_windows again"),
    })?;
    if window.process_id != expected_process_id {
        return Err(PlatformError::InvalidArgument {
            argument: "window_id".to_owned(),
            reason: "stale or recycled window ID; call list_windows again".to_owned(),
        });
    }
    Ok(WindowInfo {
        id: window_id.to_owned(),
        ..window
    })
}

pub(super) fn validate_window_wait(request: &WaitForWindowRequest) -> Result<(), PlatformError> {
    if request.window_id.is_none()
        && request.title_contains.is_none()
        && request.class_name.is_none()
        && request.process_id.is_none()
        && request.is_foreground.is_none()
    {
        return Err(PlatformError::InvalidArgument {
            argument: "match".to_owned(),
            reason: "provide at least one window match criterion".to_owned(),
        });
    }
    if request.timeout_ms > MAX_WAIT_MS {
        return Err(PlatformError::InvalidArgument {
            argument: "timeout_ms".to_owned(),
            reason: format!("must be at most {MAX_WAIT_MS}"),
        });
    }
    for (argument, value) in [
        ("window_id", request.window_id.as_deref()),
        ("title_contains", request.title_contains.as_deref()),
        ("class_name", request.class_name.as_deref()),
    ] {
        if value.is_some_and(str::is_empty) {
            return Err(PlatformError::InvalidArgument {
                argument: argument.to_owned(),
                reason: "must not be empty".to_owned(),
            });
        }
    }
    Ok(())
}

/// Matches every criterion except `window_id`, which callers resolve to a handle first.
pub(super) fn window_matches(window: &WindowInfo, request: &WaitForWindowRequest) -> bool {
    request
        .title_contains
        .as_ref()
        .is_none_or(|title| window.title.to_lowercase().contains(&title.to_lowercase()))
        && request
            .class_name
            .as_ref()
            .is_none_or(|class| window.class_name.eq_ignore_ascii_case(class))
        && request
            .process_id
            .is_none_or(|process_id| window.process_id == process_id)
        && request
            .is_foreground
            .is_none_or(|foreground| window.is_foreground == foreground)
}

pub(super) fn parse_window_id(window_id: &str) -> Result<(HWND, u32), PlatformError> {
    super::target::resolve(window_id)
}

fn window_not_available(reason: &str) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: "window".to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod enumeration_tests {
    use super::*;

    #[test]
    fn shared_reference_failures_abort_instead_of_reporting_an_empty_desktop() {
        let mut enumeration = WindowEnumeration::default();
        assert!(
            enumeration
                .accept(
                    HWND::default(),
                    Err(window_not_available("window disappeared"))
                )
                .as_bool()
        );
        assert!(enumeration.error.is_none());
        assert!(
            enumeration
                .accept(
                    HWND::default(),
                    Err(super::super::target::invalid("process exited"))
                )
                .as_bool()
        );
        assert!(enumeration.error.is_none());
        let failure = PlatformError::Unavailable {
            reason: "target reference initialization failed".into(),
        };
        assert!(
            !enumeration
                .accept(HWND::default(), Err(failure.clone()))
                .as_bool()
        );
        assert_eq!(enumeration.error, Some(failure));
    }

    #[test]
    fn enumeration_stops_and_reports_truncation_at_the_candidate_cap() {
        let window = WindowInfo {
            id: String::new(),
            title: "fixture".into(),
            class_name: "fixture".into(),
            process_id: 1,
            bounds: WindowBounds {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
            },
            display_id: "display".into(),
            is_foreground: false,
            is_minimized: false,
        };
        let mut enumeration = WindowEnumeration::default();
        for _ in 0..MAX_WINDOW_CANDIDATES {
            assert!(
                enumeration
                    .accept(HWND::default(), Ok(window.clone()))
                    .as_bool()
            );
        }
        assert!(!enumeration.truncated);
        assert!(!enumeration.accept(HWND::default(), Ok(window)).as_bool());
        assert!(enumeration.truncated);
        assert_eq!(enumeration.candidates.len(), MAX_WINDOW_CANDIDATES);
    }
}
