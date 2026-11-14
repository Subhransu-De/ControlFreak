#![allow(unsafe_code)]

use super::{desktop, environment::ensure_interactive_input_desktop, privilege, shell};
use controlfreak_core::{DisplayInfo, MutationControl, PlatformError, RefusalCause};
use std::{
    cell::RefCell,
    collections::VecDeque,
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows::Win32::UI::WindowsAndMessaging::{GUITHREADINFO, GetGUIThreadInfo};
use windows::Win32::{
    Foundation::{ERROR_SUCCESS, FILETIME, GetLastError, HANDLE, HWND, SetLastError},
    System::{
        Com::CoCreateGuid,
        SystemInformation::GetSystemWindowsDirectoryW,
        Threading::{
            GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        WindowsAndMessaging::{
            EVENT_OBJECT_DESTROY, EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND,
            FindWindowExW, GA_ROOT, GA_ROOTOWNER, GW_OWNER, GetAncestor, GetClassNameW,
            GetMessageW, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindow, MSG,
            OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WindowFromPoint,
        },
    },
};
use windows::core::PWSTR;

const MAX_REFERENCES: usize = 512;
const REFERENCE_TTL: Duration = Duration::from_mins(5);

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    hwnd: usize,
    pid: u32,
    thread: u32,
    created: u64,
    owner: usize,
    class: String,
    /// The virtual desktop, or `shell::ALL_DESKTOPS` for a verified shell surface.
    desktop: String,
    /// The owning process hosts unrelated applications (see `shell::is_shared_host_image`), or
    /// its image cannot be read, so "same process" does not mean "same application".
    shell_host: bool,
    /// The owning process is `ApplicationFrameHost.exe`, so this window is the frame of a
    /// packaged app whose content window belongs to the app's own process.
    app_frame_host: bool,
}

#[derive(Clone)]
struct Reference {
    id: String,
    identity: Identity,
    bounds: [i32; 4],
    displays: Vec<DisplayInfo>,
    issued: Instant,
    armed: bool,
}

/// What the same-process rule needs to know about one window.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowFacts {
    hwnd: usize,
    pid: u32,
    class: String,
    /// The window at the top of the owner chain; the window itself if it is unowned.
    root_owner: usize,
}

/// Class of the popup menus that Windows creates in the process that opens them.
const POPUP_MENU_CLASS: &str = "#32768";

/// Whether `window` may receive the session's input in place of the approved `target`.
///
/// The target itself always may. A window of the same process may too: the dialogs, popup menus
/// and drop-downs an approved application opens are part of that application, and refusing them
/// would make menus unreachable by pointer and every dialog end the session. Another process
/// never may, whatever its window looks like. The checks on the actual window (integrity, and
/// for pointer input the destination point) run separately and are unchanged.
///
/// Some processes are an exception to "same process is the same application": `explorer.exe`
/// owns the taskbar, the desktop and every File Explorer window, and `ApplicationFrameHost.exe`
/// owns the frame of every packaged app. For a target in such a shared host, and for a target
/// whose process image cannot be read, only windows in the target's owner chain and popup menus
/// count as part of it, so approving one File Explorer window does not approve the taskbar or
/// another folder window, and approving one packaged app does not approve another's frame.
fn accepts_same_process(target: &WindowFacts, shell_host: bool, window: &WindowFacts) -> bool {
    if window.hwnd == target.hwnd {
        return true;
    }
    if window.pid != target.pid {
        return false;
    }
    if !shell_host {
        return true;
    }
    window.root_owner == target.root_owner || window.class == POPUP_MENU_CLASS
}

/// A refused validation. Only a failure that cannot heal poisons the session's target; a
/// foreground or destination that is wrong right now only refuses this action.
#[derive(Debug)]
enum Failure {
    /// The target changed identity, geometry or desktop, or its own integrity cannot be
    /// verified, or the input desktop is not usable.
    Fatal(PlatformError),
    /// The foreground window, the keyboard focus or the pointer destination is wrong. The target
    /// is unchanged and the next action checks again, so a window that comes back is usable
    /// without a new session. The error keeps its own code (a higher-integrity foreground is
    /// still `higher_integrity_target`); it just does not end the session.
    Recoverable(PlatformError),
}

impl From<PlatformError> for Failure {
    fn from(error: PlatformError) -> Self {
        Self::Fatal(error)
    }
}

/// Marks a failure about a window other than the target as one that only refuses this action.
fn recoverable<T>(result: Result<T, PlatformError>) -> Result<T, Failure> {
    result.map_err(Failure::Recoverable)
}

/// The window that would receive a pointer event: the root of the window under the point, and
/// the process of the window itself (a child can belong to another process than its root).
struct Hit {
    root: WindowFacts,
    window_pid: u32,
    /// For a packaged-app frame target: the process of the content window hosted inside the
    /// target's own frame, read when the check runs. `None` for every other target.
    hosted_pid: Option<u32>,
}

impl Hit {
    /// Whether the window that would get the input belongs to the target's application. Its
    /// process is the target's, or it is the content window of the packaged app that the
    /// target's `ApplicationFrameHost.exe` frame displays: the frame and the content are one
    /// application even though they run in two processes. Only a window inside that very frame
    /// qualifies, and the integrity of the window actually hit is checked separately.
    fn belongs_to(&self, target: &Identity, target_window: &WindowFacts) -> bool {
        self.window_pid == target.pid
            || (target.app_frame_host
                && self.hosted_pid == Some(self.window_pid)
                && self.root.hwnd == target_window.hwnd)
    }
}

impl Reference {
    fn validate_snapshot(
        &self,
        current: &Identity,
        bounds: [i32; 4],
        displays: &[DisplayInfo],
        target: &WindowFacts,
        foreground: &WindowFacts,
        hit: Option<&Hit>,
    ) -> Result<(), Failure> {
        if &self.identity != current {
            return Err(Failure::Fatal(invalid(
                "window, process lifetime, owner, or desktop changed",
            )));
        }
        if self.bounds != bounds || self.displays != displays {
            return Err(Failure::Fatal(invalid(
                "window bounds or display layout changed; end the session and observe again",
            )));
        }
        let shell_host = current.shell_host;
        if !accepts_same_process(target, shell_host, foreground) {
            return Err(Failure::Recoverable(refused(
                RefusalCause::NotForeground,
                "approved target is no longer foreground; call focus_window to bring it back, or a window of another process now owns the foreground",
            )));
        }
        if let Some(hit) = hit
            && (!hit.belongs_to(current, target)
                || !accepts_same_process(target, shell_host, &hit.root))
        {
            return Err(Failure::Recoverable(destination_refusal(hit)));
        }
        Ok(())
    }
}

static REFERENCES: Mutex<VecDeque<Reference>> = Mutex::new(VecDeque::new());
static WATCHING: AtomicBool = AtomicBool::new(false);
static OBSERVATION_GENERATION: AtomicU64 = AtomicU64::new(0);

