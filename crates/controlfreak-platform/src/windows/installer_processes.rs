#![allow(unsafe_code)]
//! Read-only Restart Manager queries for installer diagnostics. Never shuts down processes.
use std::{io, os::windows::ffi::OsStrExt, path::PathBuf};
use windows::{
    Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS},
        System::RestartManager::{
            CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
            RmStartSession,
        },
    },
    core::{PCWSTR, PWSTR},
};

#[derive(Debug, PartialEq, Eq)]
pub struct InstallerBlocker {
    pub pid: u32,
    pub name: String,
}
struct Session(u32);
impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: This guard owns a successfully started session and ends it once,
        // after all resource registrations and queries have finished.
        unsafe {
            let _ = RmEndSession(self.0);
        }
    }
}

/// List processes using exactly the registered files. An empty list is not proof
/// of writability: permissions and undetectable locks can still prevent an update.
pub fn installer_blockers(files: &[PathBuf]) -> io::Result<Vec<InstallerBlocker>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    if files.len() > 16 {
        return Err(io::Error::other("Too many installer resources"));
    }
    let paths = files
        .iter()
        .map(|path| {
            let path = std::path::absolute(path)?;
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.contains(&0) {
                return Err(io::Error::other("Invalid resource path"));
            }
            wide.push(0);
            Ok(wide)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let pointers: Vec<PCWSTR> = paths.iter().map(|path| PCWSTR(path.as_ptr())).collect();
    let mut handle = 0;
    let mut key = [0_u16; CCH_RM_SESSION_KEY as usize + 1];
    // SAFETY: Both outputs point to live storage. The key buffer has the required
    // CCH_RM_SESSION_KEY + 1 UTF-16 code units. No existing session is joined.
    unsafe { RmStartSession(&raw mut handle, None, PWSTR(key.as_mut_ptr())) }.ok()?;
    let session = Session(handle);
    // SAFETY: All NUL-terminated paths and the pointer array remain live through
    // this synchronous registration; the session is owned by the guard.
    unsafe { RmRegisterResources(session.0, Some(&pointers), None, None) }.ok()?;
    query(&session)
}

fn query(session: &Session) -> io::Result<Vec<InstallerBlocker>> {
    let mut entries = Vec::<RM_PROCESS_INFO>::new();
    for _ in 0..4 {
        let mut needed = 0;
        let mut count = u32::try_from(entries.len()).map_err(io::Error::other)?;
        let mut reboot_reasons = 0;
        let buffer = if entries.is_empty() {
            None
        } else {
            Some(entries.as_mut_ptr())
        };
        // SAFETY: The live buffer contains count initialized entries (or is null
        // for the size query). All scalar outputs and the session remain valid.
        let result = unsafe {
            RmGetList(
                session.0,
                &raw mut needed,
                &raw mut count,
                buffer,
                &raw mut reboot_reasons,
            )
        };
        if result == ERROR_SUCCESS {
            if count as usize > entries.len() {
                return Err(io::Error::other("Invalid process count"));
            }
            return Ok(describe(&entries[..count as usize]));
        }
        if result != ERROR_MORE_DATA {
            result.ok()?;
        }
        if needed == 0 || needed > 4096 {
            return Err(io::Error::other("Invalid blocker list size"));
        }
        entries.resize(needed as usize, RM_PROCESS_INFO::default());
    }
    Err(io::Error::other(
        "Process list changed repeatedly; retry inspection",
    ))
}

fn describe(entries: &[RM_PROCESS_INFO]) -> Vec<InstallerBlocker> {
    let mut blockers: Vec<_> = entries
        .iter()
        .map(|entry| {
            let length = entry
                .strAppName
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.strAppName.len());
            let name =
                String::from_utf16_lossy(&entry.strAppName[..length]).replace(['\r', '\n'], " ");
            InstallerBlocker {
                pid: entry.Process.dwProcessId,
                name: if name.is_empty() {
                    "Unknown process".into()
                } else {
                    name
                },
            }
        })
        .collect();
    blockers.sort_by_key(|blocker| blocker.pid);
    blockers.dedup_by_key(|blocker| blocker.pid);
    blockers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn running_test_executable_is_identified_without_stopping_it() {
        let blockers = installer_blockers(&[std::env::current_exe().unwrap()]).unwrap();
        assert!(
            blockers
                .iter()
                .any(|blocker| blocker.pid == std::process::id())
        );
    }
    #[test]
    fn descriptions_keep_every_pid_once_and_remove_newlines() {
        let mut info = RM_PROCESS_INFO::default();
        info.Process.dwProcessId = 123;
        for (slot, value) in info
            .strAppName
            .iter_mut()
            .zip("synthetic\napp".encode_utf16())
        {
            *slot = value;
        }
        assert_eq!(
            describe(&[info, info]),
            vec![InstallerBlocker {
                pid: 123,
                name: "synthetic app".into()
            }]
        );
    }
}
