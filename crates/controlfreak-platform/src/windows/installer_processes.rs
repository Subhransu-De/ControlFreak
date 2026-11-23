#![allow(unsafe_code)]
//! Read-only Restart Manager and process-tree queries for installer diagnostics.
//! Never stops processes or reads process memory/command lines.
use std::{
    collections::{HashMap, HashSet},
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
};
use windows::{
    Win32::{
        Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, FILETIME, HANDLE},
        Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            RestartManager::{
                CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
                RmStartSession,
            },
            Threading::{
                GetProcessTimes, OpenProcess, PROCESS_NAME_FORMAT,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
    },
    core::{PCWSTR, PWSTR, w},
};

const MAX_ANCESTOR_DEPTH: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAncestor {
    pub pid: u32,
    pub exe_name: String,
    pub friendly_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallerBlocker {
    pub pid: u32,
    pub name: String,
    pub identity_verified: bool,
    pub ancestors: Vec<ProcessAncestor>,
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

#[derive(Debug, Clone)]
struct SnapshotEntry {
    parent_pid: u32,
    exe_name: String,
}

#[derive(Debug, Clone, Default)]
struct ProcessMetadata {
    creation_time: Option<u64>,
    friendly_name: Option<String>,
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
            let snapshot = process_snapshot().unwrap_or_default();
            let mut meta_cache = HashMap::new();
            return Ok(describe_with_snapshot(
                &entries[..count as usize],
                &snapshot,
                |pid| {
                    meta_cache
                        .entry(pid)
                        .or_insert_with(|| query_process_metadata(pid))
                        .clone()
                },
            ));
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

fn describe_with_snapshot(
    entries: &[RM_PROCESS_INFO],
    snapshot: &HashMap<u32, SnapshotEntry>,
    mut metadata_for: impl FnMut(u32) -> ProcessMetadata,
) -> Vec<InstallerBlocker> {
    let mut blockers: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let pid = entry.Process.dwProcessId;
            let name = sanitize(&utf16_nul(&entry.strAppName));
            // Verify the live snapshot process still matches Restart Manager's process instance.
            // If both creation times are known and differ, the original locker exited and the PID
            // was reused by an unrelated process — omit it so users are never told to end it.
            let rm_start = filetime_to_u64(entry.Process.ProcessStartTime);
            let live_start = metadata_for(pid).creation_time;
            let (identity_verified, ancestors) = match (rm_start, live_start) {
                (Some(expected), Some(actual)) if expected == actual => (
                    true,
                    walk_ancestors(pid, expected, snapshot, &mut metadata_for),
                ),
                (Some(_), Some(_)) => return None,
                _ => (false, Vec::new()),
            };
            Some(InstallerBlocker {
                pid,
                name: if name.is_empty() {
                    "Unknown process".into()
                } else {
                    name
                },
                identity_verified,
                ancestors,
            })
        })
        .collect();
    blockers.sort_by_key(|blocker| blocker.pid);
    blockers.dedup_by_key(|blocker| blocker.pid);
    blockers
}

fn walk_ancestors(
    pid: u32,
    start_time: u64,
    snapshot: &HashMap<u32, SnapshotEntry>,
    metadata_for: &mut impl FnMut(u32) -> ProcessMetadata,
) -> Vec<ProcessAncestor> {
    let mut ancestors = Vec::new();
    let mut visited = HashSet::from([pid]);
    let mut current_pid = pid;
    let mut current_start = start_time;

    for _ in 0..MAX_ANCESTOR_DEPTH {
        let Some(current) = snapshot.get(&current_pid) else {
            break;
        };
        let parent_pid = current.parent_pid;
        if parent_pid == 0 || parent_pid == 4 || !visited.insert(parent_pid) {
            break;
        }
        let Some(parent) = snapshot.get(&parent_pid) else {
            break;
        };
        if is_system_root(&parent.exe_name) {
            break;
        }
        let meta = metadata_for(parent_pid);
        // Guard against Windows PID reuse: stop whenever a parent's creation time cannot be
        // verified or is newer than its child.
        let Some(parent_created) = meta.creation_time else {
            break;
        };
        if parent_created > current_start {
            break;
        }
        ancestors.push(ProcessAncestor {
            pid: parent_pid,
            exe_name: parent.exe_name.clone(),
            friendly_name: meta.friendly_name,
        });
        current_pid = parent_pid;
        current_start = parent_created;
    }

    ancestors
}

fn is_system_root(exe_name: &str) -> bool {
    matches!(
        exe_name.to_ascii_lowercase().as_str(),
        "explorer.exe"
            | "services.exe"
            | "svchost.exe"
            | "wininit.exe"
            | "winlogon.exe"
            | "csrss.exe"
            | "smss.exe"
            | "lsass.exe"
            | "sihost.exe"
            | "userinit.exe"
            | "taskhostw.exe"
            | "runtimebroker.exe"
            | "dwm.exe"
            | "powertoys.exe"
            | "powertoys.powerlauncher.exe"
            | "powertoys.cmdpalui.exe"
            | "flow.launcher.exe"
            | "raycast.exe"
            | "wox.exe"
            | "keypirinha.exe"
            | "launchy.exe"
    )
}

fn process_snapshot() -> io::Result<HashMap<u32, SnapshotEntry>> {
    // SAFETY: TH32CS_SNAPPROCESS takes a read-only snapshot of running processes.
    // The returned handle is owned by OwnedHandle and closed on drop.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0))?;
    // SAFETY: CreateToolhelp32Snapshot returned a valid owned kernel handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw.0) };

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    let mut map = HashMap::new();
    // SAFETY: `snapshot` is a live toolhelp handle and `entry.dwSize` is initialized.
    if unsafe { Process32FirstW(HANDLE(snapshot.as_raw_handle()), &raw mut entry) }.is_ok() {
        loop {
            let exe_name = sanitize(&utf16_nul(&entry.szExeFile));
            if !exe_name.is_empty() {
                map.insert(
                    entry.th32ProcessID,
                    SnapshotEntry {
                        parent_pid: entry.th32ParentProcessID,
                        exe_name,
                    },
                );
            }
            // SAFETY: `snapshot` and `entry` remain valid for enumeration.
            if unsafe { Process32NextW(HANDLE(snapshot.as_raw_handle()), &raw mut entry) }.is_err()
            {
                break;
            }
        }
    }
    Ok(map)
}

fn query_process_metadata(pid: u32) -> ProcessMetadata {
    // SAFETY: Read-only limited query open; closed immediately via OwnedHandle on drop.
    let Ok(raw) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return ProcessMetadata::default();
    };
    // SAFETY: OpenProcess returned an owned process handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let h = HANDLE(handle.as_raw_handle());

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `h` is a live process handle with PROCESS_QUERY_LIMITED_INFORMATION and all
    // output FILETIME pointers refer to valid writable stack storage.
    let creation_time = unsafe {
        GetProcessTimes(
            h,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .ok()
    .and_then(|()| filetime_to_u64(creation));

    let friendly_name = query_image_path(h).and_then(|path| read_friendly_name(&path));
    ProcessMetadata {
        creation_time,
        friendly_name,
    }
}

fn query_image_path(handle: HANDLE) -> Option<Vec<u16>> {
    let mut buffer = vec![0_u16; 512];
    for _ in 0..3 {
        let mut size = u32::try_from(buffer.len()).ok()?;
        // SAFETY: `handle` is live with PROCESS_QUERY_LIMITED_INFORMATION and `buffer` has
        // `size` writable UTF-16 code units.
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buffer.as_mut_ptr()),
                &raw mut size,
            )
        };
        if result.is_ok() {
            let len = (size as usize).min(buffer.len().saturating_sub(1));
            buffer.truncate(len);
            buffer.push(0);
            return Some(buffer);
        }
        if buffer.len() >= 32_768 {
            return None;
        }
        buffer.resize(buffer.len() * 2, 0);
    }
    None
}