pub(super) fn invalid(reason: &str) -> PlatformError {
    PlatformError::TargetInvalidated {
        reason: reason.into(),
    }
}

pub(super) fn refused(cause: RefusalCause, reason: &str) -> PlatformError {
    PlatformError::refused(cause, reason)
}

/// Why a pointer or keyboard destination in another process is refused. A window of this very
/// process is the server's own indicator, which screenshots never show, so the message says
/// so instead of blaming an unnamed other process.
fn destination_refusal(hit: &Hit) -> PlatformError {
    if hit.window_pid == std::process::id() {
        return refused(
            RefusalCause::OwnIndicator,
            "this point is under ControlFreak's own indicator tab or Stop button, which screenshots do not show; choose another point or use the keyboard",
        );
    }
    refused(
        RefusalCause::ForeignDestination,
        "input destination belongs to a window of a different process; only the approved window and windows of its own process (its dialogs and popups) receive input",
    )
}

fn unavailable(reason: impl std::fmt::Display) -> PlatformError {
    PlatformError::Unavailable {
        reason: format!("target references unavailable: {reason}"),
    }
}

fn identity(hwnd: HWND) -> Result<Identity, PlatformError> {
    let _timer = controlfreak_core::timing::start("identity");
    let mut pid = 0;
    // SAFETY: These queries retain no pointers; pid is writable for the synchronous call.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if thread == 0 || pid == 0 {
        return Err(invalid("window identity is unavailable"));
    }
    // SAFETY: Query-only process handle is immediately wrapped in an owning RAII handle.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|_| invalid("process lifetime cannot be verified"))?;
    // SAFETY: OpenProcess returned an owned, closeable handle.
    let _owned = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: The process handle and four writable FILETIME records outlive the call.
    unsafe {
        GetProcessTimes(
            process,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|_| invalid("process lifetime cannot be verified"))?;
    if exited.dwLowDateTime != 0 || exited.dwHighDateTime != 0 {
        return Err(invalid("approved process exited"));
    }
    let mut class = [0_u16; 256];
    // SAFETY: class is writable and GetClassNameW does not retain the buffer.
    let length = unsafe { GetClassNameW(hwnd, &mut class) };
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length > 0)
        .ok_or_else(|| invalid("window class cannot be verified"))?;
    let owner = window_owner(hwnd)?;
    let class = String::from_utf16_lossy(&class[..length]);
    let image = process_image(process);
    let windows_directory = windows_directory();
    // Failing to read the image must narrow the same-process rule, never widen it.
    let shell_host = is_shared_host(image.as_deref(), windows_directory.as_deref());
    let app_frame_host = matches!(
        (&image, &windows_directory),
        (Some(image), Some(directory)) if shell::is_app_frame_host_image(image, directory)
    );
    let desktop = controlfreak_core::timing::time("identity_desktop", || {
        desktop::with_virtual_desktop_manager(|manager| desktop::window_desktop_id(manager, hwnd))
    })
    .map_err(unavailable)?;
    let desktop = match desktop {
        Ok(desktop) => format!("{desktop:?}"),
        // The shell's own windows have no desktop identity because they are on every desktop.
        // That exemption needs the class and the owning image together; a failure for any other
        // window still means its desktop cannot be verified.
        Err(_)
            if matches!(
                (&image, &windows_directory),
                (Some(image), Some(directory)) if shell::is_shell_surface(&class, image, directory)
            ) =>
        {
            shell::ALL_DESKTOPS.to_owned()
        }
        Err(_) => return Err(invalid("window desktop cannot be verified")),
    };
    Ok(Identity {
        hwnd: hwnd.0 as usize,
        pid,
        thread,
        created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        owner: owner.0 as usize,
        class,
        desktop,
        shell_host,
        app_frame_host,
    })
}

/// Whether the target's process hosts unrelated applications. A process whose image or whose
/// Windows directory cannot be read counts as one: failing to know narrows the same-process
/// rule to the owner chain instead of widening it to everything the process owns.
fn is_shared_host(image: Option<&str>, windows_directory: Option<&str>) -> bool {
    match (image, windows_directory) {
        (Some(image), Some(directory)) => shell::is_shared_host_image(image, directory),
        _ => true,
    }
}

/// The Win32 path of the executable a process runs, or `None` if it cannot be read, in which
/// case no shell exemption applies.
fn process_image(process: HANDLE) -> Option<String> {
    let mut buffer = [0_u16; 1024];
    let mut length = u32::try_from(buffer.len()).ok()?;
    // SAFETY: `process` is a live handle with query rights; the buffer is writable for `length`
    // units, and `length` is updated to the count written, excluding the terminator.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(
        &buffer[..usize::try_from(length).ok()?],
    ))
}

/// The Windows directory, read from the system rather than from an environment variable that
/// the user's process environment could change.
fn windows_directory() -> Option<String> {
    static DIRECTORY: OnceLock<Option<String>> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            let mut buffer = [0_u16; 512];
            // SAFETY: The buffer is writable for its full length; the call returns the number of
            // units written excluding the terminator, or zero on failure.
            let length = unsafe { GetSystemWindowsDirectoryW(Some(&mut buffer)) };
            let length = usize::try_from(length).ok()?;
            (length > 0 && length < buffer.len())
                .then(|| String::from_utf16_lossy(&buffer[..length]))
        })
        .clone()
}

fn window_facts(hwnd: HWND) -> Result<WindowFacts, PlatformError> {
    let mut pid = 0;
    // SAFETY: pid is writable and the query retains no pointers.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if thread == 0 || pid == 0 {
        return Err(invalid("window identity is unavailable"));
    }
    let mut class = [0_u16; 256];
    // SAFETY: class is writable and GetClassNameW does not retain the buffer.
    let length = unsafe { GetClassNameW(hwnd, &mut class) };
    let length = usize::try_from(length).unwrap_or(0);
    // SAFETY: GetAncestor reads window metadata for a borrowed handle.
    let root_owner = unsafe { GetAncestor(hwnd, GA_ROOTOWNER) };
    Ok(WindowFacts {
        hwnd: hwnd.0 as usize,
        pid,
        class: String::from_utf16_lossy(&class[..length]),
        root_owner: if root_owner.is_invalid() {
            hwnd.0 as usize
        } else {
            root_owner.0 as usize
        },
    })
}

fn window_owner(hwnd: HWND) -> Result<HWND, PlatformError> {
    // SAFETY: Last-error state is thread-local; GetWindow only reads a borrowed handle.
    // A null result with no error means a valid unowned top-level window.
    let (owner, error) = unsafe {
        SetLastError(ERROR_SUCCESS);
        let owner = GetWindow(hwnd, GW_OWNER);
        (owner, GetLastError())
    };
    match owner {
        Ok(owner) => Ok(owner),
        Err(_) if error == ERROR_SUCCESS => Ok(HWND::default()),
        Err(_) => Err(invalid("window ownership cannot be verified")),
    }
}

