#![allow(unsafe_code)]

//! Win32 plumbing shared by capture, input, the indicator and the lamp helper: error text, DPI
//! awareness, the COM apartment guard and monitor enumeration.

use std::{ffi::c_void, fmt, io};

use controlfreak_core::DpiAwareness;
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, LPARAM, RPC_E_CHANGED_MODE},
        Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
        System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize,
        },
        UI::HiDpi::{
            AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            DPI_AWARENESS_PER_MONITOR_AWARE, DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE,
            GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext,
            SetProcessDpiAwarenessContext,
        },
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

/// Why the process cannot use physical-pixel coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DpiAwarenessError {
    /// Awareness was already set, by the manifest or a compatibility override, to this mode.
    NotPerMonitor(DpiAwareness),
    /// `SetProcessDpiAwarenessContext` failed for another reason.
    Os(String),
}

impl DpiAwarenessError {
    /// The awareness in effect, as far as it is known.
    pub(super) const fn awareness(&self) -> DpiAwareness {
        match self {
            Self::NotPerMonitor(awareness) => *awareness,
            Self::Os(_) => DpiAwareness::Unknown,
        }
    }
}

impl fmt::Display for DpiAwarenessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPerMonitor(awareness) => write!(
                formatter,
                "Windows runs ControlFreak with DPI awareness '{awareness}', not per-monitor, so \
                 it would scale window and pointer coordinates on a scaled display. Remove the \
                 high DPI override from controlfreak.exe (Properties > Compatibility > Change \
                 high DPI settings) and restart the server from the MCP client"
            ),
            Self::Os(reason) => write!(formatter, "SetProcessDpiAwarenessContext failed: {reason}"),
        }
    }
}

/// Makes the process per-monitor v2 DPI aware and returns the awareness in effect.
///
/// The executable manifest normally sets v2 before any code runs, and Windows then refuses this
/// call with access denied, as it does for any awareness set earlier, including an unaware or
/// system-aware compatibility override. So access denied is not success: the awareness actually
/// in effect is read and accepted only if it is per-monitor.
pub(super) fn set_per_monitor_dpi_awareness() -> Result<DpiAwareness, DpiAwarenessError> {
    per_monitor_dpi_awareness(
        || {
            // SAFETY: This process-wide API takes a constant Windows-provided awareness context
            // and does not retain Rust references.
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        },
        effective_dpi_awareness,
    )
}

/// [`set_per_monitor_dpi_awareness`] with the Win32 calls passed in, so tests can fake them.
fn per_monitor_dpi_awareness(
    set: impl FnOnce() -> Result<(), WindowsError>,
    effective: impl FnOnce() -> DpiAwareness,
) -> Result<DpiAwareness, DpiAwarenessError> {
    match set() {
        Ok(()) => Ok(DpiAwareness::PerMonitorV2),
        Err(error) if error.code() == E_ACCESSDENIED => {
            // Per-monitor v1 is accepted: its coordinates are physical like v2, which only adds
            // non-client and dialog scaling the server does not rely on.
            let awareness = effective();
            if awareness.is_per_monitor() {
                Ok(awareness)
            } else {
                Err(DpiAwarenessError::NotPerMonitor(awareness))
            }
        }
        Err(error) => Err(DpiAwarenessError::Os(error.to_string())),
    }
}

/// The awareness of the calling thread, which is the process awareness: the server never changes
/// a thread's awareness.
fn effective_dpi_awareness() -> DpiAwareness {
    // SAFETY: This takes no arguments and returns a context value for the calling thread; it
    // stays valid for the thread's lifetime and needs no release.
    let context = unsafe { GetThreadDpiAwarenessContext() };
    // SAFETY: Both arguments are awareness-context values, one from Windows above and one a
    // Windows-defined constant; the call only compares them.
    if unsafe { AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .as_bool()
    {
        return DpiAwareness::PerMonitorV2;
    }
    // SAFETY: `context` is the awareness context Windows returned above; the call only reads it.
    match unsafe { GetAwarenessFromDpiAwarenessContext(context) } {
        DPI_AWARENESS_PER_MONITOR_AWARE => DpiAwareness::PerMonitor,
        DPI_AWARENESS_SYSTEM_AWARE => DpiAwareness::SystemAware,
        DPI_AWARENESS_UNAWARE => DpiAwareness::Unaware,
        _ => DpiAwareness::Unknown,
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

#[cfg(test)]
mod tests {
    use super::{DpiAwarenessError, per_monitor_dpi_awareness};
    use controlfreak_core::DpiAwareness;
    use windows::{
        Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG},
        core::Error as WindowsError,
    };

    fn denied() -> Result<(), WindowsError> {
        Err(WindowsError::from_hresult(E_ACCESSDENIED))
    }

    #[test]
    fn setting_awareness_is_per_monitor_v2_without_reading_it_back() {
        let awareness = per_monitor_dpi_awareness(|| Ok(()), || unreachable!("not read"));
        assert_eq!(awareness, Ok(DpiAwareness::PerMonitorV2));
    }

    #[test]
    fn access_denied_accepts_only_a_per_monitor_awareness_already_in_effect() {
        for awareness in [DpiAwareness::PerMonitorV2, DpiAwareness::PerMonitor] {
            assert_eq!(
                per_monitor_dpi_awareness(denied, || awareness),
                Ok(awareness)
            );
        }
        for awareness in [
            DpiAwareness::SystemAware,
            DpiAwareness::Unaware,
            DpiAwareness::Unknown,
        ] {
            let error = per_monitor_dpi_awareness(denied, || awareness).unwrap_err();
            assert_eq!(error, DpiAwarenessError::NotPerMonitor(awareness));
            assert_eq!(error.awareness(), awareness);
            assert!(
                error
                    .to_string()
                    .contains(&format!("DPI awareness '{awareness}'")),
                "the message names the awareness: {error}"
            );
        }
    }

    #[test]
    fn other_failures_are_reported_without_guessing_the_awareness() {
        let error = per_monitor_dpi_awareness(
            || Err(WindowsError::from_hresult(E_INVALIDARG)),
            || unreachable!("not read"),
        )
        .unwrap_err();
        assert!(matches!(error, DpiAwarenessError::Os(_)), "{error:?}");
        assert_eq!(error.awareness(), DpiAwareness::Unknown);
    }
}
