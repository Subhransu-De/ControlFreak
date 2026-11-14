#![allow(unsafe_code)]

//! The session tab, its Stop button and the physical-Esc monitor.
//!
//! Two threads, so a slow paint can never delay a keystroke. The tab thread paints the tab and
//! the Stop button and owns every window. The Esc thread owns the low-level keyboard hook, and
//! only while a session is owned; it does nothing else.

use controlfreak_core::StopController;
use std::{
    cell::{Cell, RefCell},
    io,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WAIT_FAILED, WAIT_TIMEOUT, WPARAM},
        Graphics::Gdi::{CreateFontIndirectW, DeleteObject, HFONT, HGDIOBJ, LOGFONTW},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Accessibility::NotifyWinEvent,
            HiDpi::SystemParametersInfoForDpi,
            Input::{
                GetCurrentInputMessageSource, IMO_INJECTED, INPUT_MESSAGE_ORIGIN_ID,
                INPUT_MESSAGE_SOURCE,
                KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE},
            },
            Shell::{
                DefSubclassProc, IVirtualDesktopManager, RemoveWindowSubclass, SetWindowSubclass,
            },
            WindowsAndMessaging::{
                BM_CLICK, BN_CLICKED, BS_PUSHBUTTON, CHILDID_SELF, CallNextHookEx, CreateWindowExW,
                DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_NAMECHANGE,
                EVENT_SYSTEM_ALERT, GetForegroundWindow, HHOOK, HTTRANSPARENT, HWND_TOPMOST,
                KBDLLHOOKSTRUCT, LLKHF_INJECTED, MA_NOACTIVATE, MSG, MWMO_INPUTAVAILABLE,
                MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PM_REMOVE, PeekMessageW,
                PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SPI_GETICONTITLELOGFONT, SW_HIDE,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowDisplayAffinity,
                SetWindowPos, SetWindowTextW, SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx,
                UnregisterClassW, WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WINDOW_STYLE, WM_APP,
                WM_CHAR, WM_CLOSE, WM_COMMAND, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN,
                WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
                WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_NCDESTROY, WM_NCHITTEST, WM_RBUTTONDOWN,
                WM_RBUTTONUP, WM_SETFONT, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_SYSKEYDOWN,
                WM_SYSKEYUP, WM_THEMECHANGED, WNDCLASSW, WS_DISABLED, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{GUID, PCWSTR, w},
};

use super::desktop::{ComApartment, virtual_desktop_manager};
use super::indicator::{
    Activity, FADE_IN, FADE_IN_FROM, Monitor, PaintedTab, Painter, Palette, READING, StopStatus,
    TabInputs, TabKind, TabModel, TabView, animations_enabled, enumerate_monitors, spoken,
    text_scale,
};

const CLASS_NAME: windows::core::PCWSTR = w!("ControlFreakUserStop");
/// Longest the UI thread blocks while a tab is shown or about to change. Window messages, hook
/// callbacks and every session, stop or observation change wake the wait immediately, so this
/// only paces the tab's own timers (the waiting delay, the handback and the reading state).
const POLL_INTERVAL_MS: u32 = 100;
/// Frame interval while a tab fades in or out.
const FADE_FRAME_MS: u32 = 16;
/// The tab fades in with the lamp, and fades out over the same short time.
const TAB_FADE: Duration = FADE_IN;
/// While a tab is shown, display, contrast, text-size and animation settings and the foreground
/// desktop are re-read at least this often, and at once when Windows announces a change.
const SETTINGS_INTERVAL: Duration = Duration::from_millis(500);
/// With nothing shown the tab thread has nothing to follow: Windows announces display and
/// setting changes as messages, and a session wakes the thread. This is only a safety net.
const IDLE_SETTINGS_INTERVAL: Duration = Duration::from_secs(2);
/// The label of the Stop button, which is also its accessible name.
const STOP_BUTTON_LABEL: PCWSTR = w!("Stop agent");
/// Identifies this module's subclass on the Stop button.
const BUTTON_SUBCLASS_ID: usize = 0x5354_4f50;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    Released,
    PassedThrough,
    Consumed,
}

thread_local! {
    static CONTROLLER: RefCell<Option<StopController>> = const { RefCell::new(None) };
    static ESC_STATE: Cell<EscapeState> = const { Cell::new(EscapeState::Released) };
    static SETTINGS_CHANGED: Cell<bool> = const { Cell::new(false) };
}

/// What the desktop glow controller last had acknowledged, so the session tab can say whether
/// the agent is acting right now. Written by the server's glow controller; read by the tab.
pub struct SessionSignals {
    activity: AtomicU8,
    /// Counts acknowledged actions, so an action that starts and ends between two polls of the
    /// tab still shows as working.
    actions: AtomicU64,
    /// The tab's UI thread, woken on every change so it does not wait for its next poll.
    ui_thread: AtomicU32,
    elevated: bool,
}

impl SessionSignals {
    pub fn new(elevated: bool) -> Self {
        Self {
            activity: AtomicU8::new(0),
            actions: AtomicU64::new(0),
            ui_thread: AtomicU32::new(0),
            elevated,
        }
    }

    /// Records an acknowledged indicator level: `true` while an input action runs.
    pub fn set_acting(&self, acting: bool) {
        if acting {
            self.actions.fetch_add(1, Ordering::AcqRel);
        }
        self.activity
            .store(if acting { 2 } else { 1 }, Ordering::Release);
        self.wake();
    }

    /// Records that the indicator was hidden at the end of a session.
    pub fn clear(&self) {
        self.activity.store(0, Ordering::Release);
        self.wake();
    }

    fn activity(&self) -> Activity {
        match self.activity.load(Ordering::Acquire) {
            2 => Activity::Working,
            1 => Activity::Waiting,
            _ => Activity::Idle,
        }
    }

    fn actions(&self) -> u64 {
        self.actions.load(Ordering::Acquire)
    }