fn bounds(hwnd: HWND) -> Result<[i32; 4], PlatformError> {
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: rect is writable and the synchronous call retains no references.
    unsafe { GetWindowRect(hwnd, &raw mut rect) }
        .map_err(|_| invalid("window bounds are unavailable"))?;
    Ok([rect.left, rect.top, rect.right, rect.bottom])
}

// Out-of-context WinEvents require a message pump on the installing thread.
// Destroy events retire references even if Windows reuses a handle within the same process.
unsafe extern "system" fn destroyed(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND || event == EVENT_SYSTEM_DESKTOPSWITCH {
        OBSERVATION_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
    if event == EVENT_SYSTEM_DESKTOPSWITCH {
        REFERENCES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    // A foreground change only advances the observation generation above. It deliberately keeps
    // every reference: the session's target is re-checked against the live foreground window at
    // each action, so a window that took the foreground briefly cannot widen what input may reach,
    // and a window that gives it back does not cost the session its approval.
    } else if event == EVENT_OBJECT_DESTROY && object == OBJID_WINDOW.0 && child == 0 {
        OBSERVATION_GENERATION.fetch_add(1, Ordering::AcqRel);
        retire_window(
            &mut REFERENCES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            hwnd.0 as usize,
        );
    }
}

fn retire_window(references: &mut VecDeque<Reference>, hwnd: usize) {
    if hwnd != 0 {
        references.retain(|reference| {
            reference.identity.hwnd != hwnd && reference.identity.owner != hwnd
        });
    }
}

pub(super) fn ensure_available() -> Result<(), PlatformError> {
    static WATCHER: OnceLock<Result<(), String>> = OnceLock::new();
    WATCHER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("window-lifetimes".into())
                .spawn(move || {
                    // SAFETY: The static callback remains valid until unhooked on this thread.
                    let hook = unsafe {
                        SetWinEventHook(
                            EVENT_OBJECT_DESTROY,
                            EVENT_OBJECT_DESTROY,
                            None,
                            Some(destroyed),
                            0,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };
                    if hook.is_invalid() {
                        let _ = sender.send(Err("window lifetime watcher unavailable".into()));
                        return;
                    }
                    // SAFETY: Same static callback and installing-thread lifetime as the destroy hook.
                    let focus_hook = unsafe {
                        SetWinEventHook(
                            EVENT_SYSTEM_FOREGROUND,
                            EVENT_SYSTEM_DESKTOPSWITCH,
                            None,
                            Some(destroyed),
                            0,
                            0,
                            WINEVENT_OUTOFCONTEXT,
                        )
                    };
                    WATCHING.store(!focus_hook.is_invalid(), Ordering::Release);
                    let ready = if focus_hook.is_invalid() {
                        Err("desktop watcher unavailable".into())
                    } else {
                        Ok(())
                    };
                    if sender.send(ready).is_ok() && WATCHING.load(Ordering::Acquire) {
                        let mut message = MSG::default();
                        // SAFETY: message is writable throughout the pump. WinEvents are dispatched by GetMessageW.
                        while unsafe { GetMessageW(&raw mut message, None, 0, 0) }.0 > 0 {}
                    }
                    // SAFETY: hook was installed by this thread and is unhooked exactly once.
                    let _ = unsafe { UnhookWinEvent(hook) };
                    if !focus_hook.is_invalid() {
                        // SAFETY: This thread owns the successfully installed hook and unhooks once.
                        let _ = unsafe { UnhookWinEvent(focus_hook) };
                    }
                    WATCHING.store(false, Ordering::Release);
                    REFERENCES
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clear();
                })
                .map_err(|error| error.to_string())?;
            receiver
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| error.to_string())?
        })
        .clone()
        .map_err(unavailable)?;
    if WATCHING.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(unavailable("window lifetime watcher stopped"))
    }
}

pub(super) fn issue(hwnd: HWND) -> Result<String, PlatformError> {
    let _timer = controlfreak_core::timing::start("issue_reference");
    ensure_available()?;
    let generation = OBSERVATION_GENERATION.load(Ordering::Acquire);
    let identity = identity(hwnd)?;
    let bounds = bounds(hwnd)?;
    let displays = desktop::enumerate_displays().map_err(unavailable)?;
    let mut references = REFERENCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !WATCHING.load(Ordering::Acquire) {
        return Err(unavailable("window lifetime watcher stopped"));
    }
    if OBSERVATION_GENERATION.load(Ordering::Acquire) != generation {
        return Err(invalid("window state changed during observation"));
    }
    references.retain(|reference| reference.issued.elapsed() < REFERENCE_TTL);
    if let Some(id) = refresh_reference(
        &mut references,
        &identity,
        bounds,
        &displays,
        Instant::now(),
    ) {
        return Ok(id);
    }
    // SAFETY: CoCreateGuid takes no borrowed input and the generated value is returned by value.
    let guid = unsafe { CoCreateGuid() }.map_err(unavailable)?;
    let id = format!("target-{guid:?}");
    evict_for_capacity(&mut references);
    references.push_back(Reference {
        id: id.clone(),
        identity,
        bounds,
        displays,
        issued: Instant::now(),
        armed: false,
    });
    Ok(id)
}

// Observation churn must not evict a reference that an active session has armed.
fn evict_for_capacity(references: &mut VecDeque<Reference>) {
    while references.len() >= MAX_REFERENCES {
        let index = references
            .iter()
            .position(|reference| !reference.armed)
            .unwrap_or(0);
        references.remove(index);
    }
}

fn refresh_reference(
    references: &mut VecDeque<Reference>,
    identity: &Identity,
    bounds: [i32; 4],
    displays: &[DisplayInfo],
    now: Instant,
) -> Option<String> {
    let index = references.iter().position(|reference| {
        &reference.identity == identity
            && reference.bounds == bounds
            && reference.displays == displays
    })?;
    let mut reference = references.remove(index)?;
    reference.issued = now;
    let id = reference.id.clone();
    references.push_back(reference);
    Some(id)
}

fn lookup(id: &str) -> Result<Reference, PlatformError> {
    if !WATCHING.load(Ordering::Acquire) {
        return Err(invalid("window lifetime watcher is unavailable"));
    }
    REFERENCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|reference| reference.id == id && reference.issued.elapsed() < REFERENCE_TTL)
        .cloned()
        .ok_or_else(|| invalid("unknown, expired, or destroyed target reference; observe again"))
}

