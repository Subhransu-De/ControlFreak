#![allow(unsafe_code)]

use controlfreak_core::StopController;
use std::{
    cell::{Cell, RefCell},
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WAIT_FAILED, WAIT_TIMEOUT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            Accessibility::NotifyWinEvent,
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE},
            Shell::IVirtualDesktopManager,
            WindowsAndMessaging::{
                CHILDID_SELF, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
                DispatchMessageW, EVENT_OBJECT_NAMECHANGE, EVENT_SYSTEM_ALERT, GetForegroundWindow,
                HHOOK, HTTRANSPARENT, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MA_NOACTIVATE,
                MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PM_REMOVE,
                PeekMessageW, PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SW_HIDE,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowDisplayAffinity,
                SetWindowPos, SetWindowTextW, SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx,
                UnregisterClassW, WDA_EXCLUDEFROMCAPTURE, WH_KEYBOARD_LL, WM_APP, WM_CLOSE,
                WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WM_KEYUP, WM_MOUSEACTIVATE,
                WM_NCHITTEST, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_SYSKEYDOWN, WM_SYSKEYUP,
                WM_THEMECHANGED, WNDCLASSW, WS_DISABLED, WS_EX_LAYERED, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{GUID, PCWSTR, w},
};

use super::desktop::{ComApartment, virtual_desktop_manager};
use super::indicator::{
    Activity, FADE_IN, FADE_IN_FROM, Monitor, PaintedTab, Painter, Palette, StopStatus, TabInputs,
    TabKind, TabModel, TabView, animations_enabled, enumerate_monitors, spoken, text_scale,
};

const CLASS_NAME: windows::core::PCWSTR = w!("ControlFreakUserStop");
/// Longest the UI thread blocks while idle. It bounds how soon a session start, a state change
/// or shutdown is noticed; hook callbacks and window messages wake the wait immediately.
const POLL_INTERVAL_MS: u32 = 100;
/// Frame interval while a tab fades in or out.
const FADE_FRAME_MS: u32 = 16;
/// The tab fades in with the lamp, and fades out over the same short time.
const TAB_FADE: Duration = FADE_IN;
/// Display, contrast, text-size and animation settings are re-read at least this often, and at
/// once when Windows announces a change.
const SETTINGS_INTERVAL: Duration = Duration::from_millis(500);

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

/// Independent native session tab and physical Esc monitor. Never calls a provider.
pub struct UserStop {
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl UserStop {
    pub fn start(controller: StopController, signals: Arc<SessionSignals>) -> Result<Self, String> {
        super::ensure_dpi_awareness().map_err(|error| error.to_string())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let exiting = Arc::clone(&shutdown);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("controlfreak-user-stop".to_owned())
            .spawn(move || {
                CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
                if let Err(error) = run(&controller, &signals, &exiting, &sender) {
                    controller.stop();
                    let _ = sender.send(Err(error.to_string()));
                }
                CONTROLLER.with(|slot| *slot.borrow_mut() = None);
            })
            .map_err(|error| error.to_string())?;
        let guard = Self {
            shutdown,
            worker: Some(worker),
        };
        receiver.recv().map_err(|error| error.to_string())??;
        Ok(guard)
    }
}

impl Drop for UserStop {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Owned by the UI thread, including partially initialized failure paths.
struct NativeStop {
    hook: HHOOK,
    instance: HINSTANCE,
    tabs: Vec<TabWindow>,
}

impl NativeStop {
    fn create() -> io::Result<Self> {
        Self::create_with_hook(true)
    }

    /// Tests pass `false`: a low-level hook on a thread that never pumps messages would delay
    /// every keystroke on the machine.
    fn create_with_hook(install_hook: bool) -> io::Result<Self> {
        // SAFETY: This UI thread owns the class and hook. The guard releases every successfully
        // created resource if a later call fails.
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
            let mut native = Self {
                hook: HHOOK::default(),
                instance,
                tabs: Vec::new(),
            };
            // A key already held before monitoring must retain its matching release.
            ESC_STATE.with(|state| {
                state.set(if GetAsyncKeyState(i32::from(VK_ESCAPE.0)) < 0 {
                    EscapeState::PassedThrough
                } else {
                    EscapeState::Released
                });
            });
            if install_hook {
                native.hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance), 0)
                        .map_err(|error| windows_error("SetWindowsHookExW", &error))?;
            }
            Ok(native)
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
        // Remove the callback before destroying the windows and their class.
        self.tabs.clear();
        // SAFETY: These handles were created on this thread and are released once.
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.hook);
            }
            let _ = UnregisterClassW(CLASS_NAME, Some(self.instance));
        }
    }
}

/// The tab on one display: a click-through, capture-excluded layered window centred on the
/// display's top edge, hanging from the lamp the glow helper draws there.
struct TabWindow {
    hwnd: HWND,
    monitor: Monitor,
    tab: Option<PaintedTab>,
    origin: POINT,
    visible: bool,
    /// The accessible name last given to the window.
    name: String,
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
        })
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
        if self.visible {
            // SAFETY: Hiding a window owned by this thread retains no pointers.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.visible = false;
        }
    }
}

impl Drop for TabWindow {
    fn drop(&mut self) {
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
            next.expanded && matches!(previous, None | Some(TabKind::HandedBack { .. }))
        }
        TabKind::Stopping => previous != Some(TabKind::Stopping),
        TabKind::InputHeld => previous != Some(TabKind::InputHeld),
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
    let mut next_settings = Instant::now() + SETTINGS_INTERVAL;
    let mut was_shown = false;
    while !exiting.load(Ordering::Acquire) {
        // SAFETY: Dispatch only messages retrieved for this dedicated UI thread.
        unsafe {
            let mut message = MSG::default();
            for _ in 0..64 {
                if !PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    break;
                }
                DispatchMessageW(&raw const message);
            }
        }
        let now = Instant::now();
        let mut repaint = false;
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
            next_settings = now + SETTINGS_INTERVAL;
        }
        let view = model.view(
            now,
            TabInputs {
                session_active: controller.session_active(),
                stop: StopStatus::parse(controller.status()),
                activity: signals.activity(),
                actions: signals.actions(),
                elevated: signals.elevated,
            },
        );
        // A tab about to appear first moves to the desktop the user is looking at.
        if view.is_some()
            && !was_shown
            && let Some(manager) = desktops
        {
            follow_foreground_desktop(manager, &mut desktop, &native.tabs);
        }
        was_shown = view.is_some();
        presenter.update(&mut native.tabs, view, &settings, repaint, now)?;
        wait_for_input(if presenter.fading() {
            FADE_FRAME_MS
        } else {
            POLL_INTERVAL_MS
        })
        .map_err(|error| windows_error("MsgWaitForMultipleObjectsEx", &error))?;
    }
    Ok(())
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
        _ => {}
    }
    // SAFETY: Forward unchanged Win32 callback arguments to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
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
        let mut native = NativeStop::create_with_hook(false).unwrap();
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
        let mut native = NativeStop::create_with_hook(false).unwrap();
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
        let mut native = NativeStop::create_with_hook(false).unwrap();
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
}