    fn wake(&self) {
        let thread = self.ui_thread.load(Ordering::Acquire);
        if thread != 0 {
            // SAFETY: Posts a value-only message to a thread ID; a thread that has exited only
            // makes the call fail, which is harmless because its next poll picks up the change.
            let _ = unsafe { PostThreadMessageW(thread, WM_APP, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// Independent native session tab, Stop button and physical Esc monitor. Never calls a provider.
pub struct UserStop {
    shutdown: Arc<AtomicBool>,
    signals: Arc<SessionSignals>,
    worker: Option<thread::JoinHandle<()>>,
    /// Owns the keyboard hook, which exists only while a session is owned.
    escape: Option<EscapeMonitor>,
}

impl UserStop {
    /// Starts the tab and the Esc monitor. A failure to start is returned and stops nothing: the
    /// server keeps running and the caller refuses sessions with the reason. After a successful
    /// start, a failure of the tab thread closes admission with its reason (see
    /// [`StopController::stop_with_failure`]) and calls `report` with the same text.
    pub fn start(
        controller: &StopController,
        signals: Arc<SessionSignals>,
        report: impl Fn(String) + Send + 'static,
    ) -> Result<Self, String> {
        super::ensure_dpi_awareness().map_err(|error| error.to_string())?;
        // Every session, stop and observation change wakes the tab thread at once.
        let waker: Weak<SessionSignals> = Arc::downgrade(&signals);
        controller.subscribe(move || {
            waker.upgrade().is_some_and(|signals| {
                signals.wake();
                true
            })
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let exiting = Arc::clone(&shutdown);
        let thread_signals = Arc::clone(&signals);
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread_controller = controller.clone();
        let worker = thread::Builder::new()
            .name("controlfreak-user-stop".to_owned())
            .spawn(move || {
                let controller = thread_controller;
                CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
                let started = Cell::new(false);
                if let Err(error) = run(&controller, &thread_signals, &exiting, &sender, &started) {
                    if started.get() {
                        record_tab_failure(&controller, &report, &error);
                    } else {
                        let _ = sender.send(Err(error.to_string()));
                    }
                }
                CONTROLLER.with(|slot| *slot.borrow_mut() = None);
            })
            .map_err(|error| error.to_string())?;
        let mut guard = Self {
            shutdown,
            signals,
            worker: Some(worker),
            escape: None,
        };
        receiver.recv().map_err(|error| error.to_string())??;
        // The hook belongs to a thread of its own and is installed only while a session is owned.
        guard.escape = Some(
            EscapeMonitor::start(controller, || Box::new(SystemHook::default()))
                .map_err(|error| format!("the Esc monitor could not start: {error}"))?,
        );
        Ok(guard)
    }
}

impl Drop for UserStop {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.signals.wake();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.escape.take();
    }
}

/// The tab and its Stop button are gone after a successful start, so fail closed and keep the
/// reason where the agent (refusals), `get_server_status` (through `report`) and the log see it.
fn record_tab_failure(controller: &StopController, report: &impl Fn(String), error: &io::Error) {
    let reason = format!("the session tab stopped working: {error}");
    eprintln!(
        "{}",
        serde_json::json!({"event": "session_tab_failed", "reason": reason})
    );
    controller.stop_with_failure(reason.clone());
    report(reason);
}

/// Installs and removes the low-level keyboard hook on the thread that calls it. A trait so that
/// tests can count installs without hooking the developer's keyboard.
trait KeyboardHook {
    fn install(&mut self) -> io::Result<()>;
    fn uninstall(&mut self);
}

/// The real hook. Both calls must run on the Esc monitor thread, which pumps its messages.
#[derive(Default)]
struct SystemHook {
    hook: Option<HHOOK>,
}

impl KeyboardHook for SystemHook {
    fn install(&mut self) -> io::Result<()> {
        if self.hook.is_some() {
            return Ok(());
        }
        // SAFETY: The module handle call takes no pointers; the hook callback is a plain function
        // and the calling thread pumps messages, as a low-level hook requires.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None)
                .map_err(|error| windows_error("GetModuleHandleW", &error))?
                .into();
            // A key already held before monitoring must retain its matching release.
            ESC_STATE.with(|state| {
                state.set(if GetAsyncKeyState(i32::from(VK_ESCAPE.0)) < 0 {
                    EscapeState::PassedThrough
                } else {
                    EscapeState::Released
                });
            });
            self.hook = Some(
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance), 0)
                    .map_err(|error| windows_error("SetWindowsHookExW", &error))?,
            );
        }
        Ok(())
    }

    fn uninstall(&mut self) {
        if let Some(hook) = self.hook.take() {
            // SAFETY: The handle was returned by SetWindowsHookExW on this thread and is released
            // exactly once.
            let _ = unsafe { UnhookWindowsHookEx(hook) };
        }
        ESC_STATE.with(|state| state.set(EscapeState::Released));
    }
}

impl Drop for SystemHook {
    fn drop(&mut self) {
        self.uninstall();
    }
}

/// Watches for session ownership and keeps the keyboard hook installed exactly while a session is
/// owned. The thread does only that, so the hook is never starved by painting or shell calls.
struct EscapeMonitor {
    exiting: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    worker: Option<thread::JoinHandle<()>>,
}

impl EscapeMonitor {
    fn start(
        controller: &StopController,
        make_hook: impl FnOnce() -> Box<dyn KeyboardHook> + Send + 'static,
    ) -> io::Result<Self> {
        let exiting = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(AtomicU32::new(0));
        let wake_id = Arc::clone(&thread_id);
        let wake_exiting = Arc::clone(&exiting);
        controller.subscribe(move || {
            post_wake(wake_id.load(Ordering::Acquire));
            !wake_exiting.load(Ordering::Acquire)
        });
        let worker = {
            let controller = controller.clone();
            let thread_id = Arc::clone(&thread_id);
            let exiting = Arc::clone(&exiting);
            thread::Builder::new()
                .name("controlfreak-escape-hook".to_owned())
                .spawn(move || escape_loop(&controller, make_hook(), &thread_id, &exiting))?
        };
        Ok(Self {
            exiting,
            thread_id,
            worker: Some(worker),
        })
    }
}

impl Drop for EscapeMonitor {
    fn drop(&mut self) {
        self.exiting.store(true, Ordering::Release);
        post_wake(self.thread_id.load(Ordering::Acquire));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Wakes a thread that sleeps in its message queue.
fn post_wake(thread: u32) {
    if thread != 0 {
        // SAFETY: Posts a value-only message to a thread ID; a thread that has exited only makes
        // the call fail, which is harmless because it re-reads the state before it sleeps again.
        let _ = unsafe { PostThreadMessageW(thread, WM_APP, WPARAM(0), LPARAM(0)) };
    }
}

fn escape_loop(
    controller: &StopController,
    mut hook: Box<dyn KeyboardHook>,
    thread_id: &AtomicU32,
    exiting: &AtomicBool,
) {
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
    // Create this thread's message queue before announcing it, so a wake-up is never lost.
    drain_messages();
    // SAFETY: Takes no arguments and returns this thread's ID.
    thread_id.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    let mut installed = false;
    // One failed install per session is reported; retrying on every wake-up would spam.
    let mut failed = false;
    while !exiting.load(Ordering::Acquire) {
        drain_messages();
        match hook_action(controller.session_active(), installed, failed) {
            HookAction::Install => match hook.install() {
                Ok(()) => {
                    installed = true;
                    controller.set_esc_stop_available(true);
                }
                Err(error) => {
                    failed = true;
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "event": "esc_monitor_unavailable",
                            "reason": error.to_string(),
                        })
                    );
                }
            },
            HookAction::Uninstall => {
                controller.set_esc_stop_available(false);
                hook.uninstall();
                installed = false;
            }
            HookAction::EndSession => failed = false,
            HookAction::Wait => {}
        }
        // Hook callbacks arrive as sent messages and run inside this wait. A session change, or
        // shutdown, posts a message that ends it.
        if wait_for_input(u32::MAX).is_err() {
            break;
        }
    }
    if installed {
        controller.set_esc_stop_available(false);
        hook.uninstall();
    }
    CONTROLLER.with(|slot| *slot.borrow_mut() = None);
}

/// What the Esc monitor does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookAction {
    Install,
    Uninstall,
    /// The session ended after a failed install: forget the failure so the next one retries.
    EndSession,
    Wait,
}

fn hook_action(session_owned: bool, installed: bool, failed: bool) -> HookAction {
    match (session_owned, installed, failed) {
        (true, false, false) => HookAction::Install,
        (false, true, _) => HookAction::Uninstall,
        (false, false, true) => HookAction::EndSession,
        _ => HookAction::Wait,
    }
}

/// Dispatches every queued message so hook callbacks and window messages are answered.
fn drain_messages() {
    let mut message = MSG::default();
    // SAFETY: `message` is writable for each synchronous dequeue on this thread's own queue.
    while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        // SAFETY: The message was just retrieved for a window owned by this thread, or has none.
        unsafe { DispatchMessageW(&raw const message) };
    }
}

// Owned by the UI thread, including partially initialized failure paths.
struct NativeStop {
    instance: HINSTANCE,
    tabs: Vec<TabWindow>,
}

impl NativeStop {
    fn create() -> io::Result<Self> {
        // SAFETY: This UI thread owns the class. The guard releases every successfully created
        // resource if a later call fails.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None)
                .map_err(|error| windows_error("GetModuleHandleW", &error))?
                .into();
            let descriptor = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            if RegisterClassW(&raw const descriptor) == 0 {
                return Err(windows_error(
                    "RegisterClassW",
                    &windows::core::Error::from_thread(),
                ));
            }
            Ok(Self {
                instance,
                tabs: Vec::new(),
            })
        }
    }

    /// Keeps one tab window per display, rebuilding them when displays or their DPI change.
    fn sync_monitors(&mut self, monitors: &[Monitor]) -> io::Result<bool> {
        let current: Vec<Monitor> = self.tabs.iter().map(|tab| tab.monitor).collect();
        if current == monitors {
            return Ok(false);
        }
        self.tabs.clear();
        for monitor in monitors.iter().copied() {
            self.tabs.push(TabWindow::create(self.instance, monitor)?);
        }
        Ok(true)
    }
}

impl Drop for NativeStop {
    fn drop(&mut self) {
        // Destroy the windows before their class.
        self.tabs.clear();
        // SAFETY: The class was registered on this thread and is released once.
        unsafe {
            let _ = UnregisterClassW(CLASS_NAME, Some(self.instance));
        }
    }
}

/// The tab on one display: a click-through, capture-excluded layered window centred on the
/// display's top edge, hanging from the lamp the glow helper draws there. Its Stop button is the
/// one part that takes clicks.
struct TabWindow {
    hwnd: HWND,
    monitor: Monitor,
    tab: Option<PaintedTab>,
    origin: POINT,
    visible: bool,
    /// The accessible name last given to the window.
    name: String,
    button: StopButton,
    text_scale: f32,
}

/// Size of the Stop button in physical pixels for one display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ButtonMetrics {
    width: i32,
    height: i32,
    /// Space between the tab body and the button.
    gap: i32,
}

/// The button grows with the display's DPI and with the Windows text size, so its label never
/// clips.
fn button_metrics(dpi: u32, text_scale: f32) -> ButtonMetrics {
    let dpi_scale = f64::from(dpi.max(96)) / 96.0;
    let text = f64::from(text_scale.clamp(1.0, 2.25));
    let px = |dip: f64, scale: f64| -> i32 {
        // Small positive DIP values times a bounded scale.
        #[allow(clippy::cast_possible_truncation)]
        let value = (dip * scale).round() as i32;
        value
    };
    ButtonMetrics {
        width: px(104.0, dpi_scale * text),
        height: px(32.0, dpi_scale * text),
        gap: px(6.0, dpi_scale),
    }
}

/// Where the button goes: centred under the tab body, kept inside the display. `None` when the
/// display is too short to show it.
fn button_origin(
    monitor: Monitor,
    tab_left: i32,
    tab_width: i32,
    body_bottom: i32,
    metrics: ButtonMetrics,
) -> Option<POINT> {
    let y = monitor.top + body_bottom + metrics.gap;
    if y + metrics.height > monitor.bottom || metrics.width > monitor.width() {
        return None;
    }
    let centred = tab_left + (tab_width - metrics.width) / 2;
    Some(POINT {
        x: centred.clamp(monitor.left, monitor.right - metrics.width),
        y,
    })
}

/// Whether a mouse or key message that reached the Stop button came from a person or from
/// hardware, not from `SendInput` and its relatives. The agent's own input tools are injected, so
/// the agent can never press the button; touch, pen and mouse hardware can. When the system
/// cannot say where the message came from, it is treated as injected.
fn input_is_from_a_person(source: windows::core::Result<INPUT_MESSAGE_SOURCE>) -> bool {
    source.is_ok_and(|source| !is_injected(source.originId))
}

fn is_injected(origin: INPUT_MESSAGE_ORIGIN_ID) -> bool {
    origin == IMO_INJECTED
}