pub(super) fn resolve(id: &str) -> Result<(HWND, u32), PlatformError> {
    let reference = lookup(id)?;
    let hwnd = HWND(reference.identity.hwnd as *mut std::ffi::c_void);
    // SAFETY: IsWindow only inspects a borrowed handle.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || identity(hwnd)? != reference.identity {
        REFERENCES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| entry.id != id);
        return Err(invalid(
            "window, process lifetime, owner, or desktop changed",
        ));
    }
    Ok((hwnd, reference.identity.pid))
}

/// Marks the reference as in use by a session when the target, or a window of its own process,
/// holds the foreground. An armed reference is the last to be evicted. Not holding the
/// foreground neither arms nor retires it; each action decides again from the live foreground.
pub(super) fn arm_if_foreground(id: &str) -> Result<(), PlatformError> {
    let snapshot = REFERENCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|reference| reference.id == id)
        .cloned()
        .ok_or_else(|| invalid("approved target reference was retired"))?;
    if foreground_serves(&snapshot.identity)
        && let Some(reference) = REFERENCES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
            .find(|reference| reference.id == id)
    {
        reference.armed = true;
    }
    Ok(())
}

/// Whether `window` is the approved target of reference `approved_id` or a window of its own
/// process that may stand in for it (an owned dialog, a popup), by the same rule as the
/// foreground and pointer checks. An unreadable window or reference is not.
pub(super) fn window_serves_target(approved_id: &str, window: HWND) -> bool {
    let Ok(reference) = lookup(approved_id) else {
        return false;
    };
    let target = HWND(reference.identity.hwnd as *mut std::ffi::c_void);
    let (Ok(target), Ok(window)) = (window_facts(target), window_facts(window)) else {
        return false;
    };
    accepts_same_process(&target, reference.identity.shell_host, &window)
}

/// Whether the live foreground window is the target or a same-process window that may stand in
/// for it. An unreadable foreground or target is not.
fn foreground_serves(identity: &Identity) -> bool {
    let target = HWND(identity.hwnd as *mut std::ffi::c_void);
    let foreground = desktop::foreground_window_handle();
    if foreground.is_invalid() {
        return false;
    }
    let (Ok(target), Ok(foreground)) = (window_facts(target), window_facts(foreground)) else {
        return false;
    };
    accepts_same_process(&target, identity.shell_host, &foreground)
}

pub(super) fn display(
    control: &MutationControl,
    display_id: &str,
) -> Result<DisplayInfo, PlatformError> {
    let id = control
        .approved_target()
        .ok_or_else(|| invalid("an approved target reference is required"))?;
    lookup(&id)?
        .displays
        .into_iter()
        .find(|display| display.id.eq_ignore_ascii_case(display_id))
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "display_id".into(),
            reason: "display is not part of the approved observation".into(),
        })
}

pub(super) fn validate(
    control: &MutationControl,
    operation: &str,
    point: Option<windows::Win32::Foundation::POINT>,
) -> Result<(), PlatformError> {
    let _timer = controlfreak_core::timing::start("validate_target");
    control.check(operation)?;
    control.check_target()?;
    let id = control
        .approved_target()
        .ok_or_else(|| invalid("an approved target reference is required"))?;
    let result = validate_reference(&id, operation, point);
    match result {
        Ok(()) => Ok(()),
        Err(Failure::Recoverable(error)) => Err(error),
        Err(Failure::Fatal(error)) => {
            control.invalidate_target();
            Err(error)
        }
    }
}

fn validate_reference(
    id: &str,
    operation: &str,
    point: Option<windows::Win32::Foundation::POINT>,
) -> Result<(), Failure> {
    controlfreak_core::timing::time("check_input_desktop", || {
        ensure_interactive_input_desktop(operation)
    })?;
    let hwnd = HWND(lookup(id)?.identity.hwnd as *mut std::ffi::c_void);
    let current_identity = cached_identity(hwnd)?;
    let reference = lookup(id)?;
    // SAFETY: IsWindow only inspects a borrowed handle.
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || current_identity != reference.identity {
        REFERENCES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| entry.id != id);
        return Err(Failure::Fatal(invalid(
            "window, process lifetime, owner, or desktop changed",
        )));
    }
    let foreground = desktop::foreground_window_handle();
    // Integrity is checked on the actual windows involved: the foreground window, which may be
    // a dialog or popup of the target's process rather than the target, and the target itself.
    // A problem with the foreground window only refuses this action (the target is unchanged and
    // the foreground can come back); a problem with the target itself ends the session.
    if foreground.is_invalid() {
        return Err(Failure::Recoverable(refused(
            RefusalCause::NotForeground,
            "no window holds the foreground right now; call focus_window to bring the target back",
        )));
    }
    controlfreak_core::timing::time("check_integrity", || {
        recoverable(cached_integrity(operation, foreground))?;
        cached_integrity(operation, hwnd).map_err(Failure::Fatal)
    })?;
    // A shell surface is on every virtual desktop and has no desktop query result to compare.
    if reference.identity.desktop != shell::ALL_DESKTOPS {
        let on_current = controlfreak_core::timing::time("check_virtual_desktop", || {
            desktop::with_virtual_desktop_manager(|manager| {
                desktop::window_on_current_desktop(manager, hwnd)
            })
        })?
        .unwrap_or(false);
        if !on_current {
            return Err(Failure::Fatal(invalid(
                "approved target left the current desktop",
            )));
        }
    }
    let current_bounds = bounds(hwnd)?;
    let displays =
        controlfreak_core::timing::time("enumerate_displays", desktop::enumerate_displays)?;
    let target = window_facts(hwnd)?;
    let foreground_facts = recoverable(window_facts(foreground).map_err(|_| {
        refused(
            RefusalCause::NotForeground,
            "the foreground window cannot be identified; call focus_window to bring the target back",
        )
    }))?;
    let mut hit = destination_hit(operation, point)?;
    if let Some(hit) = &mut hit
        && reference.identity.app_frame_host
    {
        hit.hosted_pid = hosted_content_pid(hwnd);
    }
    reference.validate_snapshot(
        &current_identity,
        current_bounds,
        &displays,
        &target,
        &foreground_facts,
        hit.as_ref(),
    )?;
    // A final foreground read follows the slower process, COM, and display queries. If a window
    // appeared in between, it must pass the same integrity and process checks as the first.
    let after = desktop::foreground_window_handle();
    if after != foreground {
        recoverable(cached_integrity(operation, after))?;
        let after_facts = recoverable(window_facts(after).map_err(|_| {
            refused(
                RefusalCause::ForegroundChanged,
                "foreground changed during target validation",
            )
        }))?;
        if !accepts_same_process(&target, reference.identity.shell_host, &after_facts) {
            return Err(Failure::Recoverable(refused(
                RefusalCause::ForegroundChanged,
                "foreground changed during target validation",
            )));
        }
    }
    Ok(arm_if_foreground(id)?)
}

