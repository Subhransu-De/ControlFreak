#![allow(unsafe_code)]

//! Setup-only file security; not reachable through the MCP backend.
use std::{io, os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::Foundation::ERROR_INSUFFICIENT_BUFFER,
    Win32::Security::{
        DACL_SECURITY_INFORMATION, GetFileSecurityW, GetSecurityDescriptorControl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        SetFileSecurityW, UNPROTECTED_DACL_SECURITY_INFORMATION,
    },
    core::PCWSTR,
};

/// Copy a configuration's DACL to an empty staging or backup file before writing
/// sensitive data. Preserve whether inheritance is protected. No privilege changes.
pub fn copy_configuration_permissions(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    let mut needed = 0;
    // SAFETY: The source string is NUL-terminated. This size query provides no
    // descriptor buffer and a live writable size output.
    let queried = unsafe {
        GetFileSecurityW(
            PCWSTR(source.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &raw mut needed,
        )
    };
    if queried.as_bool()
        || io::Error::last_os_error()
            .raw_os_error()
            .map(i32::unsigned_abs)
            != Some(ERROR_INSUFFICIENT_BUFFER.0)
    {
        return Err(io::Error::other("Cannot query configuration permissions"));
    }
    if needed == 0 || needed > 1024 * 1024 {
        return Err(io::Error::other(
            "Invalid configuration security descriptor size",
        ));
    }
    // DWORD alignment is required for a Windows security descriptor.
    let mut buffer = vec![0_u32; (needed as usize).div_ceil(size_of::<u32>())];
    let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
    // SAFETY: The aligned buffer has at least `needed` bytes and remains alive
    // throughout all descriptor operations. Windows fills a self-relative descriptor.
    unsafe {
        GetFileSecurityW(
            PCWSTR(source.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(descriptor),
            needed,
            &raw mut needed,
        )
    }
    .ok()?;
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: The descriptor was successfully initialized by GetFileSecurityW;
    // both output pointers refer to live writable scalars.
    unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) }?;
    let inheritance = if control & SE_DACL_PROTECTED.0 != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // SAFETY: Destination is NUL-terminated and the initialized descriptor's
    // backing allocation is still alive. The API synchronously copies its DACL.
    unsafe {
        SetFileSecurityW(
            PCWSTR(destination.as_ptr()),
            DACL_SECURITY_INFORMATION | inheritance,
            descriptor,
        )
    }
    .ok()?;
    Ok(())
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    // Both files already exist. Canonicalization also supplies the extended-length
    // Windows path prefix needed by these legacy security APIs.
    let path = path.canonicalize()?;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}