/// The current message's source. `BM_CLICK` and the other messages that do not come from the
/// input queue report no origin; they are not input and are handled before this is asked.
fn current_input_is_from_a_person() -> bool {
    let mut source = INPUT_MESSAGE_SOURCE::default();
    // SAFETY: `source` is writable for the synchronous call, which reads the calling thread's
    // current message.
    input_is_from_a_person(
        unsafe { GetCurrentInputMessageSource(&raw mut source) }.map(|()| source),
    )
}

/// A real, native push button on its own small window, owned by the tab. Being a standard
/// `BUTTON` it exposes the Invoke pattern to UI Automation, so on-screen keyboards, voice control
/// and switch access can press it. It is enabled and not transparent, so hit-testing resolves to
/// it, and its process is not the target's, which the pointer checks refuse; its subclass also
/// drops injected input. It never takes focus.
struct StopButton {
    hwnd: HWND,
    font: Option<HFONT>,
    /// The DPI and text scale the font and size were made for.
    fitted: Option<(u32, u32)>,
    shown: Option<(i32, i32, i32, i32)>,
}

impl StopButton {
    fn create(instance: HINSTANCE, owner: HWND) -> io::Result<Self> {
        // SAFETY: The class is a system class, the owner is a live window of this thread, and the
        // returned HWND is owned by this value until Drop destroys it.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("BUTTON"),
                STOP_BUTTON_LABEL,
                WS_POPUP | WINDOW_STYLE(BS_PUSHBUTTON.cast_unsigned()),
                0,
                0,
                1,
                1,
                Some(owner),
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| windows_error("CreateWindowExW(BUTTON)", &error))?;
        // SAFETY: This thread owns `hwnd`; capture affinity stores no Rust pointers.
        if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            // SAFETY: Window creation succeeded and ownership has not escaped this function.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(windows_error("SetWindowDisplayAffinity", &error));
        }
        // SAFETY: `hwnd` belongs to this thread; the callback is a plain function and keeps no
        // reference data.
        if !unsafe { SetWindowSubclass(hwnd, Some(button_subclass), BUTTON_SUBCLASS_ID, 0) }
            .as_bool()
        {
            // SAFETY: Window creation succeeded and ownership has not escaped this function.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(windows_error(
                "SetWindowSubclass",
                &windows::core::Error::from_thread(),
            ));
        }
        Ok(Self {
            hwnd,
            font: None,
            fitted: None,
            shown: None,
        })
    }

    /// Gives the button the system message font at this display's DPI and the Windows text size.
    fn fit(&mut self, dpi: u32, text_scale: f32) {
        let key = (dpi, text_scale.to_bits());
        if self.fitted == Some(key) {
            return;
        }
        let mut font = LOGFONTW::default();
        // SAFETY: `font` is writable and its size is passed; the call fills it for this DPI.
        let found = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETICONTITLELOGFONT.0,
                u32::try_from(size_of::<LOGFONTW>()).unwrap_or(0),
                Some((&raw mut font).cast()),
                0,
                dpi,
            )
        };
        if found.is_err() {
            return;
        }
        // The icon-title font height is negative (character height); scale its magnitude.
        #[allow(clippy::cast_possible_truncation)]
        {
            font.lfHeight =
                (f64::from(font.lfHeight) * f64::from(text_scale.clamp(1.0, 2.25))).round() as i32;
        }
        font.lfWeight = 600;
        // SAFETY: `font` is fully initialized by the call above.
        let created = unsafe { CreateFontIndirectW(&raw const font) };
        if created.is_invalid() {
            return;
        }
        // SAFETY: The button owns its window; the font stays alive until it replaces or drops it,
        // and the button is told not to repaint twice (WM_SETFONT with a redraw flag).
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                self.hwnd,
                WM_SETFONT,
                Some(WPARAM(created.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        if let Some(previous) = self.font.replace(created) {
            // SAFETY: The previous font was created here and is no longer selected anywhere.
            let _ = unsafe { DeleteObject(HGDIOBJ(previous.0)) };
        }
        self.fitted = Some(key);
    }

    /// Shows the button at `rect` (left, top, width, height) above everything, or hides it.
    fn place(&mut self, rect: Option<(i32, i32, i32, i32)>) {
        if self.shown == rect {
            return;
        }
        match rect {
            Some((x, y, width, height)) => {
                // SAFETY: This thread owns the HWND; the call only moves, sizes and shows it
                // above other topmost windows, without activating it.
                let _ = unsafe {
                    SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        x,
                        y,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                };
            }
            None => {
                // SAFETY: Hiding a window owned by this thread retains no pointers.
                let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            }
        }
        self.shown = rect;
    }

    /// Keeps a shown button above windows that became topmost after it.
    fn raise(&self) {
        if self.shown.is_some() {
            // SAFETY: This thread owns the HWND; the call changes only z-order.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            };
        }
    }
}

impl Drop for StopButton {
    fn drop(&mut self) {
        // SAFETY: The window and font are owned by this value and released once. Destroying the
        // owner earlier would already have destroyed the window, which only makes this fail.
        unsafe {
            let _ = RemoveWindowSubclass(self.hwnd, Some(button_subclass), BUTTON_SUBCLASS_ID);
            let _ = DestroyWindow(self.hwnd);
            if let Some(font) = self.font.take() {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }
}

/// Stops the control session as the user's stop, if one is owned.
fn invoke_stop() -> bool {
    CONTROLLER.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(StopController::stop_by_user)
    })
}

/// Whether a message is mouse or key input, which the Stop button ignores when injected.
fn is_person_only_message(message: u32) -> bool {
    matches!(
        message,
        WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_LBUTTONDBLCLK
            | WM_RBUTTONDOWN
            | WM_RBUTTONUP
            | WM_MBUTTONDOWN
            | WM_MBUTTONUP
            | WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
    )
}

unsafe extern "system" fn button_subclass(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        // A click never activates the button's window, so it never takes focus from the user.
        WM_MOUSEACTIVATE => return LRESULT(isize::try_from(MA_NOACTIVATE).unwrap_or(3)),
        // UI Automation's Invoke pattern, and therefore assistive technology, clicks this way.
        // It is not input, so it needs no origin.
        BM_CLICK => {
            invoke_stop();
            return LRESULT(0);
        }
        message if is_person_only_message(message) && !current_input_is_from_a_person() => {
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: The window is being destroyed; this removes the subclass that is running.
            let _ =
                unsafe { RemoveWindowSubclass(window, Some(button_subclass), BUTTON_SUBCLASS_ID) };
        }
        _ => {}
    }
    // SAFETY: Forward unchanged callback arguments to the next procedure in the chain.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

impl TabWindow {
    fn create(instance: HINSTANCE, monitor: Monitor) -> io::Result<Self> {
        // SAFETY: The registered class, module and constant strings remain valid; Windows owns the
        // returned HWND until Drop destroys it.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                CLASS_NAME,
                w!("ControlFreak session"),
                // The tab is purely informational. WindowFromPoint skips disabled windows, so
                // point-integrity checks resolve the application beneath the tab instead of this
                // process, and physical clicks pass through the transparent layered window.
                WS_POPUP | WS_DISABLED,
                monitor.left,
                monitor.top,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| windows_error("CreateWindowExW", &error))?;
        // SAFETY: This thread owns `hwnd`; capture affinity stores no Rust pointers.
        if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            // SAFETY: Window creation succeeded and ownership has not escaped this function.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(windows_error("SetWindowDisplayAffinity", &error));
        }
        let button = match StopButton::create(instance, hwnd) {
            Ok(button) => button,
            Err(error) => {
                // SAFETY: Window creation succeeded and ownership has not escaped this function.
                let _ = unsafe { DestroyWindow(hwnd) };
                return Err(error);
            }
        };
        Ok(Self {
            hwnd,
            monitor,
            tab: None,
            origin: POINT {
                x: monitor.left,
                y: monitor.top,
            },
            visible: false,
            name: String::new(),
            button,
            text_scale: 1.0,
        })
    }

    /// Shows or hides the Stop button under the tab body. It is offered only while `offered`,
    /// the tab is fully shown and the display has room for it.
    fn place_stop_button(&mut self, offered: bool) {
        let rect = self
            .tab
            .as_ref()
            .filter(|_| offered && self.visible)
            .and_then(|tab| {
                let metrics = button_metrics(self.monitor.dpi, self.text_scale);
                button_origin(
                    self.monitor,
                    self.origin.x,
                    tab.width,
                    tab.body_bottom,
                    metrics,
                )
                .map(|origin| (origin.x, origin.y, metrics.width, metrics.height))
            });
        if rect.is_some() {
            self.button.fit(self.monitor.dpi, self.text_scale);
        }
        self.button.place(rect);
    }

    /// Paints `view` for this display and names the window with its words, so a window rebuilt
    /// after a display change is named as soon as it has something to show.
    fn paint(
        &mut self,
        painter: &Painter,
        view: TabView,
        settings: &Settings,
        with_lamp: bool,
    ) -> io::Result<()> {
        let tab = painter.paint_with_lamp(
            view,
            &settings.palette,
            self.monitor.dpi,
            self.monitor.tab_space(),
            settings.text_scale,
            with_lamp,
        )?;
        // Name the window with the words actually painted, which a narrow display or a large
        // text size can change. A possibly held key keeps every recovery step in its name.
        let name = if view.kind == TabKind::InputHeld {
            spoken(view)
        } else {
            tab.spoken.clone()
        };
        self.origin = tab_origin(self.monitor, tab.width);
        self.text_scale = settings.text_scale;
        self.tab = Some(tab);
        self.set_name(&name);
        Ok(())
    }

    /// Names the window with the tab's message, so screen readers that read windows through UI
    /// Automation or MSAA get the words the tab shows, and tells them when the name changes.
    fn set_name(&mut self, text: &str) {
        if self.name == text {
            return;
        }
        text.clone_into(&mut self.name);
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated buffer that outlives this synchronous call, and this
        // thread owns the window.
        let _ = unsafe { SetWindowTextW(self.hwnd, PCWSTR(wide.as_ptr())) };
        // SAFETY: Value-only notification about a window this thread owns.
        unsafe {
            NotifyWinEvent(
                EVENT_OBJECT_NAMECHANGE,
                self.hwnd,
                OBJID_WINDOW.0,
                i32::try_from(CHILDID_SELF).unwrap_or(0),
            );
        }
    }

    /// Raises a system alert for the window, which screen readers announce without the window
    /// taking focus.
    fn alert(&self) {
        // SAFETY: Value-only notification about a window this thread owns.
        unsafe {
            NotifyWinEvent(
                EVENT_SYSTEM_ALERT,
                self.hwnd,
                OBJID_WINDOW.0,
                i32::try_from(CHILDID_SELF).unwrap_or(0),
            );
        }
    }

    /// Keeps a visible tab above windows that became topmost after it.
    fn raise(&self) {
        self.button.raise();
        if self.visible {
            // SAFETY: This thread owns the HWND; the call changes only z-order, without
            // activating the window or changing its size or position.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                )
            };
        }
    }

    fn present(&mut self, alpha: u8) -> io::Result<()> {
        let Some(tab) = self.tab.as_ref() else {
            return Ok(());
        };
        tab.surface.present(self.hwnd, self.origin, alpha)?;
        if !self.visible {
            // SAFETY: This thread owns the HWND; the call shows it above other topmost windows
            // without activating it and keeps the size and position UpdateLayeredWindow set.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                )
            }
            .map_err(|error| windows_error("SetWindowPos", &error))?;
            self.visible = true;
        }
        Ok(())
    }

    fn hide(&mut self) {
        self.button.place(None);
        if self.visible {
            // SAFETY: Hiding a window owned by this thread retains no pointers.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.visible = false;
        }
    }
}

