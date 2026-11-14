#![allow(unsafe_code)]

//! The native helper that draws the control lamp: a steady line along the top edge of every
//! display while a client owns the desktop. The server process draws the tab that hangs from it;
//! both take their geometry and colour from [`super::indicator`].

use std::{
    io::{self, BufRead, BufWriter, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            E_ACCESSDENIED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RPC_E_CHANGED_MODE,
            WAIT_FAILED, WPARAM,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::GetCurrentThreadId,
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Shell::{IVirtualDesktopManager, VirtualDesktopManager},
            WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
                DispatchMessageW, GetForegroundWindow, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE,
                MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW,
                PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
                TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_DISPLAYCHANGE, WM_DPICHANGED,
                WM_MOUSEACTIVATE, WM_NCHITTEST, WM_SETTINGCHANGE, WM_SYSCOLORCHANGE,
                WM_THEMECHANGED, WNDCLASSW, WS_DISABLED, WS_EX_LAYERED, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{Error as WindowsError, GUID, w},
};

use super::indicator::{
    DibSurface, FADE_IN, FADE_IN_FROM, LampGeometry, LampTone, Monitor, Palette, Rgb,
    animations_enabled, enumerate_monitors, lamp_color, lamp_geometry, warning_geometry,
};

const PROTOCOL_PREFIX: &str = "CFP/1";
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// While the lamp is shown, the shell is polled this often to follow the desktop and keep the
/// lamp above windows that became topmost after it. Neither has a window message.
const TOPOLOGY_INTERVAL: Duration = Duration::from_millis(500);
/// While the lamp is hidden nothing needs following; display, DPI and colour changes arrive as
/// window messages, so this is only a safety net.
const HIDDEN_TOPOLOGY_INTERVAL: Duration = Duration::from_secs(2);
/// Set by the window procedure when Windows announces a display, DPI or colour change.
static TOPOLOGY_DIRTY: AtomicBool = AtomicBool::new(false);
const MINIMUM_VISIBLE: Duration = Duration::from_millis(800);
const FADE_OUT: Duration = Duration::from_millis(250);
const INITIAL_VISIBLE_OPACITY: f64 = FADE_IN_FROM as f64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HelperConfig {
    generation: u64,
}

impl HelperConfig {
    pub(crate) fn parse(arguments: &[String]) -> io::Result<Option<Self>> {
        if arguments
            .first()
            .is_none_or(|argument| argument != "--desktop-glow-helper")
        {
            return Ok(None);
        }
        if arguments.len() != 3 || arguments[1] != "--protocol-generation" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native desktop glow helper requires its correlated protocol generation",
            ));
        }
        let generation = arguments[2].parse::<u64>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("desktop glow generation is invalid: {error}"),
            )
        })?;
        if generation == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "desktop glow generation must be non-zero",
            ));
        }
        Ok(Some(Self { generation }))
    }
}

pub(crate) fn run(config: HelperConfig) -> io::Result<()> {
    ensure_dpi_awareness()?;
    let _com = ComApartment::initialize()?;
    register_window_class()?;
    // SAFETY: COM is initialized on this thread and the class/interface IDs are Windows-provided.
    let manager: IVirtualDesktopManager =
        unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL) }
            .map_err(|error| windows_error("CoCreateInstance(VirtualDesktopManager)", &error))?;
    let mut renderer = Renderer::new(manager)?;
    let (sender, receiver) = mpsc::channel();
    // SAFETY: Takes no arguments and returns this thread's ID.
    spawn_command_reader(sender, unsafe { GetCurrentThreadId() })?;
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    write_state(&mut output, config.generation, 0, "HIDDEN")?;

    let mut visual = VisualState::new();
    let mut next_frame = Instant::now();
    let mut next_topology = Instant::now() + HIDDEN_TOPOLOGY_INTERVAL;
    loop {
        if !pump_messages() {
            return Ok(());
        }

        while let Ok(event) = receiver.try_recv() {
            match event {
                InputEvent::Line(line) => {
                    let Some(command) = parse_command(&line, config.generation)? else {
                        continue;
                    };
                    if handle_command(
                        command,
                        config.generation,
                        &mut output,
                        &mut visual,
                        &mut renderer,
                    )? {
                        return Ok(());
                    }
                }
                InputEvent::Error(reason) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, reason));
                }
                InputEvent::Eof => return Ok(()),
            }
        }

        let now = Instant::now();
        if let Some(completion) = visual.take_completed_hide(now) {
            renderer.set_visible(false)?;
            write_state(
                &mut output,
                config.generation,
                completion.sequence,
                completion.state,
            )?;
            if completion.shutdown {
                return Ok(());
            }
        }

        if now >= next_topology || TOPOLOGY_DIRTY.swap(false, Ordering::AcqRel) {
            renderer.sync_topology()?;
            next_topology = now + topology_interval(visual.visible);
        }

        if visual.visible && now >= next_frame {
            renderer.render(visual.level, visual.opacity(now, renderer.animated))?;
            next_frame = now + FRAME_INTERVAL;
        }

        // Sleep until the next frame, or while hidden until the safety poll. A command from the
        // server, which waits for its acknowledgement before each input action, or a window
        // message ends the wait at once.
        let until = if visual.visible {
            next_frame.min(next_topology)
        } else {
            next_topology
        };
        wait_for_events(until.saturating_duration_since(Instant::now()))?;
    }
}

/// How often the shell is polled for the lamp's desktop and z-order.
const fn topology_interval(visible: bool) -> Duration {
    if visible {
        TOPOLOGY_INTERVAL
    } else {
        HIDDEN_TOPOLOGY_INTERVAL
    }
}