/// The window that would receive this action's input: the one under the pointer, or the one
/// with keyboard focus for keyboard operations, after its integrity check. `None` for operations
/// that deliver no input to a destination.
fn destination_hit(
    operation: &str,
    point: Option<windows::Win32::Foundation::POINT>,
) -> Result<Option<Hit>, Failure> {
    Ok(if let Some(point) = point {
        // SAFETY: These APIs inspect value coordinates and return borrowed window handles.
        let hit = unsafe { WindowFromPoint(point) };
        recoverable(privilege::ensure_point_integrity(operation, point))?;
        Some(hit_facts(hit)?)
    } else if matches!(
        operation,
        "press_keys" | "type_text" | "switch_virtual_desktop"
    ) {
        let mut gui = GUITHREADINFO {
            cbSize: u32::try_from(std::mem::size_of::<GUITHREADINFO>())
                .map_err(|_| invalid("GUI thread record size is unavailable"))?,
            ..Default::default()
        };
        // SAFETY: gui is a correctly sized writable record; zero queries the foreground thread.
        unsafe { GetGUIThreadInfo(0, &raw mut gui) }.map_err(|_| {
            Failure::Recoverable(refused(
                RefusalCause::DestinationUnidentified,
                "keyboard focus cannot be verified; observe again",
            ))
        })?;
        recoverable(privilege::ensure_window_integrity(operation, gui.hwndFocus))?;
        Some(hit_facts(gui.hwndFocus)?)
    } else {
        None
    })
}

/// The window an input event would reach, as the root window and the process of the window itself.
/// A window that cannot be read is treated as belonging to a different process.
fn hit_facts(window: HWND) -> Result<Hit, Failure> {
    let unreadable = || {
        Failure::Recoverable(refused(
            RefusalCause::DestinationUnidentified,
            "the input destination window cannot be identified; observe again",
        ))
    };
    if window.is_invalid() {
        return Err(unreadable());
    }
    // SAFETY: GetAncestor reads metadata for the borrowed window handle.
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    let root = if root.is_invalid() { window } else { root };
    let window = window_facts(window).map_err(|_| unreadable())?;
    let root = window_facts(root).map_err(|_| unreadable())?;
    Ok(Hit {
        root,
        window_pid: window.pid,
        hosted_pid: None,
    })
}

/// The process of the content window a packaged app hosts inside `frame`, if it has one.
fn hosted_content_pid(frame: HWND) -> Option<u32> {
    let class = windows::core::HSTRING::from(shell::core_window_class());
    // SAFETY: FindWindowExW only reads window metadata; `frame` is a handle value, a stale one
    // simply finds nothing, and `class` outlives the synchronous call.
    let content = unsafe { FindWindowExW(Some(frame), None, &class, None) }.ok()?;
    let mut pid = 0;
    // SAFETY: `pid` is writable for the synchronous call.
    unsafe { GetWindowThreadProcessId(content, Some(&raw mut pid)) };
    (pid != 0).then_some(pid)
}

/// Identity and integrity facts that cannot change within one input lock's scope in a way the
/// per-frame checks would not catch. Validation runs for every frame of a timed move and every
/// batch of typed text, and each identity query opens the process and asks the shell's virtual
/// desktop manager, so those two results are reused for a short time inside one input lock.
/// Foreground, hit-window, desktop-membership, bounds and display checks are never cached.
const SCOPE_TTL: Duration = Duration::from_millis(250);

#[derive(Default)]
struct ScopeCache {
    identity: Option<(usize, Identity, Instant)>,
    integrity: Vec<(usize, u32, Instant)>,
}

impl ScopeCache {
    fn fresh(at: Instant, now: Instant) -> bool {
        now.saturating_duration_since(at) < SCOPE_TTL
    }

    fn identity(&self, hwnd: usize, now: Instant) -> Option<Identity> {
        self.identity
            .as_ref()
            .filter(|(cached, _, at)| *cached == hwnd && Self::fresh(*at, now))
            .map(|(_, identity, _)| identity.clone())
    }

    fn integrity_verified(&self, hwnd: usize, pid: u32, now: Instant) -> bool {
        self.integrity
            .iter()
            .any(|(cached, owner, at)| *cached == hwnd && *owner == pid && Self::fresh(*at, now))
    }

    fn remember_integrity(&mut self, hwnd: usize, pid: u32, now: Instant) {
        self.integrity.retain(|(_, _, at)| Self::fresh(*at, now));
        self.integrity.push((hwnd, pid, now));
    }
}

thread_local! {
    static SCOPE: RefCell<Option<ScopeCache>> = const { RefCell::new(None) };
}

/// Enables the validation cache on this thread until dropped. Held by the input lock, so the
/// cache never outlives one action's exclusive use of the desktop and is never shared by threads.
pub(super) struct ValidationScope;

impl ValidationScope {
    pub(super) fn enter() -> Self {
        SCOPE.with(|scope| *scope.borrow_mut() = Some(ScopeCache::default()));
        Self
    }
}

impl Drop for ValidationScope {
    fn drop(&mut self) {
        SCOPE.with(|scope| *scope.borrow_mut() = None);
    }
}

fn cached_identity(hwnd: HWND) -> Result<Identity, PlatformError> {
    let now = Instant::now();
    let key = hwnd.0 as usize;
    if let Some(identity) = SCOPE.with(|scope| {
        scope
            .borrow()
            .as_ref()
            .and_then(|cache| cache.identity(key, now))
    }) {
        return Ok(identity);
    }
    let identity = identity(hwnd)?;
    SCOPE.with(|scope| {
        if let Some(cache) = scope.borrow_mut().as_mut() {
            cache.identity = Some((key, identity.clone(), now));
        }
    });
    Ok(identity)
}

/// `privilege::ensure_window_integrity`, remembered per window and process within a scope.
/// Only a pass is remembered; a refusal is always recomputed.
fn cached_integrity(operation: &str, hwnd: HWND) -> Result<(), PlatformError> {
    let now = Instant::now();
    let key = hwnd.0 as usize;
    let mut pid = 0;
    // SAFETY: pid is writable and the query retains no pointers; an invalid handle reports zero.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if pid != 0
        && SCOPE.with(|scope| {
            scope
                .borrow()
                .as_ref()
                .is_some_and(|cache| cache.integrity_verified(key, pid, now))
        })
    {
        return Ok(());
    }
    privilege::ensure_window_integrity(operation, hwnd)?;
    if pid != 0 {
        SCOPE.with(|scope| {
            if let Some(cache) = scope.borrow_mut().as_mut() {
                cache.remember_integrity(key, pid, now);
            }
        });
    }
    Ok(())
}

/// Screen bounds of the approved window as observed when its reference was issued.
pub(super) fn approved_window_bounds(
    control: &MutationControl,
) -> Option<controlfreak_core::DisplayBounds> {
    let reference = lookup(&control.approved_target()?).ok()?;
    let [left, top, right, bottom] = reference.bounds;
    Some(controlfreak_core::DisplayBounds {
        left,
        top,
        width: u32::try_from(right.checked_sub(left)?).ok()?,
        height: u32::try_from(bottom.checked_sub(top)?).ok()?,
    })
}

