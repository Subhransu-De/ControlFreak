#![allow(unsafe_code)]

//! Win32 plumbing shared by capture, input, the indicator and the lamp helper: error text, DPI
//! awareness, the COM apartment guard and monitor enumeration.

use std::{ffi::c_void, io};

use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, LPARAM, RPC_E_CHANGED_MODE},
        Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
        },
        UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
    },
    core::{BOOL, Error as WindowsError},
};

/// Formats a failed Win32 call the same way for every I/O-style error in this crate.
pub(super) fn windows_error(context: &str, error: &WindowsError) -> io::Error {
    io::Error::other(format!("{context} failed: {error}"))
}

/// [`windows_error`] for the calling thread's last error.
pub(super) fn last_windows_error(context: &str) -> io::Error {
    windows_error(context, &WindowsError::from_thread())
}

/// Makes the process per-monitor DPI aware. Windows refuses a second change with access denied,
/// which means awareness is already set, so that counts as success.
pub(super) fn set_per_monitor_dpi_awareness() -> Result<(), WindowsError> {
    // SAFETY: This process-wide API takes a constant Windows-provided awareness context and does
    // not retain Rust references.
    let result =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    match result {
        Err(error) if error.code() != E_ACCESSDENIED => Err(error),
        _ => Ok(()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Apartment {
    Multithreaded,
    SingleThreaded,
}

/// Balances a COM initialization on the current thread when it is dropped.
pub(super) struct ComApartment {
    apartment: Apartment,
    /// False when the host had already initialized this thread with another apartment model.
    uninitialize: bool,
}

impl ComApartment {
    /// Joins the multithreaded apartment.
    pub(super) fn multithreaded() -> Result<Self, WindowsError> {
        Self::initialize(Apartment::Multithreaded)
    }

    /// Joins a single-threaded apartment, as a thread that owns windows needs.
    pub(super) fn single_threaded() -> Result<Self, WindowsError> {
        Self::initialize(Apartment::SingleThreaded)
    }

    fn initialize(apartment: Apartment) -> Result<Self, WindowsError> {
        let model = match apartment {
            Apartment::Multithreaded => COINIT_MULTITHREADED,
            Apartment::SingleThreaded => COINIT_APARTMENTTHREADED,
        };
        // SAFETY: This initializes COM only for the current thread and passes no borrowed pointers.
        let result = unsafe { CoInitializeEx(None, model) };
        if result.is_ok() {
            Ok(Self {
                apartment,
                uninitialize: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            // The host initialized this thread with another apartment model. COM is still
            // usable; its owner remains responsible for it.
            Ok(Self {
                apartment,
                uninitialize: false,
            })
        } else {
            Err(WindowsError::from_hresult(result))
        }
    }

    /// Whether this thread is in the multithreaded apartment rather than a host-owned one.
    pub(super) fn is_multithreaded(&self) -> bool {
        self.uninitialize && self.apartment == Apartment::Multithreaded
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

struct MonitorWalk<'a> {
    visit: &'a mut dyn FnMut(HMONITOR) -> Result<(), String>,
    error: Option<String>,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _monitor_dc: HDC,
    _monitor_rect: *mut windows::Win32::Foundation::RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` points to the `MonitorWalk` that `visit_monitors` keeps alive and uniquely
    // borrowed for the whole synchronous EnumDisplayMonitors call.
    let walk = unsafe { &mut *(data.0 as *mut MonitorWalk<'_>) };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (walk.visit)(monitor)));
    match outcome {
        Ok(Ok(())) => BOOL(1),
        Ok(Err(error)) => {
            walk.error = Some(error);
            BOOL(0)
        }
        Err(_) => {
            walk.error = Some("monitor callback panicked".to_owned());
            BOOL(0)
        }
    }
}

/// Calls `visit` for every active monitor. A visitor error or panic stops the walk and is
/// returned as text; Windows' own failure is returned the same way.
pub(super) fn visit_monitors(
    mut visit: impl FnMut(HMONITOR) -> Result<(), String>,
) -> Result<(), String> {
    let mut walk = MonitorWalk {
        visit: &mut visit,
        error: None,
    };
    // SAFETY: The LPARAM points to `walk`, which stays uniquely borrowed for this synchronous
    // enumeration. The callback catches panics before returning across the FFI boundary.
    let succeeded = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM((&raw mut walk).cast::<c_void>() as isize),
        )
    };
    if succeeded.as_bool() {
        return Ok(());
    }
    Err(walk
        .error
        .unwrap_or_else(|| WindowsError::from_thread().to_string()))
}