/// Blocks until a window message or posted command wake-up arrives, or `timeout` elapses.
/// `MWMO_INPUTAVAILABLE` also returns for messages an earlier peek saw but left in the queue.
fn wait_for_events(timeout: Duration) -> io::Result<()> {
    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: No handles are passed; the call only waits on this thread's message queue.
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(None, milliseconds, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    if result == WAIT_FAILED {
        return Err(last_windows_error("MsgWaitForMultipleObjectsEx"));
    }
    Ok(())
}

/// Reads commands on their own thread and wakes the UI thread, which sleeps in its message
/// queue, after each one.
fn spawn_command_reader(sender: mpsc::Sender<InputEvent>, ui_thread: u32) -> io::Result<()> {
    let wake = move || {
        // SAFETY: Posts a value-only message to a thread ID; if that thread has exited the call
        // only fails, and the helper is already shutting down.
        let _ = unsafe { PostThreadMessageW(ui_thread, WM_APP, WPARAM(0), LPARAM(0)) };
    };
    thread::Builder::new()
        .name("controlfreak-native-glow-input".to_owned())
        .spawn(move || {
            for line in io::stdin().lock().lines() {
                match line {
                    Ok(line) => {
                        if sender.send(InputEvent::Line(line)).is_err() {
                            return;
                        }
                        wake();
                    }
                    Err(error) => {
                        let _ = sender.send(InputEvent::Error(error.to_string()));
                        wake();
                        return;
                    }
                }
            }
            let _ = sender.send(InputEvent::Eof);
            wake();
        })
        .map(|_| ())
}

enum InputEvent {
    Line(String),
    Error(String),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Armed,
    Acting,
    ElevatedArmed,
    ElevatedActing,
}

impl Level {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "ARMED" => Some(Self::Armed),
            "ACTING" => Some(Self::Acting),
            "ELEVATED_ARMED" => Some(Self::ElevatedArmed),
            "ELEVATED_ACTING" => Some(Self::ElevatedActing),
            _ => None,
        }
    }

    const fn state(self) -> &'static str {
        match self {
            Self::Armed => "ARMED",
            Self::Acting => "ACTING",
            Self::ElevatedArmed => "ELEVATED_ARMED",
            Self::ElevatedActing => "ELEVATED_ACTING",
        }
    }

    const fn elevated(self) -> bool {
        matches!(self, Self::ElevatedArmed | Self::ElevatedActing)
    }
}

/// The lamp's message beyond the session itself. `Warning` is sent once the server knows input
/// may still be held, and turns the lamp amber until the helper exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Normal,
    Warning,
}

impl Tone {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "NORMAL" => Some(Self::Normal),
            "WARNING" => Some(Self::Warning),
            _ => None,
        }
    }

    const fn state(self) -> &'static str {
        match self {
            Self::Normal => "TONE_NORMAL",
            Self::Warning => "TONE_WARNING",
        }
    }
}

#[derive(Clone, Copy)]
enum ProtocolCommand {
    Level { sequence: u64, level: Level },
    Tone { sequence: u64, tone: Tone },
    Ping { sequence: u64 },
    Hide { sequence: u64, shutdown: bool },
}

fn parse_command(line: &str, expected_generation: u64) -> io::Result<Option<ProtocolCommand>> {
    let fields: Vec<&str> = line.split_ascii_whitespace().collect();
    if fields.len() < 4 || fields[0] != PROTOCOL_PREFIX {
        return Err(protocol_error(format!(
            "invalid native glow protocol command: {line:?}"
        )));
    }
    let generation = fields[1]
        .parse::<u64>()
        .map_err(|error| protocol_error(format!("invalid glow generation: {error}")))?;
    if generation != expected_generation {
        return Ok(None);
    }
    let sequence = fields[2]
        .parse::<u64>()
        .map_err(|error| protocol_error(format!("invalid glow sequence: {error}")))?;
    let command = match fields[3] {
        "LEVEL" if fields.len() == 5 => ProtocolCommand::Level {
            sequence,
            level: Level::parse(fields[4]).ok_or_else(|| {
                protocol_error("LEVEL requires a supported standard or elevated state")
            })?,
        },
        "TONE" if fields.len() == 5 => ProtocolCommand::Tone {
            sequence,
            tone: Tone::parse(fields[4])
                .ok_or_else(|| protocol_error("TONE requires NORMAL or WARNING"))?,
        },
        "PING" if fields.len() == 4 => ProtocolCommand::Ping { sequence },
        "HIDE" if fields.len() == 4 => ProtocolCommand::Hide {
            sequence,
            shutdown: false,
        },
        "SHUTDOWN" if fields.len() == 4 => ProtocolCommand::Hide {
            sequence,
            shutdown: true,
        },
        _ => {
            return Err(protocol_error(format!(
                "unknown native glow protocol command: {line:?}"
            )));
        }
    };
    Ok(Some(command))
}