/// Records after an action whether the approved target, or a window of its own process, still
/// holds the foreground. A target that is gone or changed identity ends the session's approval;
/// a target that merely lost the foreground to another process does not, because the next action
/// checks the live foreground again.
pub(super) fn evidence(control: &MutationControl) {
    let approved = control.approved_target();
    let target = approved.as_deref().and_then(|id| lookup(id).ok());
    let intact = approved.as_deref().is_some_and(|id| resolve(id).is_ok());
    let remained = intact
        && target
            .as_ref()
            .is_some_and(|reference| foreground_serves(&reference.identity));
    control.target_foreground(remained);
    if !intact {
        control.invalidate_target();
    }
}

/// Whether an observation's `target_ref` names the approved target or a window that may stand
/// in for it (a same-process dialog or popup that was in front during the observation).
pub(super) fn observation_matches_approved(approved: &str, observed: Option<&str>) -> bool {
    let Some(observed) = observed else {
        return false;
    };
    if observed == approved {
        return true;
    }
    let (Ok(approved), Ok(observed)) = (lookup(approved), lookup(observed)) else {
        return false;
    };
    let (Ok(target), Ok(window)) = (
        window_facts(HWND(approved.identity.hwnd as *mut std::ffi::c_void)),
        window_facts(HWND(observed.identity.hwnd as *mut std::ffi::c_void)),
    ) else {
        return false;
    };
    accepts_same_process(&target, approved.identity.shell_host, &window)
}

/// Counts foreground changes, window destruction and desktop switches. Equal values at two
/// moments mean none of those happened between them.
pub(super) fn observation_generation() -> u64 {
    OBSERVATION_GENERATION.load(Ordering::Acquire)
}

pub(super) fn observe() -> Option<(String, u64)> {
    let generation = OBSERVATION_GENERATION.load(Ordering::Acquire);
    Some((issue(desktop::foreground_window_handle()).ok()?, generation))
}

