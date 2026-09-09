#![allow(unsafe_code)]

use std::{
    ffi::{OsStr, OsString, c_void},
    fs::File,
    io,
    mem::{size_of, size_of_val, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    ptr,
};

use windows::{
    Win32::{
        Foundation::{
            HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation, WAIT_OBJECT_0,
            WAIT_TIMEOUT,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::CreatePipe,
            Threading::{
                CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
                DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
                InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
                PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

pub struct ManagedChild {
    process: OwnedHandle,
    helper_job: Option<OwnedHandle>,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
    process_id: u32,
    containment_reason: Option<String>,
}

impl ManagedChild {
    pub fn id(&self) -> u32 {
        self.process_id
    }

    pub fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn containment(&self) -> (&'static str, Option<String>) {
        if self.helper_job.is_some() {
            ("job", None)
        } else {
            (
                "unavailable",
                self.containment_reason
                    .clone()
                    .or_else(|| Some("helper job assignment was unavailable".to_owned())),
            )
        }
    }

    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        let process = raw_handle(&self.process);
        // SAFETY: `process` remains valid for the duration of this synchronous wait.
        let outcome = unsafe { WaitForSingleObject(process, 0) };
        if outcome == WAIT_TIMEOUT {
            return Ok(None);
        }
        if outcome != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0_u32;
        // SAFETY: The process is signaled and `code` points to valid writable storage.
        unsafe { GetExitCodeProcess(process, &raw mut code) }.map_err(windows_error)?;
        Ok(Some(ExitStatus::from_raw(code)))
    }

    pub fn terminate(&self) -> io::Result<()> {
        if let Some(job) = self.helper_job.as_ref() {
            // SAFETY: The job handle is live and contains this helper from process creation.
            unsafe { TerminateJobObject(raw_handle(job), 1) }.map_err(windows_error)
        } else {
            // SAFETY: The process handle is live and the caller subsequently confirms exit.
            unsafe { TerminateProcess(raw_handle(&self.process), 1) }.map_err(windows_error)
        }
    }
}

pub(super) fn spawn_managed_child(
    program: &Path,
    arguments: &[OsString],
) -> io::Result<ManagedChild> {
    let stdin_pipe = anonymous_pipe(true)?;
    let stdout_pipe = anonymous_pipe(false)?;
    let stderr_pipe = anonymous_pipe(false)?;
    clear_inherit(&stdin_pipe.parent)?;
    clear_inherit(&stdout_pipe.parent)?;
    clear_inherit(&stderr_pipe.parent)?;

    let helper_job = create_job().ok();
    let (process_info, containment_reason) = match helper_job.as_ref() {
        Some(job) => match create_process(
            program,
            arguments,
            &stdin_pipe,
            &stdout_pipe,
            &stderr_pipe,
            Some(job),
        ) {
            Ok(process) => (process, None),
            Err(error) => (
                create_process(
                    program,
                    arguments,
                    &stdin_pipe,
                    &stdout_pipe,
                    &stderr_pipe,
                    None,
                )?,
                Some(format!(
                    "creation-time helper job assignment failed; cooperative cleanup only: {error}"
                )),
            ),
        },
        None => (
            create_process(
                program,
                arguments,
                &stdin_pipe,
                &stdout_pipe,
                &stderr_pipe,
                None,
            )?,
            Some("the per-helper Windows job object could not be created".to_owned()),
        ),
    };

    drop(stdin_pipe.child);
    drop(stdout_pipe.child);
    drop(stderr_pipe.child);
    // SAFETY: CreateProcessW returned owned process/thread handles. The thread handle is not needed.
    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess.0) };
    // SAFETY: The primary thread handle is live and no longer needed after process creation.
    drop(unsafe { OwnedHandle::from_raw_handle(process_info.hThread.0) });
    let contained = containment_reason.is_none();
    Ok(ManagedChild {
        process,
        helper_job: contained.then_some(helper_job).flatten(),
        stdin: Some(File::from(stdin_pipe.parent)),
        stdout: Some(File::from(stdout_pipe.parent)),
        stderr: Some(File::from(stderr_pipe.parent)),
        process_id: process_info.dwProcessId,
        containment_reason,
    })
}

struct PipePair {
    parent: OwnedHandle,
    child: OwnedHandle,
}

fn anonymous_pipe(parent_writes: bool) -> io::Result<PipePair> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        bInheritHandle: true.into(),
        ..Default::default()
    };
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    // SAFETY: Both outputs point to valid handle storage and attributes live through the call.
    unsafe {
        CreatePipe(
            &raw mut read,
            &raw mut write,
            Some(&raw const attributes),
            0,
        )
    }
    .map_err(windows_error)?;
    // SAFETY: CreatePipe returned two independently owned handles.
    let read = unsafe { OwnedHandle::from_raw_handle(read.0) };
    // SAFETY: CreatePipe returned two independently owned handles.
    let write = unsafe { OwnedHandle::from_raw_handle(write.0) };
    if parent_writes {
        Ok(PipePair {
            parent: write,
            child: read,
        })
    } else {
        Ok(PipePair {
            parent: read,
            child: write,
        })
    }
}