fn protocol_error(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

fn handle_command(
    command: ProtocolCommand,
    generation: u64,
    output: &mut impl Write,
    visual: &mut VisualState,
    renderer: &mut Renderer,
) -> io::Result<bool> {
    match command {
        ProtocolCommand::Level { sequence, level } => {
            let now = Instant::now();
            visual.set_level(level, now);
            renderer.present_level(level, visual.opacity(now, renderer.animated))?;
            write_state(output, generation, sequence, level.state())?;
            Ok(false)
        }
        ProtocolCommand::Tone { sequence, tone } => {
            renderer.tone = tone;
            if visual.visible {
                renderer.render(
                    visual.level,
                    visual.opacity(Instant::now(), renderer.animated),
                )?;
            }
            write_state(output, generation, sequence, tone.state())?;
            Ok(false)
        }
        ProtocolCommand::Ping { sequence } => {
            write_state(output, generation, sequence, "PONG")?;
            Ok(false)
        }
        ProtocolCommand::Hide { sequence, shutdown } => {
            if visual.visible {
                visual.begin_hide(sequence, shutdown, Instant::now());
                Ok(false)
            } else {
                write_state(
                    output,
                    generation,
                    sequence,
                    if shutdown { "STOPPED" } else { "HIDDEN" },
                )?;
                Ok(shutdown)
            }
        }
    }
}

fn write_state(
    output: &mut impl Write,
    generation: u64,
    sequence: u64,
    state: &str,
) -> io::Result<()> {
    writeln!(output, "{PROTOCOL_PREFIX} {generation} {sequence} {state}")?;
    output.flush()
}

struct VisualState {
    level: Level,
    visible: bool,
    visible_since: Instant,
    transition: Transition,
    pending_hide: Option<PendingHide>,
}

impl VisualState {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            level: Level::Armed,
            visible: false,
            visible_since: now,
            transition: Transition::constant(0.0, now),
            pending_hide: None,
        }
    }

    fn set_level(&mut self, level: Level, now: Instant) {
        // The lamp is equally strong while acting and while waiting; only the first level of a
        // session fades it in.
        let current = self.transition.value(now).max(INITIAL_VISIBLE_OPACITY);
        if !self.visible {
            self.visible = true;
            self.visible_since = now;
            self.transition = Transition {
                started: now,
                duration: FADE_IN,
                from: current,
                to: 1.0,
            };
        } else if self.pending_hide.is_some() {
            self.transition = Transition {
                started: now,
                duration: FADE_IN,
                from: self.opacity(now, true).max(INITIAL_VISIBLE_OPACITY),
                to: 1.0,
            };
        }
        self.level = level;
        self.pending_hide = None;
    }

    fn begin_hide(&mut self, sequence: u64, shutdown: bool, now: Instant) {
        let fade_start = now.max(self.visible_since + MINIMUM_VISIBLE);
        let fade_from = self.transition.value(fade_start);
        self.pending_hide = Some(PendingHide {
            sequence,
            state: if shutdown { "STOPPED" } else { "HIDDEN" },
            shutdown,
            fade_start,
            finish: fade_start + FADE_OUT,
            from: fade_from,
        });
    }

    /// Lamp opacity. With Windows animations off, fades are skipped: the lamp is either fully on
    /// or, once a hide has begun, off.
    fn opacity(&self, now: Instant, animated: bool) -> f64 {
        if let Some(hide) = self.pending_hide.as_ref()
            && now >= hide.fade_start
        {
            if !animated {
                return 0.0;
            }
            let progress = duration_fraction(now - hide.fade_start, FADE_OUT);
            return hide.from * (1.0 - smooth(progress));
        }
        if animated {
            self.transition.value(now)
        } else {
            self.transition.to
        }
    }

    fn take_completed_hide(&mut self, now: Instant) -> Option<HideCompletion> {
        let pending = self.pending_hide.as_ref()?;
        if now < pending.finish {
            return None;
        }
        let completion = HideCompletion {
            sequence: pending.sequence,
            state: pending.state,
            shutdown: pending.shutdown,
        };
        self.pending_hide = None;
        self.visible = false;
        self.transition = Transition::constant(0.0, now);
        Some(completion)
    }
}

struct PendingHide {
    sequence: u64,
    state: &'static str,
    shutdown: bool,
    fade_start: Instant,
    finish: Instant,
    from: f64,
}

struct HideCompletion {
    sequence: u64,
    state: &'static str,
    shutdown: bool,
}

struct Transition {
    started: Instant,
    duration: Duration,
    from: f64,
    to: f64,
}

impl Transition {
    fn constant(value: f64, now: Instant) -> Self {
        Self {
            started: now,
            duration: Duration::ZERO,
            from: value,
            to: value,
        }
    }

    fn value(&self, now: Instant) -> f64 {
        if self.duration.is_zero() {
            return self.to;
        }
        let progress =
            duration_fraction(now.saturating_duration_since(self.started), self.duration);
        self.from + (self.to - self.from) * smooth(progress)
    }
}

fn duration_fraction(elapsed: Duration, duration: Duration) -> f64 {
    (elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
}

fn smooth(value: f64) -> f64 {
    value * value * (3.0 - 2.0 * value)
}

/// What to do about the desktop the foreground window is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesktopFollow {
    /// Leave the lamp where it is.
    Stay,
    Move(GUID),
}

/// A desktop ID from the shell, or `None` when the shell could not name one. The shell reports
/// `TYPE_E_ELEMENTNOTFOUND` for the taskbar, the desktop and the Start surfaces, which belong to
/// every desktop, and an all-zero ID for windows it does not track.
fn usable_desktop<E>(lookup: Result<GUID, E>) -> Option<GUID> {
    lookup.ok().filter(|desktop| *desktop != GUID::zeroed())
}

fn desktop_follow(current: Option<GUID>, foreground: Option<GUID>) -> DesktopFollow {
    match foreground {
        Some(desktop) if current != Some(desktop) => DesktopFollow::Move(desktop),
        _ => DesktopFollow::Stay,
    }
}

struct Renderer {
    manager: IVirtualDesktopManager,
    desktop_id: Option<GUID>,
    monitors: Vec<Monitor>,
    windows: Vec<LampWindow>,
    visible: bool,
    /// Mirrors the Windows "Show animations" setting; re-read on every topology poll.
    animated: bool,
    palette: Palette,
    tone: Tone,
}

