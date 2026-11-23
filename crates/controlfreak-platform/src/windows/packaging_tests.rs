#![allow(unsafe_code)]
//! Native support for the developer-only installer harness, gated behind a feature.
use std::{
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
    core::{PCWSTR, w},
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

/// Unloads a module that was mapped only to read its resources.
struct ResourceModule(windows::Win32::Foundation::HMODULE);
impl Drop for ResourceModule {
    fn drop(&mut self) {
        // SAFETY: The handle came from a successful LoadLibraryExW, is owned by this guard and
        // is released exactly once after every borrowed resource pointer is gone.
        let _ = unsafe { windows::Win32::Foundation::FreeLibrary(self.0) };
    }
}

/// The application manifest Windows reads from `path` (resource `RT_MANIFEST` #1), or `None`
/// when the executable has none. The file is mapped as data only and never executed.
pub fn embedded_manifest(path: &Path) -> io::Result<Option<String>> {
    use windows::Win32::{
        System::LibraryLoader::{
            FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
            LoadLibraryExW, LoadResource, LockResource, SizeofResource,
        },
        UI::WindowsAndMessaging::RT_MANIFEST,
    };
    // `MAKEINTRESOURCE(1)`: the manifest Windows applies to a process.
    const PROCESS_MANIFEST: PCWSTR = PCWSTR(std::ptr::without_provenance(1));
    let path = wide(path)?;
    // SAFETY: The path is NUL-terminated and outlives the call. The data-file flags map the
    // image for resource reads only, so none of its code runs.
    let module = unsafe {
        LoadLibraryExW(
            PCWSTR(path.as_ptr()),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
    }
    .map(ResourceModule)?;
    // SAFETY: `module` is loaded until the guard drops; the name and type are integer resource
    // identifiers, not pointers.
    let resource = unsafe { FindResourceW(Some(module.0), PROCESS_MANIFEST, RT_MANIFEST) };
    if resource.is_invalid() {
        return Ok(None);
    }
    // SAFETY: `resource` was found in the still-loaded `module`.
    let size = unsafe { SizeofResource(Some(module.0), resource) } as usize;
    // SAFETY: As above; the returned handle refers to memory owned by `module`.
    let data = unsafe { LoadResource(Some(module.0), resource) }?;
    // SAFETY: `data` is a loaded resource of `module`; locking it only returns its address.
    let pointer = unsafe { LockResource(data) };
    if pointer.is_null() || size == 0 {
        return Err(io::Error::other("Unreadable manifest resource"));
    }
    // SAFETY: Windows reports `size` readable bytes at `pointer`, which stay mapped while
    // `module` is alive; the bytes are copied before the guard unloads it.
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
    drop(module);
    String::from_utf8(bytes)
        .map(|text| Some(text.trim_start_matches('\u{feff}').to_owned()))
        .map_err(io::Error::other)
}
