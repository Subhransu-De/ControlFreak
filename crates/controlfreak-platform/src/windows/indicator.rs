//! Shared look of the control indicator: the lamp the glow helper draws along the top of every
//! display, and the tab the server draws hanging from it. Both processes take geometry and colour
//! from this module, so a lamp segment painted by one lines up with the lamp painted by the other.

#![allow(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    io,
    mem::size_of,
    ptr::NonNull,
    time::{Duration, Instant},
};

use super::win32::{visit_monitors, windows_error};
use windows::{
    Win32::{
        Foundation::{COLORREF, D2DERR_RECREATE_TARGET, HWND, POINT, RECT, SIZE},
        Graphics::{
            Direct2D::{
                Common::{
                    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
                    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
                    D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT,
                },
                D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL,
                D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
                D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_LINE_JOIN_ROUND, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE,
                D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
                D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Brush,
                ID2D1DCRenderTarget, ID2D1Factory, ID2D1PathGeometry, ID2D1SolidColorBrush,
                ID2D1StrokeStyle,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT, DWRITE_LINE_METRICS, DWRITE_LINE_SPACING_METHOD_UNIFORM,
                DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
                DWriteCreateFactory, IDWriteFactory, IDWriteFactory5, IDWriteFontCollection,
                IDWriteTextLayout,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
                COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT,
                CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
                GetMonitorInfoW, GetSysColor, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO,
                SelectObject,
            },
        },
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, ULW_ALPHA,
                UpdateLayeredWindow,
            },
        },
    },
    core::{BOOL, Error as WindowsError, IUnknown, Interface, PCWSTR, w},
};
use windows_numerics::Vector2;

// ---------------------------------------------------------------------------------------------
// Geometry shared by both processes
// ---------------------------------------------------------------------------------------------

/// Lamp thickness in DIPs. High Contrast administrator sessions use a thicker lamp, so the
/// difference survives a theme that replaces every colour with the system highlight.
const LAMP_DIP: f32 = 4.0;
const LAMP_HIGH_CONTRAST_ADMIN_DIP: f32 = 7.0;
/// When input may still be held the lamp becomes a double rule, so the warning is told apart by
/// shape as well as colour: in High Contrast, where every lamp is the highlight colour, and for
/// colour-blind users, for whom amber and the administrator red converge.
const WARNING_LAMP_DIP: f32 = 8.0;
/// An opaque line in the surface colour under the lamp keeps turquoise legible on white, where
/// the lamp alone measures about 1.8:1.
const KEYLINE_DIP: f32 = 1.0;

/// Physical-pixel lamp geometry. The helper and the server both call [`lamp_geometry`] or
/// [`warning_geometry`], so a tab's own lamp segment always covers exactly the rows of the
/// helper's lamp beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LampGeometry {
    pub(crate) lamp: i32,
    keyline: i32,
    /// Two lit bands with a gap between them, instead of one solid band.
    double: bool,
}

impl LampGeometry {
    pub(crate) const fn height(self) -> i32 {
        self.lamp + self.keyline
    }

    /// Whether lamp row `row` (from the top of the display) is lit; unlit lamp rows and the
    /// keyline below them take the surface colour.
    pub(crate) const fn lit(self, row: i32) -> bool {
        if row >= self.lamp {
            return false;
        }
        if !self.double {
            return true;
        }
        // Three eighths lit, a quarter dark, three eighths lit: 3/2/3 at 100%, 6 lit pixels.
        let band = (3 * self.lamp + 4) / 8;
        row < band || row >= self.lamp - band
    }
}

pub(crate) fn lamp_geometry(dpi: u32, high_contrast: bool, admin: bool) -> LampGeometry {
    let lamp = if high_contrast && admin {
        LAMP_HIGH_CONTRAST_ADMIN_DIP
    } else {
        LAMP_DIP
    };
    LampGeometry {
        lamp: scale_px(lamp, dpi),
        keyline: scale_px(KEYLINE_DIP, dpi),
        double: false,
    }
}

/// The lamp while input may still be held, in every palette.
pub(crate) fn warning_geometry(dpi: u32) -> LampGeometry {
    LampGeometry {
        lamp: scale_px(WARNING_LAMP_DIP, dpi),
        keyline: scale_px(KEYLINE_DIP, dpi),
        double: true,
    }
}

/// Rounds a DIP length to whole physical pixels, never below one pixel.
fn scale_px(dip: f32, dpi: u32) -> i32 {
    to_px(dip * scale(dpi)).max(1)
}

#[allow(clippy::cast_precision_loss)]
fn scale(dpi: u32) -> f32 {
    // Monitor DPI values are small (96..=960), so the conversion is exact.
    dpi.max(96) as f32 / 96.0
}

#[allow(clippy::cast_possible_truncation)]
fn to_px(value: f32) -> i32 {
    // Indicator lengths are bounded by monitor sizes, far inside the i32 range.
    value.round() as i32
}

#[allow(clippy::cast_precision_loss)]
fn to_f(value: i32) -> f32 {
    // Pixel coordinates within a monitor are exactly representable.
    value as f32
}

#[allow(clippy::cast_precision_loss)]
fn count_f(value: u32) -> f32 {
    // Line counts are tiny.
    value as f32
}

/// One monitor, in signed virtual-desktop pixels, with its effective DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Monitor {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) right: i32,
    pub(crate) bottom: i32,
    pub(crate) dpi: u32,
}

impl Monitor {
    pub(crate) const fn width(self) -> i32 {
        self.right - self.left
    }

    pub(crate) const fn height(self) -> i32 {
        self.bottom - self.top
    }

    /// The width and height a tab may take on this display, keeping clear of each edge.
    pub(crate) fn tab_space(self) -> (i32, i32) {
        let clearance = scale_px(TAB_CLEARANCE_DIP, self.dpi);
        (
            (self.width() - 2 * clearance).max(1),
            (self.height() - 2 * clearance).max(1),
        )
    }
}

/// Space kept clear on each side of a tab, in DIPs.
const TAB_CLEARANCE_DIP: f32 = 16.0;

/// Active monitors sorted by position, so an unchanged layout compares equal between polls.
pub(crate) fn enumerate_monitors() -> io::Result<Vec<Monitor>> {
    let mut monitors = Vec::new();
    visit_monitors(|monitor| {
        monitors.push(monitor_info(monitor).map_err(|error| error.to_string())?);
        Ok(())
    })
    .map_err(io::Error::other)?;
    if monitors.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Windows reported no active monitors for the indicator",
        ));
    }
    monitors.sort_by_key(|monitor| (monitor.left, monitor.top, monitor.right, monitor.bottom));
    Ok(monitors)
}

fn monitor_info(monitor: HMONITOR) -> io::Result<Monitor> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    // SAFETY: `info` is correctly sized writable storage and `monitor` came from enumeration.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(windows_error(
            "GetMonitorInfoW",
            &WindowsError::from_thread(),
        ));
    }
    let (mut dpi_x, mut dpi_y) = (96_u32, 96_u32);
    // SAFETY: Both outputs are writable locals that outlive this synchronous call. A failure keeps
    // the 96 DPI default, which changes only the indicator's size, never its position.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) };
    let rect = Monitor {
        left: info.rcMonitor.left,
        top: info.rcMonitor.top,
        right: info.rcMonitor.right,
        bottom: info.rcMonitor.bottom,
        dpi: dpi_x.max(96),
    };
    if rect.width() <= 0 || rect.height() <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows reported invalid monitor bounds for the indicator",
        ));
    }
    Ok(rect)
}

// ---------------------------------------------------------------------------------------------
// Colour and system settings
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rgb(pub(crate) u8, pub(crate) u8, pub(crate) u8);

impl Rgb {
    fn from_colorref(color: COLORREF) -> Self {
        let [red, green, blue, _] = color.0.to_le_bytes();
        Self(red, green, blue)
    }

    /// `amount` of `self` over `base`; the tab's rim takes its tint from the state colour.
    fn mix(self, base: Self, amount: f32) -> Self {
        let channel = |a: u8, b: u8| {
            let value = f32::from(a) * amount + f32::from(b) * (1.0 - amount);
            u8::try_from(to_px(value).clamp(0, 255)).unwrap_or(u8::MAX)
        };
        Self(
            channel(self.0, base.0),
            channel(self.1, base.1),
            channel(self.2, base.2),
        )
    }

    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: f32::from(self.0) / 255.0,
            g: f32::from(self.1) / 255.0,
            b: f32::from(self.2) / 255.0,
            a: 1.0,
        }
    }
}

/// SAE J3134 recommends turquoise marker lamps to show that an automated system is driving.
pub(crate) const TURQUOISE: Rgb = Rgb(0x38, 0xd6, 0xc4);
const TURQUOISE_INK: Rgb = Rgb(0x06, 0x30, 0x2b);
pub(crate) const ADMIN_RED: Rgb = Rgb(0xff, 0x5d, 0x55);
const ADMIN_INK: Rgb = Rgb(0x3a, 0x0d, 0x0b);
pub(crate) const AMBER: Rgb = Rgb(0xf5, 0xa5, 0x24);
const AMBER_INK: Rgb = Rgb(0x3a, 0x25, 0x00);
const STOP_GREY: Rgb = Rgb(0x8d, 0x91, 0x96);
const STOP_INK: Rgb = Rgb(0x17, 0x18, 0x1a);
const NEUTRAL_BADGE: Rgb = Rgb(0x3a, 0x3d, 0x41);
const NEUTRAL_RIM: Rgb = Rgb(0x6b, 0x6f, 0x74);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    pub(crate) high_contrast: bool,
    pub(crate) surface: Rgb,
    ink: Rgb,
    ink2: Rgb,
    divider: Rgb,
    key_face: Rgb,
    key_rim: Rgb,
    key_lip: Rgb,
    highlight: Rgb,
    highlight_text: Rgb,
}

impl Palette {
    pub(crate) const STANDARD: Self = Self {
        high_contrast: false,
        surface: Rgb(0x20, 0x21, 0x23),
        ink: Rgb(0xf3, 0xf4, 0xf4),
        ink2: Rgb(0xa3, 0xa7, 0xab),
        divider: Rgb(0x34, 0x37, 0x3b),
        key_face: Rgb(0x2a, 0x2c, 0x2f),
        key_rim: Rgb(0x4a, 0x4d, 0x52),
        key_lip: Rgb(0x17, 0x18, 0x1b),
        highlight: TURQUOISE,
        highlight_text: TURQUOISE_INK,
    };

    /// High Contrast uses only the theme's window, window text, highlight and highlight text
    /// colours.
    const fn high_contrast(
        window: Rgb,
        window_text: Rgb,
        highlight: Rgb,
        highlight_text: Rgb,
    ) -> Self {
        Self {
            high_contrast: true,
            surface: window,
            ink: window_text,
            ink2: window_text,
            divider: window_text,
            key_face: window,
            key_rim: window_text,
            key_lip: window,
            highlight,
            highlight_text,
        }
    }

    /// The palette for the current Windows contrast setting.
    pub(crate) fn system() -> Self {
        if !high_contrast_enabled() {
            return Self::STANDARD;
        }
        let color = |index| {
            // SAFETY: GetSysColor takes a constant index and returns a value; no pointers.
            Rgb::from_colorref(COLORREF(unsafe { GetSysColor(index) }))
        };
        Self::high_contrast(
            color(COLOR_WINDOW),
            color(COLOR_WINDOWTEXT),
            color(COLOR_HIGHLIGHT),
            color(COLOR_HIGHLIGHTTEXT),
        )
    }
}

/// What the lamp signals. The helper picks it from its protocol level and tone; the tab picks it
/// from its own state, through the same mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LampTone {
    Session { admin: bool },
    Warning,
}

pub(crate) fn lamp_color(tone: LampTone, palette: &Palette) -> Rgb {
    if palette.high_contrast {
        return palette.highlight;
    }
    match tone {
        LampTone::Session { admin: false } => TURQUOISE,
        LampTone::Session { admin: true } => ADMIN_RED,
        LampTone::Warning => AMBER,
    }
}

fn high_contrast_enabled() -> bool {
    let mut contrast = HIGHCONTRASTW {
        cbSize: u32::try_from(size_of::<HIGHCONTRASTW>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: For SPI_GETHIGHCONTRAST, pvParam is a writable HIGHCONTRASTW whose cbSize matches
    // uiParam; it outlives this synchronous call and is not retained.
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            Some((&raw mut contrast).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    result.is_ok() && contrast.dwFlags.contains(HCF_HIGHCONTRASTON)
}

/// Reads the per-user "Show animations in Windows" setting. Keeps the animated default if the
/// query fails, so an unreadable setting never changes the indicator unexpectedly.
pub(crate) fn animations_enabled() -> bool {
    let mut enabled = BOOL(1);
    // SAFETY: For SPI_GETCLIENTAREAANIMATION, pvParam must point to a writable BOOL that outlives
    // this synchronous call; `enabled` is such a local and is not retained.
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut enabled).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    result.is_err() || enabled.as_bool()
}

/// The Windows "Text size" accessibility setting, 1.0 to 2.25. It enlarges the tab's type, not
/// its lamp or spacing.
pub(crate) fn text_scale() -> f32 {
    let mut value = 100_u32;
    let mut size = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: The key, value name and type filter are constants; `value` and `size` are writable
    // locals sized for a REG_DWORD and outlive this synchronous call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Accessibility"),
            w!("TextScaleFactor"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut size),
        )
    };
    if status.is_err() {
        return 1.0;
    }
    f32::from(u16::try_from(value.clamp(100, 225)).unwrap_or(100)) / 100.0
}

