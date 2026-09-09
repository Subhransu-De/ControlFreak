#![allow(unsafe_code)]

use std::{ffi::c_void, mem::size_of};

use controlfreak_core::PlatformError;
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::{
            RemoteDesktop::{
                ProcessIdToSessionId, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory,
                WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
            },
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS,
                GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
            },
        },
    },
    core::PWSTR,
};

pub(super) fn ensure_interactive_input_desktop(operation: &str) -> Result<(), PlatformError> {
    let session = session_info()?;
    // SAFETY: Level 1 was validated before this function returned the record.
    let session = unsafe { session.Data.WTSInfoExLevel1 };
    if session.SessionState != WTSActive {
        return Err(environment_error(
            operation,
            "the Windows session is disconnected or inactive",
        ));
    }
    if u32::try_from(session.SessionFlags).ok() != Some(WTS_SESSIONSTATE_UNLOCK) {
        return Err(environment_error(
            operation,
            "the Windows session is locked",
        ));
    }

    let desktop_name = input_desktop_name()?;
    if !desktop_name.eq_ignore_ascii_case("Default") {
        return Err(environment_error(
            operation,
            format!(
                "the active input desktop is {desktop_name:?}, not the expected interactive Default desktop"
            ),
        ));
    }
    Ok(())
}

fn session_info() -> Result<WTSINFOEXW, PlatformError> {
    let mut session_id = 0_u32;
    // SAFETY: The output points to a valid u32 for this synchronous call.
    unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session_id) }
        .map_err(|error| environment_api_error("ProcessIdToSessionId", &error))?;

    let mut buffer = PWSTR::null();
    let mut bytes = 0_u32;
    // SAFETY: WTS allocates the returned buffer and reports its byte length; it is freed below.
    unsafe {
        WTSQuerySessionInformationW(
            None,
            session_id,
            WTSSessionInfoEx,
            &raw mut buffer,
            &raw mut bytes,
        )
    }
    .map_err(|error| environment_api_error("WTSQuerySessionInformationW", &error))?;
    if buffer.is_null() || usize::try_from(bytes).unwrap_or(0) < size_of::<WTSINFOEXW>() {
        if !buffer.is_null() {
            // SAFETY: The buffer was allocated by WTSQuerySessionInformationW.
            unsafe { WTSFreeMemory(buffer.as_ptr().cast::<c_void>()) };
        }
        return Err(environment_api_error(
            "WTSQuerySessionInformationW",
            &"returned an undersized session record",
        ));
    }
    // SAFETY: The length check above proves the allocation contains a complete WTSINFOEXW.
    let info = unsafe { buffer.as_ptr().cast::<WTSINFOEXW>().read_unaligned() };
    // SAFETY: The buffer was allocated by WTSQuerySessionInformationW and is no longer used.
    unsafe { WTSFreeMemory(buffer.as_ptr().cast::<c_void>()) };
    if info.Level != 1 {
        return Err(environment_api_error(
            "WTSQuerySessionInformationW",
            &format!("returned unsupported WTSINFOEX level {}", info.Level),
        ));
    }
    Ok(info)
}

fn input_desktop_name() -> Result<String, PlatformError> {
    // SAFETY: Access is limited to reading the current input desktop's metadata.
    let desktop =
        unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS::default(), false, DESKTOP_READOBJECTS) }
            .map_err(|error| environment_api_error("OpenInputDesktop", &error))?;
    let mut required = 0_u32;
    // SAFETY: A null buffer with length zero requests the required byte count.
    let _ = unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            None,
            0,
            Some(&raw mut required),
        )
    };
    let mut name = vec![
        0_u16;
        usize::try_from(required)
            .unwrap_or(0)
            .div_ceil(size_of::<u16>())
    ];
    let query = if name.is_empty() {
        Err(environment_api_error(
            "GetUserObjectInformationW",
            &"returned an empty desktop name",
        ))
    } else {
        // SAFETY: `name` is writable for the reported byte count and lives through the call.
        unsafe {
            GetUserObjectInformationW(
                HANDLE(desktop.0),
                UOI_NAME,
                Some(name.as_mut_ptr().cast::<c_void>()),
                required,
                Some(&raw mut required),
            )
        }
        .map_err(|error| environment_api_error("GetUserObjectInformationW", &error))
        .map(|()| {
            let length = name
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(name.len());
            String::from_utf16_lossy(&name[..length])
        })
    };
    // SAFETY: `desktop` is a live handle returned by OpenInputDesktop and is closed exactly once.
    let close = unsafe { CloseDesktop(desktop) }
        .map_err(|error| environment_api_error("CloseDesktop", &error));
    close?;
    query
}

fn environment_error(operation: &str, reason: impl Into<String>) -> PlatformError {
    PlatformError::OperationFailed {
        operation: operation.to_owned(),
        reason: format!(
            "state-changing input was refused because ControlFreak cannot verify a visible interactive desktop: {}",
            reason.into()
        ),
    }
}

fn environment_api_error(api: &str, error: &impl std::fmt::Display) -> PlatformError {
    PlatformError::Unavailable {
        reason: format!("could not validate the Windows input environment with {api}: {error}"),
    }
}