impl Renderer {
    fn new(manager: IVirtualDesktopManager) -> io::Result<Self> {
        let mut renderer = Self {
            manager,
            desktop_id: None,
            monitors: Vec::new(),
            windows: Vec::new(),
            visible: false,
            animated: animations_enabled(),
            palette: Palette::system(),
            tone: Tone::Normal,
        };
        renderer.sync_topology()?;
        Ok(renderer)
    }

    fn sync_topology(&mut self) -> io::Result<()> {
        self.animated = animations_enabled();
        self.palette = Palette::system();
        let monitors = enumerate_monitors()?;
        if monitors != self.monitors {
            let mut replacement = Vec::with_capacity(monitors.len());
            for monitor in monitors.iter().copied() {
                replacement.push(LampWindow::create(monitor)?);
            }
            self.windows = replacement;
            self.monitors = monitors;
            self.desktop_id = None;
        }
        self.follow_foreground_desktop();
        // Keep a visible lamp above windows that became topmost after it.
        if self.visible {
            for window in &self.windows {
                window.position(true)?;
            }
        }
        Ok(())
    }

    /// Moves the lamp to the virtual desktop of the foreground window. The shell cannot map every
    /// window to a desktop (the taskbar, the desktop and Start have none), and a window can close
    /// between the two calls; none of that may end the helper, which would cancel the agent's
    /// input. The lamp then stays where it is, as the session tab does.
    fn follow_foreground_desktop(&mut self) {
        // SAFETY: This has no pointer parameters and returns a Windows-owned handle or null.
        let foreground = unsafe { GetForegroundWindow() };
        let found = if foreground.is_invalid() {
            None
        } else {
            // SAFETY: The COM manager is initialized and `foreground` is a Windows-owned HWND; a
            // window that closed in between only makes the call fail.
            usable_desktop(unsafe { self.manager.GetWindowDesktopId(foreground) })
        };
        let DesktopFollow::Move(desktop_id) = desktop_follow(self.desktop_id, found) else {
            return;
        };
        let mut moved = true;
        for window in &self.windows {
            // SAFETY: Both the COM interface and helper-owned HWND are live for this call.
            moved &= unsafe {
                self.manager
                    .MoveWindowToDesktop(window.hwnd, &raw const desktop_id)
            }
            .is_ok();
            // A lamp that cannot be repositioned is retried with the next poll.
            let _ = window.position(self.visible);
        }
        // Remember the desktop only once every lamp is on it, so a failed move is retried.
        if moved {
            self.desktop_id = Some(desktop_id);
        }
    }

    fn set_visible(&mut self, visible: bool) -> io::Result<()> {
        self.visible = visible;
        for window in &mut self.windows {
            if visible {
                window.position(true)?;
                // SAFETY: The helper owns this HWND and requests a non-activating visibility change.
                let _ = unsafe { ShowWindow(window.hwnd, SW_SHOWNOACTIVATE) };
            } else {
                // SAFETY: The helper owns this HWND and hiding it retains no Rust references.
                let _ = unsafe { ShowWindow(window.hwnd, SW_HIDE) };
                window.shown = None;
            }
        }
        Ok(())
    }

    fn present_level(&mut self, level: Level, opacity: f64) -> io::Result<()> {
        // A LEVEL acknowledgement authorizes the parent to mutate input, so first move the lamp to
        // the foreground desktop where the shell can tell, and give it a non-transparent frame.
        self.sync_topology()?;
        self.render(level, opacity)?;
        self.set_visible(true)
    }