fn read_friendly_name(path_wide_nul: &[u16]) -> Option<String> {
    // SAFETY: `path_wide_nul` is NUL-terminated UTF-16 and lives through the call.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(path_wide_nul.as_ptr()), None) };
    if size == 0 || size > 1024 * 1024 {
        return None;
    }
    let mut data = vec![0_u32; (size as usize).div_ceil(4)];
    // SAFETY: `data` is DWORD-aligned with at least `size` bytes and stays live for queries.
    unsafe {
        GetFileVersionInfoW(
            PCWSTR(path_wide_nul.as_ptr()),
            None,
            size,
            data.as_mut_ptr().cast(),
        )
    }
    .ok()?;
    let translation = version_bytes(&data, w!(r"\VarFileInfo\Translation"))?;
    if translation.len() < 4 {
        return None;
    }
    let language = u16::from_le_bytes([translation[0], translation[1]]);
    let codepage = u16::from_le_bytes([translation[2], translation[3]]);
    for field in ["FileDescription", "ProductName"] {
        if let Some(value) = version_string(&data, language, codepage, field) {
            let clean = sanitize(&value);
            if !clean.is_empty() {
                return Some(clean);
            }
        }
    }
    None
}

fn version_bytes(data: &[u32], key: PCWSTR) -> Option<&[u8]> {
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: `data` holds a valid version resource, `key` is NUL-terminated, and output
    // pointers are writable. Returned data borrows `data`.
    if !unsafe { VerQueryValueW(data.as_ptr().cast(), key, &raw mut pointer, &raw mut length) }
        .as_bool()
    {
        return None;
    }
    checked_slice(data, pointer.cast(), length as usize)
}

fn version_string(data: &[u32], language: u16, codepage: u16, field: &str) -> Option<String> {
    let key: Vec<u16> = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\{field}")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    // SAFETY: `data` holds a valid version resource and `key` is NUL-terminated.
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(key.as_ptr()),
            &raw mut pointer,
            &raw mut length,
        )
    }
    .as_bool()
    {
        return None;
    }
    let byte_len = (length as usize).checked_mul(2)?;
    let bytes = checked_slice(data, pointer.cast(), byte_len)?;
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|word| *word != 0)
        .collect();
    String::from_utf16(&words).ok()
}

