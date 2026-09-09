#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    io::{self, BufRead, BufWriter, Write},
    mem::size_of,
    ptr::NonNull,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{
            COLORREF, E_ACCESSDENIED, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT,
            RPC_E_CHANGED_MODE, SIZE, WPARAM,
        },
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
            EnumDisplayMonitors, GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO,
            SelectObject,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Shell::{IVirtualDesktopManager, VirtualDesktopManager},
            WindowsAndMessaging::{
                CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
                DispatchMessageW, GetForegroundWindow, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE,
                MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
                TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE,
                WM_MOUSEACTIVATE, WM_NCHITTEST, WNDCLASSW, WS_DISABLED, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{BOOL, Error as WindowsError, GUID, w},
};

const PROTOCOL_PREFIX: &str = "CFP/1";
const PARTICLE_COUNT: usize = 800;
const BAND_THICKNESS: i32 = 132;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const TOPOLOGY_INTERVAL: Duration = Duration::from_millis(500);
const MINIMUM_VISIBLE: Duration = Duration::from_millis(800);
const FADE_IN: Duration = Duration::from_millis(250);
const LEVEL_TRANSITION: Duration = Duration::from_millis(180);
const FADE_OUT: Duration = Duration::from_millis(600);
const INITIAL_VISIBLE_OPACITY: f64 = 0.18;

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
    spawn_command_reader(sender)?;
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    write_state(&mut output, config.generation, 0, "HIDDEN")?;

    let mut visual = VisualState::new();
    let started = Instant::now();
    let mut next_frame = Instant::now();
    let mut next_topology = Instant::now() + TOPOLOGY_INTERVAL;
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
                        started.elapsed().as_secs_f64(),
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

        if now >= next_topology {
            renderer.sync_topology(false)?;
            next_topology = now + TOPOLOGY_INTERVAL;
        }

        if visual.visible && now >= next_frame {
            renderer.render(
                started.elapsed().as_secs_f64(),
                visual.level,
                visual.opacity(now),
            )?;
            next_frame = now + FRAME_INTERVAL;
        }

        let sleep = if visual.visible {
            next_frame.saturating_duration_since(Instant::now())
        } else {
            Duration::from_millis(20)
        };
        thread::sleep(sleep.min(Duration::from_millis(20)));
    }
}

