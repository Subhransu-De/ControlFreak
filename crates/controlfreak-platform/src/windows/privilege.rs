#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    mem::{MaybeUninit, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};

use controlfreak_core::{IntegrityLevel, PlatformError, SecurityContext};
use windows::Win32::{
    Foundation::{HANDLE, HWND, POINT},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION,
        TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenIntegrityLevel,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetWindowThreadProcessId, WindowFromPoint},
};

const UNTRUSTED_RID: u32 = 0x0000;
const LOW_RID: u32 = 0x1000;
const MEDIUM_RID: u32 = 0x2000;
const MEDIUM_PLUS_RID: u32 = 0x2100;
const HIGH_RID: u32 = 0x3000;
const SYSTEM_RID: u32 = 0x4000;
const PROTECTED_RID: u32 = 0x5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TokenIntegrity {
    level: IntegrityLevel,
    rid: u32,
}

pub(super) fn current_security_context(
    elevated_operation_allowed: bool,
) -> Result<SecurityContext, PlatformError> {
    // SAFETY: GetCurrentProcess returns a process pseudo-handle valid in this process.
    let process = unsafe { GetCurrentProcess() };
    let token = open_process_token(process).map_err(|reason| PlatformError::Unavailable {
        reason: format!("could not inspect the ControlFreak process token: {reason}"),
    })?;
    let integrity = token_integrity(&token).map_err(|reason| PlatformError::Unavailable {
        reason: format!("could not determine the ControlFreak integrity level: {reason}"),
    })?;
    let elevated = token_is_elevated(&token).map_err(|reason| PlatformError::Unavailable {
        reason: format!("could not determine whether ControlFreak is elevated: {reason}"),
    })?;
    Ok(SecurityContext {
        elevated,
        windows_integrity_level: integrity.level,
        elevated_operation_allowed,
    })
}

pub(super) fn ensure_window_integrity(operation: &str, hwnd: HWND) -> Result<(), PlatformError> {
    if hwnd.is_invalid() {
        return Err(PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: "Windows reported no target window".to_owned(),
        });
    }
    let mut process_id = 0_u32;
    // SAFETY: The function only reads the owning process ID for the supplied window handle.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process_id)) };
    if process_id == 0 {
        return Err(PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: "Windows reported no process for the target window".to_owned(),
        });
    }
    let target = process_integrity(process_id).map_err(|reason| {
        PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: format!("process {process_id}: {reason}"),
        }
    })?;
    let server = current_process_integrity().map_err(|reason| {
        PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: format!("could not revalidate the ControlFreak process integrity: {reason}"),
        }
    })?;
    validate_integrity_boundary(operation, process_id, server, target)
}

pub(super) fn ensure_point_integrity(operation: &str, point: POINT) -> Result<(), PlatformError> {
    // SAFETY: WindowFromPoint only reads the supplied value and returns a borrowed window handle.
    let target = unsafe { WindowFromPoint(point) };
    if target.is_invalid() {
        return Err(PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: format!(
                "Windows reported no input target at virtual-screen point ({}, {})",
                point.x, point.y
            ),
        });
    }
    ensure_window_integrity(operation, target)?;
    // SAFETY: GetAncestor reads window ownership metadata and returns a borrowed handle.
    let root = unsafe { GetAncestor(target, GA_ROOT) };
    if root.is_invalid() || root == target {
        Ok(())
    } else {
        ensure_window_integrity(operation, root)
    }
}

fn current_process_integrity() -> Result<TokenIntegrity, String> {
    // SAFETY: GetCurrentProcess returns a process pseudo-handle valid in this process.
    let process = unsafe { GetCurrentProcess() };
    let token = open_process_token(process)?;
    token_integrity(&token)
}

fn process_integrity(process_id: u32) -> Result<TokenIntegrity, String> {
    // SAFETY: The requested handle has query-only rights and is wrapped in OwnedHandle immediately.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
        .map_err(|error| format!("could not open the process for an integrity query: {error}"))?;
    // SAFETY: OpenProcess returned an owned, closeable process handle.
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let token = open_process_token(raw_handle(&process))?;
    token_integrity(&token)
}

fn open_process_token(process: HANDLE) -> Result<OwnedHandle, String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is live and `token` points to writable storage for the returned handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }
        .map_err(|error| format!("OpenProcessToken failed: {error}"))?;
    // SAFETY: OpenProcessToken returned an owned, closeable token handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(token.0) })
}

fn token_is_elevated(token: &OwnedHandle) -> Result<bool, String> {
    let mut elevation = MaybeUninit::<TOKEN_ELEVATION>::zeroed();
    let mut returned = 0_u32;
    // SAFETY: The output buffer is correctly sized and valid for the synchronous query.
    unsafe {
        GetTokenInformation(
            raw_handle(token),
            TokenElevation,
            Some(elevation.as_mut_ptr().cast::<c_void>()),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).map_err(|error| error.to_string())?,
            &raw mut returned,
        )
    }
    .map_err(|error| format!("TokenElevation query failed: {error}"))?;
    if usize::try_from(returned).unwrap_or_default() < size_of::<TOKEN_ELEVATION>() {
        return Err("TokenElevation returned a truncated record".to_owned());
    }
    // SAFETY: GetTokenInformation initialized the complete record, as checked above.
    Ok(unsafe { elevation.assume_init() }.TokenIsElevated != 0)
}