fn clear_inherit(handle: &OwnedHandle) -> io::Result<()> {
    // SAFETY: The handle is live and this only clears its inheritance flag.
    unsafe {
        SetHandleInformation(
            raw_handle(handle),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAGS::default(),
        )
    }
    .map_err(windows_error)
}

fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: No name or security descriptor is supplied; the returned handle is owned below.
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(windows_error)?;
    // SAFETY: CreateJobObjectW returned one owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(job.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `job` and the correctly sized limit record remain valid through this call.
    unsafe {
        SetInformationJobObject(
            raw_handle(&job),
            JobObjectExtendedLimitInformation,
            ptr::from_ref(&limits).cast::<c_void>(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap_or(u32::MAX),
        )
    }
    .map_err(windows_error)?;
    Ok(job)
}

fn create_process(
    program: &Path,
    arguments: &[OsString],
    stdin: &PipePair,
    stdout: &PipePair,
    stderr: &PipePair,
    helper_job: Option<&OwnedHandle>,
) -> io::Result<PROCESS_INFORMATION> {
    let application = wide_null(program.as_os_str());
    let mut command_line = wide_null(&build_command_line(program.as_os_str(), arguments));
    let environment = encode_environment_block(normalized_environment(std::env::vars_os()));
    // SAFETY: Both Win32 structures are plain data whose documented initial state is zeroed.
    let mut process_info: PROCESS_INFORMATION = unsafe { zeroed() };
    // SAFETY: `cb` and all required fields are initialized immediately below before use.
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap_or(u32::MAX);
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = raw_handle(&stdin.child);
    startup.StartupInfo.hStdOutput = raw_handle(&stdout.child);
    startup.StartupInfo.hStdError = raw_handle(&stderr.child);

    let attribute_count = if helper_job.is_some() { 2 } else { 1 };
    let mut bytes = 0_usize;
    // SAFETY: The documented sizing call writes only the required byte count.
    let _ =
        unsafe { InitializeProcThreadAttributeList(None, attribute_count, None, &raw mut bytes) };
    let mut attribute_storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(attribute_storage.as_mut_ptr().cast::<c_void>());
    // SAFETY: Storage is pointer-aligned and large enough for the size returned above.
    unsafe { InitializeProcThreadAttributeList(Some(list), attribute_count, None, &raw mut bytes) }
        .map_err(windows_error)?;

    let inherited_handles = [
        raw_handle(&stdin.child),
        raw_handle(&stdout.child),
        raw_handle(&stderr.child),
    ];
    // SAFETY: The attribute list and handle array live through CreateProcessW below.
    let handles_result = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            usize::try_from(PROC_THREAD_ATTRIBUTE_HANDLE_LIST).unwrap_or(0),
            Some(inherited_handles.as_ptr().cast::<c_void>()),
            size_of_val(&inherited_handles),
            None,
            None,
        )
    };
    if let Err(error) = handles_result {
        // SAFETY: The attribute list was initialized successfully above.
        unsafe { DeleteProcThreadAttributeList(list) };
        return Err(windows_error(error));
    }

    if let Some(job) = helper_job {
        let job_handle = raw_handle(job);
        // SAFETY: The attribute list and job handle live through CreateProcessW below.
        if let Err(error) = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                usize::try_from(PROC_THREAD_ATTRIBUTE_JOB_LIST).unwrap_or(0),
                Some(ptr::from_ref(&job_handle).cast::<c_void>()),
                size_of::<HANDLE>(),
                None,
                None,
            )
        } {
            // SAFETY: The attribute list was initialized successfully above.
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(windows_error(error));
        }
    }
    startup.lpAttributeList = list;
    let creation_flags =
        CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;

    // SAFETY: All strings, startup data, inherited pipe handles, and attribute-list
    // storage remain valid through this synchronous call. Process/thread handles are returned.
    let result = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            true,
            creation_flags,
            Some(environment.as_ptr().cast::<c_void>()),
            PCWSTR::null(),
            ptr::from_ref(&startup.StartupInfo),
            &raw mut process_info,
        )
    };
    // SAFETY: CreateProcessW has returned and no longer reads the attribute list.
    unsafe { DeleteProcThreadAttributeList(list) };
    result.map_err(windows_error)?;
    Ok(process_info)
}