fn spawn_command_reader(sender: mpsc::Sender<InputEvent>) -> io::Result<()> {
    thread::Builder::new()
        .name("controlfreak-native-glow-input".to_owned())
        .spawn(move || {
            for line in io::stdin().lock().lines() {
                match line {
                    Ok(line) => {
                        if sender.send(InputEvent::Line(line)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(InputEvent::Error(error.to_string()));
                        return;
                    }
                }
            }
            let _ = sender.send(InputEvent::Eof);
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

    const fn acting(self) -> bool {
        matches!(self, Self::Acting | Self::ElevatedActing)
    }

    const fn target_opacity(self) -> f64 {
        if self.acting() { 1.0 } else { 0.42 }
    }
}

#[derive(Clone, Copy)]
enum ProtocolCommand {
    Level { sequence: u64, level: Level },
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
    seconds: f64,
) -> io::Result<bool> {
    match command {
        ProtocolCommand::Level { sequence, level } => {
            let now = Instant::now();
            visual.set_level(level, now);
            renderer.present_level(seconds, level, visual.opacity(now))?;
            write_state(output, generation, sequence, level.state())?;
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
        let current = self
            .opacity(now)
            .max(INITIAL_VISIBLE_OPACITY.min(level.target_opacity()));
        let duration = if self.visible {
            LEVEL_TRANSITION
        } else {
            self.visible = true;
            self.visible_since = now;
            FADE_IN
        };
        self.level = level;
        self.pending_hide = None;
        self.transition = Transition {
            started: now,
            duration,
            from: current,
            to: level.target_opacity(),
        };
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

    fn opacity(&self, now: Instant) -> f64 {
        if let Some(hide) = self.pending_hide.as_ref()
            && now >= hide.fade_start
        {
            let progress = duration_fraction(now - hide.fade_start, FADE_OUT);
            return hide.from * (1.0 - smooth(progress));
        }
        self.transition.value(now)
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

struct Renderer {
    manager: IVirtualDesktopManager,
    desktop_id: Option<GUID>,
    monitors: Vec<MonitorRect>,
    windows: Vec<LayerWindow>,
    visible: bool,
}

impl Renderer {
    fn new(manager: IVirtualDesktopManager) -> io::Result<Self> {
        let mut renderer = Self {
            manager,
            desktop_id: None,
            monitors: Vec::new(),
            windows: Vec::new(),
            visible: false,
        };
        renderer.sync_topology(false)?;
        Ok(renderer)
    }

    fn sync_topology(&mut self, require_foreground: bool) -> io::Result<()> {
        let monitors = enumerate_monitors()?;
        if monitors != self.monitors {
            let mut replacement = Vec::with_capacity(monitors.len() * 4);
            for (monitor_index, monitor) in monitors.iter().copied().enumerate() {
                for side in Side::ALL {
                    replacement.push(LayerWindow::create(monitor, side, monitor_index)?);
                }
            }
            self.windows = replacement;
            self.monitors = monitors;
            self.desktop_id = None;
        }
        if self.follow_foreground_desktop()? || !require_foreground {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Windows reported no foreground window for the desktop glow",
            ))
        }
    }

    fn follow_foreground_desktop(&mut self) -> io::Result<bool> {
        // SAFETY: This has no pointer parameters and returns a Windows-owned handle or null.
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_invalid() {
            return Ok(false);
        }
        // SAFETY: The COM manager is initialized and `foreground` is a live Windows-owned HWND.
        let desktop_id = unsafe { self.manager.GetWindowDesktopId(foreground) }
            .map_err(|error| windows_error("IVirtualDesktopManager::GetWindowDesktopId", &error))?;
        if self.desktop_id == Some(desktop_id) {
            return Ok(true);
        }
        for window in &self.windows {
            // SAFETY: Both the COM interface and helper-owned HWND are live for this call.
            unsafe {
                self.manager
                    .MoveWindowToDesktop(window.hwnd, &raw const desktop_id)
            }
            .map_err(|error| {
                windows_error("IVirtualDesktopManager::MoveWindowToDesktop", &error)
            })?;
            window.position(self.visible)?;
        }
        self.desktop_id = Some(desktop_id);
        Ok(true)
    }

    fn set_visible(&mut self, visible: bool) -> io::Result<()> {
        self.visible = visible;
        for window in &self.windows {
            if visible {
                window.position(true)?;
                // SAFETY: The helper owns this HWND and requests a non-activating visibility change.
                let _ = unsafe { ShowWindow(window.hwnd, SW_SHOWNOACTIVATE) };
            } else {
                // SAFETY: The helper owns this HWND and hiding it retains no Rust references.
                let _ = unsafe { ShowWindow(window.hwnd, SW_HIDE) };
            }
        }
        Ok(())
    }

    fn present_level(&mut self, seconds: f64, level: Level, opacity: f64) -> io::Result<()> {
        // A LEVEL acknowledgement authorizes the parent to mutate input, so first make sure the
        // overlay is on the current foreground desktop and has a non-transparent rendered frame.
        self.sync_topology(true)?;
        self.render(seconds, level, opacity)?;
        self.set_visible(true)
    }

    fn render(&mut self, seconds: f64, level: Level, opacity: f64) -> io::Result<()> {
        let pulse = 0.93 + (seconds * std::f64::consts::TAU / 6.8).sin() * 0.07;
        let opacity = (opacity * pulse).clamp(0.0, 1.0);
        for window in &mut self.windows {
            window.render(seconds, level, opacity)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MonitorRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl MonitorRect {
    const fn width(self) -> i32 {
        self.right - self.left
    }

    const fn height(self) -> i32 {
        self.bottom - self.top
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    const fn seed(self, monitor_index: usize) -> u64 {
        let base = match self {
            Self::Left => 1103,
            Self::Right => 2207,
            Self::Top => 3301,
            Self::Bottom => 4409,
        };
        base + monitor_index as u64
    }

    const fn band_rect(self, monitor: MonitorRect) -> MonitorRect {
        let thickness = if matches!(self, Self::Left | Self::Right) {
            if monitor.width() < BAND_THICKNESS {
                monitor.width()
            } else {
                BAND_THICKNESS
            }
        } else if monitor.height() < BAND_THICKNESS {
            monitor.height()
        } else {
            BAND_THICKNESS
        };
        match self {
            Self::Left => MonitorRect {
                right: monitor.left + thickness,
                ..monitor
            },
            Self::Right => MonitorRect {
                left: monitor.right - thickness,
                ..monitor
            },
            Self::Top => MonitorRect {
                bottom: monitor.top + thickness,
                ..monitor
            },
            Self::Bottom => MonitorRect {
                top: monitor.bottom - thickness,
                ..monitor
            },
        }
    }
}

struct LayerWindow {
    hwnd: HWND,
    bounds: MonitorRect,
    side: Side,
    surface: DibSurface,
    particles: Vec<Particle>,
}

impl LayerWindow {
    fn create(monitor: MonitorRect, side: Side, monitor_index: usize) -> io::Result<Self> {
        let bounds = side.band_rect(monitor);
        let width = bounds.width();
        let height = bounds.height();
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
                bounds.left,
                bounds.top,
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
        let surface = match DibSurface::create(width, height) {
            Ok(surface) => surface,
            Err(error) => {
                // SAFETY: Window creation succeeded and ownership has not escaped this function.
                let _ = unsafe { DestroyWindow(hwnd) };
                return Err(error);
            }
        };
        let mut random = Random::new(side.seed(monitor_index));
        let particles = (0..PARTICLE_COUNT)
            .map(|index| Particle::new(index, &mut random))
            .collect();
        let window = Self {
            hwnd,
            bounds,
            side,
            surface,
            particles,
        };
        window.position(false)?;
        Ok(window)
    }

    fn position(&self, show: bool) -> io::Result<()> {
        let flags = if show {
            SWP_NOACTIVATE | SWP_SHOWWINDOW
        } else {
            SWP_NOACTIVATE
        };
        // SAFETY: This helper owns `hwnd`; all dimensions are validated monitor bounds.
        unsafe {
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.bounds.left,
                self.bounds.top,
                self.bounds.width(),
                self.bounds.height(),
                flags,
            )
        }
        .map_err(|error| windows_error("SetWindowPos", &error))
    }

    fn render(&mut self, seconds: f64, level: Level, opacity: f64) -> io::Result<()> {
        let width = self.surface.width;
        let height = self.surface.height;
        let width_f64 = f64::from(u32::try_from(width).unwrap_or(u32::MAX));
        let height_f64 = f64::from(u32::try_from(height).unwrap_or(u32::MAX));
        let length = if matches!(self.side, Side::Left | Side::Right) {
            height_f64
        } else {
            width_f64
        };
        let thickness = if matches!(self.side, Side::Left | Side::Right) {
            width_f64
        } else {
            height_f64
        };
        let visible_count = if level.acting() {
            self.particles.len()
        } else {
            (self.particles.len() * 55).div_ceil(100)
        };
        let colors = if level.elevated() {
            &RED_PARTICLES
        } else {
            &BLUE_PARTICLES
        };
        let halo = if level.elevated() {
            RED_HALO
        } else {
            BLUE_HALO
        };
        let pixels = self.surface.pixels();
        pixels.fill(0);

        for (index, particle) in self.particles[..visible_count].iter().copied().enumerate() {
            let direction = if index % 2 == 0 { 1.0 } else { -1.0 };
            let mut along = particle.long_position * length
                + (seconds * particle.long_speed + particle.phase).sin()
                    * particle.long_drift
                    * direction;
            along %= length;
            if along < 0.0 {
                along += length;
            }
            let inward = (particle.distance * (thickness - 4.0).max(1.0)
                + (seconds * particle.cross_speed + particle.phase_two).sin() * particle.sway)
                .clamp(0.0, thickness);
            let pulse = 0.88 + (seconds * 1.6 + particle.phase_two).sin() * 0.12;
            let radius_x = particle.width * pulse * 0.5;
            let radius_y = particle.height * pulse * 0.5;
            let (x, y) = match self.side {
                Side::Left => (inward, along),
                Side::Right => (width_f64 - inward, along),
                Side::Top => (along, inward),
                Side::Bottom => (along, height_f64 - inward),
            };
            if particle.has_halo {
                draw_ellipse(
                    pixels,
                    width,
                    height,
                    x,
                    y,
                    radius_x * 3.1,
                    radius_y * 3.1,
                    halo,
                    opacity,
                );
            }
            draw_ellipse(
                pixels,
                width,
                height,
                x,
                y,
                radius_x,
                radius_y,
                colors[particle.color_index],
                opacity,
            );
        }
        self.surface.present(self.hwnd, self.bounds)
    }
}

impl Drop for LayerWindow {
    fn drop(&mut self) {
        // SAFETY: `hwnd` is owned by this value and is destroyed exactly once during Drop.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

struct DibSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: NonNull<u32>,
    width: usize,
    height: usize,
}

impl DibSurface {
    fn create(width: i32, height: i32) -> io::Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "desktop glow window dimensions must be positive",
            ));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(u32::MAX),
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut raw_bits = std::ptr::null_mut::<c_void>();
        // SAFETY: `info` and the writable pixel-pointer output remain valid for this synchronous
        // call. The returned bitmap and its storage are owned by this surface.
        let bitmap = unsafe {
            CreateDIBSection(
                None,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut raw_bits,
                None,
                0,
            )
        }
        .map_err(|error| windows_error("CreateDIBSection", &error))?;
        let bits = NonNull::new(raw_bits.cast::<u32>()).ok_or_else(|| {
            // SAFETY: CreateDIBSection returned this owned bitmap, but no pixel storage, so it has
            // not been selected and can be released immediately.
            let _ = unsafe { DeleteObject(bitmap.into()) };
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "CreateDIBSection returned no pixel storage",
            )
        })?;
        // SAFETY: A null compatible DC creates a process-owned memory DC.
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            // SAFETY: `bitmap` is owned here and was never selected into a DC.
            let _ = unsafe { DeleteObject(bitmap.into()) };
            return Err(last_windows_error("CreateCompatibleDC"));
        }
        // SAFETY: Both the memory DC and compatible bitmap are live and owned here.
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        if previous.is_invalid() {
            // SAFETY: Selection failed, so both independently owned GDI resources can be deleted.
            unsafe {
                let _ = DeleteDC(dc);
                let _ = DeleteObject(bitmap.into());
            }
            return Err(last_windows_error("SelectObject"));
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
            bits,
            width: usize::try_from(width).unwrap_or(0),
            height: usize::try_from(height).unwrap_or(0),
        })
    }

    fn pixels(&mut self) -> &mut [u32] {
        let len = self.width.saturating_mul(self.height);
        // SAFETY: CreateDIBSection allocated exactly width*height 32-bit pixels. The mutable borrow
        // of the surface guarantees exclusive access for the returned slice lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.bits.as_ptr(), len) }
    }

    fn present(&self, hwnd: HWND, bounds: MonitorRect) -> io::Result<()> {
        let destination = POINT {
            x: bounds.left,
            y: bounds.top,
        };
        let size = SIZE {
            cx: bounds.width(),
            cy: bounds.height(),
        };
        let source = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: u8::try_from(AC_SRC_OVER).unwrap_or(0),
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: u8::try_from(AC_SRC_ALPHA).unwrap_or(1),
        };
        // SAFETY: The helper owns the HWND, memory DC, selected DIB, and all parameter structures;
        // the synchronous call retains none of their pointers.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&raw const destination),
                Some(&raw const size),
                Some(self.dc),
                Some(&raw const source),
                COLORREF(0),
                Some(&raw const blend),
                ULW_ALPHA,
            )
        }
        .map_err(|error| windows_error("UpdateLayeredWindow", &error))
    }
}