    fn render(&mut self, level: Level, opacity: f64) -> io::Result<()> {
        let tone = match self.tone {
            Tone::Warning => LampTone::Warning,
            Tone::Normal => LampTone::Session {
                admin: level.elevated(),
            },
        };
        let frame = LampFrame {
            color: lamp_color(tone, &self.palette),
            surface: self.palette.surface,
            alpha: opacity_byte(opacity),
            high_contrast: self.palette.high_contrast,
            admin: level.elevated(),
            warning: self.tone == Tone::Warning,
        };
        for window in &mut self.windows {
            window.render(frame)?;
        }
        Ok(())
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn opacity_byte(opacity: f64) -> u8 {
    // Clamped to 0..=255 first, so the conversion is exact.
    (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Everything one lamp frame depends on; a window repaints only when this changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LampFrame {
    color: Rgb,
    surface: Rgb,
    alpha: u8,
    high_contrast: bool,
    admin: bool,
    warning: bool,
}

impl LampFrame {
    /// The shared geometry for this frame: the double warning rule, or the session lamp.
    fn geometry(self, dpi: u32) -> LampGeometry {
        if self.warning {
            warning_geometry(dpi)
        } else {
            lamp_geometry(dpi, self.high_contrast, self.admin)
        }
    }
}

/// Bounds of a monitor's lamp: the full width of its top edge, as tall as the shared geometry.
fn lamp_bounds(monitor: Monitor, geometry: LampGeometry) -> (POINT, i32, i32) {
    let height = geometry.height().min(monitor.height());
    (
        POINT {
            x: monitor.left,
            y: monitor.top,
        },
        monitor.width(),
        height,
    )
}

/// Fills a lamp raster: lit lamp rows in the lamp colour, unlit rows and the keyline below in the
/// opaque surface colour, all scaled by `alpha` and premultiplied.
fn render_lamp(pixels: &mut [u32], width: usize, geometry: LampGeometry, frame: LampFrame) {
    for (row, line) in pixels.chunks_exact_mut(width).enumerate() {
        let color = if geometry.lit(i32::try_from(row).unwrap_or(i32::MAX)) {
            frame.color
        } else {
            frame.surface
        };
        line.fill(premultiplied(color, frame.alpha));
    }
}

fn premultiplied(color: Rgb, alpha: u8) -> u32 {
    let scale = |channel: u8| (u32::from(channel) * u32::from(alpha) + 127) / 255;
    (u32::from(alpha) << 24) | (scale(color.0) << 16) | (scale(color.1) << 8) | scale(color.2)
}

struct LampWindow {
    hwnd: HWND,
    monitor: Monitor,
    geometry: Option<LampGeometry>,
    surface: Option<DibSurface>,
    shown: Option<LampFrame>,
}

impl LampWindow {
    fn create(monitor: Monitor) -> io::Result<Self> {
        let geometry = lamp_geometry(monitor.dpi, false, false);
        let (origin, width, height) = lamp_bounds(monitor, geometry);
        // SAFETY: A null module name requests the current process module without borrowed pointers.
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| windows_error("GetModuleHandleW", &error))?;
        // SAFETY: The registered class, module, coordinates, and constant strings remain valid;
        // Windows owns the returned HWND until this helper destroys it.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST,
                w!("ControlFreakNativeGlowWindow"),
                w!("ControlFreak Desktop Glow"),
                // WindowFromPoint deliberately skips disabled windows. Keeping the purely visual
                // overlay disabled ensures point-integrity checks resolve the application beneath
                // it instead of this same-integrity helper process.
                WS_POPUP | WS_DISABLED,
                origin.x,
                origin.y,
                width,
                height,
                None,
                None,
                Some(HINSTANCE(module.0)),
                None,
            )
        }
        .map_err(|error| windows_error("CreateWindowExW", &error))?;
        // SAFETY: The helper owns `hwnd`; capture affinity stores no Rust pointers.
        if let Err(error) = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } {
            // SAFETY: Window creation succeeded and ownership has not escaped this function.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(windows_error("SetWindowDisplayAffinity", &error));
        }
        let window = Self {
            hwnd,
            monitor,
            geometry: None,
            surface: None,
            shown: None,
        };
        window.position(false)?;
        Ok(window)
    }

    fn bounds(&self) -> (POINT, i32, i32) {
        lamp_bounds(
            self.monitor,
            self.geometry
                .unwrap_or_else(|| lamp_geometry(self.monitor.dpi, false, false)),
        )
    }

    fn position(&self, show: bool) -> io::Result<()> {
        let flags = if show {
            SWP_NOACTIVATE | SWP_SHOWWINDOW
        } else {
            SWP_NOACTIVATE
        };
        let (origin, width, height) = self.bounds();
        // SAFETY: This helper owns `hwnd`; all dimensions are validated monitor bounds.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                origin.x,
                origin.y,
                width,
                height,
                flags,
            )
        }
        .map_err(|error| windows_error("SetWindowPos", &error))
    }

    fn render(&mut self, frame: LampFrame) -> io::Result<()> {
        if self.shown == Some(frame) {
            return Ok(());
        }
        let geometry = frame.geometry(self.monitor.dpi);
        if self.geometry != Some(geometry) || self.surface.is_none() {
            let (_, width, height) = lamp_bounds(self.monitor, geometry);
            self.surface = Some(DibSurface::create(width, height)?);
            self.geometry = Some(geometry);
        }
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| io::Error::other("lamp surface was not created"))?;
        let width = surface.width;
        render_lamp(surface.pixels(), width, geometry, frame);
        let (origin, _, _) = lamp_bounds(self.monitor, geometry);
        surface.present(self.hwnd, origin, 255)?;
        self.shown = Some(frame);
        Ok(())
    }
}