/// The lamp and the tab fade in over the same time, from the same opacity, on the same smoothstep
/// curve, so they arrive together.
pub(crate) const FADE_IN: Duration = Duration::from_millis(150);
/// The opacity a fade-in starts from, so its first frame is already visible.
pub(crate) const FADE_IN_FROM: f32 = 0.18;

// ---------------------------------------------------------------------------------------------
// What the tab says
// ---------------------------------------------------------------------------------------------

/// How long the two-line tab introduces a new session before settling into one row.
const INTRO: Duration = Duration::from_secs(3);
/// Waiting shows only after this long without an action, so quick actions never flicker.
const WAITING_DELAY: Duration = Duration::from_secs(1);
/// How long "This PC is yours again" stays, with or without animations.
const HANDBACK_AFTER_STOP: Duration = Duration::from_millis(2_500);
/// A normal end of a session hands the PC back quietly and quickly, so the tab is dormant again
/// soon after `end_control_session`.
const HANDBACK: Duration = Duration::from_secs(1);
/// How long "reading the screen" stays after the last observation, so a quick capture is still
/// seen.
pub(crate) const READING: Duration = Duration::from_millis(1_500);
/// A session that starts this soon after the last one ended skips the two-line introduction, so
/// an agent working through one-shot actions does not keep re-announcing itself.
const RETURN_GRACE: Duration = Duration::from_mins(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// No indicator level has been acknowledged yet in this session.
    Idle,
    Waiting,
    Working,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopStatus {
    Ready,
    Draining,
    Stopped,
    CleanupFailed,
}

impl StopStatus {
    pub(crate) fn parse(status: &str) -> Self {
        match status {
            "draining" => Self::Draining,
            "stopped" => Self::Stopped,
            "cleanup_failed" => Self::CleanupFailed,
            _ => Self::Ready,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    Working,
    Waiting,
    Stopping,
    InputHeld,
    HandedBack {
        after_stop: bool,
    },
    /// An observation tool is reading the screen. No session is owned and no input is sent.
    Reading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabView {
    pub(crate) kind: TabKind,
    pub(crate) admin: bool,
    pub(crate) expanded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabInputs {
    pub(crate) session_active: bool,
    pub(crate) stop: StopStatus,
    pub(crate) activity: Activity,
    /// How many actions have been acknowledged so far; a change means an action ran, even one
    /// that started and finished between two polls.
    pub(crate) actions: u64,
    pub(crate) elevated: bool,
    /// An observation tool read the screen within [`READING`].
    pub(crate) reading: bool,
}

/// Decides what the tab shows. Precedence, highest first: input may be held (until the server
/// exits); stopping (at once on Esc or a client stop); working (at once when an action starts);
/// waiting (only after [`WAITING_DELAY`] without an action); handed back (after a session ends,
/// for [`HANDBACK`], cleared at once by a new session); reading the screen (an observation with
/// no session, for [`READING`]).
#[derive(Debug, Default)]
pub(crate) struct TabModel {
    session_started: Option<Instant>,
    introduce: bool,
    last_working: Option<Instant>,
    last_session_end: Option<Instant>,
    handback: Option<(Instant, bool)>,
    seen_actions: u64,
    was_active: bool,
}

impl TabModel {
    pub(crate) fn view(&mut self, now: Instant, input: TabInputs) -> Option<TabView> {
        let admin = input.elevated;
        // The warning never changes its words while someone may be reading them; only a display
        // too short for the full card shortens it (see `Painter::layout_within`).
        if input.stop == StopStatus::CleanupFailed {
            self.handback = None;
            self.was_active = input.session_active;
            return Some(TabView {
                kind: TabKind::InputHeld,
                admin,
                expanded: true,
            });
        }
        if input.session_active {
            if !self.was_active {
                self.session_started = Some(now);
                self.last_working = None;
                self.introduce = self
                    .last_session_end
                    .is_none_or(|end| now.saturating_duration_since(end) >= RETURN_GRACE);
            }
            self.was_active = true;
            self.handback = None;
            if matches!(input.stop, StopStatus::Draining | StopStatus::Stopped) {
                return Some(TabView {
                    kind: TabKind::Stopping,
                    admin,
                    expanded: true,
                });
            }
            if input.activity == Activity::Working || input.actions != self.seen_actions {
                self.last_working = Some(now);
            }
            self.seen_actions = input.actions;
            let working = self
                .last_working
                .is_some_and(|at| now.saturating_duration_since(at) < WAITING_DELAY);
            let expanded = self.introduce
                && self
                    .session_started
                    .is_some_and(|at| now.saturating_duration_since(at) < INTRO);
            return Some(TabView {
                kind: if working {
                    TabKind::Working
                } else {
                    TabKind::Waiting
                },
                admin,
                expanded,
            });
        }
        if self.was_active {
            self.was_active = false;
            self.last_session_end = Some(now);
            let after_stop = input.stop != StopStatus::Ready;
            let hold = if after_stop {
                HANDBACK_AFTER_STOP
            } else {
                HANDBACK
            };
            self.handback = Some((now + hold, after_stop));
        }
        match self.handback {
            // A normal end is one quiet row; after a stop, the second line explains that
            // ControlFreak stays stopped.
            Some((until, after_stop)) if now < until => Some(TabView {
                kind: TabKind::HandedBack { after_stop },
                admin: false,
                expanded: after_stop,
            }),
            _ => {
                self.handback = None;
                input.reading.then_some(TabView {
                    kind: TabKind::Reading,
                    admin: false,
                    expanded: false,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Badge {
    Pointer { filled: bool },
    Stop,
    Warning,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Content {
    title: &'static str,
    status: &'static str,
    /// Recovery steps, each its own short paragraph under the status.
    steps: &'static [&'static str],
    badge: Badge,
    esc: bool,
}

/// The remedy, its escalation, then how to use the server again. Shorter cards drop steps from
/// the end, so the remedy and the escalation are the last to go.
const RECOVERY_STEPS: [&str; 3] = [
    "Press and release any key or mouse button that seems stuck.",
    "If typing or the pointer still misbehaves, sign out or restart Windows.",
    "Restart your agent app to use ControlFreak again.",
];

/// The tab's words as sentences, for screen readers, assuming one row says "Esc to stop" and two
/// lines say "Press Esc to stop". The window name normally comes from what was actually painted
/// (see `PaintedTab::spoken`); this is the name for the input-held warning, which always keeps
/// the full card, even where a short display shows less.
pub(crate) fn spoken(view: TabView) -> String {
    say(&content(view), view.admin, !view.expanded)
}

/// Joins visible words into sentences: title, what the "Administrator" chip means, status,
/// steps, then the stop instruction in the form shown.
fn say(words: &Content, admin: bool, short_esc: bool) -> String {
    let mut sentences = vec![words.title.to_owned()];
    if admin {
        // A bare "Administrator" read aloud does not say what it is about.
        sentences.push("Running as administrator".to_owned());
    }
    if !words.status.is_empty() {
        sentences.push(words.status.trim_end_matches('.').to_owned());
    }
    sentences.extend(
        words
            .steps
            .iter()
            .map(|step| step.trim_end_matches('.').to_owned()),
    );
    if words.esc {
        sentences.push(
            if short_esc {
                "Esc to stop"
            } else {
                "Press Esc to stop"
            }
            .to_owned(),
        );
    }
    sentences.join(". ") + "."
}

const TITLE_ACTIVE: &str = "An agent is using this PC";
const WORKING_STATUS: &str = "ControlFreak is working";
const WAITING_STATUS: &str = "ControlFreak is waiting for the next action";
const READING_TITLE: &str = "ControlFreak is reading the screen";
const READING_STATUS: &str = "It is not using the keyboard or mouse";
const HELD_TITLE: &str = "A key or mouse button may still be held";
const CHIP: &str = "Administrator";

fn content(view: TabView) -> Content {
    let plain = |title, status, badge, esc| Content {
        title,
        status,
        steps: &[],
        badge,
        esc,
    };
    match view.kind {
        TabKind::Working => plain(
            TITLE_ACTIVE,
            WORKING_STATUS,
            Badge::Pointer { filled: true },
            true,
        ),
        TabKind::Waiting => plain(
            TITLE_ACTIVE,
            WAITING_STATUS,
            Badge::Pointer { filled: false },
            true,
        ),
        TabKind::Stopping => plain(
            "Stopping desktop control",
            "ControlFreak is releasing held keys and mouse buttons",
            Badge::Stop,
            false,
        ),
        // Shorter forms for short displays are cut from this card in `Painter::layout_within`.
        TabKind::InputHeld => Content {
            title: HELD_TITLE,
            status: "ControlFreak stopped but could not confirm it released everything.",
            steps: &RECOVERY_STEPS,
            badge: Badge::Warning,
            esc: false,
        },
        TabKind::HandedBack { after_stop: false } => plain(
            "This PC is yours again",
            "ControlFreak released the keyboard and mouse",
            Badge::Check,
            false,
        ),
        TabKind::HandedBack { after_stop: true } => plain(
            "This PC is yours again",
            "Restart your agent app to use ControlFreak",
            Badge::Check,
            false,
        ),
        // No session, so no Esc: reading never swallows a key.
        TabKind::Reading => plain(
            READING_TITLE,
            READING_STATUS,
            Badge::Pointer { filled: false },
            false,
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tones {
    lamp: Option<Rgb>,
    badge: Rgb,
    badge_ink: Rgb,
    rim: Rgb,
}

fn tones(view: TabView, palette: &Palette) -> Tones {
    let session = LampTone::Session { admin: view.admin };
    let (lamp, badge, badge_ink, rim) = match view.kind {
        TabKind::Working | TabKind::Waiting => {
            let (color, ink, rim_amount) = if view.admin {
                (ADMIN_RED, ADMIN_INK, 0.55)
            } else {
                (TURQUOISE, TURQUOISE_INK, 0.40)
            };
            (
                Some(lamp_color(session, palette)),
                color,
                ink,
                color.mix(palette.surface, rim_amount),
            )
        }
        // Stopping keeps the session's lamp: input may still be moving while keys are released.
        // Its bare stop square is drawn in the text colour, so it never reads as disabled.
        TabKind::Stopping => (
            Some(lamp_color(session, palette)),
            palette.ink,
            STOP_INK,
            STOP_GREY.mix(palette.surface, 0.40),
        ),
        TabKind::InputHeld => (
            Some(lamp_color(LampTone::Warning, palette)),
            AMBER,
            AMBER_INK,
            AMBER.mix(palette.surface, 0.40),
        ),
        // Neither owns the PC: no lamp, and the neutral grey says nothing is being controlled.
        TabKind::HandedBack { .. } | TabKind::Reading => (
            None,
            NEUTRAL_BADGE,
            palette.ink,
            NEUTRAL_RIM.mix(palette.surface, 0.40),
        ),
    };
    if palette.high_contrast {
        return Tones {
            lamp: lamp.map(|_| palette.highlight),
            badge: palette.highlight,
            badge_ink: palette.highlight_text,
            rim: palette.ink,
        };
    }
    Tones {
        lamp,
        badge,
        badge_ink,
        rim,
    }
}

// ---------------------------------------------------------------------------------------------
// Layered-window surface
// ---------------------------------------------------------------------------------------------

/// A premultiplied BGRA DIB selected into a memory DC, presented with `UpdateLayeredWindow`.
pub(crate) struct DibSurface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: NonNull<u32>,
    pub(crate) width: usize,
    height: usize,
}

impl DibSurface {
    pub(crate) fn create(width: i32, height: i32) -> io::Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "indicator surface dimensions must be positive",
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
            return Err(windows_error(
                "CreateCompatibleDC",
                &WindowsError::from_thread(),
            ));
        }
        // SAFETY: Both the memory DC and compatible bitmap are live and owned here.
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        if previous.is_invalid() {
            // SAFETY: Selection failed, so both independently owned GDI resources can be deleted.
            unsafe {
                let _ = DeleteDC(dc);
                let _ = DeleteObject(bitmap.into());
            }
            return Err(windows_error("SelectObject", &WindowsError::from_thread()));
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

    pub(crate) fn pixels(&mut self) -> &mut [u32] {
        let len = self.width.saturating_mul(self.height);
        // SAFETY: CreateDIBSection allocated exactly width*height 32-bit pixels. The mutable borrow
        // of the surface guarantees exclusive access for the returned slice lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.bits.as_ptr(), len) }
    }

    /// Presents the surface at `origin`, with `alpha` applied to the whole window for fades.
    pub(crate) fn present(&self, hwnd: HWND, origin: POINT, alpha: u8) -> io::Result<()> {
        let size = SIZE {
            cx: i32::try_from(self.width).unwrap_or(i32::MAX),
            cy: i32::try_from(self.height).unwrap_or(i32::MAX),
        };
        let source = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: u8::try_from(AC_SRC_OVER).unwrap_or(0),
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: u8::try_from(AC_SRC_ALPHA).unwrap_or(1),
        };
        // SAFETY: The caller owns `hwnd`; this surface owns the memory DC and selected DIB, and all
        // parameter structures are locals. The synchronous call retains none of their pointers.
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&raw const origin),
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

    /// Straight-alpha RGBA rows, for synthetic renders saved as PNG.
    #[cfg(test)]
    fn rgba_rows(&mut self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.width * self.height * 4);
        for pixel in self.pixels().iter().copied() {
            let [blue, green, red, alpha] = pixel.to_le_bytes();
            let straight = |channel: u8| {
                if alpha == 0 {
                    0
                } else {
                    u8::try_from(u32::from(channel) * 255 / u32::from(alpha)).unwrap_or(255)
                }
            };
            rgba.extend_from_slice(&[straight(red), straight(green), straight(blue), alpha]);
        }
        rgba
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

// ---------------------------------------------------------------------------------------------
// Painter
// ---------------------------------------------------------------------------------------------

// Inter 4.1 (SIL Open Font License 1.1, see THIRD-PARTY-NOTICES.md), subset to Latin-1 and common
// punctuation so copy edits do not need a new subset.
static INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
static INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
static INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

const REGULAR: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(400);
const MEDIUM: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(500);
const SEMIBOLD: DWRITE_FONT_WEIGHT = DWRITE_FONT_WEIGHT(600);
/// Inter's cap height, used to centre the "Esc" keycap on the letters beside it.
const CAP_HEIGHT: f32 = 0.727;
/// The recovery steps wrap at this width, in DIPs: wide enough that each step is one line at
/// normal text size, narrow enough to read as a short list.
const DETAIL_WIDTH: f32 = 480.0;

/// Draws tabs with Direct2D and DirectWrite on the thread that created it.
pub(crate) struct Painter {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    fonts: IDWriteFontCollection,
    family: PCWSTR,
    /// Replaced when Direct2D loses it, as after a display driver reset (see `Painter::draw`).
    target: RefCell<ID2D1DCRenderTarget>,
    /// Whether tabs may say "Press Esc to stop". Cleared while no keyboard hook is confirmed
    /// installed, so the tab never promises a stop key nothing is listening for.
    esc_hint: Cell<bool>,
}

/// A painted tab: lamp segment, shoulders, body and shadow, in physical pixels.
pub(crate) struct PaintedTab {
    pub(crate) surface: DibSurface,
    pub(crate) width: i32,
    /// Where the tab body ends, from the top of the bitmap; a control hangs from here.
    pub(crate) body_bottom: i32,
    /// The painted words as sentences, for the window name.
    pub(crate) spoken: String,
}

struct Text {
    layout: IDWriteTextLayout,
    width: f32,
    natural_line: f32,
    baseline: f32,
    lines: u32,
}

impl Text {
    /// Gives every line exactly `line` pixels with the baseline placed the way the design's line
    /// boxes centre text, and returns the baseline offset from the top of the first line.
    fn set_line(&self, line: f32) -> io::Result<f32> {
        let baseline = self.baseline + (line - self.natural_line) / 2.0;
        // SAFETY: The layout is live and owned by this value.
        unsafe {
            self.layout
                .SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, baseline)
        }
        .map_err(|error| windows_error("SetLineSpacing", &error))?;
        Ok(baseline)
    }
}

/// A rectangle in physical pixels: left, top, right, bottom.
type Rect = (f32, f32, f32, f32);

/// A text and where it goes: `y` is the top of its first line box and `baseline` the distance
/// from there to its first baseline.
struct Placed {
    text: Text,
    x: f32,
    y: f32,
    baseline: f32,
    color: Rgb,
}

struct EscLayout {
    press: (Text, f32, f32),
    key: Rect,
    key_text: (Text, f32, f32),
    to_stop: (Text, f32, f32),
}

/// Everything measured before drawing, in physical pixels of the painted bitmap.
struct Layout {
    palette: Palette,
    tones: Tones,
    badge_kind: Badge,
    /// The painted words as sentences, the window name screen readers read.
    spoken: String,
    width: i32,
    height: i32,
    lamp: Option<LampGeometry>,
    /// Whether the lamp rows are painted. During a fade they are left clear, so the tab's copy
    /// of the lamp never doubles the helper's lamp while both are partly transparent.
    with_lamp: bool,
    /// Tab body, excluding the shoulders and the shadow margin.
    body: Rect,
    shoulder: f32,
    radius: f32,
    rim_width: f32,
    s: f32,
    badge: (f32, f32),
    badge_size: f32,
    chip: Option<(Rect, Placed)>,
    texts: Vec<Placed>,
    divider: Option<Rect>,
    esc: Option<EscLayout>,
}

impl Painter {
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: Creates a factory owned by the returned interface; no borrowed pointers.
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|error| windows_error("D2D1CreateFactory", &error))?;
        // SAFETY: Creates a shared DirectWrite factory owned by the returned interface.
        let dwrite5: IDWriteFactory5 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|error| windows_error("DWriteCreateFactory", &error))?;
        let dwrite: IDWriteFactory = dwrite5
            .cast()
            .map_err(|error| windows_error("IDWriteFactory", &error))?;
        let (fonts, family) = match load_inter(&dwrite5) {
            Ok(fonts) => (fonts, w!("Inter")),
            Err(_) => (system_fonts(&dwrite)?, w!("Segoe UI")),
        };
        let target = RefCell::new(create_target(&d2d)?);
        Ok(Self {
            d2d,
            dwrite,
            fonts,
            family,
            target,
            esc_hint: Cell::new(true),
        })
    }

    /// Sets whether tabs may offer Esc; returns `true` when that changes what they say.
    pub(crate) fn set_esc_hint(&self, offered: bool) -> bool {
        self.esc_hint.replace(offered) != offered
    }

    /// Whether the bundled Inter loaded; otherwise Segoe UI stands in.
    #[cfg(test)]
    fn uses_inter(&self) -> bool {
        // SAFETY: PCWSTR constants from `w!` are static, NUL-terminated UTF-16.
        unsafe { self.family.to_string() }.is_ok_and(|family| family == "Inter")
    }

    /// Paints `view` for a display of `dpi` whose usable width is `max_width` physical pixels.
    #[cfg(test)]
    pub(crate) fn paint(
        &self,
        view: TabView,
        palette: &Palette,
        dpi: u32,
        max_width: i32,
        text_scale: f32,
    ) -> io::Result<PaintedTab> {
        self.paint_with_lamp(view, palette, dpi, (max_width, i32::MAX), text_scale, true)
    }

    /// Paints `view` within `(max_width, max_height)` physical pixels of a display, optionally
    /// leaving the lamp rows clear for a fade.
    pub(crate) fn paint_with_lamp(
        &self,
        view: TabView,
        palette: &Palette,
        dpi: u32,
        (max_width, max_height): (i32, i32),
        text_scale: f32,
        with_lamp: bool,
    ) -> io::Result<PaintedTab> {
        let mut layout =
            self.layout_within(view, palette, dpi, max_width, max_height, text_scale)?;
        layout.with_lamp = with_lamp;
        let mut surface = DibSurface::create(layout.width, layout.height)?;
        self.draw(&layout, &mut surface)?;
        // The body is a few pixels inside the bitmap, so rounding up cannot leave it.
        #[allow(clippy::cast_possible_truncation)]
        let body_bottom = layout.body.3.ceil() as i32;
        Ok(PaintedTab {
            surface,
            width: layout.width,
            body_bottom,
            spoken: layout.spoken,
        })
    }

    /// Wraps `text` within `wrap` and then evens out its lines, so no heading or paragraph ends
    /// with a single word on its own line.
    fn balanced(
        &self,
        text: &str,
        weight: DWRITE_FONT_WEIGHT,
        size: f32,
        wrap: f32,
    ) -> io::Result<Text> {
        let wrapped = self.text(text, weight, size, Some(wrap))?;
        if wrapped.lines < 2 {
            return Ok(wrapped);
        }
        let natural = self.text(text, weight, size, None)?.width;
        // Start from an even split and widen by about a word until no extra line appears.
        let mut width = (natural / count_f(wrapped.lines)).ceil();
        while width < wrap {
            let attempt = self.text(text, weight, size, Some(width))?;
            if attempt.lines <= wrapped.lines {
                return Ok(attempt);
            }
            width += size * 2.0;
        }
        Ok(wrapped)
    }

    fn text(
        &self,
        text: &str,
        weight: DWRITE_FONT_WEIGHT,
        size: f32,
        wrap_width: Option<f32>,
    ) -> io::Result<Text> {
        // SAFETY: The family name and locale are static NUL-terminated strings and the font
        // collection is a live COM object; DirectWrite copies what it keeps.
        let format = unsafe {
            self.dwrite.CreateTextFormat(
                self.family,
                &self.fonts,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
        }
        .map_err(|error| windows_error("CreateTextFormat", &error))?;
        let wrapping = if wrap_width.is_some() {
            DWRITE_WORD_WRAPPING_WRAP
        } else {
            DWRITE_WORD_WRAPPING_NO_WRAP
        };
        // SAFETY: `format` is live and owned here.
        unsafe { format.SetWordWrapping(wrapping) }
            .map_err(|error| windows_error("SetWordWrapping", &error))?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        // A pixel of slack keeps a line that exactly fits its measured width from wrapping.
        let max_width = wrap_width.map_or(100_000.0, |width| width + 1.0);
        // SAFETY: `wide` and `format` outlive this synchronous call; DirectWrite copies the text.
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&wide, &format, max_width, 100_000.0)
        }
        .map_err(|error| windows_error("CreateTextLayout", &error))?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: `metrics` is a writable local for this synchronous call.
        unsafe { layout.GetMetrics(&raw mut metrics) }
            .map_err(|error| windows_error("GetMetrics", &error))?;
        let mut lines = vec![
            DWRITE_LINE_METRICS::default();
            usize::try_from(metrics.lineCount.max(1)).unwrap_or(1)
        ];
        let mut count = 0_u32;
        // SAFETY: `lines` has room for every line the metrics reported, and `count` is writable.
        unsafe { layout.GetLineMetrics(Some(&mut lines), &raw mut count) }
            .map_err(|error| windows_error("GetLineMetrics", &error))?;
        Ok(Text {
            layout,
            width: metrics.width,
            natural_line: lines[0].height,
            baseline: lines[0].baseline,
            lines: count.max(1),
        })
    }

    /// Lays out `view` without a height limit.
    #[cfg(test)]
    fn layout(
        &self,
        view: TabView,
        palette: &Palette,
        dpi: u32,
        max_width: i32,
        text_scale: f32,
    ) -> io::Result<Layout> {
        self.layout_within(view, palette, dpi, max_width, i32::MAX, text_scale)
    }

    fn layout_within(
        &self,
        view: TabView,
        palette: &Palette,
        dpi: u32,
        max_width: i32,
        max_height: i32,
        text_scale: f32,
    ) -> io::Result<Layout> {
        // At each type size the input-held warning first gives up its explanation, then its last
        // step, keeping the remedy and its escalation; only then does the type step down from the
        // Windows text size, never below 100%. A single step is the last resort. Every other
        // state has one form.
        let mut full = content(view);
        full.esc &= self.esc_hint.get();
        let short = Content { status: "", ..full };
        let forms: &[Content] = if view.kind == TabKind::InputHeld {
            &[
                full,
                short,
                Content {
                    steps: &RECOVERY_STEPS[..2],
                    ..short
                },
            ]
        } else {
            &[full]
        };
        let mut scale = text_scale.clamp(1.0, 2.25);
        loop {
            for words in forms {
                let layout =
                    self.layout_words(view, *words, palette, dpi, (max_width, max_height), scale)?;
                if layout.width <= max_width && layout.height <= max_height {
                    return Ok(layout);
                }
            }
            if scale <= 1.0 {
                let last = if view.kind == TabKind::InputHeld {
                    Content {
                        steps: &RECOVERY_STEPS[..1],
                        ..short
                    }
                } else {
                    full
                };
                return self.layout_words(view, last, palette, dpi, (max_width, max_height), 1.0);
            }
            scale = (scale - 0.25).max(1.0);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn layout_words(
        &self,
        view: TabView,
        words: Content,
        palette: &Palette,
        dpi: u32,
        (max_width, max_height): (i32, i32),
        text_scale: f32,
    ) -> io::Result<Layout> {
        let s = scale(dpi);
        let text_scale = text_scale.clamp(1.0, 2.25);
        let t = s * text_scale;
        let dip = |value: f32| value * s;
        let tones = tones(view, palette);
        let lamp = tones.lamp.map(|_| {
            if view.kind == TabKind::InputHeld {
                warning_geometry(dpi)
            } else {
                lamp_geometry(dpi, palette.high_contrast, view.admin)
            }
        });
        let lamp_height = lamp.map_or(0.0, |lamp| to_f(lamp.height()));
        // The input-held warning is never one row.
        let compact = !view.expanded && view.kind != TabKind::InputHeld;
        // One row is 34 DIP below the lamp: 6 + a 22 DIP keycap + 6.
        let (pad_top, pad_bottom) = if compact {
            (dip(6.0), dip(6.0))
        } else {
            (dip(9.0), dip(11.0))
        };
        let pad_x = dip(16.0);
        let shoulder = dip(8.0).round();
        let radius = dip(8.0).round();
        let margin = if palette.high_contrast {
            dip(2.0).round()
        } else {
            dip(12.0).round()
        };
        // The badge grows with Windows text size, up to half again, so it stays in proportion
        // with the title without crowding the row.
        let badge_size = (18.0 * s * text_scale.min(1.5)).round();
        let text_left = pad_x + badge_size + dip(11.0);
        let title_line = (22.0 * t).round();
        let body_line = (20.0 * t).round();
        let rule = s.round().max(1.0);
        let gap = dip(7.0);
        let max_body = to_f(max_width) - 2.0 * (shoulder + margin);

        // Measure everything on one line first.
        let title = self.text(words.title, SEMIBOLD, 16.0 * t, None)?;
        let chip = if view.admin {
            Some(self.text(CHIP, SEMIBOLD, 12.0 * t, None)?)
        } else {
            None
        };
        let chip_width = chip.as_ref().map_or(0.0, |chip| chip.width + dip(12.0));
        // One row carries the status in the slot after the title. Working and waiting reserve the
        // wider of their two statuses, so the row never changes width between them.
        let attribution = if compact {
            Some(self.text(words.status, REGULAR, 14.0 * t, None)?)
        } else {
            None
        };
        let mut attribution_slot = attribution.as_ref().map_or(0.0, |text| text.width);
        if compact && words.esc {
            for other in [WORKING_STATUS, WAITING_STATUS] {
                attribution_slot =
                    attribution_slot.max(self.text(other, REGULAR, 14.0 * t, None)?.width);
            }
        }
        let held = view.kind == TabKind::InputHeld;
        let full_press = self.text("Press", MEDIUM, 14.0 * t, None)?;
        let key_text = self.text("Esc", SEMIBOLD, 13.0 * t, None)?;
        let to_stop = self.text("to stop", MEDIUM, 14.0 * t, None)?;
        let key_width = (key_text.width + dip(14.0)).max(36.0 * t).round();
        let esc_span = |press: f32| {
            let lead = if press > 0.0 { press + gap } else { 0.0 };
            lead + key_width + gap + to_stop.width
        };
        // One row, and the narrowest stacked tab, say "[Esc] to stop" without "Press".
        let mut short = compact;
        let mut esc_width = esc_span(if short { 0.0 } else { full_press.width });
        let esc_block = if words.esc {
            dip(14.0) + rule + dip(14.0) + esc_width
        } else {
            0.0
        };
        let mut column = title.width
            + if chip.is_some() {
                dip(8.0) + chip_width
            } else {
                0.0
            }
            + attribution
                .as_ref()
                .map_or(0.0, |_| dip(10.0) + attribution_slot);
        if !compact && !held {
            column = column.max(self.text(words.status, REGULAR, 14.0 * t, None)?.width);
            // Working and waiting share one width so the tab never resizes between them.
            if words.esc {
                for other in [WORKING_STATUS, WAITING_STATUS] {
                    column = column.max(self.text(other, REGULAR, 14.0 * t, None)?.width);
                }
            }
        }
        if !words.steps.is_empty() {
            column = column.max(DETAIL_WIDTH * t);
        }
        let single = text_left + column + esc_block + pad_x;
        if compact && single > max_body {
            // One row does not fit this display; the two-line tab can stack instead.
            return self.layout_within(
                TabView {
                    expanded: true,
                    ..view
                },
                palette,
                dpi,
                max_width,
                max_height,
                text_scale,
            );
        }
        let stacked = single > max_body;
        let wrap = if stacked {
            (max_body - text_left - pad_x).max(dip(120.0))
        } else {
            column
        };
        // A stacked stop line that still does not fit drops "Press", then moves under the badge.
        if stacked && text_left + esc_width + pad_x > max_body {
            short = true;
            esc_width = esc_span(0.0);
        }
        let esc_indent = if stacked && text_left + esc_width + pad_x > max_body {
            pad_x
        } else {
            text_left
        };
        let press = if short {
            self.text("", MEDIUM, 14.0 * t, None)?
        } else {
            full_press
        };

        // Final text, wrapped where the width requires it.
        let title = if stacked {
            self.balanced(words.title, SEMIBOLD, 16.0 * t, wrap)?
        } else {
            title
        };
        let status = if compact || words.status.is_empty() {
            None
        } else {
            Some(self.balanced(words.status, REGULAR, 14.0 * t, wrap)?)
        };
        // Steps wrap with the type: Windows text size widens them as it enlarges the letters. The
        // first step is the action, so it is the one set in Medium.
        let paragraph_width = wrap.min(DETAIL_WIDTH * t);
        let steps = words
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let weight = if index == 0 { MEDIUM } else { REGULAR };
                self.balanced(step, weight, 14.0 * t, paragraph_width)
            })
            .collect::<io::Result<Vec<_>>>()?;

        // Stack the text column. Every text records where its first baseline sits, so texts that
        // share a row can share one whole-pixel baseline.
        let title_baseline = title.set_line(title_line)?;
        let mut texts = Vec::new();
        let mut y = 0.0_f32;
        let mut used = title.width;
        let title_rows = count_f(title.lines);
        let title_width = title.width;
        texts.push(Placed {
            text: title,
            x: text_left,
            y,
            baseline: title_baseline,
            color: palette.ink,
        });
        y += title_line * title_rows;
        let chip_height = (18.0 * t).round();
        let chip = chip.map(|text| {
            let (left, top) = if stacked {
                let top = y + dip(3.0);
                y = top + chip_height + dip(4.0);
                (text_left, top)
            } else {
                (
                    text_left + title_width + dip(8.0),
                    ((title_line - chip_height) / 2.0).round(),
                )
            };
            used = used.max(left - text_left + chip_width);
            let rect = (left, top, left + chip_width, top + chip_height);
            let text_y = top + (chip_height - text.natural_line) / 2.0;
            let baseline = text.baseline;
            (
                rect,
                Placed {
                    text,
                    x: left + dip(6.0),
                    y: text_y,
                    baseline,
                    color: palette.ink,
                },
            )
        });
        if let Some(text) = attribution {
            let baseline = text.set_line(title_line)?;
            let x = used + text_left + dip(10.0);
            used = x - text_left + attribution_slot;
            texts.push(Placed {
                text,
                x,
                // On the title's baseline.
                y: title_baseline - baseline,
                baseline,
                color: palette.ink2,
            });
        }
        // The status, then each recovery step as its own paragraph. The explanation and the action
        // share the full ink, the action set heavier; the rest are quieter. Gaps grow with the
        // type so steps stay distinct at large text sizes; without a status, the first step sits
        // under the title like one.
        let status_color = if steps.is_empty() && !held {
            palette.ink2
        } else {
            palette.ink
        };
        let first_gap = if status.is_some() { 10.0 } else { 0.0 };
        let paragraphs = status
            .map(|text| (text, status_color, 0.0))
            .into_iter()
            .chain(steps.into_iter().enumerate().map(|(index, text)| {
                let (gap, color) = if index == 0 {
                    (first_gap, palette.ink)
                } else {
                    (6.0, palette.ink2)
                };
                (text, color, (gap * t).round())
            }));
        for (text, color, space) in paragraphs {
            let baseline = text.set_line(body_line)?;
            used = used.max(text.width);
            y += space;
            let rows = count_f(text.lines);
            texts.push(Placed {
                text,
                x: text_left,
                y,
                baseline,
                color,
            });
            y += body_line * rows;
        }
        let text_height = y;
        let column = if stacked { used } else { column };
        let content_top = lamp_height + pad_top;

        // The stop instruction: beside the text behind a vertical rule, or under it when stacked.
        let key_height = (if compact { 22.0 } else { 24.0 } * t).round();
        let mut divider = None;
        let mut esc = None;
        let (body_width, body_height, text_offset) = if words.esc {
            let press_baseline = press.set_line(body_line)?;
            let key_baseline = key_text.set_line(body_line)?;
            to_stop.set_line(body_line)?;
            let row = |left: f32, baseline: f32, press, key_text, to_stop| {
                esc_row(
                    (press, key_text, to_stop),
                    (left, baseline),
                    (press_baseline, key_baseline),
                    (key_width, key_height),
                    s,
                    t,
                )
            };
            if stacked {
                let rule_top = (content_top + text_height + dip(9.0)).round();
                let row_top = rule_top + rule + dip(9.0);
                let row_height = key_height.max(body_line);
                let width = (text_left + column).max(esc_indent + esc_width) + pad_x;
                divider = Some((esc_indent, rule_top, width - pad_x, rule_top + rule));
                let baseline = row_top + (row_height - body_line) / 2.0 + press_baseline;
                esc = Some(row(esc_indent, baseline, press, key_text, to_stop));
                (width, row_top + row_height + pad_bottom, content_top)
            } else {
                let block = text_height.max(key_height);
                let divider_x = (text_left + column + dip(14.0)).round();
                let esc_left = divider_x + rule + dip(14.0);
                let inset = if compact { dip(2.0) } else { dip(3.0) };
                divider = Some((
                    divider_x,
                    content_top + inset,
                    divider_x + rule,
                    content_top + block - inset,
                ));
                let text_offset = content_top + ((block - text_height) / 2.0).round();
                // One row shares the title's baseline; two lines centre the stop line on the
                // block between them.
                let baseline = if compact {
                    text_offset + title_baseline
                } else {
                    content_top + (block - body_line) / 2.0 + press_baseline
                };
                esc = Some(row(esc_left, baseline, press, key_text, to_stop));
                (
                    esc_left + esc_width + pad_x,
                    content_top + block + pad_bottom,
                    text_offset,
                )
            }
        } else {
            (
                text_left + column + pad_x,
                content_top + text_height + pad_bottom,
                content_top,
            )
        };

        // Shift everything into bitmap coordinates, snapping each baseline to a whole pixel.
        let body_width = body_width.round();
        let body_height = body_height.round();
        let left = margin + shoulder;
        let snap = |placed: Placed, dy: f32| Placed {
            x: (placed.x + left).round(),
            y: (placed.y + dy + placed.baseline).round() - placed.baseline,
            ..placed
        };
        let texts = texts
            .into_iter()
            .map(|placed| snap(placed, text_offset))
            .collect();
        let chip = chip.map(|((x0, y0, x1, y1), text)| {
            let dy = text_offset;
            ((x0 + left, y0 + dy, x1 + left, y1 + dy), snap(text, dy))
        });
        let badge = (
            (left + pad_x).round(),
            (text_offset + (title_line - badge_size) / 2.0).round(),
        );
        let spoken = say(&words, view.admin, short);
        Ok(Layout {
            palette: *palette,
            tones,
            badge_kind: words.badge,
            spoken,
            width: to_px(body_width + 2.0 * (shoulder + margin)),
            height: to_px(body_height + margin),
            lamp,
            with_lamp: true,
            body: (left, 0.0, left + body_width, body_height),
            shoulder,
            radius,
            rim_width: if palette.high_contrast {
                dip(2.0).round().max(2.0)
            } else {
                rule
            },
            s,
            badge,
            badge_size,
            chip,
            texts,
            divider: divider.map(|(x0, y0, x1, y1)| (x0 + left, y0, x1 + left, y1)),
            esc: esc.map(|esc| esc.shifted(left)),
        })
    }
}

/// Lays out "Press [Esc] to stop" with all three texts on `baseline`, rounded to a whole pixel.
fn esc_row(
    (press, key_text, to_stop): (Text, Text, Text),
    (left, baseline): (f32, f32),
    (press_baseline, key_baseline): (f32, f32),
    (key_width, key_height): (f32, f32),
    s: f32,
    t: f32,
) -> EscLayout {
    let gap = 7.0 * s;
    let baseline = baseline.round();
    // "Press" is empty in the short "[Esc] to stop" form, and then takes no gap either.
    let press_gap = if press.width > 0.0 { gap } else { 0.0 };
    let key_left = (left + press.width + press_gap).round();
    // Centre the key face on the 14 px capitals beside it; the lip sits below the face.
    let cap_centre = baseline - CAP_HEIGHT * 14.0 * t / 2.0;
    let lip = s.round().max(1.0);
    let key_top = (cap_centre - (key_height - lip) / 2.0).round();
    let key_text_x = key_left + (key_width - key_text.width) / 2.0;
    let key_text_y = baseline - key_baseline;
    let to_stop_x = key_left + key_width + gap;
    let text_top = baseline - press_baseline;
    EscLayout {
        press: (press, left, text_top),
        key: (
            key_left,
            key_top,
            key_left + key_width,
            key_top + key_height,
        ),
        key_text: (key_text, key_text_x, key_text_y),
        to_stop: (to_stop, to_stop_x, text_top),
    }
}

impl EscLayout {
    fn shifted(self, dx: f32) -> Self {
        let Self {
            press,
            key,
            key_text,
            to_stop,
        } = self;
        Self {
            press: (press.0, (press.1 + dx).round(), press.2),
            key: (key.0 + dx, key.1, key.2 + dx, key.3),
            key_text: (key_text.0, (key_text.1 + dx).round(), key_text.2),
            to_stop: (to_stop.0, (to_stop.1 + dx).round(), to_stop.2),
        }
    }
}

fn load_inter(factory: &IDWriteFactory5) -> io::Result<IDWriteFontCollection> {
    // SAFETY: Each call takes live COM objects created here. The in-memory loader copies the font
    // bytes because no owner object is supplied, so nothing borrows the statics afterwards.
    unsafe {
        let loader = factory
            .CreateInMemoryFontFileLoader()
            .map_err(|error| windows_error("CreateInMemoryFontFileLoader", &error))?;
        factory
            .RegisterFontFileLoader(&loader)
            .map_err(|error| windows_error("RegisterFontFileLoader", &error))?;
        let builder = factory
            .CreateFontSetBuilder()
            .map_err(|error| windows_error("CreateFontSetBuilder", &error))?;
        for data in [INTER_REGULAR, INTER_MEDIUM, INTER_SEMIBOLD] {
            let file = loader
                .CreateInMemoryFontFileReference(
                    factory,
                    data.as_ptr().cast(),
                    u32::try_from(data.len()).unwrap_or(0),
                    None::<&IUnknown>,
                )
                .map_err(|error| windows_error("CreateInMemoryFontFileReference", &error))?;
            builder
                .AddFontFile(&file)
                .map_err(|error| windows_error("AddFontFile", &error))?;
        }
        let set = builder
            .CreateFontSet()
            .map_err(|error| windows_error("CreateFontSet", &error))?;
        factory
            .CreateFontCollectionFromFontSet(&set)
            .and_then(|collection| collection.cast())
            .map_err(|error| windows_error("CreateFontCollectionFromFontSet", &error))
    }
}

fn system_fonts(factory: &IDWriteFactory) -> io::Result<IDWriteFontCollection> {
    let mut collection = None;
    // SAFETY: `collection` is a writable local that receives an owned COM reference.
    unsafe { factory.GetSystemFontCollection(&raw mut collection, false) }
        .map_err(|error| windows_error("GetSystemFontCollection", &error))?;
    collection.ok_or_else(|| io::Error::other("Windows returned no system font collection"))
}

/// Unit tests draw on the CPU. Many painters are created and dropped on parallel test threads,
/// and some GPU user-mode drivers crash when their devices are torn down concurrently. The tests
/// compare text and geometry, which the rasterizer does not change; the server uses the default.
const RENDER_TARGET_TYPE: D2D1_RENDER_TARGET_TYPE = if cfg!(test) {
    D2D1_RENDER_TARGET_TYPE_SOFTWARE
} else {
    D2D1_RENDER_TARGET_TYPE_DEFAULT
};

fn create_target(d2d: &ID2D1Factory) -> io::Result<ID2D1DCRenderTarget> {
    let properties = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: RENDER_TARGET_TYPE,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // One drawing unit is one physical pixel; the painter scales every DIP itself so
        // straight edges land on whole pixels.
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    // SAFETY: `properties` is a fully initialized local that outlives this synchronous call.
    unsafe { d2d.CreateDCRenderTarget(&raw const properties) }
        .map_err(|error| windows_error("CreateDCRenderTarget", &error))
}

/// Whether Direct2D has lost the drawing target, as after a display driver reset, so it must be
/// rebuilt before the next draw.
fn target_lost(ended: &windows::core::Result<()>) -> bool {
    ended
        .as_ref()
        .is_err_and(|error| error.code() == D2DERR_RECREATE_TARGET)
}

impl Painter {
    /// Draws `layout` into `surface`. A lost target is rebuilt and the tab drawn again once, so a
    /// graphics reset does not take the tab, and with it desktop work, down; any other failure,
    /// or losing the new target too, is returned.
    fn draw(&self, layout: &Layout, surface: &mut DibSurface) -> io::Result<()> {
        if self.draw_once(layout, surface)? {
            return Ok(());
        }
        self.target.replace(create_target(&self.d2d)?);
        if self.draw_once(layout, surface)? {
            Ok(())
        } else {
            Err(io::Error::other(
                "Direct2D lost the tab's drawing target twice in a row",
            ))
        }
    }

    /// Draws once; `Ok(false)` means Direct2D lost the target and nothing usable was drawn.
    fn draw_once(&self, layout: &Layout, surface: &mut DibSurface) -> io::Result<bool> {
        let rect = RECT {
            left: 0,
            top: 0,
            right: layout.width,
            bottom: layout.height,
        };
        let target = self.target.borrow();
        let clear = D2D1_COLOR_F::default();
        // SAFETY: The surface's memory DC and DIB are live for the whole draw, `rect` matches the
        // DIB, and every Direct2D object used below is created from this factory and target.
        unsafe {
            target
                .BindDC(surface.dc, &raw const rect)
                .map_err(|error| windows_error("BindDC", &error))?;
            target.BeginDraw();
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.Clear(Some(&raw const clear));
        }
        // Without the lamp, everything is clipped below the lamp rows so they stay transparent
        // and the helper's lamp shows through unchanged.
        let clip = layout
            .lamp
            .filter(|_| !layout.with_lamp)
            .map(|lamp| D2D_RECT_F {
                left: 0.0,
                top: to_f(lamp.height()),
                right: to_f(layout.width),
                bottom: to_f(layout.height),
            });
        if let Some(clip) = clip {
            // SAFETY: `clip` is a local; the matching pop follows the draw below.
            unsafe { target.PushAxisAlignedClip(&raw const clip, D2D1_ANTIALIAS_MODE_ALIASED) };
        }
        let drawn = self.draw_tab(layout);
        if clip.is_some() {
            // SAFETY: Pops the clip pushed above on the same target.
            unsafe { target.PopAxisAlignedClip() };
        }
        // SAFETY: Ends the draw begun above on the same target, even if a step failed.
        let ended = unsafe { target.EndDraw(None, None) };
        if target_lost(&ended) {
            return Ok(false);
        }
        drawn?;
        ended
            .map(|()| true)
            .map_err(|error| windows_error("EndDraw", &error))
    }

    fn draw_tab(&self, layout: &Layout) -> io::Result<()> {
        let target = self.target.borrow();
        let palette = &layout.palette;
        let s = layout.s;
        if !palette.high_contrast {
            // A soft 0 2 6 shadow at about 18%, built from widening low-alpha strokes so it needs
            // no effect pipeline. It follows the open outline: closing the top edge would fold the
            // path back on itself at each shoulder and the miter join would smear a spike along
            // the lamp.
            let shadow = self.tab_path(layout, s * 2.0, false)?;
            let black = self.brush(Rgb(0, 0, 0))?;
            for (width, alpha) in [(2.0, 0.07), (5.0, 0.05), (9.0, 0.035), (13.0, 0.02)] {
                // SAFETY: The brush and geometry are live objects created from this target.
                unsafe {
                    black.SetOpacity(alpha);
                    target.DrawGeometry(&shadow, &black, width * s, None::<&ID2D1StrokeStyle>);
                }
            }
        }
        let body = self.tab_path(layout, 0.0, true)?;
        let rim = self.tab_path(layout, 0.0, false)?;
        let surface = self.brush(palette.surface)?;
        let rim_brush = self.brush(layout.tones.rim)?;
        // SAFETY: Geometry and brushes are live objects created from this factory and target.
        unsafe {
            target.FillGeometry(&body, &surface, None::<&ID2D1Brush>);
            target.DrawGeometry(
                &rim,
                &rim_brush,
                layout.rim_width,
                None::<&ID2D1StrokeStyle>,
            );
        }
        if let (Some(lamp), Some(color), true) = (layout.lamp, layout.tones.lamp, layout.with_lamp)
        {
            // Row by row through the shared rule, so a double warning lamp matches the helper's
            // pixel for pixel.
            let width = to_f(layout.width);
            let lamp_brush = self.brush(color)?;
            for row in 0..lamp.height() {
                let line = D2D_RECT_F {
                    left: 0.0,
                    top: to_f(row),
                    right: width,
                    bottom: to_f(row + 1),
                };
                let brush = if lamp.lit(row) { &lamp_brush } else { &surface };
                // SAFETY: The rectangle is a local and the brushes are live.
                unsafe { target.FillRectangle(&raw const line, brush) };
            }
        }
        self.draw_badge(layout)?;
        if let Some((rect, text)) = &layout.chip {
            self.draw_chip(layout, *rect, text)?;
        }
        for placed in &layout.texts {
            self.draw_text(placed, placed.color)?;
        }
        if let Some((x0, y0, x1, y1)) = layout.divider {
            let rule = D2D_RECT_F {
                left: x0,
                top: y0,
                right: x1,
                bottom: y1,
            };
            let brush = self.brush(palette.divider)?;
            // SAFETY: The rectangle is a local and the brush is live.
            unsafe { target.FillRectangle(&raw const rule, &brush) };
        }
        if let Some(esc) = &layout.esc {
            self.draw_key(layout, esc)?;
        }
        Ok(())
    }

    fn draw_key(&self, layout: &Layout, esc: &EscLayout) -> io::Result<()> {
        let target = self.target.borrow();
        let palette = &layout.palette;
        let s = layout.s;
        let (x0, y0, x1, y1) = esc.key;
        let line = s.round().max(1.0);
        let face = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: x0 + line / 2.0,
                top: y0 + line / 2.0,
                right: x1 - line / 2.0,
                bottom: y1 - line / 2.0,
            },
            radiusX: 4.0 * s,
            radiusY: 4.0 * s,
        };
        let lip = D2D_RECT_F {
            left: x0 + 4.0 * s,
            top: y1 - 2.0 * line,
            right: x1 - 4.0 * s,
            bottom: y1 - line,
        };
        let face_brush = self.brush(palette.key_face)?;
        let lip_brush = self.brush(palette.key_lip)?;
        let rim_brush = self.brush(palette.key_rim)?;
        // SAFETY: The rectangles are locals and the brushes are live.
        unsafe {
            target.FillRoundedRectangle(&raw const face, &face_brush);
            if !palette.high_contrast {
                target.FillRectangle(&raw const lip, &lip_brush);
            }
            target.DrawRoundedRectangle(
                &raw const face,
                &rim_brush,
                line,
                None::<&ID2D1StrokeStyle>,
            );
        }
        for (text, x, y) in [&esc.press, &esc.key_text, &esc.to_stop] {
            self.draw_at(text, *x, *y, palette.ink)?;
        }
        Ok(())
    }

    fn draw_text(&self, placed: &Placed, color: Rgb) -> io::Result<()> {
        self.draw_at(&placed.text, placed.x, placed.y, color)
    }

    fn draw_at(&self, text: &Text, x: f32, y: f32, color: Rgb) -> io::Result<()> {
        let brush = self.brush(color)?;
        // SAFETY: The layout and brush are live objects; the origin is a value.
        unsafe {
            self.target.borrow().DrawTextLayout(
                Vector2::new(x, y),
                &text.layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(())
    }

    fn brush(&self, color: Rgb) -> io::Result<ID2D1SolidColorBrush> {
        let color = color.d2d();
        // SAFETY: `color` is a local that outlives this synchronous call.
        unsafe {
            self.target
                .borrow()
                .CreateSolidColorBrush(&raw const color, None)
        }
        .map_err(|error| windows_error("CreateSolidColorBrush", &error))
    }

    /// The tab outline: concave shoulders flaring into the lamp, straight sides, rounded lower
    /// corners. `closed` adds the top edge for filling; the rim strokes the open outline only, so
    /// the tab joins the lamp without a line across it.
    fn tab_path(&self, layout: &Layout, dy: f32, closed: bool) -> io::Result<ID2D1PathGeometry> {
        let (x0, _, x1, bottom) = layout.body;
        let (f, r) = (layout.shoulder, layout.radius);
        let bottom = bottom + dy;
        let top = dy;
        let arc = |x: f32, y: f32, radius: f32, clockwise: bool| D2D1_ARC_SEGMENT {
            point: Vector2::new(x, y),
            size: D2D_SIZE_F {
                width: radius,
                height: radius,
            },
            rotationAngle: 0.0,
            sweepDirection: if clockwise {
                D2D1_SWEEP_DIRECTION_CLOCKWISE
            } else {
                D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
            },
            arcSize: D2D1_ARC_SIZE_SMALL,
        };
        let left_shoulder = arc(x0, top + f, f, true);
        let bottom_left = arc(x0 + r, bottom, r, false);
        let bottom_right = arc(x1, bottom - r, r, false);
        let right_shoulder = arc(x1 + f, top, f, true);
        // SAFETY: The geometry and its sink are created and closed here; every argument is a
        // value or a local that outlives the synchronous call.
        unsafe {
            let path = self
                .d2d
                .CreatePathGeometry()
                .map_err(|error| windows_error("CreatePathGeometry", &error))?;
            let sink = path
                .Open()
                .map_err(|error| windows_error("ID2D1PathGeometry::Open", &error))?;
            sink.BeginFigure(
                Vector2::new(x0 - f, top),
                if closed {
                    D2D1_FIGURE_BEGIN_FILLED
                } else {
                    D2D1_FIGURE_BEGIN_HOLLOW
                },
            );
            sink.AddArc(&raw const left_shoulder);
            sink.AddLine(Vector2::new(x0, bottom - r));
            sink.AddArc(&raw const bottom_left);
            sink.AddLine(Vector2::new(x1 - r, bottom));
            sink.AddArc(&raw const bottom_right);
            sink.AddLine(Vector2::new(x1, top + f));
            sink.AddArc(&raw const right_shoulder);
            sink.EndFigure(if closed {
                D2D1_FIGURE_END_CLOSED
            } else {
                D2D1_FIGURE_END_OPEN
            });
            sink.Close()
                .map_err(|error| windows_error("ID2D1GeometrySink::Close", &error))?;
            Ok(path)
        }
    }

    /// The "Administrator" chip: filled red normally, outlined in the text colour in High Contrast.
    fn draw_chip(&self, layout: &Layout, (x0, y0, x1, y1): Rect, text: &Placed) -> io::Result<()> {
        let s = layout.s;
        let palette = &layout.palette;
        let chip = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: x0,
                top: y0,
                right: x1,
                bottom: y1,
            },
            radiusX: 4.0 * s,
            radiusY: 4.0 * s,
        };
        if palette.high_contrast {
            let outline = (1.5 * s).max(1.0);
            let inner = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: x0 + outline / 2.0,
                    top: y0 + outline / 2.0,
                    right: x1 - outline / 2.0,
                    bottom: y1 - outline / 2.0,
                },
                ..chip
            };
            let ink = self.brush(palette.ink)?;
            // SAFETY: The rectangle is a local and the brush is live.
            unsafe {
                self.target.borrow().DrawRoundedRectangle(
                    &raw const inner,
                    &ink,
                    outline,
                    None::<&ID2D1StrokeStyle>,
                );
            }
            self.draw_text(text, palette.ink)
        } else {
            let red = self.brush(ADMIN_RED)?;
            // SAFETY: The rectangle is a local and the brush is live.
            unsafe {
                self.target
                    .borrow()
                    .FillRoundedRectangle(&raw const chip, &red);
            }
            self.draw_text(text, ADMIN_INK)
        }
    }

    fn draw_badge(&self, layout: &Layout) -> io::Result<()> {
        // Badge geometry scales with the badge, which also follows the Windows text size.
        let s = layout.badge_size / 18.0;
        let (x, y) = layout.badge;
        let size = layout.badge_size;
        let tones = layout.tones;
        let target = self.target.borrow();
        let rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: x,
                top: y,
                right: x + size,
                bottom: y + size,
            },
            radiusX: 4.0 * s,
            radiusY: 4.0 * s,
        };
        // Waiting draws the badge as an outline, and stopping draws a bare stop square, so it never
        // reads as the empty box of the handback's check; every other state fills it.
        let outline = matches!(layout.badge_kind, Badge::Pointer { filled: false });
        let bare = layout.badge_kind == Badge::Stop;
        let badge = self.brush(tones.badge)?;
        if outline {
            let stroke = (1.5 * s).max(1.0);
            let inner = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: x + stroke / 2.0,
                    top: y + stroke / 2.0,
                    right: x + size - stroke / 2.0,
                    bottom: y + size - stroke / 2.0,
                },
                ..rect
            };
            // SAFETY: The rectangle is a local and the brush is live.
            unsafe {
                target.DrawRoundedRectangle(
                    &raw const inner,
                    &badge,
                    stroke,
                    None::<&ID2D1StrokeStyle>,
                );
            }
        } else if !bare {
            // SAFETY: The rectangle is a local and the brush is live.
            unsafe { target.FillRoundedRectangle(&raw const rect, &badge) };
        }
        let glyph = self.brush(if outline || bare {
            tones.badge
        } else {
            tones.badge_ink
        })?;
        self.draw_glyph(layout, (x + size / 2.0, y + size / 2.0), &glyph)
    }

    /// The badge symbol, centred on `(cx, cy)`.
    fn draw_glyph(
        &self,
        layout: &Layout,
        (cx, cy): (f32, f32),
        glyph: &ID2D1SolidColorBrush,
    ) -> io::Result<()> {
        let s = layout.badge_size / 18.0;
        let target = self.target.borrow();
        match layout.badge_kind {
            Badge::Pointer { .. } => {
                // The arrow from a 16 x 22 design grid, drawn 12 DIP tall.
                let k = 12.0 * s / 22.0;
                let points = [
                    (1.0, 1.0),
                    (1.0, 18.5),
                    (5.6, 14.3),
                    (8.6, 21.0),
                    (11.6, 19.7),
                    (8.7, 13.2),
                    (14.8, 13.2),
                ];
                let path = self.polyline(&points, (cx - 7.9 * k, cy - 11.0 * k), k, true, true)?;
                // SAFETY: The geometry and brush are live.
                unsafe { target.FillGeometry(&path, glyph, None::<&ID2D1Brush>) };
            }
            Badge::Stop => {
                // Drawn without a tile, so 12 DIP keeps it as heavy as the other badges.
                let half = 6.0 * s;
                let square = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: cx - half,
                        top: cy - half,
                        right: cx + half,
                        bottom: cy + half,
                    },
                    radiusX: 2.0 * s,
                    radiusY: 2.0 * s,
                };
                // SAFETY: The rectangle is a local and the brush is live.
                unsafe { target.FillRoundedRectangle(&raw const square, glyph) };
            }
            Badge::Warning => {
                // An 11 DIP triangle with an exclamation mark, from a 12-unit grid.
                let k = 11.0 * s / 12.0;
                let (ox, oy) = (cx - 6.0 * k, cy - 6.0 * k);
                let round = self.round_stroke()?;
                let triangle = self.polyline(
                    &[(6.0, 1.4), (11.0, 10.4), (1.0, 10.4)],
                    (ox, oy),
                    k,
                    false,
                    true,
                )?;
                let dot = D2D1_ELLIPSE {
                    point: Vector2::new(ox + 6.0 * k, oy + 8.8 * k),
                    radiusX: 0.85 * k,
                    radiusY: 0.85 * k,
                };
                // SAFETY: Geometry, stroke style and brush are live; the ellipse is a local.
                unsafe {
                    target.DrawGeometry(&triangle, glyph, 1.5 * s, &round);
                    target.DrawLine(
                        Vector2::new(ox + 6.0 * k, oy + 4.8 * k),
                        Vector2::new(ox + 6.0 * k, oy + 7.1 * k),
                        glyph,
                        1.5 * s,
                        &round,
                    );
                    target.FillEllipse(&raw const dot, glyph);
                }
            }
            Badge::Check => {
                let k = 10.0 * s / 12.0;
                let round = self.round_stroke()?;
                let check = self.polyline(
                    &[(2.4, 6.3), (5.0, 8.8), (9.7, 3.5)],
                    (cx - 6.0 * k, cy - 6.0 * k),
                    k,
                    false,
                    false,
                )?;
                // SAFETY: Geometry, stroke style and brush are live.
                unsafe { target.DrawGeometry(&check, glyph, 1.7 * s, &round) };
            }
        }
        Ok(())
    }

    fn round_stroke(&self) -> io::Result<ID2D1StrokeStyle> {
        let properties = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            ..Default::default()
        };
        // SAFETY: `properties` is a local that outlives this synchronous call.
        unsafe { self.d2d.CreateStrokeStyle(&raw const properties, None) }
            .map_err(|error| windows_error("CreateStrokeStyle", &error))
    }

    /// A glyph path from design-grid points scaled by `k` and placed at `origin`.
    fn polyline(
        &self,
        points: &[(f32, f32)],
        (ox, oy): (f32, f32),
        k: f32,
        filled: bool,
        closed: bool,
    ) -> io::Result<ID2D1PathGeometry> {
        let point = |(x, y): (f32, f32)| Vector2::new(ox + x * k, oy + y * k);
        // SAFETY: The geometry and sink are created and closed here from value arguments.
        unsafe {
            let path = self
                .d2d
                .CreatePathGeometry()
                .map_err(|error| windows_error("CreatePathGeometry", &error))?;
            let sink = path
                .Open()
                .map_err(|error| windows_error("ID2D1PathGeometry::Open", &error))?;
            sink.BeginFigure(
                point(points[0]),
                if filled {
                    D2D1_FIGURE_BEGIN_FILLED
                } else {
                    D2D1_FIGURE_BEGIN_HOLLOW
                },
            );
            for &next in &points[1..] {
                sink.AddLine(point(next));
            }
            sink.EndFigure(if closed {
                D2D1_FIGURE_END_CLOSED
            } else {
                D2D1_FIGURE_END_OPEN
            });
            sink.Close()
                .map_err(|error| windows_error("ID2D1GeometrySink::Close", &error))?;
            Ok(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    fn input(session_active: bool, stop: StopStatus, activity: Activity) -> TabInputs {
        TabInputs {
            session_active,
            stop,
            activity,
            actions: 0,
            elevated: false,
            reading: false,
        }
    }

    const ALL_KINDS: [TabKind; 7] = [
        TabKind::Working,
        TabKind::Waiting,
        TabKind::Stopping,
        TabKind::InputHeld,
        TabKind::HandedBack { after_stop: false },
        TabKind::HandedBack { after_stop: true },
        TabKind::Reading,
    ];

    #[test]
    fn both_processes_round_the_lamp_identically_at_every_scale() {
        for (dpi, lamp, keyline) in [
            (96, 4, 1),
            (120, 5, 1),
            (144, 6, 2),
            (168, 7, 2),
            (192, 8, 2),
        ] {
            assert_eq!(
                lamp_geometry(dpi, false, false),
                LampGeometry {
                    lamp,
                    keyline,
                    double: false
                },
                "{dpi} DPI"
            );
        }
        assert_eq!(lamp_geometry(96, true, true).lamp, 7);
        assert_eq!(lamp_geometry(96, false, true).lamp, 4);
        assert_eq!(scale_px(0.1, 96), 1, "never thinner than one pixel");
    }

    #[test]
    fn lamp_colour_follows_session_and_contrast() {
        let standard = Palette::STANDARD;
        assert_eq!(
            lamp_color(LampTone::Session { admin: false }, &standard),
            TURQUOISE
        );
        assert_eq!(
            lamp_color(LampTone::Session { admin: true }, &standard),
            ADMIN_RED
        );
        assert_eq!(lamp_color(LampTone::Warning, &standard), AMBER);
        let contrast =
            Palette::high_contrast(Rgb(0, 0, 0), Rgb(255, 255, 255), Rgb(1, 2, 3), Rgb(4, 5, 6));
        for tone in [LampTone::Session { admin: true }, LampTone::Warning] {
            assert_eq!(lamp_color(tone, &contrast), Rgb(1, 2, 3));
        }
    }

    #[test]
    fn rim_is_a_mix_of_the_state_colour_over_the_surface() {
        // 0.4 * 0xd6 + 0.6 * 0x21 = 105.4 for green.
        assert_eq!(
            TURQUOISE.mix(Palette::STANDARD.surface, 0.40),
            Rgb(0x2a, 0x69, 0x63)
        );
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(255, 255, 255), 1.0), Rgb(0, 0, 0));
        assert_eq!(
            Rgb::from_colorref(COLORREF(0x0012_3456)),
            Rgb(0x56, 0x34, 0x12)
        );
    }

    #[test]
    fn a_session_opens_expanded_then_settles_into_one_row() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let working = model
            .view(start, input(true, StopStatus::Ready, Activity::Working))
            .unwrap();
        assert_eq!(working.kind, TabKind::Working);
        assert!(working.expanded);
        let later = model
            .view(
                at(start, 3_100),
                input(true, StopStatus::Ready, Activity::Working),
            )
            .unwrap();
        assert!(!later.expanded);
    }

    #[test]
    fn waiting_appears_only_after_a_second_without_an_action() {
        let start = Instant::now();
        let mut model = TabModel::default();
        model.view(start, input(true, StopStatus::Ready, Activity::Working));
        let quick = model
            .view(
                at(start, 400),
                input(true, StopStatus::Ready, Activity::Waiting),
            )
            .unwrap();
        assert_eq!(
            quick.kind,
            TabKind::Working,
            "short gaps never flicker to waiting"
        );
        let idle = model
            .view(
                at(start, 1_050),
                input(true, StopStatus::Ready, Activity::Waiting),
            )
            .unwrap();
        assert_eq!(idle.kind, TabKind::Waiting);
        let again = model
            .view(
                at(start, 1_060),
                input(true, StopStatus::Ready, Activity::Working),
            )
            .unwrap();
        assert_eq!(
            again.kind,
            TabKind::Working,
            "working interrupts waiting at once"
        );
    }

    #[test]
    fn stopping_and_held_input_take_over_immediately() {
        let start = Instant::now();
        let mut model = TabModel::default();
        model.view(start, input(true, StopStatus::Ready, Activity::Working));
        let stopping = model
            .view(
                at(start, 10),
                input(true, StopStatus::Draining, Activity::Working),
            )
            .unwrap();
        assert_eq!(stopping.kind, TabKind::Stopping);
        assert!(stopping.expanded);
        let held = model
            .view(
                at(start, 20),
                input(true, StopStatus::CleanupFailed, Activity::Working),
            )
            .unwrap();
        assert_eq!(held.kind, TabKind::InputHeld);
        let after = model
            .view(
                at(start, 60_000),
                input(false, StopStatus::CleanupFailed, Activity::Idle),
            )
            .unwrap();
        assert_eq!(
            after.kind,
            TabKind::InputHeld,
            "the warning stays until the server exits"
        );
    }

    #[test]
    fn handback_lasts_its_full_time_and_yields_to_a_new_session() {
        let start = Instant::now();
        let mut model = TabModel::default();
        model.view(start, input(true, StopStatus::Ready, Activity::Working));
        let back = model
            .view(
                at(start, 100),
                input(false, StopStatus::Ready, Activity::Idle),
            )
            .unwrap();
        assert_eq!(back.kind, TabKind::HandedBack { after_stop: false });
        assert!(!back.admin);
        assert!(
            model
                .view(
                    at(start, 1_000),
                    input(false, StopStatus::Ready, Activity::Idle)
                )
                .is_some()
        );
        assert!(
            model
                .view(
                    at(start, 1_200),
                    input(false, StopStatus::Ready, Activity::Idle)
                )
                .is_none(),
            "a normal end returns to dormant promptly"
        );

        model.view(
            at(start, 3_000),
            input(true, StopStatus::Ready, Activity::Working),
        );
        model.view(
            at(start, 3_100),
            input(false, StopStatus::Ready, Activity::Idle),
        );
        let next = model
            .view(
                at(start, 3_200),
                input(true, StopStatus::Ready, Activity::Working),
            )
            .unwrap();
        assert_eq!(
            next.kind,
            TabKind::Working,
            "a new session replaces the handback at once"
        );
        assert!(
            !next.expanded,
            "a return within a minute skips the introduction"
        );
    }

    #[test]
    fn one_shot_sessions_introduce_themselves_once_a_minute() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let active = |model: &mut TabModel, millis| {
            model
                .view(
                    at(start, millis),
                    input(true, StopStatus::Ready, Activity::Working),
                )
                .unwrap()
        };
        let idle = |model: &mut TabModel, millis| {
            model.view(
                at(start, millis),
                input(false, StopStatus::Ready, Activity::Idle),
            )
        };
        assert!(
            active(&mut model, 0).expanded,
            "the first session introduces itself"
        );
        let back = idle(&mut model, 8_000).unwrap();
        assert_eq!(back.kind, TabKind::HandedBack { after_stop: false });
        assert!(!back.expanded, "a normal end is one quiet row");
        // A one-shot action every 12 seconds: each 8 s hold ends, then the next begins.
        for cycle in 1..5 {
            let begin = cycle * 12_000;
            assert!(!active(&mut model, begin).expanded, "cycle {cycle}");
            assert!(!active(&mut model, begin + 1_000).expanded);
            idle(&mut model, begin + 8_000);
        }
        assert!(
            active(&mut model, 48_000 + 8_000 + 61_000).expanded,
            "after a quiet minute the next session introduces itself again"
        );
    }

    #[test]
    fn an_action_shorter_than_a_poll_still_shows_as_working() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let poll = |model: &mut TabModel, millis, actions| {
            model
                .view(
                    at(start, millis),
                    TabInputs {
                        actions,
                        ..input(true, StopStatus::Ready, Activity::Waiting)
                    },
                )
                .unwrap()
                .kind
        };
        assert_eq!(poll(&mut model, 0, 0), TabKind::Waiting);
        assert_eq!(poll(&mut model, 1_500, 0), TabKind::Waiting);
        // Between these polls an action started and finished; only the counter shows it.
        assert_eq!(poll(&mut model, 1_600, 1), TabKind::Working);
        assert_eq!(poll(&mut model, 2_500, 1), TabKind::Working);
        assert_eq!(poll(&mut model, 2_700, 1), TabKind::Waiting);
    }

    #[test]
    fn recovery_steps_are_separate_and_spoken_text_matches_the_tab() {
        let view = TabView {
            kind: TabKind::InputHeld,
            admin: true,
            expanded: true,
        };
        assert_eq!(content(view).steps.len(), 3);
        let text = spoken(view);
        assert!(text.starts_with(
            "A key or mouse button may still be held. Running as administrator. ControlFreak \
             stopped"
        ));
        assert!(text.contains("sign out or restart Windows."));
        assert!(text.ends_with("Restart your agent app to use ControlFreak again."));
        let working = spoken(TabView {
            kind: TabKind::Working,
            admin: false,
            expanded: false,
        });
        assert_eq!(
            working,
            "An agent is using this PC. ControlFreak is working. Esc to stop."
        );
    }

    #[test]
    fn narrow_displays_with_large_text_keep_the_whole_stop_line_inside() {
        let painter = Painter::new().unwrap();
        for kind in [TabKind::Working, TabKind::Waiting, TabKind::InputHeld] {
            for (width, scale) in [(320, 2.0), (320, 2.25), (420, 2.25), (360, 1.5)] {
                let layout = painter
                    .layout(
                        TabView {
                            kind,
                            admin: true,
                            expanded: false,
                        },
                        &Palette::STANDARD,
                        96,
                        width,
                        scale,
                    )
                    .unwrap();
                assert!(
                    layout.width <= width,
                    "{kind:?} at {width} DIP and {scale}x text is {} wide",
                    layout.width
                );
                if let Some(esc) = &layout.esc {
                    assert!(esc.to_stop.1 + esc.to_stop.0.width <= layout.body.2);
                }
            }
        }
    }

    #[test]
    fn one_row_is_thirty_four_dip_below_the_lamp() {
        let painter = Painter::new().unwrap();
        for dpi in [96, 144] {
            let layout = painter
                .layout(
                    TabView {
                        kind: TabKind::Working,
                        admin: true,
                        expanded: false,
                    },
                    &Palette::STANDARD,
                    dpi,
                    3840,
                    1.0,
                )
                .unwrap();
            let lamp = to_f(layout.lamp.unwrap().height());
            let below = layout.body.3 - lamp;
            assert!(
                (below - 34.0 * scale(dpi)).abs() <= 1.0,
                "{dpi} DPI: {below}"
            );
            assert!(
                layout.esc.as_ref().unwrap().press.0.width < 0.5,
                "one row says \"[Esc] to stop\""
            );
        }
    }

    #[test]
    fn short_displays_drop_the_explanation_then_the_last_step_but_never_reword() {
        let painter = Painter::new().unwrap();
        let held = TabView {
            kind: TabKind::InputHeld,
            admin: false,
            expanded: true,
        };
        let within = |height| {
            painter
                .layout_within(held, &Palette::STANDARD, 96, 1920, height, 1.0)
                .unwrap()
        };
        let step = |index: usize| RECOVERY_STEPS[index].trim_end_matches('.');
        let full = within(1080);
        assert!(full.spoken.contains("could not confirm"));

        let short = within(full.height - 1);
        assert!(short.height < full.height);
        assert!(
            !short.spoken.contains("could not confirm"),
            "explanation gone"
        );
        assert_eq!(
            short
                .texts
                .iter()
                .map(|placed| placed.text.lines)
                .collect::<Vec<_>>(),
            [1, 1, 1, 1],
            "the title, then the three steps word for word, a line each"
        );
        for index in 0..3 {
            assert!(short.spoken.contains(step(index)), "{}", short.spoken);
        }

        let shorter = within(short.height - 1);
        assert!(shorter.spoken.contains(step(0)) && shorter.spoken.contains(step(1)));
        assert!(!shorter.spoken.contains(step(2)), "the restart goes first");

        assert!(
            spoken(held).contains("could not confirm") && spoken(held).contains(step(2)),
            "the window name always keeps the full card"
        );
    }

    #[test]
    fn the_window_name_follows_what_was_painted() {
        let painter = Painter::new().unwrap();
        let row = TabView {
            kind: TabKind::Working,
            admin: false,
            expanded: false,
        };
        let wide = painter
            .paint(row, &Palette::STANDARD, 96, 1920, 1.0)
            .unwrap();
        assert_eq!(
            wide.spoken,
            "An agent is using this PC. ControlFreak is working. Esc to stop."
        );
        // Too narrow for one row: painted with two lines, so it says "Press".
        let narrow = painter
            .paint(row, &Palette::STANDARD, 96, 360, 1.0)
            .unwrap();
        assert_eq!(
            narrow.spoken,
            "An agent is using this PC. ControlFreak is working. Press Esc to stop."
        );
    }

    #[test]
    fn recovery_fits_short_displays_at_large_text_sizes() {
        let painter = Painter::new().unwrap();
        let full = TabView {
            kind: TabKind::InputHeld,
            admin: true,
            expanded: true,
        };
        // The space the running tab gets, after its clearance from the display's edges.
        for (width, height) in [(320, 640), (640, 360), (1280, 720)] {
            for dpi in [96, 144] {
                let display = Monitor {
                    left: 0,
                    top: 0,
                    right: scale_px(to_f(width), dpi),
                    bottom: scale_px(to_f(height), dpi),
                    dpi,
                };
                let (room_x, room_y) = display.tab_space();
                for scale in [1.0, 2.0, 2.25] {
                    let layout = painter
                        .layout_within(full, &Palette::STANDARD, dpi, room_x, room_y, scale)
                        .unwrap();
                    assert!(
                        layout.width <= room_x && layout.height <= room_y,
                        "{width}x{height} DIP at {dpi} DPI and {scale}x text is {}x{}",
                        layout.width,
                        layout.height
                    );
                    for step in &RECOVERY_STEPS[..2] {
                        assert!(
                            layout.spoken.contains(step.trim_end_matches('.')),
                            "{width}x{height} at {scale}x text keeps the remedy and its \
                             escalation: {}",
                            layout.spoken
                        );
                    }
                }
            }
        }
        // With room to spare, all three steps stay.
        let roomy = painter
            .layout_within(full, &Palette::STANDARD, 96, 1920, 1080, 1.0)
            .unwrap();
        assert_eq!(roomy.texts.len(), 5, "title, status and three steps");
    }

    #[test]
    fn held_input_warning_keeps_its_words_until_the_server_exits() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let held = |model: &mut TabModel, millis| {
            model
                .view(
                    at(start, millis),
                    input(false, StopStatus::CleanupFailed, Activity::Idle),
                )
                .unwrap()
        };
        let first = held(&mut model, 0);
        assert_eq!(first.kind, TabKind::InputHeld);
        assert!(first.expanded);
        for millis in [15_000, 60_000, 3_600_000] {
            assert_eq!(
                held(&mut model, millis),
                first,
                "nothing is reworded or hidden while it may be read"
            );
        }
    }

    #[test]
    fn a_stopped_session_hands_back_with_the_restart_note() {
        let start = Instant::now();
        let mut model = TabModel::default();
        model.view(start, input(true, StopStatus::Ready, Activity::Working));
        model.view(
            at(start, 10),
            input(true, StopStatus::Draining, Activity::Working),
        );
        let back = model
            .view(
                at(start, 20),
                input(false, StopStatus::Stopped, Activity::Idle),
            )
            .unwrap();
        assert_eq!(back.kind, TabKind::HandedBack { after_stop: true });
        assert!(back.expanded, "the restart note needs the second line");
    }

    #[test]
    fn a_stop_keeps_its_handback_longer_than_a_normal_end() {
        let start = Instant::now();
        let mut model = TabModel::default();
        model.view(start, input(true, StopStatus::Ready, Activity::Working));
        let stopped = input(false, StopStatus::Stopped, Activity::Idle);
        model.view(
            at(start, 100),
            input(true, StopStatus::Draining, Activity::Idle),
        );
        let back = model.view(at(start, 200), stopped).unwrap();
        assert_eq!(back.kind, TabKind::HandedBack { after_stop: true });
        assert!(model.view(at(start, 2_500), stopped).is_some());
        assert!(model.view(at(start, 2_800), stopped).is_none());
    }

    #[test]
    fn reading_the_screen_shows_briefly_without_a_session_and_yields_to_everything() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let reading = |active| TabInputs {
            reading: true,
            ..input(active, StopStatus::Ready, Activity::Idle)
        };
        let view = model.view(start, reading(false)).unwrap();
        assert_eq!(view.kind, TabKind::Reading);
        assert!(!view.admin);
        assert!(!view.expanded, "one quiet row");
        assert!(
            model
                .view(
                    at(start, 10),
                    input(false, StopStatus::Ready, Activity::Idle)
                )
                .is_none()
        );

        // An owned session outranks it, so reading never hides ownership or the Stop button.
        let owned = model.view(at(start, 20), reading(true)).unwrap();
        assert_eq!(owned.kind, TabKind::Waiting);
        // A handback shows first, then reading resumes if the agent is still looking.
        let back = model.view(at(start, 30), reading(false)).unwrap();
        assert_eq!(back.kind, TabKind::HandedBack { after_stop: false });
        let after = model.view(at(start, 1_100), reading(false)).unwrap();
        assert_eq!(after.kind, TabKind::Reading);
        // Input that may still be held outranks reading.
        let held = model
            .view(
                at(start, 1_200),
                TabInputs {
                    reading: true,
                    ..input(false, StopStatus::CleanupFailed, Activity::Idle)
                },
            )
            .unwrap();
        assert_eq!(held.kind, TabKind::InputHeld);
    }

    #[test]
    fn reading_never_offers_esc_and_says_it_is_not_using_input() {
        let words = content(TabView {
            kind: TabKind::Reading,
            admin: false,
            expanded: false,
        });
        assert!(!words.esc);
        assert!(words.title.contains("reading the screen"));
        assert!(words.status.contains("not using the keyboard or mouse"));
    }

    #[test]
    fn the_waiting_tab_says_what_it_waits_for() {
        let waiting = content(TabView {
            kind: TabKind::Waiting,
            admin: false,
            expanded: false,
        });
        assert!(waiting.status.contains("waiting for the next action"));
    }

    #[test]
    fn administrator_status_is_kept_while_the_session_holds_the_pc() {
        let start = Instant::now();
        let mut model = TabModel::default();
        let elevated = |stop| TabInputs {
            session_active: true,
            stop,
            activity: Activity::Working,
            actions: 0,
            elevated: true,
            reading: false,
        };
        for (stop, kind) in [
            (StopStatus::Ready, TabKind::Working),
            (StopStatus::Draining, TabKind::Stopping),
            (StopStatus::CleanupFailed, TabKind::InputHeld),
        ] {
            let view = model.view(start, elevated(stop)).unwrap();
            assert_eq!(view.kind, kind);
            assert!(view.admin);
        }
    }

    #[test]
    fn every_state_names_controlfreak_and_only_live_states_offer_esc() {
        for kind in ALL_KINDS {
            let words = content(TabView {
                kind,
                admin: false,
                expanded: true,
            });
            let all = format!("{} {} {:?}", words.title, words.status, words.steps);
            assert!(all.contains("ControlFreak"), "{kind:?}");
            assert_eq!(
                words.esc,
                matches!(kind, TabKind::Working | TabKind::Waiting)
            );
        }
    }

    /// Where the first baseline of a placed text lands in the bitmap.
    fn baseline_of(text: &Text, y: f32) -> f32 {
        let mut lines = [DWRITE_LINE_METRICS::default(); 4];
        let mut count = 0;
        // SAFETY: `lines` and `count` are writable locals for this synchronous call.
        unsafe { text.layout.GetLineMetrics(Some(&mut lines), &raw mut count) }.unwrap();
        y + lines[0].baseline
    }

    #[test]
    fn every_text_in_the_one_row_tab_shares_one_whole_pixel_baseline() {
        let painter = Painter::new().unwrap();
        for dpi in [96, 120, 144, 168] {
            for admin in [false, true] {
                let view = TabView {
                    kind: TabKind::Working,
                    admin,
                    expanded: false,
                };
                let layout = painter
                    .layout(view, &Palette::STANDARD, dpi, 3840, 1.0)
                    .unwrap();
                let esc = layout.esc.as_ref().unwrap();
                let mut baselines: Vec<f32> = layout
                    .texts
                    .iter()
                    .map(|placed| baseline_of(&placed.text, placed.y))
                    .collect();
                for (text, _, y) in [&esc.press, &esc.key_text, &esc.to_stop] {
                    baselines.push(baseline_of(text, *y));
                }
                assert_eq!(baselines.len(), 5, "title, attribution and the stop line");
                for baseline in &baselines {
                    assert!(
                        (baseline - baselines[0]).abs() < 0.01,
                        "{dpi} DPI admin={admin}: {baselines:?}"
                    );
                    assert!(
                        (baseline - baseline.round()).abs() < 0.01,
                        "whole pixel at {dpi} DPI: {baseline}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_badge_grows_with_text_size_up_to_half_again() {
        let painter = Painter::new().unwrap();
        let view = TabView {
            kind: TabKind::Working,
            admin: false,
            expanded: true,
        };
        let size = |scale| {
            painter
                .layout(view, &Palette::STANDARD, 96, 3840, scale)
                .unwrap()
                .badge_size
        };
        assert!((size(1.0) - 18.0).abs() < f32::EPSILON);
        assert!((size(1.25) - 23.0).abs() < f32::EPSILON);
        assert!((size(2.0) - 27.0).abs() < f32::EPSILON);
    }

    fn lamp_rows_are_opaque(tab: &mut PaintedTab, lamp: LampGeometry) -> bool {
        let width = usize::try_from(tab.width).unwrap();
        let rows = usize::try_from(lamp.height()).unwrap();
        tab.surface.pixels()[..width * rows]
            .iter()
            .all(|pixel| *pixel >> 24 == 255)
    }

    #[test]
    fn a_lost_drawing_target_is_rebuilt_instead_of_failing_the_tab() {
        assert!(target_lost(&Err(D2DERR_RECREATE_TARGET.into())));
        assert!(
            !target_lost(&Err(windows::Win32::Foundation::E_FAIL.into())),
            "other drawing failures still fail closed"
        );
        assert!(!target_lost(&Ok(())));

        let painter = Painter::new().unwrap();
        let view = TabView {
            kind: TabKind::Working,
            admin: false,
            expanded: true,
        };
        let mut before = painter
            .paint(view, &Palette::STANDARD, 96, 1920, 1.0)
            .unwrap();
        painter.target.replace(create_target(&painter.d2d).unwrap());
        let mut after = painter
            .paint(view, &Palette::STANDARD, 96, 1920, 1.0)
            .unwrap();
        assert!(
            before.surface.rgba_rows() == after.surface.rgba_rows(),
            "a rebuilt target draws the same tab"
        );
    }

    #[test]
    fn painter_draws_every_state_with_the_bundled_font() {
        let painter = Painter::new().unwrap();
        assert!(painter.uses_inter(), "Inter loads from memory");
        for palette in [
            Palette::STANDARD,
            Palette::high_contrast(
                Rgb(0, 0, 0),
                Rgb(255, 255, 255),
                Rgb(0x1a, 0xeb, 0xff),
                Rgb(0, 0, 0),
            ),
        ] {
            for kind in ALL_KINDS {
                for (expanded, admin) in [(true, false), (false, true)] {
                    let view = TabView {
                        kind,
                        admin,
                        expanded,
                    };
                    let mut tab = painter.paint(view, &palette, 144, 2560, 1.0).unwrap();
                    let opaque = tab
                        .surface
                        .pixels()
                        .iter()
                        .filter(|pixel| **pixel >> 24 == 255)
                        .count();
                    assert!(
                        opaque > tab.surface.pixels().len() / 3,
                        "{view:?} paints an opaque body"
                    );
                    if !matches!(kind, TabKind::HandedBack { .. } | TabKind::Reading) {
                        // The tab's own lamp segment is opaque edge to edge, so it meets the
                        // helper's lamp without a seam.
                        let lamp = if kind == TabKind::InputHeld {
                            warning_geometry(144)
                        } else {
                            lamp_geometry(144, palette.high_contrast, admin)
                        };
                        assert!(lamp_rows_are_opaque(&mut tab, lamp), "{view:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn working_and_waiting_share_one_width() {
        let painter = Painter::new().unwrap();
        for (expanded, admin) in [(true, false), (false, false), (true, true), (false, true)] {
            let width = |kind| {
                painter
                    .paint(
                        TabView {
                            kind,
                            admin,
                            expanded,
                        },
                        &Palette::STANDARD,
                        120,
                        3840,
                        1.0,
                    )
                    .unwrap()
                    .width
            };
            assert_eq!(width(TabKind::Working), width(TabKind::Waiting));
        }
    }

    #[test]
    fn compact_tab_is_one_row_and_narrow_displays_stack_instead_of_clipping() {
        let painter = Painter::new().unwrap();
        let view = |expanded| TabView {
            kind: TabKind::Waiting,
            admin: true,
            expanded,
        };
        let compact = painter
            .paint(view(false), &Palette::STANDARD, 96, 1920, 1.0)
            .unwrap();
        let expanded = painter
            .paint(view(true), &Palette::STANDARD, 96, 1920, 1.0)
            .unwrap();
        assert!(compact.surface.height < expanded.surface.height);
        let narrow = painter
            .paint(view(true), &Palette::STANDARD, 96, 360, 1.0)
            .unwrap();
        assert!(narrow.width <= 360, "fits the display");
        assert!(
            narrow.surface.height > expanded.surface.height,
            "the stop line moves below the message"
        );
        let narrow_compact = painter
            .paint(view(false), &Palette::STANDARD, 96, 360, 1.0)
            .unwrap();
        assert_eq!(
            narrow_compact.surface.height, narrow.surface.height,
            "one row gives way to the stacked tab"
        );
        let large_text = painter
            .paint(view(true), &Palette::STANDARD, 96, 1280, 2.0)
            .unwrap();
        assert!(large_text.width <= 1280);
    }

    /// Writes synthetic renders for design review. Set `CONTROLFREAK_INDICATOR_RENDERS` to a
    /// folder.
    #[test]
    #[ignore = "writes PNG renders for design review"]
    #[allow(clippy::too_many_lines)]
    fn render_review_pngs() {
        let Some(folder) = std::env::var_os("CONTROLFREAK_INDICATOR_RENDERS") else {
            return;
        };
        let folder = std::path::PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        let painter = Painter::new().unwrap();
        let night = Palette::high_contrast(
            Rgb(0, 0, 0),
            Rgb(255, 255, 255),
            Rgb(0xd6, 0xb4, 0xfd),
            Rgb(0x2b, 0x2b, 0x2b),
        );
        let desert = Palette::high_contrast(
            Rgb(0xff, 0xfa, 0xef),
            Rgb(0x3d, 0x3d, 0x3d),
            Rgb(0x90, 0x39, 0x09),
            Rgb(0xff, 0xf5, 0xe3),
        );
        let names = [
            ("working", TabKind::Working),
            ("waiting", TabKind::Waiting),
            ("stopping", TabKind::Stopping),
            ("held", TabKind::InputHeld),
            ("back", TabKind::HandedBack { after_stop: false }),
            ("back-stopped", TabKind::HandedBack { after_stop: true }),
        ];
        let mut cases = Vec::new();
        // Only views TabModel can produce: stopping and the input-held warning always have two
        // lines or more, a handback never carries the administrator chip, and only the handback
        // after a stop has two lines.
        let occurs = |view: TabView| match view.kind {
            TabKind::Stopping | TabKind::InputHeld => view.expanded,
            TabKind::HandedBack { after_stop } => !view.admin && view.expanded == after_stop,
            _ => true,
        };
        for (name, kind) in names {
            for admin in [false, true] {
                for expanded in [true, false] {
                    if !occurs(TabView {
                        kind,
                        admin,
                        expanded,
                    }) {
                        continue;
                    }
                    for dpi in [96, 120, 144] {
                        cases.push((
                            format!(
                                "{name}{}{}-{dpi}",
                                if admin { "-admin" } else { "" },
                                if expanded { "" } else { "-compact" }
                            ),
                            TabView {
                                kind,
                                admin,
                                expanded,
                            },
                            Palette::STANDARD,
                            dpi,
                            3840,
                            1.0,
                        ));
                    }
                }
            }
        }
        for (palette_name, palette) in [("night", night), ("desert", desert)] {
            for (name, kind) in names {
                for admin in [false, true] {
                    for expanded in [true, false] {
                        let view = TabView {
                            kind,
                            admin,
                            expanded,
                        };
                        if !occurs(view) {
                            continue;
                        }
                        cases.push((
                            format!(
                                "hc-{palette_name}-{name}{}{}",
                                if admin { "-admin" } else { "" },
                                if expanded { "" } else { "-compact" }
                            ),
                            view,
                            palette,
                            96,
                            3840,
                            1.0,
                        ));
                    }
                }
            }
        }
        for (name, kind, admin, width, text) in [
            ("narrow-420-working", TabKind::Working, false, 420, 1.0),
            ("narrow-320-waiting-admin", TabKind::Waiting, true, 320, 1.0),
            ("narrow-320-held-admin", TabKind::InputHeld, true, 320, 1.0),
            ("text-200-waiting-admin", TabKind::Waiting, true, 1280, 2.0),
            ("text-200-held", TabKind::InputHeld, false, 1280, 2.0),
            ("text-200-narrow-held", TabKind::InputHeld, false, 640, 2.0),
        ] {
            cases.push((
                name.to_owned(),
                TabView {
                    kind,
                    admin,
                    expanded: true,
                },
                Palette::STANDARD,
                96,
                width,
                text,
            ));
        }
        let mut tabs: Vec<(String, PaintedTab)> = cases
            .into_iter()
            .map(|(name, view, palette, dpi, width, text)| {
                (
                    name,
                    painter.paint(view, &palette, dpi, width, text).unwrap(),
                )
            })
            .collect();
        // A display too short for the whole warning at a large text size.
        for (name, admin, text) in [
            ("short-640x360-held", false, 2.0),
            ("short-640x360-held-admin", true, 2.25),
        ] {
            let display = Monitor {
                left: 0,
                top: 0,
                right: 640,
                bottom: 360,
                dpi: 96,
            };
            let view = TabView {
                kind: TabKind::InputHeld,
                admin,
                expanded: true,
            };
            let tab = painter
                .paint_with_lamp(
                    view,
                    &Palette::STANDARD,
                    96,
                    display.tab_space(),
                    text,
                    true,
                )
                .unwrap();
            tabs.push((name.to_owned(), tab));
        }
        for (name, mut tab) in tabs {
            let rgba = tab.surface.rgba_rows();
            let file = std::fs::File::create(folder.join(format!("{name}.png"))).unwrap();
            let mut encoder = png::Encoder::new(
                std::io::BufWriter::new(file),
                u32::try_from(tab.width).unwrap(),
                u32::try_from(tab.surface.height).unwrap(),
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
        }
    }
}
