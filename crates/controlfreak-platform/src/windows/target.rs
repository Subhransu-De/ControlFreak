#![allow(unsafe_code)]

use super::{desktop, environment::ensure_interactive_input_desktop, privilege};
use controlfreak_core::{DisplayInfo, MutationControl, PlatformError};
use std::{
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
    Foundation::{ERROR_SUCCESS, FILETIME, GetLastError, HWND, SetLastError},
    System::{
        Com::CoCreateGuid,
        Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        WindowsAndMessaging::{
            EVENT_OBJECT_DESTROY, EVENT_SYSTEM_DESKTOPSWITCH, EVENT_SYSTEM_FOREGROUND, GA_ROOT,
            GW_OWNER, GetAncestor, GetClassNameW, GetMessageW, GetWindow, GetWindowRect,
            GetWindowThreadProcessId, IsWindow, MSG, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT,
            WindowFromPoint,
        },
    },
};

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
    desktop: String,
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

impl Reference {
    fn validate_snapshot(
        &self,
        current: &Identity,
        bounds: [i32; 4],
        displays: &[DisplayInfo],
        foreground: usize,
        hit: Option<(usize, u32)>,
    ) -> Result<(), PlatformError> {
        if &self.identity != current {
            return Err(invalid(
                "window, process lifetime, owner, or desktop changed",
            ));
        }
        if foreground != current.hwnd {
            return Err(invalid("approved target is no longer foreground"));
        }
        if self.bounds != bounds || self.displays != displays {
            return Err(invalid(
                "window bounds or display layout changed; end the session and observe again",
            ));
        }
        if hit.is_some_and(|(root, pid)| root != current.hwnd || pid != current.pid) {
            return Err(invalid(
                "input destination belongs to a different window or process; owned popups require separate approval",
            ));
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

fn unavailable(reason: impl std::fmt::Display) -> PlatformError {
    PlatformError::Unavailable {
        reason: format!("target references unavailable: {reason}"),
    }
}

fn identity(hwnd: HWND) -> Result<Identity, PlatformError> {
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
    let _apartment = desktop::ComApartment::initialize().map_err(unavailable)?;
    let manager = desktop::virtual_desktop_manager().map_err(unavailable)?;
    let desktop = desktop::window_desktop_id(&manager, hwnd)
        .map_err(|_| invalid("window desktop cannot be verified"))?;
    Ok(Identity {
        hwnd: hwnd.0 as usize,
        pid,
        thread,
        created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
        owner: owner.0 as usize,
        class: String::from_utf16_lossy(&class[..length]),
        desktop: format!("{desktop:?}"),
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
    } else if event == EVENT_SYSTEM_FOREGROUND {
        retire_foreground(
            &mut REFERENCES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            hwnd.0 as usize,
        );
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

fn retire_foreground(references: &mut VecDeque<Reference>, foreground: usize) {
    references.retain(|reference| !reference.armed || reference.identity.hwnd == foreground);
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
    if references.len() >= MAX_REFERENCES {
        references.pop_front();
    }
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

pub(super) fn arm_if_foreground(id: &str) -> Result<(), PlatformError> {
    let mut references = REFERENCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reference = references
        .iter_mut()
        .find(|reference| reference.id == id)
        .ok_or_else(|| invalid("approved target reference was retired"))?;
    let foreground = desktop::foreground_window_handle().0 as usize;
    if reference.identity.hwnd == foreground {
        reference.armed = true;
    } else if reference.armed {
        references.retain(|reference| reference.id != id);
        return Err(invalid("approved target lost foreground ownership"));
    }
    Ok(())
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
    control.check(operation)?;
    control.check_target()?;
    let id = control
        .approved_target()
        .ok_or_else(|| invalid("an approved target reference is required"))?;
    let result = validate_reference(&id, operation, point);
    if result.is_err() {
        control.invalidate_target();
    }
    result
}

fn validate_reference(
    id: &str,
    operation: &str,
    point: Option<windows::Win32::Foundation::POINT>,
) -> Result<(), PlatformError> {
    ensure_interactive_input_desktop(operation)?;
    let (hwnd, _) = resolve(id)?;
    let foreground = desktop::foreground_window_handle();
    privilege::ensure_window_integrity(operation, foreground)?;
    privilege::ensure_window_integrity(operation, hwnd)?;
    let _apartment = desktop::ComApartment::initialize()?;
    let manager = desktop::virtual_desktop_manager()?;
    if !desktop::window_is_on_current_desktop(&manager, hwnd) {
        return Err(invalid("approved target left the current desktop"));
    }
    let reference = lookup(id)?;
    let current_bounds = bounds(hwnd)?;
    let displays = desktop::enumerate_displays()?;
    let current_identity = identity(hwnd)?;
    let hit = if let Some(point) = point {
        // SAFETY: These APIs inspect value coordinates and return borrowed window handles.
        let hit = unsafe { WindowFromPoint(point) };
        // SAFETY: GetAncestor reads metadata for the borrowed hit window.
        let root = unsafe { GetAncestor(hit, GA_ROOT) };
        let mut pid = 0;
        // SAFETY: pid is writable and this query retains no references to the hit window.
        unsafe { GetWindowThreadProcessId(hit, Some(&raw mut pid)) };
        privilege::ensure_point_integrity(operation, point)?;
        Some((root.0 as usize, pid))
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
        unsafe { GetGUIThreadInfo(0, &raw mut gui) }
            .map_err(|_| invalid("keyboard focus cannot be verified"))?;
        privilege::ensure_window_integrity(operation, gui.hwndFocus)?;
        // SAFETY: GetAncestor reads metadata for this borrowed focus window.
        let root = unsafe { GetAncestor(gui.hwndFocus, GA_ROOT) };
        let mut pid = 0;
        // SAFETY: pid is writable and no pointers are retained.
        unsafe { GetWindowThreadProcessId(gui.hwndFocus, Some(&raw mut pid)) };
        Some((root.0 as usize, pid))
    } else {
        None
    };
    reference.validate_snapshot(
        &current_identity,
        current_bounds,
        &displays,
        foreground.0 as usize,
        hit,
    )?;
    // A final foreground read follows the slower process, COM, and display queries.
    if desktop::foreground_window_handle() != hwnd {
        return Err(invalid("foreground changed during target validation"));
    }
    arm_if_foreground(id)
}

pub(super) fn evidence(control: &MutationControl) {
    let remained = control.approved_target().is_some_and(|id| {
        resolve(&id).is_ok_and(|(hwnd, _)| desktop::foreground_window_handle() == hwnd)
    });
    control.target_foreground(remained);
    if !remained {
        control.invalidate_target();
    }
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
            assert!(
                expected
                    .validate_snapshot(
                        &current.identity,
                        current.bounds,
                        &current.displays,
                        1,
                        None
                    )
                    .is_err(),
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
    fn foreground_alone_does_not_authorize_pointer_dispatch() {
        let expected = reference();
        for hit in [None, Some((1, 2))] {
            expected
                .validate_snapshot(&expected.identity, expected.bounds, &[], 1, hit)
                .unwrap();
            assert!(
                expected
                    .validate_snapshot(&expected.identity, expected.bounds, &[], 99, hit)
                    .is_err()
            );
        }
        assert!(
            expected
                .validate_snapshot(&expected.identity, expected.bounds, &[], 1, Some((99, 2)))
                .is_err()
        );
        assert!(
            expected
                .validate_snapshot(&expected.identity, expected.bounds, &[], 1, Some((1, 99)))
                .is_err()
        );
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

    #[test]
    fn focus_theft_retires_approval_even_if_focus_later_returns() {
        let mut approved = reference();
        approved.armed = true;
        let mut discovered = reference();
        discovered.id = "not-yet-approved".into();
        let mut references = VecDeque::from([approved, discovered]);
        retire_foreground(&mut references, 99);
        retire_foreground(&mut references, 1);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].id, "not-yet-approved");
    }
}