impl Drop for LampWindow {
    fn drop(&mut self) {
        // SAFETY: `hwnd` is owned by this value and is destroyed exactly once during Drop.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

fn ensure_dpi_awareness() -> io::Result<()> {
    // SAFETY: This process-wide call takes a Windows-provided constant and retains no references.
    let result =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.code() == E_ACCESSDENIED => Ok(()),
        Err(error) => Err(windows_error("SetProcessDpiAwarenessContext", &error)),
    }
}

struct ComApartment(bool);

impl ComApartment {
    fn initialize() -> io::Result<Self> {
        // SAFETY: This initializes COM for only the calling helper UI thread.
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_ok() {
            Ok(Self(true))
        } else if result == RPC_E_CHANGED_MODE {
            Ok(Self(false))
        } else {
            Err(windows_error(
                "CoInitializeEx",
                &WindowsError::from_hresult(result),
            ))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: This exactly balances the successful initialization performed by this guard.
            unsafe { CoUninitialize() };
        }
    }
}

fn register_window_class() -> io::Result<()> {
    // SAFETY: A null module name requests the current process module without borrowed pointers.
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| windows_error("GetModuleHandleW", &error))?;
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(window_proc),
        hInstance: HINSTANCE(module.0),
        lpszClassName: w!("ControlFreakNativeGlowWindow"),
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and Windows copies its registration data synchronously.
    if unsafe { RegisterClassW(&raw const class) } == 0 {
        return Err(last_windows_error("RegisterClassW"));
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => LRESULT(isize::try_from(HTTRANSPARENT).unwrap_or(-1)),
        WM_MOUSEACTIVATE => LRESULT(isize::try_from(MA_NOACTIVATE).unwrap_or(3)),
        // Displays, DPI and colours are re-read at once instead of at the next safety poll.
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE | WM_SYSCOLORCHANGE
        | WM_THEMECHANGED => {
            TOPOLOGY_DIRTY.store(true, Ordering::Release);
            // SAFETY: Windows supplied all callback arguments and owns the referenced HWND.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        // SAFETY: Windows supplied all callback arguments and owns the referenced HWND.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn pump_messages() -> bool {
    let mut message = MSG::default();
    // SAFETY: `message` is writable for the synchronous dequeue operation on this UI thread.
    while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if message.message == 0x0012 {
            return false;
        }
        // SAFETY: `message` was initialized by PeekMessageW and is dispatched on its owning thread.
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
    true
}

fn windows_error(context: &str, error: &WindowsError) -> io::Error {
    io::Error::other(format!("{context} failed: {error}"))
}

fn last_windows_error(context: &str) -> io::Error {
    windows_error(context, &WindowsError::from_thread())
}

#[cfg(test)]
mod tests {
    use super::super::indicator::{ADMIN_RED, AMBER, Painter, TURQUOISE, TabKind, TabView};
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowDisplayAffinity, WindowFromPoint};

    fn frame(color: Rgb, alpha: u8, admin: bool) -> LampFrame {
        LampFrame {
            color,
            surface: Palette::STANDARD.surface,
            alpha,
            high_contrast: false,
            admin,
            warning: false,
        }
    }

    #[test]
    fn private_helper_arguments_are_correlated() {
        let arguments = [
            "--desktop-glow-helper".to_owned(),
            "--protocol-generation".to_owned(),
            "42".to_owned(),
        ];
        assert_eq!(
            HelperConfig::parse(&arguments).unwrap(),
            Some(HelperConfig { generation: 42 })
        );
        assert_eq!(HelperConfig::parse(&[]).unwrap(), None);
    }

    #[test]
    fn protocol_rejects_uncorrelated_and_unknown_commands() {
        assert!(parse_command("CFP/1 41 1 PING", 42).unwrap().is_none());
        assert!(matches!(
            parse_command("CFP/1 42 1 LEVEL ELEVATED_ACTING", 42).unwrap(),
            Some(ProtocolCommand::Level {
                level: Level::ElevatedActing,
                ..
            })
        ));
        assert!(matches!(
            parse_command("CFP/1 42 2 TONE WARNING", 42).unwrap(),
            Some(ProtocolCommand::Tone {
                tone: Tone::Warning,
                ..
            })
        ));
        assert!(parse_command("CFP/1 42 3 TONE LOUD", 42).is_err());
        assert!(parse_command("CFP/1 42 1 EXPLODE", 42).is_err());
    }

    #[test]
    fn lamp_raster_is_opaque_lamp_rows_over_a_surface_keyline() {
        let geometry = lamp_geometry(144, false, false);
        let width = 7;
        let mut pixels = vec![0_u32; width * usize::try_from(geometry.height()).unwrap()];
        render_lamp(&mut pixels, width, geometry, frame(TURQUOISE, 255, false));
        let rows: Vec<&[u32]> = pixels.chunks_exact(width).collect();
        assert_eq!(rows.len(), 8, "6 lamp rows and a 2 pixel keyline at 150%");
        for (index, row) in rows.iter().enumerate() {
            assert!(row.iter().all(|pixel| *pixel == row[0]), "uniform row");
            let expected = if index < 6 { 0xff38_d6c4 } else { 0xff20_2123 };
            assert_eq!(row[0], expected, "row {index}");
        }
    }

    #[test]
    fn the_warning_lamp_is_a_double_rule_at_every_scale() {
        for (dpi, expected) in [
            (96, "###..###"),
            (120, "####..####"),
            (144, "#####..#####"),
            (192, "######....######"),
        ] {
            let geometry = warning_geometry(dpi);
            let pattern: String = (0..geometry.lamp)
                .map(|row| if geometry.lit(row) { '#' } else { '.' })
                .collect();
            assert_eq!(pattern, expected, "{dpi} DPI");
            assert!(!geometry.lit(geometry.lamp), "the keyline is never lit");
        }
        let solid = lamp_geometry(96, false, false);
        assert!((0..solid.lamp).all(|row| solid.lit(row)));
    }

    #[test]
    fn faded_lamp_stays_premultiplied_and_clears_completely() {
        for color in [TURQUOISE, ADMIN_RED, AMBER] {
            let pixel = premultiplied(color, 128);
            assert_eq!(pixel >> 24, 128);
            for shift in [16, 8, 0] {
                assert!((pixel >> shift) & 0xff <= 128);
            }
            assert_eq!(premultiplied(color, 0), 0);
        }
    }

    #[test]
    fn lamps_cover_each_top_edge_at_negative_origins_and_small_displays() {
        let monitor = Monitor {
            left: -1920,
            top: -300,
            right: 0,
            bottom: 780,
            dpi: 120,
        };
        let (origin, width, height) = lamp_bounds(monitor, lamp_geometry(120, false, false));
        assert_eq!((origin.x, origin.y, width, height), (-1920, -300, 1920, 6));
        let tiny = Monitor {
            left: -30,
            top: -10,
            right: 50,
            bottom: -6,
            dpi: 96,
        };
        let (_, width, height) = lamp_bounds(tiny, lamp_geometry(96, false, false));
        assert_eq!((width, height), (80, 4), "never taller than the display");
    }

    /// `TYPE_E_ELEMENTNOTFOUND`, which the shell returns for the taskbar, the desktop and Start.
    const SHELL_WINDOW_HAS_NO_DESKTOP: i32 = 0x8002_802B_u32.cast_signed();

    #[test]
    fn shell_windows_without_a_desktop_id_leave_the_lamp_where_it_is() {
        let shell: Result<GUID, WindowsError> = Err(WindowsError::from_hresult(
            windows::core::HRESULT(SHELL_WINDOW_HAS_NO_DESKTOP),
        ));
        let current = GUID::from_u128(7);
        assert_eq!(usable_desktop(shell), None, "a failed lookup is tolerated");
        assert_eq!(usable_desktop::<()>(Ok(GUID::zeroed())), None);
        assert_eq!(
            desktop_follow(Some(current), None),
            DesktopFollow::Stay,
            "the lamp stays on its desktop"
        );
        assert_eq!(desktop_follow(None, None), DesktopFollow::Stay);
    }

    #[test]
    fn the_lamp_follows_a_real_desktop_change_once() {
        let first = GUID::from_u128(1);
        let second = GUID::from_u128(2);
        assert_eq!(
            desktop_follow(None, Some(first)),
            DesktopFollow::Move(first)
        );
        assert_eq!(
            desktop_follow(Some(first), Some(first)),
            DesktopFollow::Stay
        );
        assert_eq!(
            desktop_follow(Some(first), Some(second)),
            DesktopFollow::Move(second)
        );
        assert_eq!(usable_desktop::<()>(Ok(second)), Some(second));
    }

    #[test]
    fn the_idle_helper_sleeps_long_and_a_shown_lamp_polls_the_shell() {
        assert!(topology_interval(false) >= Duration::from_secs(1));
        assert_eq!(topology_interval(true), TOPOLOGY_INTERVAL);
    }

    #[test]
    fn the_helper_wait_returns_for_a_posted_command_wake_up() {
        let mut message = MSG::default();
        // SAFETY: `message` is writable and this test thread owns its queue.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}
        let started = Instant::now();
        wait_for_events(Duration::from_millis(40)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(30), "times out");
        // SAFETY: Posting a plain-value message to this thread's own queue.
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_APP, WPARAM(0), LPARAM(0)) }.unwrap();
        let started = Instant::now();
        wait_for_events(Duration::from_secs(5)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "woken, not polled"
        );
        // SAFETY: Remove the test message so the queue is left empty.
        while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {}
    }

    #[test]
    fn lamp_is_equally_strong_while_acting_and_waiting() {
        let mut visual = VisualState::new();
        let started = Instant::now();
        visual.set_level(Level::Acting, started);
        let settled = started + FADE_IN;
        assert!((visual.opacity(settled, true) - 1.0).abs() < 1e-12);
        visual.set_level(Level::Armed, settled);
        assert!((visual.opacity(settled + Duration::from_millis(50), true) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn disabled_animation_skips_fades_but_keeps_the_minimum_visible_time() {
        let mut visual = VisualState::new();
        let started = Instant::now();
        visual.set_level(Level::Acting, started);
        assert!(
            (visual.opacity(started, false) - 1.0).abs() < 1e-12,
            "no fade in"
        );
        visual.begin_hide(1, false, started);
        assert!(
            (visual.opacity(started + MINIMUM_VISIBLE / 2, false) - 1.0).abs() < 1e-12,
            "a short session still shows for the minimum time"
        );
        assert!(visual.opacity(started + MINIMUM_VISIBLE, false).abs() < 1e-12);
        assert!(
            visual
                .take_completed_hide(started + MINIMUM_VISIBLE)
                .is_none()
        );
        assert!(
            visual
                .take_completed_hide(started + MINIMUM_VISIBLE + FADE_OUT)
                .is_some()
        );
    }

    #[test]
    fn acknowledged_levels_start_with_a_visible_opacity() {
        let mut visual = VisualState::new();
        let started = Instant::now();
        visual.set_level(Level::Acting, started);
        assert!(visual.opacity(started, true) >= INITIAL_VISIBLE_OPACITY);

        visual.begin_hide(1, false, started);
        let nearly_hidden = started + MINIMUM_VISIBLE + FADE_OUT;
        visual.set_level(Level::ElevatedActing, nearly_hidden);
        assert!(visual.opacity(nearly_hidden, true) >= INITIAL_VISIBLE_OPACITY);
        assert!(visual.pending_hide.is_none());
    }

    #[test]
    fn point_targeting_skips_the_disabled_lamp() {
        ensure_dpi_awareness().unwrap();
        register_window_class().unwrap();
        let monitor = enumerate_monitors().unwrap().remove(0);
        let mut window = LampWindow::create(monitor).unwrap();
        window.render(frame(TURQUOISE, 255, false)).unwrap();
        window.position(true).unwrap();
        let point = POINT {
            x: monitor.left + monitor.width() / 2,
            y: monitor.top + 1,
        };

        // SAFETY: `point` is a value inside the visible lamp bounds and the returned HWND is
        // borrowed only for this equality check.
        let target = unsafe { WindowFromPoint(point) };

        assert_ne!(target, window.hwnd);
        let mut affinity = 0_u32;
        // SAFETY: `affinity` is writable for the synchronous call and the window is live.
        unsafe { GetWindowDisplayAffinity(window.hwnd, &raw mut affinity) }.unwrap();
        assert_eq!(
            affinity, WDA_EXCLUDEFROMCAPTURE.0,
            "screenshots never show the lamp"
        );
    }

    fn lamp_rows(dpi: u32, admin: bool, tone: LampTone) -> (LampGeometry, Vec<u32>) {
        let geometry = if tone == LampTone::Warning {
            warning_geometry(dpi)
        } else {
            lamp_geometry(dpi, false, admin)
        };
        let color = lamp_color(tone, &Palette::STANDARD);
        let rows = usize::try_from(geometry.height()).unwrap();
        let mut pixels = vec![0_u32; rows];
        render_lamp(&mut pixels, 1, geometry, frame(color, 255, admin));
        (geometry, pixels)
    }

    #[test]
    fn the_tab_lamp_segment_matches_the_helper_lamp_at_every_scale() {
        let painter = Painter::new().unwrap();
        for dpi in [96, 120, 144, 168, 192] {
            for (kind, admin, tone) in [
                (TabKind::Working, false, LampTone::Session { admin: false }),
                (TabKind::Waiting, true, LampTone::Session { admin: true }),
                (TabKind::Stopping, true, LampTone::Session { admin: true }),
                (TabKind::InputHeld, false, LampTone::Warning),
            ] {
                let (geometry, lamp) = lamp_rows(dpi, admin, tone);
                let view = TabView {
                    kind,
                    admin,
                    expanded: true,
                };
                let mut tab = painter
                    .paint(view, &Palette::STANDARD, dpi, 3840, 1.0)
                    .unwrap();
                let width = tab.surface.width;
                let pixels = tab.surface.pixels();
                for (row, expected) in lamp.iter().enumerate() {
                    assert!(
                        pixels[row * width..(row + 1) * width]
                            .iter()
                            .all(|pixel| pixel == expected),
                        "{view:?} at {dpi} DPI, row {row} differs from the helper's lamp"
                    );
                }
                let below = usize::try_from(geometry.height()).unwrap() * width;
                assert_eq!(
                    pixels[below] >> 24,
                    0,
                    "outside the shoulders the tab is transparent below the lamp"
                );
            }
        }
    }

    /// Composites the helper's lamp and the server's tab over synthetic applications, the way
    /// they meet on screen. Set `CONTROLFREAK_INDICATOR_RENDERS` to a folder.
    #[test]
    #[ignore = "writes PNG composites for design review"]
    fn render_review_composites() {
        let Some(folder) = std::env::var_os("CONTROLFREAK_INDICATOR_RENDERS") else {
            return;
        };
        let folder = std::path::PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let painter = Painter::new().unwrap();
        for dpi in [96, 120, 144, 168] {
            for (scene, kind, admin) in [
                ("light-working", TabKind::Working, false),
                ("dark-waiting-admin", TabKind::Waiting, true),
                ("light-compact", TabKind::Working, false),
                ("dark-held", TabKind::InputHeld, false),
            ] {
                let s = f64::from(dpi) / 96.0;
                let px = |dip: f64| -> usize {
                    // Scene sizes are small positive DIP values.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let value = (dip * s).round() as usize;
                    value
                };
                // Tall enough for the whole input-held warning and its shadow.
                let tall = kind == TabKind::InputHeld;
                let (width, height) = (px(960.0), px(if tall { 200.0 } else { 150.0 }));
                let dark = scene.starts_with("dark");
                let mut scene_pixels = vec![0_u32; width * height];
                for (row, line) in scene_pixels.chunks_exact_mut(width).enumerate() {
                    let color = match (dark, row) {
                        (false, row) if row < px(34.0) => 0x00f3_f3f3,
                        (false, row) if row < px(76.0) => 0x00fa_fafa,
                        (false, _) => 0x00ff_ffff,
                        (true, row) if row < px(34.0) => 0x0025_2526,
                        (true, _) => 0x001e_1e1e,
                    };
                    line.fill(color);
                }
                let tone = if kind == TabKind::InputHeld {
                    LampTone::Warning
                } else {
                    LampTone::Session { admin }
                };
                let (_, lamp) = lamp_rows(dpi, admin, tone);
                for (row, pixel) in lamp.iter().enumerate() {
                    for x in 0..width {
                        blend(&mut scene_pixels[row * width + x], *pixel);
                    }
                }
                let view = TabView {
                    kind,
                    admin,
                    expanded: scene != "light-compact",
                };
                let mut tab = painter
                    .paint(
                        view,
                        &Palette::STANDARD,
                        dpi,
                        i32::try_from(width).unwrap(),
                        1.0,
                    )
                    .unwrap();
                let tab_width = tab.surface.width;
                let left = (width - tab_width) / 2;
                for (row, line) in tab.surface.pixels().chunks_exact(tab_width).enumerate() {
                    if row >= height {
                        break;
                    }
                    for (x, pixel) in line.iter().enumerate() {
                        blend(&mut scene_pixels[row * width + left + x], *pixel);
                    }
                }
                let rgb: Vec<u8> = scene_pixels
                    .iter()
                    .flat_map(|pixel| {
                        let [blue, green, red, _] = pixel.to_le_bytes();
                        [red, green, blue]
                    })
                    .collect();
                let file =
                    std::fs::File::create(folder.join(format!("composite-{scene}-{dpi}.png")))
                        .unwrap();
                let mut encoder = png::Encoder::new(
                    std::io::BufWriter::new(file),
                    u32::try_from(width).unwrap(),
                    u32::try_from(height).unwrap(),
                );
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&rgb)
                    .unwrap();
            }
        }
    }

    /// Premultiplied source-over onto an opaque pixel.
    fn blend(destination: &mut u32, source: u32) {
        let alpha = source >> 24;
        let inverse = 255 - alpha;
        let channel = |shift: u32| {
            let over = (source >> shift) & 0xff;
            let under = (*destination >> shift) & 0xff;
            (over + under * inverse / 255).min(255) << shift
        };
        *destination = channel(16) | channel(8) | channel(0);
    }
}