fn token_integrity(token: &OwnedHandle) -> Result<TokenIntegrity, String> {
    let mut required = 0_u32;
    // SAFETY: A zero-length first query is the documented way to obtain the required size.
    let first = unsafe {
        GetTokenInformation(
            raw_handle(token),
            TokenIntegrityLevel,
            None,
            0,
            &raw mut required,
        )
    };
    if required == 0 {
        return Err(format!(
            "TokenIntegrityLevel size query failed without a required size: {:?}",
            first.err()
        ));
    }
    let required = usize::try_from(required).map_err(|error| error.to_string())?;
    let words = required.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let mut returned = 0_u32;
    // SAFETY: The aligned buffer is at least `required` bytes and remains live for the query.
    unsafe {
        GetTokenInformation(
            raw_handle(token),
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            u32::try_from(required).map_err(|error| error.to_string())?,
            &raw mut returned,
        )
    }
    .map_err(|error| format!("TokenIntegrityLevel query failed: {error}"))?;
    if usize::try_from(returned).unwrap_or_default() < size_of::<TOKEN_MANDATORY_LABEL>() {
        return Err("TokenIntegrityLevel returned a truncated record".to_owned());
    }
    // SAFETY: The query returned a complete TOKEN_MANDATORY_LABEL into aligned storage.
    let label = unsafe { &*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let sid = label.Label.Sid;
    // SAFETY: The SID belongs to the live query buffer and is valid for these synchronous reads.
    let count = unsafe { GetSidSubAuthorityCount(sid).as_ref() }
        .copied()
        .ok_or_else(|| "integrity SID has no sub-authority count".to_owned())?;
    if count == 0 {
        return Err("integrity SID has no sub-authorities".to_owned());
    }
    // SAFETY: `count - 1` is an in-range sub-authority index for this SID.
    let rid = unsafe { GetSidSubAuthority(sid, u32::from(count - 1)).as_ref() }
        .copied()
        .ok_or_else(|| "integrity SID has no terminal RID".to_owned())?;
    Ok(TokenIntegrity {
        level: integrity_level(rid),
        rid,
    })
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

const fn integrity_level(rid: u32) -> IntegrityLevel {
    match rid {
        UNTRUSTED_RID..LOW_RID => IntegrityLevel::Untrusted,
        LOW_RID..MEDIUM_RID => IntegrityLevel::Low,
        MEDIUM_RID..MEDIUM_PLUS_RID => IntegrityLevel::Medium,
        MEDIUM_PLUS_RID..HIGH_RID => IntegrityLevel::MediumPlus,
        HIGH_RID..SYSTEM_RID => IntegrityLevel::High,
        SYSTEM_RID..PROTECTED_RID => IntegrityLevel::System,
        PROTECTED_RID.. => IntegrityLevel::Protected,
    }
}

#[cfg(test)]
const fn token_integrity_from_level(level: IntegrityLevel) -> TokenIntegrity {
    let rid = match level {
        IntegrityLevel::Untrusted => UNTRUSTED_RID,
        IntegrityLevel::Low => LOW_RID,
        IntegrityLevel::Medium => MEDIUM_RID,
        IntegrityLevel::MediumPlus => MEDIUM_PLUS_RID,
        IntegrityLevel::High => HIGH_RID,
        IntegrityLevel::System => SYSTEM_RID,
        IntegrityLevel::Protected => PROTECTED_RID,
        IntegrityLevel::Unknown => 0,
    };
    TokenIntegrity { level, rid }
}

fn validate_integrity_boundary(
    operation: &str,
    process_id: u32,
    server: TokenIntegrity,
    target: TokenIntegrity,
) -> Result<(), PlatformError> {
    if server.level == IntegrityLevel::Unknown {
        return Err(PlatformError::TargetIntegrityUnavailable {
            operation: operation.to_owned(),
            reason: "the ControlFreak process integrity level is unknown".to_owned(),
        });
    }
    if target.rid > server.rid {
        return Err(PlatformError::HigherIntegrityTarget {
            operation: operation.to_owned(),
            process_id,
            server_integrity: server.level,
            target_integrity: target.level,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_windows_integrity_rids() {
        assert_eq!(integrity_level(LOW_RID), IntegrityLevel::Low);
        assert_eq!(integrity_level(MEDIUM_RID), IntegrityLevel::Medium);
        assert_eq!(integrity_level(MEDIUM_PLUS_RID), IntegrityLevel::MediumPlus);
        assert_eq!(integrity_level(HIGH_RID), IntegrityLevel::High);
        assert_eq!(integrity_level(SYSTEM_RID), IntegrityLevel::System);
    }

    #[test]
    fn mixed_integrity_policy_accepts_equal_or_lower_targets() {
        let medium = token_integrity_from_level(IntegrityLevel::Medium);
        let low = token_integrity_from_level(IntegrityLevel::Low);
        assert!(validate_integrity_boundary("test", 7, medium, medium).is_ok());
        assert!(validate_integrity_boundary("test", 7, medium, low).is_ok());
    }

    #[test]
    fn mixed_integrity_policy_has_a_dedicated_higher_target_error() {
        let error = validate_integrity_boundary(
            "test",
            42,
            token_integrity_from_level(IntegrityLevel::Medium),
            token_integrity_from_level(IntegrityLevel::High),
        )
        .expect_err("higher-integrity target must be refused");
        assert!(matches!(
            error,
            PlatformError::HigherIntegrityTarget { process_id: 42, .. }
        ));
    }
}