impl Drop for DibSurface {
    fn drop(&mut self) {
        // SAFETY: These resources are owned by this surface. Restoring the previous selection
        // makes the bitmap deletable before the memory DC is released, each exactly once.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

#[derive(Clone, Copy)]
struct Particle {
    long_position: f64,
    distance: f64,
    width: f64,
    height: f64,
    long_drift: f64,
    sway: f64,
    long_speed: f64,
    cross_speed: f64,
    phase: f64,
    phase_two: f64,
    color_index: usize,
    has_halo: bool,
}

impl Particle {
    fn new(index: usize, random: &mut Random) -> Self {
        let curve = if index.is_multiple_of(3) { 1.1 } else { 2.6 };
        let size = 0.55 + random.unit().powf(2.0) * 2.1;
        Self {
            long_position: random.unit(),
            distance: random.unit().powf(curve),
            width: size,
            height: size * (0.72 + random.unit() * 0.7),
            long_drift: 24.0 + random.unit() * 96.0,
            sway: 3.0 + random.unit() * 14.0,
            long_speed: 0.55 + random.unit() * 0.95,
            cross_speed: 0.95 + random.unit() * 1.65,
            phase: random.unit() * std::f64::consts::TAU,
            phase_two: random.unit() * std::f64::consts::TAU,
            color_index: random.index(BLUE_PARTICLES.len()),
            has_halo: index.is_multiple_of(19),
        }
    }
}

struct Random(u64);

impl Random {
    const fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn unit(&mut self) -> f64 {
        let sample = u32::try_from(self.next() >> 32).unwrap_or(u32::MAX);
        f64::from(sample) / f64::from(u32::MAX)
    }

    fn index(&mut self, length: usize) -> usize {
        usize::try_from(self.next() % length as u64).unwrap_or(0)
    }
}

#[derive(Clone, Copy)]
struct Color {
    alpha: u8,
    red: u8,
    green: u8,
    blue: u8,
}

const BLUE_PARTICLES: [Color; 6] = [
    Color::new(0xB8, 0x34, 0x47, 0x72),
    Color::new(0xC8, 0x3C, 0x52, 0x87),
    Color::new(0xD8, 0x45, 0x5B, 0x94),
    Color::new(0xE0, 0x53, 0x69, 0xAB),
    Color::new(0xE8, 0x5F, 0x77, 0xC2),
    Color::new(0xF0, 0x77, 0x95, 0xE6),
];

const RED_PARTICLES: [Color; 6] = [
    Color::new(0xB8, 0x72, 0x22, 0x22),
    Color::new(0xC8, 0x87, 0x26, 0x26),
    Color::new(0xD8, 0x9C, 0x2B, 0x2B),
    Color::new(0xE0, 0xB2, 0x32, 0x32),
    Color::new(0xE8, 0xCB, 0x3A, 0x3A),
    Color::new(0xF0, 0xE6, 0x49, 0x49),
];

const BLUE_HALO: Color = Color::new(0x32, 0x53, 0x69, 0xAB);
const RED_HALO: Color = Color::new(0x42, 0xB2, 0x32, 0x32);

impl Color {
    const fn new(alpha: u8, red: u8, green: u8, blue: u8) -> Self {
        Self {
            alpha,
            red,
            green,
            blue,
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn draw_ellipse(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    center_x: f64,
    center_y: f64,
    radius_x: f64,
    radius_y: f64,
    color: Color,
    opacity: f64,
) {
    let radius_x = radius_x.max(0.55);
    let radius_y = radius_y.max(0.55);
    let left = ((center_x - radius_x - 1.0).floor() as i32).max(0);
    let right = ((center_x + radius_x + 1.0).ceil() as i32)
        .min(i32::try_from(width).unwrap_or(i32::MAX) - 1);
    let top = ((center_y - radius_y - 1.0).floor() as i32).max(0);
    let bottom = ((center_y + radius_y + 1.0).ceil() as i32)
        .min(i32::try_from(height).unwrap_or(i32::MAX) - 1);
    for y in top..=bottom {
        for x in left..=right {
            let dx = (f64::from(x) + 0.5 - center_x) / radius_x;
            let dy = (f64::from(y) + 0.5 - center_y) / radius_y;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = ((1.15 - distance) * 3.4).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = (f64::from(color.alpha) * opacity * coverage)
                .round()
                .clamp(0.0, 255.0) as u8;
            if alpha == 0 {
                continue;
            }
            let index = usize::try_from(y).unwrap_or(0) * width + usize::try_from(x).unwrap_or(0);
            blend_pixel(&mut pixels[index], color, alpha);
        }
    }
}

fn blend_pixel(destination: &mut u32, color: Color, alpha: u8) {
    let source_alpha = u32::from(alpha);
    let inverse = 255 - source_alpha;
    let destination_alpha = (*destination >> 24) & 0xff;
    let destination_red = (*destination >> 16) & 0xff;
    let destination_green = (*destination >> 8) & 0xff;
    let destination_blue = *destination & 0xff;
    let source_red = u32::from(color.red) * source_alpha / 255;
    let source_green = u32::from(color.green) * source_alpha / 255;
    let source_blue = u32::from(color.blue) * source_alpha / 255;
    let output_alpha = source_alpha + destination_alpha * inverse / 255;
    let output_red = source_red + destination_red * inverse / 255;
    let output_green = source_green + destination_green * inverse / 255;
    let output_blue = source_blue + destination_blue * inverse / 255;
    *destination = (output_alpha << 24) | (output_red << 16) | (output_green << 8) | output_blue;
}

#[derive(Default)]
struct MonitorEnumeration {
    monitors: Vec<MonitorRect>,
    error: Option<String>,
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` points to the uniquely borrowed enumeration supplied to the synchronous
    // EnumDisplayMonitors call and remains live throughout this callback.
    let enumeration = unsafe { &mut *(data.0 as *mut MonitorEnumeration) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| monitor_rect(monitor)));
    match result {
        Ok(Ok(rect)) => {
            enumeration.monitors.push(rect);
            BOOL(1)
        }
        Ok(Err(error)) => {
            enumeration.error = Some(error.to_string());
            BOOL(0)
        }
        Err(_) => {
            enumeration.error = Some("monitor enumeration callback panicked".to_owned());
            BOOL(0)
        }
    }
}

fn enumerate_monitors() -> io::Result<Vec<MonitorRect>> {
    let mut enumeration = MonitorEnumeration::default();
    // SAFETY: The LPARAM points to `enumeration`, which is uniquely borrowed and remains alive for
    // the complete synchronous enumeration; the callback catches panics across the FFI boundary.
    let succeeded = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(monitor_callback),
            LPARAM((&raw mut enumeration).cast::<c_void>() as isize),
        )
    };
    if !succeeded.as_bool() {
        return Err(io::Error::other(
            enumeration
                .error
                .unwrap_or_else(|| WindowsError::from_thread().to_string()),
        ));
    }
    if enumeration.monitors.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Windows reported no active monitors for the desktop glow",
        ));
    }
    enumeration
        .monitors
        .sort_by_key(|monitor| (monitor.left, monitor.top, monitor.right, monitor.bottom));
    Ok(enumeration.monitors)
}

fn monitor_rect(monitor: HMONITOR) -> io::Result<MonitorRect> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    // SAFETY: `info` is correctly sized writable storage and `monitor` came from enumeration.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(last_windows_error("GetMonitorInfoW"));
    }
    let rect = MonitorRect {
        left: info.rcMonitor.left,
        top: info.rcMonitor.top,
        right: info.rcMonitor.right,
        bottom: info.rcMonitor.bottom,
    };
    if rect.width() <= 0 || rect.height() <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows reported invalid monitor bounds for the desktop glow",
        ));
    }
    Ok(rect)
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
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::WindowFromPoint;

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
        assert!(parse_command("CFP/1 42 1 EXPLODE", 42).is_err());
    }

    #[test]
    fn elevated_palette_contains_red_without_orange_bias() {
        assert!(
            RED_PARTICLES
                .iter()
                .all(|color| color.red > color.green && color.green == color.blue)
        );
        assert_eq!(RED_HALO.green, RED_HALO.blue);
    }

    #[test]
    fn premultiplied_blending_preserves_transparency() {
        let mut pixel = 0_u32;
        blend_pixel(&mut pixel, Color::new(255, 200, 40, 40), 128);
        assert_eq!(pixel >> 24, 128);
        assert_eq!((pixel >> 16) & 0xff, 100);
    }

    #[test]
    fn acknowledged_levels_start_with_a_visible_opacity() {
        let mut visual = VisualState::new();
        let started = Instant::now();
        visual.set_level(Level::Acting, started);
        assert!(visual.opacity(started) >= INITIAL_VISIBLE_OPACITY);

        visual.begin_hide(1, false, started);
        let nearly_hidden = started + MINIMUM_VISIBLE + FADE_OUT;
        visual.set_level(Level::ElevatedActing, nearly_hidden);
        assert!(visual.opacity(nearly_hidden) >= INITIAL_VISIBLE_OPACITY);
        assert!(visual.pending_hide.is_none());
    }

    #[test]
    fn point_targeting_skips_the_disabled_glow_overlay() {
        ensure_dpi_awareness().unwrap();
        register_window_class().unwrap();
        let monitor = enumerate_monitors().unwrap().remove(0);
        let window = LayerWindow::create(monitor, Side::Left, 0).unwrap();
        window.position(true).unwrap();
        let point = POINT {
            x: window.bounds.left + 1,
            y: window.bounds.top + window.bounds.height() / 2,
        };

        // SAFETY: `point` is a value inside the visible overlay bounds and the returned HWND is
        // borrowed only for this equality check.
        let target = unsafe { WindowFromPoint(point) };

        assert_ne!(target, window.hwnd);
    }
}
