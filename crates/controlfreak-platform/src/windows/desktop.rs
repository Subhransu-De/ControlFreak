#![allow(unsafe_code)]

use super::{
    AssertUnwindSafe, BOOL, BringWindowToTop, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DWMWA_CLOAKED,
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

pub(super) fn window_is_on_current_desktop(manager: &IVirtualDesktopManager, hwnd: HWND) -> bool {
    // SAFETY: `manager` is an initialized COM interface and `hwnd` comes from the current Windows
    // enumeration. The windows crate owns the result storage.
    unsafe { manager.IsWindowOnCurrentVirtualDesktop(hwnd) }.is_ok_and(BOOL::as_bool)
}

pub(super) fn restore_window(hwnd: HWND) {
    // SAFETY: `hwnd` was validated by `find_window`; this asynchronous call does not borrow data.
    let _ = unsafe { ShowWindowAsync(hwnd, SW_RESTORE) };
}

pub(super) fn bring_window_to_top(hwnd: HWND) -> Result<(), PlatformError> {
    // SAFETY: `hwnd` was validated by `find_window` and remains owned by Windows.
    unsafe { BringWindowToTop(hwnd) }.map_err(|error| win32_error("BringWindowToTop", &error))
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

#[derive(Default)]
struct WindowEnumeration {
    windows: Vec<WindowInfo>,
    error: Option<String>,
}

unsafe extern "system" fn window_callback(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` was created from the unique mutable `WindowEnumeration` reference passed to
    // synchronous EnumWindows and remains valid for the complete callback invocation.
    let enumeration = unsafe { &mut *(data.0 as *mut WindowEnumeration) };
    let outcome = catch_unwind(AssertUnwindSafe(|| window_info(hwnd)));
    match outcome {
        Ok(Ok(window)) => {
            if enumeration.windows.len() < MAX_WINDOWS {
                enumeration.windows.push(window);
            }
            BOOL(1)
        }
        Ok(Err(_)) => BOOL(1),
        Err(_) => {
            enumeration.error = Some("window callback panicked".to_owned());
            BOOL(0)
        }
    }
}

pub(super) fn enumerate_windows() -> Result<Vec<WindowInfo>, PlatformError> {
    let mut enumeration = WindowEnumeration::default();
    // SAFETY: The LPARAM points to `enumeration`, which remains uniquely borrowed for this
    // synchronous enumeration. The callback catches panics before returning across the FFI edge.
    unsafe {
        EnumWindows(
            Some(window_callback),
            LPARAM((&raw mut enumeration).cast::<c_void>() as isize),
        )
    }
    .map_err(|error| PlatformError::OperationFailed {
        operation: "EnumWindows".to_owned(),
        reason: enumeration.error.unwrap_or_else(|| error.to_string()),
    })?;

    enumeration.windows.sort_by(|left, right| {
        right
            .is_foreground
            .cmp(&left.is_foreground)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(enumeration.windows)
}

pub(super) fn window_info(hwnd: HWND) -> Result<WindowInfo, PlatformError> {
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
        id: format_window_id(hwnd, process_id),
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
    if window.process_id != expected_process_id || window.id != window_id.to_ascii_uppercase() {
        return Err(PlatformError::InvalidArgument {
            argument: "window_id".to_owned(),
            reason: "stale or recycled window ID; call list_windows again".to_owned(),
        });
    }
    Ok(window)
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

pub(super) fn window_matches(window: &WindowInfo, request: &WaitForWindowRequest) -> bool {
    request
        .window_id
        .as_ref()
        .is_none_or(|id| window.id.eq_ignore_ascii_case(id))
        && request
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

fn format_window_id(hwnd: HWND, process_id: u32) -> String {
    format!("0X{:X}:{process_id:X}", hwnd.0 as usize)
}

pub(super) fn parse_window_id(window_id: &str) -> Result<(HWND, u32), PlatformError> {
    let (handle, process_id) = window_id
        .strip_prefix("0x")
        .or_else(|| window_id.strip_prefix("0X"))
        .and_then(|value| value.split_once(':'))
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "window_id".to_owned(),
            reason: "must be an ID returned by list_windows".to_owned(),
        })?;
    let handle = usize::from_str_radix(handle, 16)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "window_id".to_owned(),
            reason: "window handle is invalid".to_owned(),
        })?;
    let process_id = u32::from_str_radix(process_id, 16)
        .ok()
        .filter(|value| *value != 0)
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "window_id".to_owned(),
            reason: "window process ID is invalid".to_owned(),
        })?;
    Ok((HWND(handle as *mut c_void), process_id))
}

fn window_not_available(reason: &str) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: "window".to_owned(),
        reason: reason.to_owned(),
    }
}