pub(super) fn finish_observation(before: Option<(String, u64)>) -> Option<String> {
    let before = before?;
    let after = observe()?;
    (before == after).then_some(before.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_owner_queries_accept_unowned_and_owned_fixture_windows() {
        use windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_POPUP,
            },
            core::w,
        };
        struct Fixture(HWND);
        impl Drop for Fixture {
            fn drop(&mut self) {
                // SAFETY: The fixture owns this window, created and destroyed on this thread.
                let _ = unsafe { DestroyWindow(self.0) };
            }
        }
        let create = |owner| {
            // SAFETY: STATIC is a system class; strings are static, no borrowed creation data.
            // The hidden window belongs to this test thread and never receives injected input.
            Fixture(
                unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        w!("ControlFreak target fixture"),
                        WS_POPUP,
                        0,
                        0,
                        10,
                        10,
                        owner,
                        None,
                        None,
                        None,
                    )
                }
                .unwrap(),
            )
        };
        let parent = create(None);
        assert!(window_owner(parent.0).unwrap().is_invalid());
        let owned = create(Some(parent.0));
        assert_eq!(window_owner(owned.0).unwrap(), parent.0);
    }

    fn reference() -> Reference {
        Reference {
            id: "synthetic-reference".into(),
            identity: Identity {
                hwnd: 1,
                pid: 2,
                thread: 3,
                created: 4,
                owner: 0,
                class: "fixture".into(),
                desktop: "desktop-a".into(),
                shell_host: false,
                app_frame_host: false,
            },
            bounds: [-1920, -200, -920, 800],
            displays: vec![],
            issued: Instant::now(),
            armed: false,
        }
    }

    #[test]
    fn refuses_restarts_reused_handles_ownership_desktop_and_geometry_changes() {
        let expected = reference();
        for change in 0..9 {
            let mut current = expected.clone();
            match change {
                0 => current.identity.hwnd += 1,
                1 => current.identity.pid += 1,
                2 => current.identity.created += 1,
                3 => current.identity.thread += 1,
                4 => current.identity.owner += 1,
                5 => current.identity.class = "replacement".into(),
                6 => current.identity.desktop = "desktop-b".into(),
                7 => current.bounds[0] += 1,
                _ => current.displays.push(DisplayInfo {
                    id: "new".into(),
                    name: "synthetic".into(),
                    bounds: controlfreak_core::DisplayBounds {
                        left: 0,
                        top: 0,
                        width: 100,
                        height: 100,
                    },
                    is_primary: true,
                }),
            }
            // Every one of these is permanent: the session's target is gone or moved.
            assert!(
                matches!(
                    expected.validate_snapshot(
                        &current.identity,
                        current.bounds,
                        &current.displays,
                        &target_facts(),
                        &target_facts(),
                        None
                    ),
                    Err(Failure::Fatal(_))
                ),
                "change {change}"
            );
        }
    }

    #[test]
    fn fresh_observations_renew_and_promote_only_compatible_references() {
        let observed = reference();
        let fresh = (observed.issued + REFERENCE_TTL)
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        let mut cached = VecDeque::from([observed.clone()]);
        for index in 1..MAX_REFERENCES {
            let mut other = reference();
            other.id = format!("other-{index}");
            other.identity.hwnd = index + 1;
            cached.push_back(other);
        }
        assert_eq!(
            refresh_reference(&mut cached, &observed.identity, observed.bounds, &[], fresh),
            Some(observed.id.clone())
        );
        // The next capacity eviction must discard an older observation, not the renewed one.
        cached.pop_front();
        assert_eq!(cached.back().unwrap().id, observed.id);
        assert_eq!(cached.back().unwrap().issued, fresh);
        let mut moved = observed.bounds;
        moved[0] += 1;
        assert!(
            refresh_reference(
                &mut cached,
                &observed.identity,
                moved,
                &[],
                fresh + Duration::from_secs(1)
            )
            .is_none()
        );
        assert_eq!(cached.back().unwrap().issued, fresh);
    }

    #[test]
    fn capacity_eviction_keeps_armed_references() {
        let mut approved = reference();
        approved.armed = true;
        let mut cached = VecDeque::from([approved]);
        for index in 1..MAX_REFERENCES {
            let mut other = reference();
            other.id = format!("other-{index}");
            cached.push_back(other);
        }
        evict_for_capacity(&mut cached);
        assert_eq!(cached.len(), MAX_REFERENCES - 1);
        assert_eq!(cached[0].id, "synthetic-reference");
        assert_eq!(cached[1].id, "other-2");

        // Only a cache made entirely of armed references falls back to the oldest entry.
        cached.push_back(reference());
        for reference in &mut cached {
            reference.armed = true;
        }
        evict_for_capacity(&mut cached);
        assert_eq!(cached[0].id, "other-2");
    }

    /// Window `hwnd` of process `pid`, unowned unless `root_owner` says otherwise.
    fn facts(hwnd: usize, pid: u32, class: &str, root_owner: usize) -> WindowFacts {
        WindowFacts {
            hwnd,
            pid,
            class: class.into(),
            root_owner,
        }
    }

    /// The approved target of `reference()`: window 1 of process 2, unowned.
    fn target_facts() -> WindowFacts {
        facts(1, 2, "fixture", 1)
    }

    fn hit(root: WindowFacts, window_pid: u32) -> Hit {
        Hit {
            root,
            window_pid,
            hosted_pid: None,
        }
    }

    fn check(foreground: &WindowFacts, point: Option<&Hit>) -> Result<(), Failure> {
        let expected = reference();
        expected.validate_snapshot(
            &expected.identity,
            expected.bounds,
            &[],
            &target_facts(),
            foreground,
            point,
        )
    }

    fn assert_recoverable(result: Result<(), Failure>, expect: &str) {
        match result {
            Err(Failure::Recoverable(error)) => {
                assert!(error.to_string().contains(expect), "{error}");
            }
            Err(Failure::Fatal(error)) => {
                panic!("expected a recoverable refusal, got fatal: {error}")
            }
            Ok(()) => panic!("expected a refusal"),
        }
    }

    #[test]
    fn the_target_and_its_own_process_may_hold_the_foreground() {
        let target = target_facts();
        check(&target, None).unwrap();
        // An owned dialog, a popup menu and a drop-down of the same process.
        for window in [
            facts(10, 2, "#32770", 1),
            facts(11, 2, POPUP_MENU_CLASS, 11),
            facts(12, 2, "ComboLBox", 12),
        ] {
            check(&window, None).unwrap();
        }
    }

    #[test]
    fn another_process_never_holds_the_foreground_whatever_it_looks_like() {
        for window in [
            facts(99, 3, "fixture", 99),
            // Same class and an owner chain that reaches the target: still another process.
            facts(98, 3, "fixture", 1),
            facts(97, 3, POPUP_MENU_CLASS, 1),
        ] {
            assert_recoverable(check(&window, None), "no longer foreground");
        }
    }

    #[test]
    fn pointer_destinations_in_the_target_process_are_accepted_and_others_refused() {
        let target = target_facts();
        // The target itself, its dialog, and a popup menu opened by the target.
        for root in [
            facts(1, 2, "fixture", 1),
            facts(10, 2, "#32770", 1),
            facts(11, 2, POPUP_MENU_CLASS, 11),
        ] {
            check(&target, Some(&hit(root, 2))).unwrap();
        }
        // A foreign root window, even with the target in front.
        assert_recoverable(
            check(&target, Some(&hit(facts(99, 3, "fixture", 99), 3))),
            "different process",
        );
        // A foreign child embedded in a window of the target's process.
        assert_recoverable(
            check(&target, Some(&hit(facts(1, 2, "fixture", 1), 3))),
            "different process",
        );
        // A same-process dialog is no use if the foreground belongs to another process.
        assert_recoverable(
            check(
                &facts(99, 3, "fixture", 99),
                Some(&hit(facts(10, 2, "#32770", 1), 2)),
            ),
            "no longer foreground",
        );
    }

    fn assert_refused_with(result: Result<(), Failure>, expected: RefusalCause) {
        match result {
            Err(Failure::Recoverable(PlatformError::TargetRefused { cause, .. })) => {
                assert_eq!(cause, expected);
                assert_eq!(
                    PlatformError::refused(cause, "").retryable(),
                    expected != RefusalCause::OwnIndicator
                );
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }
    }

    #[test]
    fn recoverable_refusals_say_why_as_data() {
        let target = target_facts();
        assert_refused_with(
            check(&facts(99, 3, "fixture", 99), None),
            RefusalCause::NotForeground,
        );
        assert_refused_with(
            check(&target, Some(&hit(facts(99, 3, "fixture", 99), 3))),
            RefusalCause::ForeignDestination,
        );
        // A point under ControlFreak's own Stop button is named as such.
        let own = std::process::id();
        assert_refused_with(
            check(&target, Some(&hit(facts(77, own, "Stop", 77), own))),
            RefusalCause::OwnIndicator,
        );
    }

    #[test]
    fn an_unreadable_image_is_treated_as_a_shared_host() {
        let windows = Some(r"C:\Windows");
        assert!(is_shared_host(None, windows));
        assert!(is_shared_host(Some(r"C:\Windows\explorer.exe"), None));
        assert!(is_shared_host(
            Some(r"C:\Windows\System32\ApplicationFrameHost.exe"),
            windows
        ));
        assert!(!is_shared_host(Some(r"C:\Tools\app.exe"), windows));
    }

    #[test]
    fn a_shared_host_target_does_not_accept_another_window_of_the_host_process() {
        // Two packaged-app frames of one host process: only the owner chain is the same app.
        let target = target_facts();
        assert!(accepts_same_process(
            &target,
            false,
            &facts(5, 2, "Frame", 5)
        ));
        assert!(!accepts_same_process(
            &target,
            true,
            &facts(5, 2, "Frame", 5)
        ));
        assert!(accepts_same_process(
            &target,
            true,
            &facts(6, 2, "#32770", 1)
        ));
    }

    #[test]
    fn losing_the_foreground_is_recoverable_and_a_changed_window_is_not() {
        let expected = reference();
        let foreign = facts(99, 3, "other", 99);
        assert_recoverable(check(&foreign, None), "focus_window");
        // The same reference passes again once the target is back in front.
        check(&target_facts(), None).unwrap();
        // Geometry, identity and desktop changes stay fatal even with the target in front.
        let mut moved = expected.bounds;
        moved[2] += 1;
        assert!(matches!(
            expected.validate_snapshot(
                &expected.identity,
                moved,
                &[],
                &target_facts(),
                &target_facts(),
                None
            ),
            Err(Failure::Fatal(_))
        ));
    }

    #[test]
    fn a_packaged_app_frame_accepts_its_own_hosted_content_window_and_nothing_else() {
        // Frame window 1 runs in ApplicationFrameHost (process 2); its content window belongs to
        // the app (process 9). Another app's content (process 8) sits in another frame.
        let mut frame = reference();
        frame.identity.shell_host = true;
        frame.identity.app_frame_host = true;
        let target = facts(1, 2, "ApplicationFrameWindow", 1);
        let run = |frame_host: bool, point: Hit| {
            let mut frame = frame.clone();
            frame.identity.app_frame_host = frame_host;
            frame.validate_snapshot(
                &frame.identity,
                frame.bounds,
                &[],
                &target,
                &target,
                Some(&point),
            )
        };
        let content = |window_pid: u32, root: &WindowFacts, hosted: Option<u32>| Hit {
            root: root.clone(),
            window_pid,
            hosted_pid: hosted,
        };
        // The frame's own content window, and the frame itself, receive input.
        run(true, content(9, &target, Some(9))).unwrap();
        run(true, content(2, &target, Some(9))).unwrap();
        // A process that is not the hosted app's is refused, as is a hosted pid for a window in
        // another frame, and a frame whose hosted content cannot be found.
        let other_frame = facts(60, 2, "ApplicationFrameWindow", 60);
        assert_recoverable(run(true, content(8, &target, Some(9))), "different process");
        assert_recoverable(
            run(true, content(9, &other_frame, Some(9))),
            "different process",
        );
        assert_recoverable(run(true, content(9, &target, None)), "different process");
        // Without the frame-host identity the same facts are refused: a hosted pid alone never
        // widens the rule for an ordinary target.
        assert_recoverable(
            run(false, content(9, &target, Some(9))),
            "different process",
        );
    }

    #[test]
    fn a_shell_hosted_target_does_not_approve_the_rest_of_the_shell() {
        // File Explorer window 1 and the taskbar share explorer.exe (process 2).
        let mut explorer = reference();
        explorer.identity.shell_host = true;
        let target = facts(1, 2, "CabinetWClass", 1);
        let taskbar = facts(50, 2, "Shell_TrayWnd", 50);
        let other_folder = facts(51, 2, "CabinetWClass", 51);
        let desktop = facts(52, 2, "WorkerW", 52);
        let dialog = facts(53, 2, "#32770", 1);
        let menu = facts(54, 2, POPUP_MENU_CLASS, 54);
        let run = |foreground: &WindowFacts, point: Option<&Hit>| {
            explorer.validate_snapshot(
                &explorer.identity,
                explorer.bounds,
                &[],
                &target,
                foreground,
                point,
            )
        };
        run(&target, None).unwrap();
        run(&dialog, None).unwrap();
        run(&menu, None).unwrap();
        run(&target, Some(&hit(menu.clone(), 2))).unwrap();
        for foreign in [&taskbar, &other_folder, &desktop] {
            assert_recoverable(run(foreign, None), "no longer foreground");
            assert_recoverable(
                run(&target, Some(&hit(foreign.clone(), 2))),
                "different process",
            );
        }
        // A taskbar target accepts the taskbar's own menus but not File Explorer windows.
        let tray_target = facts(50, 2, "Shell_TrayWnd", 50);
        let accepts = |window: &WindowFacts| accepts_same_process(&tray_target, true, window);
        assert!(accepts(&tray_target) && accepts(&menu));
        assert!(!accepts(&target) && !accepts(&other_folder));
    }

    #[test]
    fn same_process_acceptance_is_a_pure_function_of_process_owner_and_menu_class() {
        let target = target_facts();
        assert!(accepts_same_process(&target, false, &facts(7, 2, "any", 7)));
        assert!(!accepts_same_process(
            &target,
            false,
            &facts(7, 3, "any", 1)
        ));
        assert!(accepts_same_process(&target, true, &target));
        assert!(!accepts_same_process(&target, true, &facts(7, 2, "any", 7)));
        assert!(accepts_same_process(&target, true, &facts(7, 2, "any", 1)));
        assert!(!accepts_same_process(
            &target,
            true,
            &facts(7, 3, POPUP_MENU_CLASS, 1)
        ));
    }

    #[test]
    fn foreground_changes_keep_references_and_desktop_switches_do_not() {
        // The WinEvent callback no longer touches references on a foreground change; this
        // pins the retained behaviour of the other two retirement paths.
        let approved = reference();
        let mut references = VecDeque::from([approved.clone()]);
        retire_window(&mut references, 12345);
        assert_eq!(
            references.len(),
            1,
            "an unrelated window destruction keeps it"
        );
        retire_window(&mut references, approved.identity.hwnd);
        assert!(references.is_empty(), "the target's destruction retires it");
        let mut owned = reference();
        owned.identity.hwnd = 77;
        owned.identity.owner = 5;
        let mut references = VecDeque::from([owned]);
        retire_window(&mut references, 5);
        assert!(
            references.is_empty(),
            "an owner's destruction retires its dialogs"
        );
    }

    #[test]
    fn scope_cache_reuses_results_only_briefly_and_only_for_the_same_window() {
        let start = Instant::now();
        let mut cache = ScopeCache::default();
        let fixture = reference().identity;
        assert!(cache.identity(1, start).is_none());
        cache.identity = Some((1, fixture.clone(), start));
        assert!(cache.identity(1, start + SCOPE_TTL / 2).is_some());
        assert!(
            cache.identity(2, start + SCOPE_TTL / 2).is_none(),
            "another window"
        );
        assert!(cache.identity(1, start + SCOPE_TTL).is_none(), "expired");

        assert!(!cache.integrity_verified(1, 2, start));
        cache.remember_integrity(1, 2, start);
        assert!(cache.integrity_verified(1, 2, start + SCOPE_TTL / 2));
        assert!(
            !cache.integrity_verified(1, 3, start),
            "a different process"
        );
        assert!(!cache.integrity_verified(9, 2, start), "a different window");
        assert!(
            !cache.integrity_verified(1, 2, start + SCOPE_TTL),
            "expired"
        );
        // Expired entries are dropped when new ones are added.
        cache.remember_integrity(4, 5, start + SCOPE_TTL * 2);
        assert_eq!(cache.integrity.len(), 1);
    }

    #[test]
    fn validation_scopes_are_per_thread_and_cleared_on_drop() {
        let scope = ValidationScope::enter();
        assert!(SCOPE.with(|cell| cell.borrow().is_some()));
        std::thread::spawn(|| assert!(SCOPE.with(|cell| cell.borrow().is_none())))
            .join()
            .unwrap();
        drop(scope);
        assert!(SCOPE.with(|cell| cell.borrow().is_none()));
    }

    #[test]
    fn raw_handles_and_pids_are_not_target_references() {
        for id in ["0X10:20", "1", "target-not-issued", ""] {
            assert!(resolve(id).is_err());
        }
    }

    #[test]
    fn destruction_retires_all_generations_before_handle_reuse() {
        let first = reference();
        let mut second = first.clone();
        second.id = "later-observation".into();
        let mut unrelated = first.clone();
        unrelated.id = "unrelated".into();
        unrelated.identity.hwnd = 99;
        let mut references = VecDeque::from([first.clone(), second, unrelated]);
        retire_window(&mut references, first.identity.hwnd);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].id, "unrelated");
        let mut reused = first;
        reused.id = "replacement-window".into();
        references.push_back(reused);
        assert!(
            references
                .iter()
                .all(|entry| entry.id != "synthetic-reference" && entry.id != "later-observation")
        );
    }
}