fn checked_slice(data: &[u32], pointer: *const u8, length: usize) -> Option<&[u8]> {
    let start = data.as_ptr() as usize;
    let address = pointer as usize;
    if address < start
        || address
            .checked_add(length)
            .is_none_or(|end| end > start + std::mem::size_of_val(data))
    {
        return None;
    }
    // SAFETY: Bounds check above proves the byte slice lies inside live `data`.
    Some(unsafe { std::slice::from_raw_parts(pointer, length) })
}

fn filetime_to_u64(ft: FILETIME) -> Option<u64> {
    let value = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    (value != 0).then_some(value)
}

fn utf16_nul(slice: &[u16]) -> String {
    let length = slice.iter().position(|c| *c == 0).unwrap_or(slice.len());
    String::from_utf16_lossy(&slice[..length])
}

fn sanitize(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_test_executable_is_identified_without_stopping_it() {
        // OCR tests in this binary start and end copies of this executable, so the Restart
        // Manager list can change while it is read. Retry that one error, as its message asks.
        let blockers = (0..20)
            .find_map(
                |_| match installer_blockers(&[std::env::current_exe().unwrap()]) {
                    Ok(blockers) => Some(blockers),
                    Err(error) if error.to_string().contains("retry inspection") => None,
                    Err(error) => panic!("{error}"),
                },
            )
            .expect("process list kept changing for 20 attempts");
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
            describe_with_snapshot(
                &[info, info],
                &HashMap::new(),
                |_| ProcessMetadata::default()
            ),
            vec![InstallerBlocker {
                pid: 123,
                name: "synthetic app".into(),
                identity_verified: false,
                ancestors: Vec::new(),
            }]
        );
    }

    #[test]
    fn ancestor_walk_stops_at_system_root_reused_pid_and_cycles() {
        let entry = |parent_pid, exe: &str| SnapshotEntry {
            parent_pid,
            exe_name: exe.into(),
        };
        let snapshot = HashMap::from([
            (100, entry(200, "controlfreak.exe")),
            (200, entry(300, "node.exe")),
            (300, entry(400, "Code.exe")),
            (400, entry(0, "explorer.exe")),
            // Orphaned chain where PID 501 was reused AFTER child 500 started.
            (500, entry(501, "controlfreak.exe")),
            (501, entry(400, "unrelated.exe")),
            // Synthetic cycle 600 <-> 601.
            (600, entry(601, "a.exe")),
            (601, entry(600, "b.exe")),
            // Parent 701 has unreadable creation time and must stop the walk.
            (700, entry(701, "controlfreak.exe")),
            (701, entry(400, "elevated-host.exe")),
        ]);
        let mut meta = |pid: u32| match pid {
            100 => ProcessMetadata {
                creation_time: Some(30),
                friendly_name: None,
            },
            200 => ProcessMetadata {
                creation_time: Some(20),
                friendly_name: Some("Node.js".into()),
            },
            300 => ProcessMetadata {
                creation_time: Some(10),
                friendly_name: Some("Visual Studio Code".into()),
            },
            500 => ProcessMetadata {
                creation_time: Some(50),
                friendly_name: None,
            },
            501 => ProcessMetadata {
                creation_time: Some(99),
                friendly_name: Some("Unrelated".into()),
            },
            700 => ProcessMetadata {
                creation_time: Some(40),
                friendly_name: None,
            },
            701 => ProcessMetadata::default(),
            _ => ProcessMetadata {
                creation_time: Some(1),
                friendly_name: None,
            },
        };

        let chain = walk_ancestors(100, 30, &snapshot, &mut meta);
        assert_eq!(
            chain,
            vec![
                ProcessAncestor {
                    pid: 200,
                    exe_name: "node.exe".into(),
                    friendly_name: Some("Node.js".into()),
                },
                ProcessAncestor {
                    pid: 300,
                    exe_name: "Code.exe".into(),
                    friendly_name: Some("Visual Studio Code".into()),
                },
            ]
        );
        assert!(walk_ancestors(500, 50, &snapshot, &mut meta).is_empty());
        assert!(walk_ancestors(700, 40, &snapshot, &mut meta).is_empty());
        let cycle = walk_ancestors(600, 10, &snapshot, &mut meta);
        assert_eq!(cycle.len(), 1);
        assert_eq!(cycle[0].pid, 601);

        // A blocker whose live creation time differs from Restart Manager's ProcessStartTime
        // was reused by an unrelated process and must be omitted entirely.
        let mut reused = RM_PROCESS_INFO::default();
        reused.Process.dwProcessId = 100;
        reused.Process.ProcessStartTime = FILETIME {
            dwLowDateTime: 25,
            dwHighDateTime: 0,
        };
        let blockers = describe_with_snapshot(&[reused], &snapshot, &mut meta);
        assert!(blockers.is_empty());
    }
}