fn normalized_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut variables: Vec<_> = variables.into_iter().collect();
    if !variables
        .iter()
        .any(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("windir"))
    {
        let system_root = variables
            .iter()
            .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("SystemRoot"))
            .map_or_else(|| OsString::from(r"C:\Windows"), |(_, value)| value.clone());
        variables.push((OsString::from("windir"), system_root));
    }
    variables.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_ascii_lowercase());
    variables
}

fn encode_environment_block(variables: Vec<(OsString, OsString)>) -> Vec<u16> {
    let mut block = Vec::new();
    for (key, value) in variables {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn build_command_line(program: &OsStr, arguments: &[OsString]) -> OsString {
    let mut command = quote_argument(program);
    for argument in arguments {
        command.push(" ");
        command.push(quote_argument(argument));
    }
    command
}

fn quote_argument(argument: &OsStr) -> OsString {
    let text = argument.to_string_lossy();
    if !text.is_empty()
        && !text
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return argument.to_os_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0_usize;
    for character in text.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            backslashes = 0;
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    OsString::from(quoted)
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

#[allow(clippy::needless_pass_by_value)]
fn windows_error(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs::File,
        io::Read,
        mem::size_of,
        path::PathBuf,
        ptr,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use windows::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };

    use super::{
        anonymous_pipe, clear_inherit, create_job, encode_environment_block,
        normalized_environment, raw_handle, spawn_managed_child,
    };

    #[test]
    fn helper_inherits_only_its_declared_standard_streams() {
        let extra_pipe = anonymous_pipe(false).unwrap();
        clear_inherit(&extra_pipe.parent).unwrap();
        let powershell = PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows")),
        )
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
        let child = spawn_managed_child(
            &powershell,
            &[
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from("Start-Sleep -Seconds 5"),
            ],
        )
        .unwrap();
        assert!(child.try_wait().unwrap().is_none());

        drop(extra_pipe.child);
        let (eof_tx, eof_rx) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut pipe = File::from(extra_pipe.parent);
            let mut output = Vec::new();
            let _ = eof_tx.send(pipe.read_to_end(&mut output));
        });
        let eof_before_child_exit = eof_rx.recv_timeout(Duration::from_secs(1));

        let _ = child.terminate();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let reader_deadline = Instant::now() + Duration::from_secs(1);
        while !reader.is_finished() && Instant::now() < reader_deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if reader.is_finished() {
            let _ = reader.join();
        }

        assert!(matches!(eof_before_child_exit, Ok(Ok(0))));
    }

    #[test]
    fn helper_job_terminates_children_when_its_last_handle_closes() {
        let job = create_job().unwrap();
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: The job handle and writable, correctly sized limit record are valid here.
        unsafe {
            QueryInformationJobObject(
                Some(raw_handle(&job)),
                JobObjectExtendedLimitInformation,
                ptr::from_mut(&mut limits).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
                None,
            )
        }
        .unwrap();

        assert_eq!(
            limits.BasicLimitInformation.LimitFlags,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        );
    }

    #[test]
    fn helper_environment_restores_windir_from_system_root() {
        let variables = normalized_environment([
            (OsString::from("TEMP"), OsString::from(r"C:\Temp")),
            (OsString::from("SYSTEMROOT"), OsString::from(r"D:\Windows")),
        ]);

        assert!(variables.iter().any(|(key, value)| {
            key.to_string_lossy().eq_ignore_ascii_case("windir")
                && value == &OsString::from(r"D:\Windows")
        }));
        let block = encode_environment_block(variables);
        assert!(block.ends_with(&[0, 0]));
    }

    #[test]
    fn helper_environment_preserves_an_existing_windir() {
        let variables = normalized_environment([
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (OsString::from("WINDIR"), OsString::from(r"E:\Existing")),
        ]);
        let windir: Vec<_> = variables
            .iter()
            .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("windir"))
            .collect();

        assert_eq!(windir.len(), 1);
        assert_eq!(windir[0].1, OsString::from(r"E:\Existing"));
    }
}