impl Drop for TabWindow {
    fn drop(&mut self) {
        // Destroying the owner destroys the button's window with it; the button then only
        // releases its font.
        // SAFETY: `hwnd` is owned by this value and is destroyed exactly once.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Centred on the display's top edge, in signed virtual-desktop pixels.
fn tab_origin(monitor: Monitor, width: i32) -> POINT {
    POINT {
        x: monitor.left + (monitor.width() - width) / 2,
        y: monitor.top,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    palette: Palette,
    text_scale: f32,
    animated: bool,
}

impl Settings {
    fn read() -> Self {
        Self {
            palette: Palette::system(),
            text_scale: text_scale(),
            animated: animations_enabled(),
        }
    }
}

/// A whole-window fade, answered to a session starting or ending.
#[derive(Clone, Copy)]
struct Fade {
    started: Instant,
    from: f32,
    to: f32,
    duration: Duration,
}

impl Fade {
    fn alpha(self, now: Instant, animated: bool) -> f32 {
        let progress = (now.saturating_duration_since(self.started).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        // A finished fade lands exactly on its target, which the alert waits for.
        if !animated || progress >= 1.0 {
            return self.to;
        }
        // The same smoothstep easing the helper uses for the lamp.
        let eased = progress * progress * (3.0 - 2.0 * progress);
        self.from + (self.to - self.from) * eased
    }

    fn done(self, now: Instant, animated: bool) -> bool {
        !animated || now.saturating_duration_since(self.started) >= self.duration
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn alpha_byte(alpha: f32) -> u8 {
    // Clamped to 0..=255 first, so the conversion is exact.
    (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// What the tab windows are doing between two views.
#[derive(Clone, Copy)]
enum Phase {
    Steady,
    FadingIn(Fade),
    FadingOut(Fade),
}

/// Paints and presents the tab on every display. Every change of message, including between two
/// lines and one row, is painted at once at full strength. Only appearing and disappearing fade,
/// over [`TAB_FADE`], and only with Windows animations on. While a fade runs the tab leaves its
/// lamp rows clear, so its copy of the lamp never doubles the helper's lamp.
struct Presenter {
    painter: Painter,
    shown: Option<TabView>,
    requested: Option<TabView>,
    phase: Phase,
    alpha: f32,
    /// A screen-reader alert waiting until the tab is fully on screen.
    alert_pending: bool,
    #[cfg(test)]
    alerts: usize,
}

impl Presenter {
    fn new(painter: Painter) -> Self {
        Self {
            painter,
            shown: None,
            requested: None,
            phase: Phase::Steady,
            alpha: 0.0,
            alert_pending: false,
            #[cfg(test)]
            alerts: 0,
        }
    }

    fn fading(&self) -> bool {
        !matches!(self.phase, Phase::Steady)
    }

    /// Whether the Stop button is shown: only under a steady tab that says an agent owns the PC.
    /// Stopping, a possibly held key, a handback and reading the screen never offer it, because
    /// there is nothing left to stop or no session to stop.
    fn stop_button_offered(&self) -> bool {
        !self.fading() && self.shown.is_some_and(|view| stop_button_for(view.kind))
    }

    fn paint(
        &self,
        tabs: &mut [TabWindow],
        view: TabView,
        settings: &Settings,
        with_lamp: bool,
    ) -> io::Result<()> {
        for tab in tabs {
            tab.paint(&self.painter, view, settings, with_lamp)?;
        }
        Ok(())
    }

    fn present(&self, tabs: &mut [TabWindow]) -> io::Result<()> {
        for tab in tabs {
            tab.present(alpha_byte(self.alpha))?;
        }
        Ok(())
    }

    fn update(
        &mut self,
        tabs: &mut [TabWindow],
        view: Option<TabView>,
        settings: &Settings,
        repaint: bool,
        now: Instant,
    ) -> io::Result<()> {
        let animated = settings.animated;
        if view != self.requested {
            self.alert_pending |= view.is_some_and(|next| announces(self.requested, next));
            self.requested = view;
            let fade = |from, to| Fade {
                started: now,
                from,
                to,
                duration: TAB_FADE,
            };
            match view {
                None => {
                    self.alert_pending = false;
                    if let Some(shown) = self.shown {
                        self.paint(tabs, shown, settings, !animated)?;
                        self.phase = Phase::FadingOut(fade(self.alpha, 0.0));
                    }
                }
                // Stopping and a possibly held key show at once at full strength, cancelling any
                // fade in progress; with animations off, everything does.
                Some(next) if urgent(next) || !animated => {
                    self.paint(tabs, next, settings, true)?;
                    self.shown = Some(next);
                    self.alpha = 1.0;
                    self.phase = Phase::Steady;
                }
                Some(next) if self.shown.is_none() || matches!(self.phase, Phase::FadingOut(_)) => {
                    self.paint(tabs, next, settings, false)?;
                    self.shown = Some(next);
                    // The first frame presented is already at the lamp's starting opacity.
                    self.alpha = self.alpha.max(FADE_IN_FROM);
                    self.phase = Phase::FadingIn(fade(self.alpha, 1.0));
                }
                // A new message replaces the old one at once, at the current strength.
                Some(next) => {
                    let steady = matches!(self.phase, Phase::Steady);
                    self.paint(tabs, next, settings, steady)?;
                    self.shown = Some(next);
                }
            }
            self.present(tabs)?;
        } else if repaint && let Some(shown) = self.shown {
            self.paint(tabs, shown, settings, !self.fading())?;
            self.present(tabs)?;
        }
        match self.phase {
            Phase::Steady => {}
            Phase::FadingIn(fade) => {
                self.alpha = fade.alpha(now, animated);
                if fade.done(now, animated) {
                    self.phase = Phase::Steady;
                    if let Some(shown) = self.shown {
                        self.paint(tabs, shown, settings, true)?;
                    }
                }
                self.present(tabs)?;
            }
            Phase::FadingOut(fade) => {
                self.alpha = fade.alpha(now, animated);
                self.present(tabs)?;
                if fade.done(now, animated) {
                    self.shown = None;
                    self.phase = Phase::Steady;
                    tabs.iter_mut().for_each(TabWindow::hide);
                }
            }
        }
        // Announce only once the tab is fully on screen, carrying its new name.
        if self.alert_pending
            && self.shown.is_some()
            && matches!(self.phase, Phase::Steady)
            && self.alpha >= 1.0
        {
            self.alert_pending = false;
            if let Some(first) = tabs.first() {
                first.alert();
                #[cfg(test)]
                {
                    self.alerts += 1;
                }
            }
        }
        Ok(())
    }
}

/// The tab messages that go with a Stop button.
const fn stop_button_for(kind: TabKind) -> bool {
    matches!(kind, TabKind::Working | TabKind::Waiting)
}

/// Stopping and a possibly held key never wait behind a fade.
fn urgent(view: TabView) -> bool {
    matches!(view.kind, TabKind::Stopping | TabKind::InputHeld)
}

/// Whether a change of view is worth a screen-reader announcement: a session starting, stopping,
/// a possible held key, or control handed back. Working and waiting, and the switch between two
/// lines and one row, change only the window name.
fn announces(previous: Option<TabView>, next: TabView) -> bool {
    let previous = previous.map(|view| view.kind);
    match next.kind {
        // A session announces itself only when it also introduces itself on screen, so a run of
        // one-shot actions within a minute of each other is not announced again and again.
        TabKind::Working | TabKind::Waiting => {
            next.expanded
                && matches!(
                    previous,
                    None | Some(TabKind::HandedBack { .. } | TabKind::Reading)
                )
        }
        TabKind::Stopping => previous != Some(TabKind::Stopping),
        TabKind::InputHeld => previous != Some(TabKind::InputHeld),
        // Reading the screen is a quiet rename: it claims nothing and takes nothing.
        TabKind::Reading => false,
        // A normal end is a quiet rename; the handback after a stop is announced.
        TabKind::HandedBack { after_stop } => {
            after_stop && !matches!(previous, Some(TabKind::HandedBack { .. }))
        }
    }
}

/// Moves the tab windows to the virtual desktop of the foreground window, as the glow helper does
/// for the lamp, so a desktop switch never leaves the lamp without its stop instruction.
fn follow_foreground_desktop(
    manager: &IVirtualDesktopManager,
    current: &mut Option<GUID>,
    tabs: &[TabWindow],
) {
    // SAFETY: Takes no pointers and returns a Windows-owned handle or null.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() {
        return;
    }
    // SAFETY: COM is initialized on this thread and `foreground` is a Windows-owned handle; a
    // window that closed in between only makes the call fail.
    let Ok(desktop) = (unsafe { manager.GetWindowDesktopId(foreground) }) else {
        return;
    };
    if *current == Some(desktop) || desktop == GUID::zeroed() {
        return;
    }
    let mut moved = true;
    for tab in tabs {
        // SAFETY: The manager is live and `tab.hwnd` is owned by this thread and process.
        moved &= unsafe { manager.MoveWindowToDesktop(tab.hwnd, &raw const desktop) }.is_ok();
    }
    // Remember the desktop only once every tab is on it, so a failed move is retried.
    if moved {
        *current = Some(desktop);
    }
}

fn run(
    controller: &StopController,
    signals: &SessionSignals,
    exiting: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
    started: &Cell<bool>,
) -> io::Result<()> {
    let _com = ComApartment::initialize().map_err(|error| io::Error::other(error.to_string()))?;
    // Without the shell's manager the tab still works; it just cannot follow desktop switches.
    let desktops = virtual_desktop_manager().ok();
    let presenter = Presenter::new(Painter::new()?);
    let mut native = NativeStop::create()?;
    native.sync_monitors(&enumerate_monitors()?)?;
    // Indicator changes post to this thread so the tab answers them at once, not at its next poll.
    // SAFETY: Takes no arguments and returns this thread's ID.
    signals
        .ui_thread
        .store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    started.set(true);
    let _ = ready.send(Ok(()));
    let result = tab_loop(
        controller,
        signals,
        exiting,
        native,
        presenter,
        desktops.as_ref(),
    );
    signals.ui_thread.store(0, Ordering::Release);
    result
}

fn tab_loop(
    controller: &StopController,
    signals: &SessionSignals,
    exiting: &AtomicBool,
    mut native: NativeStop,
    mut presenter: Presenter,
    desktops: Option<&IVirtualDesktopManager>,
) -> io::Result<()> {
    let mut desktop = None;
    let mut model = TabModel::default();
    let mut settings = Settings::read();
    let mut next_settings = Instant::now() + IDLE_SETTINGS_INTERVAL;
    let mut was_shown = false;
    let mut seen_observations = controller.observation_count();
    let mut reading_until: Option<Instant> = None;
    while !exiting.load(Ordering::Acquire) {
        drain_messages();
        let now = Instant::now();
        let mut repaint = false;
        let observations = controller.observation_count();
        if observations != seen_observations {
            seen_observations = observations;
            reading_until = Some(now + READING);
        }
        let reading = reading_until.is_some_and(|until| now < until);
        // The Esc hint is offered only while the hook is confirmed installed.
        repaint |= presenter
            .painter
            .set_esc_hint(controller.esc_stop_available());
        if now >= next_settings || SETTINGS_CHANGED.with(|flag| flag.replace(false)) {
            let fresh = Settings::read();
            repaint |= fresh != settings;
            settings = fresh;
            if native.sync_monitors(&enumerate_monitors()?)? {
                repaint = true;
                desktop = None;
            }
            if let Some(manager) = desktops {
                follow_foreground_desktop(manager, &mut desktop, &native.tabs);
            }
            native.tabs.iter().for_each(TabWindow::raise);
            next_settings = now + settings_interval(was_shown);
        }
        let view = model.view(
            now,
            TabInputs {
                session_active: controller.session_active(),
                stop: StopStatus::parse(controller.status()),
                activity: signals.activity(),
                actions: signals.actions(),
                elevated: signals.elevated,
                reading,
            },
        );
        // A tab about to appear first moves to the desktop the user is looking at, and from then
        // on the shell is followed at the shown interval.
        if view.is_some() && !was_shown {
            next_settings = next_settings.min(now + SETTINGS_INTERVAL);
            if let Some(manager) = desktops {
                follow_foreground_desktop(manager, &mut desktop, &native.tabs);
            }
        }
        was_shown = view.is_some();
        presenter.update(&mut native.tabs, view, &settings, repaint, now)?;
        // The Stop button is offered with a tab that says an agent owns the PC, once it is fully
        // on screen, and never with any other message.
        let offered = presenter.stop_button_offered();
        for tab in &mut native.tabs {
            tab.place_stop_button(offered);
        }
        let timeout = wait_timeout_ms(
            presenter.fading(),
            was_shown,
            next_settings.saturating_duration_since(now),
        );
        wait_for_input(timeout)
            .map_err(|error| windows_error("MsgWaitForMultipleObjectsEx", &error))?;
    }
    Ok(())
}

/// How often settings and the foreground desktop are re-read.
const fn settings_interval(shown: bool) -> Duration {
    if shown {
        SETTINGS_INTERVAL
    } else {
        IDLE_SETTINGS_INTERVAL
    }
}

/// How long the tab thread sleeps before it must look again. While a tab is fading, shown or
/// about to change on a timer (the waiting delay, the handback, the reading state), it paces
/// those timers. With nothing shown there is no timer: the thread sleeps until the next settings
/// check, and a session, stop, observation or Windows message wakes it earlier.
fn wait_timeout_ms(fading: bool, shown: bool, until_settings: Duration) -> u32 {
    if fading {
        FADE_FRAME_MS
    } else if shown {
        POLL_INTERVAL_MS
    } else {
        u32::try_from(until_settings.as_millis())
            .unwrap_or(u32::MAX)
            .max(1)
    }
}

/// What ended a [`wait_for_input`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// Input, a posted message or a sent message (such as a hook callback) needs this thread.
    Input,
    /// The poll interval elapsed with nothing to dispatch.
    TimedOut,
}

/// Blocks until this thread has queue input or `timeout_ms` elapses. Low-level keyboard hook
/// calls arrive as sent messages and run inside this wait, so keystrokes are answered at once
/// instead of after a sleep. `MWMO_INPUTAVAILABLE` also returns for messages that an earlier
/// peek already saw but did not remove, which a plain wait would sleep through. The timeout is
/// only for session, state and fade polling, so an idle thread wakes `1000 / timeout_ms` times
/// per second.
fn wait_for_input(timeout_ms: u32) -> windows::core::Result<WaitOutcome> {
    // SAFETY: No handles are passed; the call only waits on this thread's message queue.
    let result =
        unsafe { MsgWaitForMultipleObjectsEx(None, timeout_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    if result == WAIT_TIMEOUT {
        Ok(WaitOutcome::TimedOut)
    } else if result == WAIT_FAILED {
        Err(windows::core::Error::from_thread())
    } else {
        Ok(WaitOutcome::Input)
    }
}

fn consume_escape(
    controller: &StopController,
    key: u32,
    injected: bool,
    down: bool,
    up: bool,
    state: &Cell<EscapeState>,
) -> bool {
    if key != u32::from(VK_ESCAPE.0) || injected {
        return false;
    }
    if down {
        if state.get() == EscapeState::Released {
            state.set(if controller.stop_by_user() {
                EscapeState::Consumed
            } else {
                EscapeState::PassedThrough
            });
        }
        return state.get() == EscapeState::Consumed;
    }
    if up {
        return state.replace(EscapeState::Released) == EscapeState::Consumed;
    }
    false
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        // SAFETY: HC_ACTION for WH_KEYBOARD_LL supplies a valid KBDLLHOOKSTRUCT
        // for this callback only. Read key/flags without retaining keyboard data.
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let handled = CONTROLLER.with(|slot| {
            slot.borrow().as_ref().is_some_and(|controller| {
                ESC_STATE.with(|state| {
                    consume_escape(
                        controller,
                        key.vkCode,
                        key.flags.contains(LLKHF_INJECTED),
                        wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize,
                        wparam.0 == WM_KEYUP as usize || wparam.0 == WM_SYSKEYUP as usize,
                        state,
                    )
                })
            })
        });
        if handled {
            return LRESULT(1);
        }
    }
    // SAFETY: Forward unchanged callback arguments for unhandled/injected keys.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => return LRESULT(0),
        // Colours, text size and displays are read on the UI loop; ask it to re-read them now.
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED | WM_DISPLAYCHANGE
        | WM_DPICHANGED => SETTINGS_CHANGED.with(|flag| flag.set(true)),
        WM_NCHITTEST => return LRESULT(isize::try_from(HTTRANSPARENT).unwrap_or(-1)),
        WM_MOUSEACTIVATE => return LRESULT(isize::try_from(MA_NOACTIVATE).unwrap_or(3)),
        // The Stop button reports a click here. Injected clicks never get this far: the button's
        // subclass drops them before the button sees them.
        WM_COMMAND if is_button_click(wparam, lparam) => {
            invoke_stop();
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: Forward unchanged Win32 callback arguments to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// A `WM_COMMAND` from a control (non-null `lparam`) with the `BN_CLICKED` notification.
fn is_button_click(wparam: WPARAM, lparam: LPARAM) -> bool {
    let notification = (wparam.0 >> 16) & 0xffff;
    lparam.0 != 0 && u32::try_from(notification) == Ok(BN_CLICKED)
}

fn windows_error(context: &str, error: &windows::core::Error) -> io::Error {
    io::Error::other(format!("{context} failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::super::indicator::{TabKind, TabView};
    use super::*;
    use windows::Win32::{
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{
            GetWindowDisplayAffinity, GetWindowRect, PM_NOREMOVE, PostThreadMessageW, SendMessageW,
            WM_APP, WindowFromPoint,
        },
    };

    /// Tests that create the tab windows register their window class, which a process can hold
    /// only once at a time; the server never creates two.
    fn window_class_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn tab_is_centred_on_each_display_even_at_negative_origins() {
        for (left, right) in [(-1920, 0), (0, 2560), (-3840, -1920)] {
            let monitor = Monitor {
                left,
                top: -1080,
                right,
                bottom: 0,
                dpi: 144,
            };
            let origin = tab_origin(monitor, 600);
            assert_eq!(origin.x * 2 + 600, monitor.left + monitor.right);
            assert_eq!(origin.y, monitor.top, "the tab hangs from the top edge");
        }
    }

    #[test]
    fn session_signals_report_the_last_acknowledged_level() {
        let signals = SessionSignals::new(true);
        assert_eq!(signals.activity(), Activity::Idle);
        signals.set_acting(true);
        assert_eq!(signals.activity(), Activity::Working);
        signals.set_acting(false);
        assert_eq!(signals.activity(), Activity::Waiting);
        signals.clear();
        assert_eq!(signals.activity(), Activity::Idle);
        assert!(signals.elevated);
    }

    #[test]
    fn screen_readers_hear_starts_stops_and_warnings_but_not_every_action() {
        let view = |kind, expanded| TabView {
            kind,
            admin: false,
            expanded,
        };
        let working = view(TabKind::Working, true);
        let waiting = view(TabKind::Waiting, false);
        assert!(announces(None, working), "a session starting");
        assert!(!announces(Some(working), waiting), "working and waiting");
        assert!(!announces(Some(waiting), view(TabKind::Working, false)));
        assert!(
            !announces(Some(working), view(TabKind::Working, false)),
            "one row"
        );
        assert!(announces(Some(waiting), view(TabKind::Stopping, true)));
        assert!(!announces(
            Some(view(TabKind::Stopping, true)),
            view(TabKind::Stopping, true)
        ));
        assert!(announces(
            Some(view(TabKind::Stopping, true)),
            view(TabKind::InputHeld, true)
        ));
        assert!(!announces(
            Some(view(TabKind::InputHeld, true)),
            view(TabKind::InputHeld, false)
        ));
        let back = view(TabKind::HandedBack { after_stop: false }, false);
        assert!(
            !announces(Some(waiting), back),
            "a normal end is a quiet rename"
        );
        let stopped_back = view(TabKind::HandedBack { after_stop: true }, true);
        assert!(announces(Some(view(TabKind::Stopping, true)), stopped_back));
        assert!(
            !announces(Some(back), view(TabKind::Working, false)),
            "a one-shot run within a minute is not announced again"
        );
        assert!(
            announces(Some(back), working),
            "a new session after a handback"
        );
    }

    #[test]
    fn messages_change_in_place_and_safety_messages_show_at_once_at_full_strength() {
        let _windows = window_class_lock();
        super::super::ensure_dpi_awareness().unwrap();
        let mut native = NativeStop::create().unwrap();
        native
            .sync_monitors(&[enumerate_monitors().unwrap().remove(0)])
            .unwrap();
        let mut presenter = Presenter::new(Painter::new().unwrap());
        let settings = Settings {
            palette: Palette::STANDARD,
            text_scale: 1.0,
            animated: true,
        };
        let view = |kind, expanded| TabView {
            kind,
            admin: false,
            expanded,
        };
        let start = Instant::now();
        let mut update = |presenter: &mut Presenter, view, at| {
            presenter
                .update(&mut native.tabs, Some(view), &settings, false, at)
                .unwrap();
        };
        update(&mut presenter, view(TabKind::Working, true), start);
        assert!(presenter.fading(), "a new tab fades in");
        assert!(
            (presenter.alpha - FADE_IN_FROM).abs() < f32::EPSILON,
            "from the lamp's starting opacity, so they arrive together"
        );
        assert!(
            presenter.alert_pending,
            "and is announced only once fully shown"
        );

        let midway = start + TAB_FADE / 2;
        update(&mut presenter, view(TabKind::Working, false), midway);
        assert!(
            matches!(presenter.phase, Phase::FadingIn(_)),
            "one row replaces two lines in place, without a dip"
        );

        update(&mut presenter, view(TabKind::Stopping, true), midway);
        assert!(!presenter.fading(), "stopping cancels the fade");
        assert!(
            (presenter.alpha - 1.0).abs() < f32::EPSILON,
            "at full strength"
        );
        assert!(
            !presenter.alert_pending,
            "announced as soon as it is fully shown"
        );
        assert!(native.tabs[0].name.starts_with("Stopping desktop control"));
        assert!(urgent(view(TabKind::InputHeld, false)));
        assert!(!urgent(view(TabKind::Waiting, false)));
    }

    #[test]
    fn a_new_session_fades_in_fully_and_is_announced_exactly_once() {
        let _windows = window_class_lock();
        super::super::ensure_dpi_awareness().unwrap();
        let mut native = NativeStop::create().unwrap();
        native
            .sync_monitors(&[enumerate_monitors().unwrap().remove(0)])
            .unwrap();
        let mut presenter = Presenter::new(Painter::new().unwrap());
        let settings = Settings {
            palette: Palette::STANDARD,
            text_scale: 1.0,
            animated: true,
        };
        let view = |kind, expanded| TabView {
            kind,
            admin: false,
            expanded,
        };
        let start = Instant::now();
        let mut update = |presenter: &mut Presenter, view, millis| {
            presenter
                .update(
                    &mut native.tabs,
                    Some(view),
                    &settings,
                    false,
                    start + Duration::from_millis(millis),
                )
                .unwrap();
        };
        let introduced = view(TabKind::Working, true);
        update(&mut presenter, introduced, 0);
        update(&mut presenter, introduced, 75);
        assert!(presenter.fading());
        assert_eq!(presenter.alerts, 0, "not announced while fading in");
        update(&mut presenter, introduced, 150);
        assert!(!presenter.fading(), "the fade ends on time");
        assert!((presenter.alpha - 1.0).abs() < f32::EPSILON);
        assert_eq!(presenter.alerts, 1, "announced once fully shown");
        update(&mut presenter, view(TabKind::Working, false), 3_000);
        update(&mut presenter, view(TabKind::Waiting, false), 4_000);
        update(&mut presenter, view(TabKind::Working, false), 4_100);
        assert_eq!(
            presenter.alerts, 1,
            "settling into one row and working or waiting only rename the window"
        );
        assert!(presenter.alpha >= 1.0, "and never fade the tab");
    }

    #[test]
    fn short_actions_and_session_changes_wake_the_tab_thread() {
        let signals = SessionSignals::new(false);
        // SAFETY: Takes no arguments and returns this thread's ID.
        signals
            .ui_thread
            .store(unsafe { GetCurrentThreadId() }, Ordering::Release);
        let mut message = MSG::default();
        // SAFETY: `message` is writable and this test thread owns its queue.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}
        signals.set_acting(true);
        signals.set_acting(false);
        assert_eq!(
            signals.actions(),
            1,
            "the action is counted even though it ended"
        );
        assert_eq!(signals.activity(), Activity::Waiting);
        let started = Instant::now();
        assert_eq!(wait_for_input(5_000).unwrap(), WaitOutcome::Input);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "woken, not polled"
        );
        // SAFETY: Remove the wake-up messages so the queue is left empty.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}
        signals.ui_thread.store(0, Ordering::Release);
    }

    #[test]
    fn fades_finish_at_once_when_animations_are_off() {
        let started = Instant::now();
        let fade = Fade {
            started,
            from: 0.0,
            to: 1.0,
            duration: TAB_FADE,
        };
        assert!((fade.alpha(started, false) - 1.0).abs() < f32::EPSILON);
        assert!(fade.done(started, false));
        assert!(fade.alpha(started, true) < 0.01);
        assert!(!fade.done(started, true));
        assert!((fade.alpha(started + TAB_FADE, true) - 1.0).abs() < f32::EPSILON);
        assert_eq!(alpha_byte(2.0), 255);
        assert_eq!(alpha_byte(-1.0), 0);
    }

    #[test]
    fn input_wait_blocks_for_the_interval_then_wakes_for_pending_messages() {
        // Create this thread's queue and make sure nothing is pending.
        let mut message = MSG::default();
        // SAFETY: `message` is writable and this test thread owns its queue.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}

        let started = std::time::Instant::now();
        assert_eq!(wait_for_input(60).unwrap(), WaitOutcome::TimedOut);
        assert!(started.elapsed() >= std::time::Duration::from_millis(40));

        // SAFETY: Posting a plain-value message to this thread's own queue.
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP, WPARAM(0), LPARAM(0)) }.unwrap();
        // A peek without removal marks the message as seen. A plain wait would sleep through
        // it; MWMO_INPUTAVAILABLE must still return at once.
        // SAFETY: Same writable message and owned queue; PM_NOREMOVE leaves it queued.
        assert!(unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_NOREMOVE) }.as_bool());
        let started = std::time::Instant::now();
        assert_eq!(wait_for_input(5_000).unwrap(), WaitOutcome::Input);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // SAFETY: Remove the test message so the queue is left empty.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}
    }

    #[test]
    fn point_targeting_skips_the_disabled_session_tab() {
        let _windows = window_class_lock();
        super::super::ensure_dpi_awareness().unwrap();
        let mut native = NativeStop::create().unwrap();
        let monitor = enumerate_monitors().unwrap().remove(0);
        native.sync_monitors(&[monitor]).unwrap();
        let painter = Painter::new().unwrap();
        let tab = &mut native.tabs[0];
        tab.paint(
            &painter,
            TabView {
                kind: TabKind::Working,
                admin: false,
                expanded: true,
            },
            &Settings {
                palette: Palette::STANDARD,
                text_scale: 1.0,
                animated: false,
            },
            true,
        )
        .unwrap();
        tab.present(255).unwrap();
        let mut rect = windows::Win32::Foundation::RECT::default();
        // SAFETY: `rect` is writable for the synchronous call and the window is live.
        unsafe { GetWindowRect(tab.hwnd, &raw mut rect) }.unwrap();
        assert_eq!(
            rect.top, monitor.top,
            "the tab hangs from the display's top edge"
        );
        let mut affinity = 0_u32;
        // SAFETY: `affinity` is writable for the synchronous call and the window is live.
        unsafe { GetWindowDisplayAffinity(tab.hwnd, &raw mut affinity) }.unwrap();
        assert_eq!(
            affinity, WDA_EXCLUDEFROMCAPTURE.0,
            "screenshots never show the tab"
        );
        for point in [
            POINT {
                x: i32::midpoint(rect.left, rect.right),
                y: rect.top + 1,
            },
            POINT {
                x: i32::midpoint(rect.left, rect.right),
                y: i32::midpoint(rect.top, rect.bottom),
            },
        ] {
            // SAFETY: `point` is a plain value and the returned HWND is only compared.
            let target = unsafe { WindowFromPoint(point) };
            assert_ne!(target, tab.hwnd);
            // SAFETY: Same value-only hit test; the tab must also answer as transparent.
            let hit = unsafe {
                SendMessageW(
                    tab.hwnd,
                    WM_NCHITTEST,
                    Some(WPARAM(0)),
                    Some(LPARAM(
                        (isize::try_from(point.y).unwrap() << 16)
                            | isize::try_from(point.x).unwrap(),
                    )),
                )
            };
            assert_eq!(hit.0, isize::try_from(HTTRANSPARENT).unwrap());
        }
        // SAFETY: Same live window; MA_NOACTIVATE keeps a physical click from focusing the tab.
        let activate =
            unsafe { SendMessageW(tab.hwnd, WM_MOUSEACTIVATE, Some(WPARAM(0)), Some(LPARAM(0))) };
        assert_eq!(activate.0, isize::try_from(MA_NOACTIVATE).unwrap());
    }

    #[test]
    fn only_physical_escape_in_an_owned_session_stops_and_consumes_key_pair() {
        let stop = StopController::default();
        let consumed = Cell::new(EscapeState::Released);
        let esc = u32::from(VK_ESCAPE.0);
        assert!(!consume_escape(&stop, esc, false, true, false, &consumed));
        stop.set_session_active(true);
        // Repeats from a press before ownership, and its release, pass through.
        assert!(!consume_escape(&stop, esc, false, true, false, &consumed));
        assert!(!consume_escape(&stop, esc, false, false, true, &consumed));
        assert!(!consume_escape(&stop, esc, true, true, false, &consumed));
        assert!(!consume_escape(&stop, 65, false, true, false, &consumed));
        assert!(!stop.is_stopped());
        assert!(consume_escape(&stop, esc, false, true, false, &consumed));
        assert!(stop.user_stopped());
        stop.set_session_active(false);
        assert!(consume_escape(&stop, esc, false, true, false, &consumed));
        assert!(consume_escape(&stop, esc, false, false, true, &consumed));
        assert!(!consume_escape(&stop, esc, false, false, true, &consumed));
    }

    #[derive(Clone, Default)]
    struct HookCounts {
        attempts: Arc<AtomicU32>,
        installs: Arc<AtomicU32>,
        removals: Arc<AtomicU32>,
        fail: Arc<AtomicBool>,
    }

    struct FakeHook(HookCounts);

    impl KeyboardHook for FakeHook {
        fn install(&mut self) -> io::Result<()> {
            self.0.attempts.fetch_add(1, Ordering::SeqCst);
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(io::Error::other("hook refused"));
            }
            self.0.installs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn uninstall(&mut self) {
            self.0.removals.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn eventually(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_keyboard_hook_exists_only_while_a_session_is_owned() {
        let controller = StopController::default();
        let counts = HookCounts::default();
        let monitor = EscapeMonitor::start(&controller, {
            let counts = counts.clone();
            move || Box::new(FakeHook(counts))
        })
        .unwrap();
        // No session: nothing is hooked, however long the server has been running.
        thread::sleep(Duration::from_millis(150));
        assert_eq!(counts.attempts.load(Ordering::SeqCst), 0);
        assert!(!controller.esc_stop_available());

        controller.set_session_active(true);
        eventually("the hook to be installed", || {
            controller.esc_stop_available()
        });
        assert_eq!(counts.installs.load(Ordering::SeqCst), 1);

        controller.set_session_active(false);
        eventually("the hook to be removed", || {
            !controller.esc_stop_available()
        });
        assert_eq!(counts.removals.load(Ordering::SeqCst), 1);

        controller.set_session_active(true);
        eventually("the hook to be installed again", || {
            controller.esc_stop_available()
        });
        assert_eq!(counts.installs.load(Ordering::SeqCst), 2);
        // Shutting down with a session still owned removes the hook too.
        drop(monitor);
        assert_eq!(counts.removals.load(Ordering::SeqCst), 2);
        assert!(!controller.esc_stop_available());
    }

    #[test]
    fn a_hook_that_cannot_be_installed_is_reported_once_and_retried_next_session() {
        let controller = StopController::default();
        let counts = HookCounts::default();
        counts.fail.store(true, Ordering::SeqCst);
        let monitor = EscapeMonitor::start(&controller, {
            let counts = counts.clone();
            move || Box::new(FakeHook(counts))
        })
        .unwrap();
        controller.set_session_active(true);
        eventually("the first attempt", || {
            counts.attempts.load(Ordering::SeqCst) == 1
        });
        // Other changes during the session do not retry in a loop.
        controller.note_observation();
        controller.note_observation();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(counts.attempts.load(Ordering::SeqCst), 1);
        assert!(
            !controller.esc_stop_available(),
            "the tab is told Esc is not available"
        );

        controller.set_session_active(false);
        counts.fail.store(false, Ordering::SeqCst);
        // Give the monitor time to see the session end before the next one begins.
        thread::sleep(Duration::from_millis(150));
        controller.set_session_active(true);
        eventually("the retry", || controller.esc_stop_available());
        assert_eq!(counts.installs.load(Ordering::SeqCst), 1);
        drop(monitor);
    }

    #[test]
    fn the_hook_decision_covers_every_state() {
        use HookAction::{EndSession, Install, Uninstall, Wait};
        for (owned, installed, failed, expected) in [
            (true, false, false, Install),
            (true, true, false, Wait),
            (true, false, true, Wait),
            (false, true, false, Uninstall),
            (false, false, true, EndSession),
            (false, false, false, Wait),
        ] {
            assert_eq!(
                hook_action(owned, installed, failed),
                expected,
                "owned {owned}, installed {installed}, failed {failed}"
            );
        }
    }

    #[test]
    fn an_idle_tab_thread_sleeps_until_its_safety_check_not_in_short_polls() {
        let long = Duration::from_secs(2);
        assert_eq!(wait_timeout_ms(false, false, long), 2_000);
        assert_eq!(wait_timeout_ms(false, false, Duration::ZERO), 1);
        assert_eq!(wait_timeout_ms(false, true, long), POLL_INTERVAL_MS);
        assert_eq!(wait_timeout_ms(true, false, long), FADE_FRAME_MS);
        assert_eq!(settings_interval(false), IDLE_SETTINGS_INTERVAL);
        assert_eq!(settings_interval(true), SETTINGS_INTERVAL);
    }

    #[test]
    fn the_stop_button_fits_the_display_and_text_size_and_stays_on_the_display() {
        let at_100 = button_metrics(96, 1.0);
        assert_eq!((at_100.width, at_100.height), (104, 32));
        let at_200 = button_metrics(192, 1.0);
        assert_eq!((at_200.width, at_200.height), (208, 64));
        let big_text = button_metrics(96, 2.0);
        assert_eq!((big_text.width, big_text.height), (208, 64));
        assert_eq!(button_metrics(96, 9.0), button_metrics(96, 2.25));

        let monitor = Monitor {
            left: -1920,
            top: -300,
            right: 0,
            bottom: 780,
            dpi: 96,
        };
        let origin = button_origin(monitor, -1260, 600, 80, at_100).unwrap();
        assert_eq!(
            origin.y,
            monitor.top + 80 + at_100.gap,
            "under the tab body"
        );
        assert_eq!(origin.x, -1260 + (600 - 104) / 2, "centred under the tab");
        let edge = button_origin(monitor, -40, 600, 80, at_100).unwrap();
        assert_eq!(
            edge.x,
            monitor.right - at_100.width,
            "never off the display"
        );
        let short = Monitor {
            bottom: monitor.top + 100,
            ..monitor
        };
        assert!(button_origin(short, -1260, 600, 80, at_100).is_none());
    }

    #[test]
    fn only_a_person_can_press_the_stop_button() {
        use windows::Win32::UI::Input::{IMO_HARDWARE, IMO_SYSTEM, IMO_UNAVAILABLE};
        let from = |origin| {
            input_is_from_a_person(Ok(INPUT_MESSAGE_SOURCE {
                originId: origin,
                ..Default::default()
            }))
        };
        assert!(from(IMO_HARDWARE), "mouse, touch and pen hardware");
        assert!(from(IMO_SYSTEM));
        assert!(from(IMO_UNAVAILABLE), "not from the input queue");
        assert!(!from(IMO_INJECTED), "SendInput and injected touch");
        assert!(
            !input_is_from_a_person(Err(windows::core::Error::from_hresult(
                windows::core::HRESULT(-1)
            ))),
            "an unknown origin is treated as injected"
        );
        for message in [WM_LBUTTONDOWN, WM_LBUTTONUP, WM_KEYDOWN, WM_CHAR] {
            assert!(is_person_only_message(message));
        }
        assert!(!is_person_only_message(BM_CLICK));
        assert!(!is_person_only_message(WM_MOUSEACTIVATE));
    }

    #[test]
    fn a_command_is_a_click_only_from_a_control_with_the_clicked_notification() {
        assert!(is_button_click(WPARAM(BN_CLICKED as usize), LPARAM(1)));
        assert!(!is_button_click(WPARAM(BN_CLICKED as usize), LPARAM(0)));
        assert!(!is_button_click(WPARAM(1 << 16), LPARAM(1)));
    }

    #[test]
    fn the_stop_button_is_offered_only_with_a_steady_session_tab() {
        let mut presenter = Presenter::new(Painter::new().unwrap());
        let settings = Settings {
            palette: Palette::STANDARD,
            text_scale: 1.0,
            animated: false,
        };
        let view = |kind| TabView {
            kind,
            admin: false,
            expanded: false,
        };
        let show = |presenter: &mut Presenter, kind| {
            presenter
                .update(&mut [], Some(view(kind)), &settings, false, Instant::now())
                .unwrap();
            presenter.stop_button_offered()
        };
        assert!(!presenter.stop_button_offered(), "nothing shown");
        assert!(show(&mut presenter, TabKind::Working));
        assert!(show(&mut presenter, TabKind::Waiting));
        assert!(!show(&mut presenter, TabKind::Stopping));
        assert!(!show(&mut presenter, TabKind::InputHeld));
        assert!(!show(
            &mut presenter,
            TabKind::HandedBack { after_stop: false }
        ));
        assert!(
            !show(&mut presenter, TabKind::Reading),
            "reading the screen owns nothing to stop"
        );
        presenter
            .update(&mut [], None, &settings, false, Instant::now())
            .unwrap();
        assert!(!presenter.stop_button_offered());
    }

    #[test]
    fn the_esc_hint_is_painted_only_while_the_hook_is_confirmed() {
        let painter = Painter::new().unwrap();
        let view = TabView {
            kind: TabKind::Working,
            admin: false,
            expanded: true,
        };
        let paint = |painter: &Painter| {
            painter
                .paint(view, &Palette::STANDARD, 96, 3840, 1.0)
                .unwrap()
        };
        assert!(
            paint(&painter).spoken.contains("Esc to stop"),
            "offered by default"
        );
        assert!(painter.set_esc_hint(false), "a change is reported");
        assert!(!painter.set_esc_hint(false), "no change, no repaint");
        let without = paint(&painter);
        assert!(!without.spoken.contains("Esc"), "{}", without.spoken);
        assert!(without.body_bottom > 0);
        assert!(painter.set_esc_hint(true));
        assert!(paint(&painter).spoken.contains("Esc to stop"));
    }

    #[test]
    fn the_stop_button_is_a_real_enabled_button_that_stops_only_an_owned_session() {
        use windows::Win32::UI::{
            Input::KeyboardAndMouse::IsWindowEnabled,
            WindowsAndMessaging::{GWL_EXSTYLE, GetParent, GetWindowLongPtrW},
        };
        let _windows = window_class_lock();
        super::super::ensure_dpi_awareness().unwrap();
        let mut native = NativeStop::create().unwrap();
        let monitor = enumerate_monitors().unwrap().remove(0);
        native.sync_monitors(&[monitor]).unwrap();
        let tab = &native.tabs[0];
        let button = tab.button.hwnd;

        // Enabled and not transparent, unlike the tab, so hit-testing resolves to this process.
        // SAFETY: Both windows are live and owned by this thread.
        unsafe {
            assert!(IsWindowEnabled(button).as_bool());
            assert!(!IsWindowEnabled(tab.hwnd).as_bool());
            let style = u32::try_from(GetWindowLongPtrW(button, GWL_EXSTYLE)).unwrap();
            assert_eq!(style & WS_EX_TRANSPARENT.0, 0);
            assert_ne!(style & WS_EX_NOACTIVATE.0, 0, "never takes focus");
            assert_eq!(GetParent(button).unwrap(), tab.hwnd, "owned by its tab");
        }
        let mut affinity = 0_u32;
        // SAFETY: `affinity` is writable for the synchronous call and the window is live.
        unsafe { GetWindowDisplayAffinity(button, &raw mut affinity) }.unwrap();
        assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE.0, "never in screenshots");

        // UI Automation's Invoke pattern clicks with BM_CLICK; the owner hears BN_CLICKED.
        let controller = StopController::default();
        CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
        // SAFETY: Same live window; BM_CLICK carries no pointers.
        unsafe { SendMessageW(button, BM_CLICK, Some(WPARAM(0)), Some(LPARAM(0))) };
        assert!(!controller.is_stopped(), "no session, nothing to stop");
        controller.set_session_active(true);
        // A command that does not come from a control is not a click.
        // SAFETY: Same live window; the message carries no pointers.
        unsafe { SendMessageW(tab.hwnd, WM_COMMAND, Some(WPARAM(0)), Some(LPARAM(0))) };
        assert!(!controller.is_stopped());
        // SAFETY: Same live window; BM_CLICK carries no pointers.
        unsafe { SendMessageW(button, BM_CLICK, Some(WPARAM(0)), Some(LPARAM(0))) };
        assert!(controller.user_stopped(), "invoked, so stopped by the user");

        let again = StopController::default();
        again.set_session_active(true);
        CONTROLLER.with(|slot| *slot.borrow_mut() = Some(again.clone()));
        // SAFETY: Same live windows; the control handle is only compared by the callee.
        unsafe {
            SendMessageW(
                tab.hwnd,
                WM_COMMAND,
                Some(WPARAM(BN_CLICKED as usize)),
                Some(LPARAM(button.0 as isize)),
            )
        };
        assert!(again.user_stopped(), "a click reported by the button");
        CONTROLLER.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn the_stop_button_takes_clicks_where_the_tab_lets_them_through() {
        use windows::Win32::UI::WindowsAndMessaging::HTCLIENT;
        let _windows = window_class_lock();
        super::super::ensure_dpi_awareness().unwrap();
        let mut native = NativeStop::create().unwrap();
        let monitor = enumerate_monitors().unwrap().remove(0);
        native.sync_monitors(&[monitor]).unwrap();
        let tab = &native.tabs[0];
        // A point inside the (still tiny, hidden) button window, in screen coordinates.
        let mut rect = windows::Win32::Foundation::RECT::default();
        // SAFETY: `rect` is writable for the synchronous call and the window is live.
        unsafe { GetWindowRect(tab.button.hwnd, &raw mut rect) }.unwrap();
        let centre = LPARAM(
            (isize::try_from(rect.top).unwrap() << 16)
                | (isize::try_from(rect.left).unwrap() & 0xffff),
        );
        let hit_test = |window| {
            // SAFETY: Value-only hit test sent to a live window of this thread.
            unsafe { SendMessageW(window, WM_NCHITTEST, Some(WPARAM(0)), Some(centre)) }.0
        };
        assert_eq!(
            hit_test(tab.hwnd),
            isize::try_from(HTTRANSPARENT).unwrap(),
            "the tab is click-through"
        );
        assert_eq!(
            hit_test(tab.button.hwnd),
            isize::try_from(HTCLIENT).unwrap(),
            "the button is not: a click on it belongs to this process, which the target \
             checks refuse"
        );
    }

    #[test]
    fn a_tab_failure_after_start_up_fails_closed_and_keeps_its_reason() {
        let controller = StopController::default();
        let reported = std::sync::Mutex::new(Vec::new());
        record_tab_failure(
            &controller,
            &|reason| reported.lock().unwrap().push(reason),
            &io::Error::other("no active monitors"),
        );
        assert!(controller.is_stopped());
        assert!(!controller.user_stopped(), "this is not a user stop");
        let refusal = controller.refusal_message().unwrap();
        assert!(refusal.contains("session tab stopped working"), "{refusal}");
        assert!(refusal.contains("no active monitors"), "{refusal}");
        assert_eq!(
            controller.failure_reason(),
            reported.lock().unwrap().first().cloned(),
            "the status report and the refusal give the same reason"
        );
    }
}
