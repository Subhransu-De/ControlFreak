#![allow(unsafe_code)]
//! Native support for the developer-only installer harness, gated behind a feature.
use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW,
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, GetFileSecurityW, PROTECTED_DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, SetFileSecurityW,
        },
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
    },
    core::{PCWSTR, PWSTR, w},
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY},
};

pub struct Registration {
    key: String,
}
impl Registration {
    pub fn new(production: bool) -> Self {
        let identity = if production {
            "ControlFreak.Windows.x64"
        } else {
            "ControlFreak.Installer.Test"
        };
        Self {
            key: format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{identity}_is1"),
        }
    }
    pub fn location(&self) -> io::Result<Option<PathBuf>> {
        match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(&self.key, KEY_READ | KEY_WOW64_64KEY)
        {
            Ok(key) => {
                let path: String = key.get_value("InstallLocation")?;
                Ok(Some(PathBuf::from(path.trim_end_matches('\\'))))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn set_version(&self, version: &str) -> io::Result<()> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(&self.key, KEY_SET_VALUE | KEY_WOW64_64KEY)?
            .set_value("DisplayVersion", &version)
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = path.canonicalize()?.as_os_str().encode_wide().collect();
    value.push(0);
    Ok(value)
}
struct LocalAllocation(HLOCAL);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: The allocation came from a successful Windows conversion API,
        // is owned by this guard and is freed exactly once after all uses end.
        unsafe {
            let _ = LocalFree(Some(self.0));
        }
    }
}

pub fn restrict_fixture_permissions(path: &Path) -> io::Result<()> {
    let path = wide(path)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: Literal SDDL and live output pointer. On success the API returns an
    // allocated descriptor; its lifetime is owned by LocalAllocation below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("D:P(A;;FA;;;OW)"),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    }?;
    let _allocation = LocalAllocation(HLOCAL(descriptor.0));
    // SAFETY: The path is NUL-terminated and the initialized descriptor remains
    // allocated during this synchronous operation. Only the synthetic fixture DACL changes.
    unsafe {
        SetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    }
    .ok()?;
    Ok(())
}

/// Compare DACL rules and protection, excluding Windows' historical AI bookkeeping.
pub fn configuration_permissions(path: &Path) -> io::Result<String> {
    let path = wide(path)?;
    let mut needed = 0;
    // SAFETY: Size-only query with no descriptor buffer and a writable length.
    unsafe {
        let _ = GetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &raw mut needed,
        );
    }
    if needed == 0 || needed > 1024 * 1024 {
        return Err(io::Error::other("Cannot size fixture security descriptor"));
    }
    let mut data = vec![0_u32; (needed as usize).div_ceil(4)];
    let descriptor = PSECURITY_DESCRIPTOR(data.as_mut_ptr().cast());
    // SAFETY: DWORD-aligned buffer is at least needed bytes; all pointers remain live.
    unsafe {
        GetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(descriptor),
            needed,
            &raw mut needed,
        )
    }
    .ok()?;
    let mut text = PWSTR::null();
    // SAFETY: Valid initialized descriptor and live output pointer. The returned
    // string is owned and freed by the following guard.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &raw mut text,
            None,
        )
    }?;
    let _allocation = LocalAllocation(HLOCAL(text.0.cast()));
    // SAFETY: Successful conversion returned a NUL-terminated UTF-16 string,
    // whose allocation remains live until after the owned Rust String is created.
    let value = unsafe { text.to_string() }.map_err(io::Error::other)?;
    Ok(value.replacen("D:PAI", "D:P", 1).replacen("D:AI", "D:", 1))
}

/// Read the version resource so a production installer cannot enter a local test.
pub fn file_description(path: &Path) -> io::Result<String> {
    let path = wide(path)?;
    // SAFETY: The file path is NUL-terminated; the optional unused output is omitted.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None) };
    if size == 0 || size > 1024 * 1024 {
        return Err(io::Error::other("Invalid version resource size"));
    }
    let mut data = vec![0_u32; (size as usize).div_ceil(4)];
    // SAFETY: Aligned buffer has at least size bytes and remains alive for queries.
    unsafe { GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, data.as_mut_ptr().cast()) }?;
    let translation = version_value(&data, w!(r"\VarFileInfo\Translation"))?;
    if translation.len() < 4 {
        return Err(io::Error::other("Missing resource translation"));
    }
    let language = u16::from_le_bytes([translation[0], translation[1]]);
    let codepage = u16::from_le_bytes([translation[2], translation[3]]);
    let key: Vec<u16> = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\FileDescription")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: The resource was initialized, the key is NUL-terminated and the
    // output pointers are writable. Returned data borrows the still-live buffer.
    unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(key.as_ptr()),
            &raw mut pointer,
            &raw mut length,
        )
    }
    .ok()?;
    let bytes = checked_resource_slice(
        &data,
        pointer.cast(),
        (length as usize)
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("Resource length overflow"))?,
    )?;
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16(&words).map_err(io::Error::other)
}

fn version_value(data: &[u32], key: PCWSTR) -> io::Result<&[u8]> {
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: Callers supply a NUL-terminated key and a successfully initialized
    // version resource; output pointers remain live. Translation length is in bytes.
    unsafe { VerQueryValueW(data.as_ptr().cast(), key, &raw mut pointer, &raw mut length) }.ok()?;
    checked_resource_slice(data, pointer.cast(), length as usize)
}

fn checked_resource_slice(data: &[u32], pointer: *const u8, length: usize) -> io::Result<&[u8]> {
    let start = data.as_ptr() as usize;
    let address = pointer as usize;
    if address < start
        || address
            .checked_add(length)
            .is_none_or(|end| end > start + std::mem::size_of_val(data))
    {
        return Err(io::Error::other("Invalid version resource pointer"));
    }
    // SAFETY: Bounds above prove the range is inside the live resource buffer.
    // The returned slice cannot outlive data and u8 has no alignment requirement.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) })
}
