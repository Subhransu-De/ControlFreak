#![allow(unsafe_code)]

//! A disposable Win32 window for the live `ControlFreak` tests.
//!
//! Never run it on a desktop in use. It is a test double for "an application": a titled window
//! with an edit-like surface, a button, an optional owned dialog and an event log. Every key,
//! character and click that reaches it is appended to the file named by `--log`, one event per
//! line, so a test can prove what the real server delivered instead of trusting the server.
//!
//! ```text
//! controlfreak-fixture --title TEXT --log FILE [--color RRGGBB] [--pos X,Y] [--topmost]
//!                      [--with-dialog] [--lifetime-secs N] [--busy-drain-ms N] [--clip-cursor]
//!                      [--popup-on-ping]
//! controlfreak-fixture --stop-button-rect     # print the screen rectangle of the Stop agent button
//! controlfreak-fixture --invoke-stop-button   # send BM_CLICK to it, as UI Automation's Invoke does
//! controlfreak-fixture --allow-again-rect     # print the screen rectangle of the Allow again control
//! controlfreak-fixture --message-allow-again  # send BM_CLICK and sent and posted clicks to it
//! controlfreak-fixture --activate-taskbar     # try to bring the shell taskbar to the foreground
//! controlfreak-fixture --inject-click X,Y     # SendInput a left click at a screen point
//! controlfreak-fixture --release-clip         # free the pointer from any ClipCursor rectangle
//! controlfreak-fixture --clip-rect            # print the rectangle the pointer is confined to
//! controlfreak-fixture --move-window TITLE DX,DY  # move the top-level window titled TITLE
//! ```
//!
//! With `--clip-cursor` the window confines the pointer to the left part of its client area with
//! `ClipCursor` whenever it is activated, as games and remote viewers do, and frees it when it is
//! deactivated.
//!
//! With `--popup-on-ping` a press of the button also opens an untitled popup owned by the window
//! and brings it to the foreground, as a popup menu does. It logs `main popup open`.
//!
//! Log lines, screen coordinates in physical pixels:
//!
//! ```text
//! ready client=L,T,R,B label=L,T,R,B edit=L,T,R,B button=L,T,R,B [dialog=L,T,R,B] [clip=L,T,R,B]
//! main keydown 0x0D | main keyup 0x0D | main char U+0041 | main activate 1
//! main mouse left_down X Y | dialog mouse left_down X Y | main button ping | main clip on
//! ```
//!
//! A `char` line is one UTF-16 unit of a `WM_CHAR`, so a character outside the Basic Multilingual
//! Plane is two lines, `main char U+D83E` then `main char U+DD80` for U+1F980. The harness pairs
//! them per window and reports a half without its partner as unpaired.

#[cfg(not(windows))]
fn main() {
    eprintln!("the ControlFreak fixture window needs Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(win::run());
}

#[cfg(windows)]
mod win {
    use std::{
        ffi::c_void,
        fs::OpenOptions,
        io::Write,
        mem::size_of,
        path::PathBuf,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicBool, AtomicU32, Ordering},
        },
        thread,
        time::Duration,
    };
    use windows::{
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                BeginPaint, ClientToScreen, CreateFontIndirectW, CreateSolidBrush, DeleteObject,
                EndPaint, FillRect, HDC, HGDIOBJ, InvalidateRect, LOGFONTW, PAINTSTRUCT,
                SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::{
                    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
                    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                    MOUSEINPUT, SendInput, SetFocus, VIRTUAL_KEY,
                },
                WindowsAndMessaging::{
                    AdjustWindowRectEx, BM_CLICK, BN_CLICKED, BS_PUSHBUTTON, ClipCursor,
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
                    FindWindowW, GetClassNameW, GetClipCursor, GetForegroundWindow, GetMessageW,
                    GetWindowRect, GetWindowTextW, HMENU, IDC_ARROW, IsWindowVisible, LoadCursorW,
                    MSG, PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassW,
                    SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SendMessageW,
                    SetCursorPos, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
                    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CHAR,
                    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_KEYUP,
                    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_PAINT, WM_PRINTCLIENT, WM_QUIT,
                    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER,
                    WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_TOPMOST, WS_MINIMIZEBOX, WS_POPUP,
                    WS_SYSMENU, WS_VISIBLE, WaitMessage,
                },
            },
        },
        core::{BOOL, PCWSTR, w},
    };

    const CLIENT_WIDTH: i32 = 360;
    const CLIENT_HEIGHT: i32 = 260;
    const CLIENT: RECT = RECT {
        left: 0,
        top: 0,
        right: CLIENT_WIDTH,
        bottom: CLIENT_HEIGHT,
    };
    const LABEL: RECT = RECT {
        left: 0,
        top: 0,
        right: CLIENT_WIDTH,
        bottom: 64,
    };
    const EDIT: RECT = RECT {
        left: 16,
        top: 76,
        right: 344,
        bottom: 156,
    };
    const BUTTON: RECT = RECT {
        left: 16,
        top: 176,
        right: 136,
        bottom: 216,
    };
    const DIALOG_WIDTH: i32 = 200;
    const DIALOG_HEIGHT: i32 = 90;
    const DIALOG_AREA: RECT = RECT {
        left: 0,
        top: 0,
        right: DIALOG_WIDTH,
        bottom: DIALOG_HEIGHT,
    };
    /// Where `--clip-cursor` confines the pointer: the left half of the client area. It holds
    /// the button and the left part of the edit surface, so a test can click inside it and ask
    /// for a point on the edit surface outside it.
    const CLIP_AREA: RECT = RECT {
        left: 0,
        top: 0,
        right: 180,
        bottom: CLIENT_HEIGHT,
    };
    const BUTTON_ID: usize = 100;
    const LIFETIME_TIMER: usize = 1;
    const LABEL_TEXT: &str = "Banana";

    static LOG: OnceLock<PathBuf> = OnceLock::new();
    static TYPED: Mutex<String> = Mutex::new(String::new());
    /// The high surrogate the main window (0) and the dialog (1) each wait to pair, for painting.
    static PENDING_HIGH: Mutex<[Option<u16>; 2]> = Mutex::new([None; 2]);
    /// COLORREF (0x00BBGGRR) of the window background.
    static BACKGROUND: AtomicU32 = AtomicU32::new(0x00D0_D0D0);
    /// Set by `--clip-cursor` before any window exists.
    static CLIP_CURSOR: AtomicBool = AtomicBool::new(false);
    /// Set by `--popup-on-ping` before any window exists.
    static POPUP_ON_PING: AtomicBool = AtomicBool::new(false);

    struct Options {
        title: String,
        log: Option<PathBuf>,
        position: (i32, i32),
        topmost: bool,
        dialog: bool,
        /// Self-destruct bound so a crashed test run cannot leave a window behind.
        lifetime_secs: u32,
        /// Nonzero: behave like a busy UI framework (see `busy_draining_loop`).
        busy_drain_ms: u32,
    }

    impl Default for Options {
        fn default() -> Self {
            Self {
                title: String::new(),
                log: None,
                position: (10, 10),
                topmost: false,
                dialog: false,
                lifetime_secs: 300,
                busy_drain_ms: 0,
            }
        }
    }

    pub fn run() -> i32 {
        // SAFETY: Called once before any window exists; the context is a constant.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let mut options = Options::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--stop-button-rect" => return print_rect(&STOP_BUTTON),
                "--invoke-stop-button" => return invoke_stop_button(),
                "--allow-again-rect" => return print_rect(&ALLOW_AGAIN),
                "--message-allow-again" => return message_allow_again(),
                "--activate-taskbar" => return activate_taskbar(),
                "--inject-click" => return inject_click(&args.next().unwrap_or_default()),
                "--hold-key" => return inject_key(&args.next().unwrap_or_default(), false),
                "--release-key" => return inject_key(&args.next().unwrap_or_default(), true),
                "--inject-unicode" => return inject_unicode(&args.next().unwrap_or_default()),
                "--release-clip" => return release_clip(),
                "--clip-rect" => return print_clip_rect(),
                "--move-window" => {
                    let title = args.next().unwrap_or_default();
                    return move_window(&title, &args.next().unwrap_or_default());
                }
                "--title" => options.title = args.next().unwrap_or_default(),
                "--log" => options.log = args.next().map(PathBuf::from),
                "--color" => {
                    if let Some(color) = args.next().and_then(|c| u32::from_str_radix(&c, 16).ok())
                    {
                        let (r, g, b) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
                        BACKGROUND.store((b << 16) | (g << 8) | r, Ordering::Relaxed);
                    }
                }
                "--pos" => {
                    let value = args.next().unwrap_or_default();
                    if let Some((x, y)) = value.split_once(',') {
                        options.position = (x.parse().unwrap_or(10), y.parse().unwrap_or(10));
                    }
                }
                "--topmost" => options.topmost = true,
                "--with-dialog" => options.dialog = true,
                "--lifetime-secs" => {
                    options.lifetime_secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(300);
                }
                "--busy-drain-ms" => {
                    options.busy_drain_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                }
                "--clip-cursor" => CLIP_CURSOR.store(true, Ordering::Relaxed),
                "--popup-on-ping" => POPUP_ON_PING.store(true, Ordering::Relaxed),
                other => {
                    eprintln!("unknown argument {other}");
                    return 2;
                }
            }
        }
        let Some(log) = options.log.clone().filter(|_| !options.title.is_empty()) else {
            eprintln!("--title and --log are required");
            return 2;
        };
        let _ = LOG.set(log);
        match create_windows(&options) {
            Ok(()) if options.busy_drain_ms > 0 => busy_draining_loop(options.busy_drain_ms),
            Ok(()) => message_loop(),
            Err(error) => {
                eprintln!("fixture window failed: {error}");
                1
            }
        }
    }

    fn log(line: &str) {
        if let Some(path) = LOG.get()
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn module() -> windows::core::Result<HINSTANCE> {
        // SAFETY: A null module name asks for the executable's own module handle.
        unsafe { GetModuleHandleW(None) }.map(|module| HINSTANCE(module.0))
    }

    fn register(
        class: PCWSTR,
        procedure: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    ) -> windows::core::Result<()> {
        // SAFETY: A system cursor needs no module and is never freed.
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }?;
        let descriptor = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module()?,
            lpszClassName: class,
            hCursor: cursor,
            ..Default::default()
        };
        // SAFETY: The descriptor and the class name outlive the call, and the callback is a plain
        // function that keeps no references.
        if unsafe { RegisterClassW(&raw const descriptor) } == 0 {
            return Err(windows::core::Error::from_thread());
        }
        Ok(())
    }

    fn screen_rect(hwnd: HWND, rect: RECT) -> RECT {
        let mut top_left = POINT {
            x: rect.left,
            y: rect.top,
        };
        let mut bottom_right = POINT {
            x: rect.right,
            y: rect.bottom,
        };
        // SAFETY: `hwnd` is a live window of this thread and both points are writable.
        unsafe {
            let _ = ClientToScreen(hwnd, &raw mut top_left);
            let _ = ClientToScreen(hwnd, &raw mut bottom_right);
        }
        RECT {
            left: top_left.x,
            top: top_left.y,
            right: bottom_right.x,
            bottom: bottom_right.y,
        }
    }

    fn format_rect(name: &str, rect: RECT) -> String {
        format!(
            "{name}={},{},{},{}",
            rect.left, rect.top, rect.right, rect.bottom
        )
    }

    fn create_windows(options: &Options) -> Result<(), String> {
        register(w!("ControlFreakFixture"), main_proc).map_err(|e| e.to_string())?;
        register(w!("ControlFreakFixtureDialog"), dialog_proc).map_err(|e| e.to_string())?;
        register(w!("ControlFreakFixturePopup"), popup_proc).map_err(|e| e.to_string())?;
        let instance = module().map_err(|e| e.to_string())?;
        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let ex_style = if options.topmost {
            WS_EX_TOPMOST
        } else {
            WINDOW_EX_STYLE(0)
        };
        let mut outer = CLIENT;
        // SAFETY: `outer` is writable for the synchronous call.
        unsafe { AdjustWindowRectEx(&raw mut outer, style, false, ex_style) }
            .map_err(|e| e.to_string())?;
        let title = wide(&options.title);
        // SAFETY: The class is registered above, the title buffer outlives the call, and the
        // window belongs to this thread, which runs the message loop.
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                w!("ControlFreakFixture"),
                PCWSTR(title.as_ptr()),
                style,
                options.position.0,
                options.position.1,
                outer.right - outer.left,
                outer.bottom - outer.top,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|e| e.to_string())?;
        // SAFETY: `hwnd` is live and owned by this thread.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            SetTimer(
                Some(hwnd),
                LIFETIME_TIMER,
                options.lifetime_secs.saturating_mul(1000),
                None,
            );
        }
        let mut ready = format!(
            "ready {} {} {} {}",
            format_rect("client", screen_rect(hwnd, CLIENT)),
            format_rect("label", screen_rect(hwnd, LABEL)),
            format_rect("edit", screen_rect(hwnd, EDIT)),
            format_rect("button", screen_rect(hwnd, BUTTON)),
        );
        if options.dialog {
            let origin = screen_rect(hwnd, CLIENT);
            let dialog_style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE;
            let mut frame = DIALOG_AREA;
            // SAFETY: `frame` is writable for the synchronous call.
            unsafe { AdjustWindowRectEx(&raw mut frame, dialog_style, false, WINDOW_EX_STYLE(0)) }
                .map_err(|e| e.to_string())?;
            // Not derived from the main title, so a title search finds only the main window.
            let dialog_title = wide("Fixture owned dialog");
            // SAFETY: As for the main window. The dialog is owned by `hwnd`, so Windows destroys
            // it with its owner.
            let dialog = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("ControlFreakFixtureDialog"),
                    PCWSTR(dialog_title.as_ptr()),
                    dialog_style,
                    origin.left + 80,
                    origin.top + 90,
                    frame.right - frame.left,
                    frame.bottom - frame.top,
                    Some(hwnd),
                    None,
                    Some(instance),
                    None,
                )
            }
            .map_err(|e| e.to_string())?;
            ready.push(' ');
            ready.push_str(&format_rect("dialog", screen_rect(dialog, DIALOG_AREA)));
        }
        if CLIP_CURSOR.load(Ordering::Relaxed) {
            ready.push(' ');
            ready.push_str(&format_rect("clip", screen_rect(hwnd, CLIP_AREA)));
        }
        log(&ready);
        Ok(())
    }

    fn message_loop() -> i32 {
        let mut message = MSG::default();
        // SAFETY: `message` is writable; the loop runs on the thread that owns every window.
        while unsafe { GetMessageW(&raw mut message, None, 0, 0) }.as_bool() {
            // SAFETY: `message` was filled by GetMessageW just above.
            unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        0
    }

    /// Behaves like a UI framework that is busy when input arrives, as Notepad was when typed
    /// text came out as copies of its last character. It waits for a message, stays busy for
    /// `busy_ms` without retrieving any, then takes every queued keystroke before translating
    /// one. Windows gives `TranslateMessage` the character of the Unicode packet taken last, so
    /// keystrokes that reach this loop in one burst all arrive as the burst's last character.
    fn busy_draining_loop(busy_ms: u32) -> i32 {
        let mut message = MSG::default();
        loop {
            // SAFETY: WaitMessage takes no arguments; it runs on the thread that owns the windows.
            if unsafe { WaitMessage() }.is_err() {
                return 1;
            }
            thread::sleep(Duration::from_millis(u64::from(busy_ms)));
            let mut keystrokes = Vec::new();
            // SAFETY: `message` is writable; the filter takes this thread's key-down and key-up
            // messages, and WM_QUIT, which PeekMessageW returns whatever the filter.
            while unsafe { PeekMessageW(&raw mut message, None, WM_KEYDOWN, WM_KEYUP, PM_REMOVE) }
                .as_bool()
            {
                if message.message == WM_QUIT {
                    return 0;
                }
                keystrokes.push(message);
            }
            for keystroke in &keystrokes {
                // SAFETY: Each message was filled by PeekMessageW above.
                unsafe {
                    let _ = TranslateMessage(keystroke);
                    DispatchMessageW(keystroke);
                }
            }
            // SAFETY: As above, for every remaining message, including the characters just posted.
            while unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                if message.message == WM_QUIT {
                    return 0;
                }
                // SAFETY: `message` was filled by PeekMessageW just above.
                unsafe {
                    let _ = TranslateMessage(&raw const message);
                    DispatchMessageW(&raw const message);
                }
            }
        }
    }

    fn lparam_to_screen(hwnd: HWND, lparam: LPARAM) -> (i32, i32) {
        // GET_X_LPARAM and GET_Y_LPARAM: signed 16-bit words, negative on a left or upper display.
        let signed_word = |shift: u32| {
            let word = u16::try_from((lparam.0 >> shift) & 0xFFFF).unwrap_or(0);
            i32::from(i16::from_ne_bytes(word.to_ne_bytes()))
        };
        let (x, y) = (signed_word(0), signed_word(16));
        let mut point = POINT { x, y };
        // SAFETY: `hwnd` is the window handling the message; `point` is writable.
        let _ = unsafe { ClientToScreen(hwnd, &raw mut point) };
        (point.x, point.y)
    }

    /// Logs and handles what both windows share. `None` means the message is not shared.
    fn shared(
        role: &str,
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> Option<LRESULT> {
        let mouse = |name: &str| {
            let (x, y) = lparam_to_screen(hwnd, lparam);
            log(&format!("{role} mouse {name} {x} {y}"));
        };
        match message {
            WM_LBUTTONDOWN => mouse("left_down"),
            WM_LBUTTONUP => mouse("left_up"),
            WM_RBUTTONDOWN => mouse("right_down"),
            WM_RBUTTONUP => mouse("right_up"),
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                log(&format!("{role} keydown 0x{:02X}", wparam.0));
                if message == WM_SYSKEYDOWN {
                    return None;
                }
            }
            WM_KEYUP | WM_SYSKEYUP => {
                log(&format!("{role} keyup 0x{:02X}", wparam.0));
                if message == WM_SYSKEYUP {
                    return None;
                }
            }
            WM_CHAR => {
                let unit = u32::try_from(wparam.0).unwrap_or(0);
                log(&format!("{role} char U+{unit:04X}"));
                if let (Ok(mut typed), Ok(mut pending), Ok(unit)) =
                    (TYPED.lock(), PENDING_HIGH.lock(), u16::try_from(unit))
                {
                    let pending = &mut pending[usize::from(role == "dialog")];
                    for c in join_surrogates(pending, unit) {
                        match c {
                            '\u{8}' => {
                                typed.pop();
                            }
                            c if !c.is_control() => typed.push(c),
                            _ => {}
                        }
                    }
                }
                // SAFETY: `hwnd` is live and owned by this thread.
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            WM_SYSCHAR => {
                // A character typed under Alt: a menu mnemonic, left to the default handling.
                let unit = u32::try_from(wparam.0).unwrap_or(0);
                log(&format!("{role} syschar U+{unit:04X}"));
                return None;
            }
            WM_ACTIVATE => {
                let state = wparam.0 & 0xFFFF;
                log(&format!("{role} activate {state}"));
                if role == "main" && CLIP_CURSOR.load(Ordering::Relaxed) {
                    clip_pointer(hwnd, state != 0);
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// The characters one `WM_CHAR` unit completes, for painting. Windows sends a character
    /// outside the Basic Multilingual Plane as two messages, one per UTF-16 surrogate, so a high
    /// surrogate waits in `pending` for its low half. A half without its partner is painted as
    /// U+FFFD, so a split pair shows on screen. The log keeps the raw units either way, and the
    /// harness pairs them the same way.
    fn join_surrogates(pending: &mut Option<u16>, unit: u16) -> Vec<char> {
        let mut units: Vec<u16> = pending.take().into_iter().collect();
        if (0xD800..=0xDBFF).contains(&unit) {
            *pending = Some(unit);
        } else {
            units.push(unit);
        }
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    /// Confines the pointer to `CLIP_AREA` while the main window is active, and frees it when
    /// the window is deactivated. Logs `main clip on` once `ClipCursor` accepted the rectangle.
    fn clip_pointer(hwnd: HWND, active: bool) {
        if active {
            let clip = screen_rect(hwnd, CLIP_AREA);
            // SAFETY: `clip` is a valid RECT for the synchronous call, which keeps no reference.
            if unsafe { ClipCursor(Some(&raw const clip)) }.is_ok() {
                log("main clip on");
            }
        } else {
            // SAFETY: A null rectangle frees the pointer; no memory is passed.
            let _ = unsafe { ClipCursor(None) };
        }
    }

    /// Frees the pointer from any `ClipCursor` rectangle. The harness kills fixtures, so a
    /// clipping fixture never sees its own deactivation and its clip may outlive it; the harness
    /// runs this after it kills one.
    fn release_clip() -> i32 {
        // SAFETY: A null rectangle frees the pointer; no memory is passed.
        i32::from(unsafe { ClipCursor(None) }.is_err())
    }

    /// Prints `left top right bottom` of the rectangle the pointer is confined to now.
    fn print_clip_rect() -> i32 {
        let mut clip = RECT::default();
        // SAFETY: `clip` is writable for the synchronous call, which keeps no reference.
        if unsafe { GetClipCursor(&raw mut clip) }.is_err() {
            return 1;
        }
        println!("{} {} {} {}", clip.left, clip.top, clip.right, clip.bottom);
        0
    }

    /// Moves the top-level window with exactly this title by DX,DY physical pixels, from
    /// another process, as a user dragging its title bar would. Its size, Z order and the
    /// foreground stay as they are. Exit code 0 only if the window was found and moved.
    fn move_window(title: &str, offset: &str) -> i32 {
        let Some((dx, dy)) = offset
            .split_once(',')
            .and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?)))
        else {
            eprintln!("--move-window needs a title and DX,DY");
            return 2;
        };
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let mut rect = RECT::default();
        // SAFETY: `title` is NUL-terminated and outlives the synchronous lookup, `rect` is
        // writable for its call, and the found handle is only passed by value.
        unsafe {
            let Ok(window) = FindWindowW(None, PCWSTR(title.as_ptr())) else {
                return 1;
            };
            if GetWindowRect(window, &raw mut rect).is_err() {
                return 1;
            }
            i32::from(
                SetWindowPos(
                    window,
                    None,
                    rect.left + dx,
                    rect.top + dy,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
                .is_err(),
            )
        }
    }

    fn destroy(hwnd: HWND) -> LRESULT {
        // SAFETY: `hwnd` is live and owned by this thread.
        let _ = unsafe { DestroyWindow(hwnd) };
        LRESULT(0)
    }

    unsafe extern "system" fn main_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if let Some(handled) = shared("main", hwnd, message, wparam, lparam) {
            return handled;
        }
        match message {
            WM_CREATE => {
                if let Ok(instance) = module() {
                    // SAFETY: The button is a system class child of `hwnd`; Windows destroys it
                    // with its parent. A child control's id travels in the menu handle slot.
                    let _ = unsafe {
                        CreateWindowExW(
                            WINDOW_EX_STYLE(0),
                            w!("BUTTON"),
                            w!("Ping"),
                            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON.cast_unsigned()),
                            BUTTON.left,
                            BUTTON.top,
                            BUTTON.right - BUTTON.left,
                            BUTTON.bottom - BUTTON.top,
                            Some(hwnd),
                            Some(HMENU(BUTTON_ID as *mut c_void)),
                            Some(instance),
                            None,
                        )
                    };
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                let code = u32::try_from((wparam.0 >> 16) & 0xFFFF).unwrap_or(0);
                if id == BUTTON_ID && code == BN_CLICKED {
                    log("main button ping");
                    // The click gave the button the focus; keys belong to the surface.
                    // SAFETY: `hwnd` is live and owned by this thread.
                    let _ = unsafe { SetFocus(Some(hwnd)) };
                    if POPUP_ON_PING.load(Ordering::Relaxed) {
                        open_popup(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_PAINT | WM_PRINTCLIENT => {
                paint(hwnd, wparam, |hdc| {
                    fill(hdc, CLIENT, BACKGROUND.load(Ordering::Relaxed));
                    fill(hdc, LABEL, 0x00FF_FFFF);
                    text(hdc, 16, 8, 44, LABEL_TEXT);
                    fill(hdc, EDIT, 0x00FF_FFFF);
                    let typed = TYPED.lock().map(|typed| typed.clone()).unwrap_or_default();
                    text(hdc, EDIT.left + 6, EDIT.top + 6, 24, &typed);
                });
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == LIFETIME_TIMER => destroy(hwnd),
            WM_CLOSE => destroy(hwnd),
            WM_DESTROY => {
                // SAFETY: Posts the quit message to this thread's own queue.
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            // SAFETY: Forwards the unchanged arguments of this window procedure.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    /// Opens an untitled popup owned by `owner` over its client area and gives it the foreground,
    /// which this process may do because it holds the foreground while it handles the click.
    fn open_popup(owner: HWND) {
        let Ok(instance) = module() else {
            return;
        };
        let origin = screen_rect(owner, CLIENT);
        // SAFETY: The class is registered at startup and the title is null, so the popup is
        // untitled. It is owned by `owner`, so Windows destroys it with its owner.
        let popup = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("ControlFreakFixturePopup"),
                PCWSTR::null(),
                WS_POPUP | WS_VISIBLE,
                origin.left + 200,
                origin.top + 170,
                120,
                60,
                Some(owner),
                None,
                Some(instance),
                None,
            )
        };
        if let Ok(popup) = popup {
            // SAFETY: `popup` is live and owned by this thread.
            let _ = unsafe { SetForegroundWindow(popup) };
            log("main popup open");
        }
    }

    unsafe extern "system" fn popup_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_PAINT | WM_PRINTCLIENT => {
                paint(hwnd, wparam, |hdc| {
                    fill(
                        hdc,
                        RECT {
                            left: 0,
                            top: 0,
                            right: 120,
                            bottom: 60,
                        },
                        0x00FF_FFFF,
                    );
                });
                LRESULT(0)
            }
            WM_CLOSE => destroy(hwnd),
            // SAFETY: Forwards the unchanged arguments of this window procedure.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    unsafe extern "system" fn dialog_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if let Some(handled) = shared("dialog", hwnd, message, wparam, lparam) {
            return handled;
        }
        match message {
            WM_PAINT | WM_PRINTCLIENT => {
                paint(hwnd, wparam, |hdc| {
                    fill(hdc, DIALOG_AREA, 0x0080_F0FF);
                    text(hdc, 12, 24, 22, "Fixture dialog");
                });
                LRESULT(0)
            }
            WM_CLOSE => destroy(hwnd),
            // SAFETY: Forwards the unchanged arguments of this window procedure.
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    /// Draws for `WM_PAINT`, or for `WM_PRINTCLIENT`, where `wparam` is the device context to draw
    /// into. `PrintWindow` can reach a window through the second form.
    fn paint(hwnd: HWND, wparam: WPARAM, draw: impl FnOnce(HDC)) {
        if wparam.0 != 0 {
            draw(HDC(wparam.0 as *mut c_void));
            return;
        }
        let mut ps = PAINTSTRUCT::default();
        // SAFETY: `hwnd` is the window handling WM_PAINT; `ps` is writable and the EndPaint
        // below pairs this BeginPaint.
        let hdc = unsafe { BeginPaint(hwnd, &raw mut ps) };
        draw(hdc);
        // SAFETY: Pairs the BeginPaint above with the same window and paint structure.
        let _ = unsafe { EndPaint(hwnd, &raw const ps) };
    }

    fn fill(hdc: HDC, rect: RECT, color: u32) {
        // SAFETY: The brush is created, used for one synchronous fill and deleted here.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(color));
            FillRect(hdc, &raw const rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }

    fn text(hdc: HDC, x: i32, y: i32, height: i32, text: &str) {
        let mut font = LOGFONTW {
            lfHeight: -height,
            lfWeight: 400,
            ..Default::default()
        };
        for (slot, unit) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *slot = unit;
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: The font is selected into the live paint DC, used for one synchronous draw,
        // then the previous object is restored and the font deleted.
        unsafe {
            let created = CreateFontIndirectW(&raw const font);
            let previous = SelectObject(hdc, HGDIOBJ(created.0));
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0));
            let _ = TextOutW(hdc, x, y, &units);
            SelectObject(hdc, previous);
            let _ = DeleteObject(HGDIOBJ(created.0));
        }
    }

    /// A window of the server's session tab, found by its class and accessible name.
    struct Control {
        class: &'static str,
        title: &'static str,
    }

    const STOP_BUTTON: Control = Control {
        class: "Button",
        title: "Stop agent",
    };
    const ALLOW_AGAIN: Control = Control {
        class: "ControlFreakAllowAgain",
        title: "Allow again, with a mouse, touch or pen only",
    };

    struct Search {
        control: &'static Control,
        found: Option<(HWND, RECT)>,
    }

    unsafe extern "system" fn control_callback(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the unique `Search` passed to the synchronous EnumWindows below.
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut class = [0_u16; 64];
        let mut title = [0_u16; 64];
        // SAFETY: Both buffers are writable for these synchronous queries on an enumerated handle.
        let (class_len, title_len, visible) = unsafe {
            (
                GetClassNameW(hwnd, &mut class),
                GetWindowTextW(hwnd, &mut title),
                IsWindowVisible(hwnd).as_bool(),
            )
        };
        let class = String::from_utf16_lossy(&class[..usize::try_from(class_len).unwrap_or(0)]);
        let title = String::from_utf16_lossy(&title[..usize::try_from(title_len).unwrap_or(0)]);
        if visible
            && class.eq_ignore_ascii_case(search.control.class)
            && title == search.control.title
        {
            let mut rect = RECT::default();
            // SAFETY: `rect` is writable for the synchronous call.
            if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_ok() {
                search.found = Some((hwnd, rect));
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    /// The visible window of `control` and its screen rectangle.
    fn find(control: &'static Control) -> Option<(HWND, RECT)> {
        let mut search = Search {
            control,
            found: None,
        };
        // SAFETY: The LPARAM points at `search`, which outlives this synchronous enumeration.
        // EnumWindows reports an error when the callback stops it early, which is expected here.
        let _ = unsafe {
            EnumWindows(
                Some(control_callback),
                LPARAM((&raw mut search).cast::<c_void>() as isize),
            )
        };
        search.found
    }

    /// Prints `left top right bottom` of the visible control, or exits with 1.
    fn print_rect(control: &'static Control) -> i32 {
        match find(control) {
            Some((_, rect)) => {
                println!("{} {} {} {}", rect.left, rect.top, rect.right, rect.bottom);
                0
            }
            None => 1,
        }
    }

    /// Presses the Stop button the way UI Automation's Invoke pattern does, with `BM_CLICK` from
    /// another process. The server accepts that as the user's stop, which is how the live test
    /// reaches the stopped tab without a person. Exit code 0 once the message was handled.
    fn invoke_stop_button() -> i32 {
        let Some((button, _)) = find(&STOP_BUTTON) else {
            return 1;
        };
        // SAFETY: A value-only message to a window of another process; it carries no pointers.
        unsafe { SendMessageW(button, BM_CLICK, Some(WPARAM(0)), Some(LPARAM(0))) };
        0
    }

    /// Tries every way another process can press the Allow again control without input
    /// hardware: `BM_CLICK` (what UI Automation would send), and a left click as sent and as
    /// posted window messages at its centre. Exit code 0 once all were delivered.
    fn message_allow_again() -> i32 {
        let Some((control, rect)) = find(&ALLOW_AGAIN) else {
            return 1;
        };
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let centre = ((height / 2) << 16) | (width / 2);
        let point = LPARAM(isize::try_from(centre).unwrap_or(0));
        // SAFETY: Value-only messages to a window of another process; they carry no pointers.
        unsafe {
            SendMessageW(control, BM_CLICK, Some(WPARAM(0)), Some(LPARAM(0)));
            for message in [WM_LBUTTONDOWN, WM_LBUTTONUP] {
                SendMessageW(control, message, Some(WPARAM(1)), Some(point));
            }
            for message in [WM_LBUTTONDOWN, WM_LBUTTONUP] {
                if PostMessageW(Some(control), message, WPARAM(1), point).is_err() {
                    return 1;
                }
            }
        }
        0
    }

    /// Sends one real `SendInput` left click at a screen point (physical pixels), the way any
    /// automation tool would. The live Stop-button test uses it to check that injected input
    /// cannot press the button. Exit code 0 only if Windows accepted every event.
    fn inject_click(point: &str) -> i32 {
        let Some((x, y)) = point
            .split_once(',')
            .and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?)))
        else {
            eprintln!("--inject-click needs X,Y");
            return 2;
        };
        let event = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        };
        let size = i32::try_from(size_of::<INPUT>()).unwrap_or(0);
        // SAFETY: Moving the cursor and sending one locally built INPUT retain no pointers; each
        // slice is valid for its synchronous call and `size` is the size of INPUT.
        unsafe {
            if SetCursorPos(x, y).is_err() {
                return 1;
            }
            thread::sleep(Duration::from_millis(50));
            if SendInput(&[event(MOUSEEVENTF_LEFTDOWN)], size) != 1 {
                return 1;
            }
            thread::sleep(Duration::from_millis(40));
            i32::from(SendInput(&[event(MOUSEEVENTF_LEFTUP)], size) != 1)
        }
    }

    /// Sends one real `SendInput` key-down or key-up for a virtual key written as `0x10`. Windows
    /// keeps the key in that state after this process exits, as it would for a person holding
    /// it, so the live held-modifier test holds Shift with `--hold-key 0x10` and lets go with
    /// `--release-key 0x10`. Exit code 0 only if Windows accepted the event.
    fn inject_key(key: &str, key_up: bool) -> i32 {
        let Some(key) = key
            .strip_prefix("0x")
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        else {
            eprintln!("--hold-key and --release-key need a virtual key such as 0x10");
            return 2;
        };
        let flags = if key_up {
            KEYEVENTF_KEYUP
        } else {
            KEYBD_EVENT_FLAGS::default()
        };
        send_keys(&[key_event(VIRTUAL_KEY(key), 0, flags)])
    }

    /// Types one UTF-16 unit written as `U+0061` as a Unicode packet, as `type_text` does.
    fn inject_unicode(unit: &str) -> i32 {
        let Some(unit) = unit
            .strip_prefix("U+")
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        else {
            eprintln!("--inject-unicode needs a UTF-16 unit such as U+0061");
            return 2;
        };
        send_keys(&[
            key_event(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
            key_event(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
        ])
    }

    fn key_event(key: VIRTUAL_KEY, unit: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: unit,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        }
    }

    fn send_keys(events: &[INPUT]) -> i32 {
        let size = i32::try_from(size_of::<INPUT>()).unwrap_or(0);
        // SAFETY: `events` holds locally built INPUT values and is valid for the synchronous
        // call, which keeps no reference; `size` is the size of INPUT.
        let sent = unsafe { SendInput(events, size) };
        i32::from(usize::try_from(sent).ok() != Some(events.len()))
    }

    /// Asks Windows to put the taskbar in the foreground. Exit code 0 only if it got there.
    fn activate_taskbar() -> i32 {
        // SAFETY: Class lookup and foreground change take no pointers beyond the constant string.
        unsafe {
            let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) else {
                return 1;
            };
            let _ = SetForegroundWindow(tray);
            thread::sleep(Duration::from_millis(300));
            i32::from(GetForegroundWindow() != tray)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::join_surrogates;

        fn paint(units: &[u16]) -> String {
            let mut pending = None;
            let mut text: String = units
                .iter()
                .flat_map(|unit| join_surrogates(&mut pending, *unit))
                .collect();
            text.extend(pending.map(|_| char::REPLACEMENT_CHARACTER));
            text
        }

        #[test]
        fn a_surrogate_pair_paints_one_character() {
            let units: Vec<u16> = "A\u{1F980}B".encode_utf16().collect();
            assert_eq!(paint(&units), "A\u{1F980}B");
        }

        #[test]
        fn a_lone_half_paints_a_replacement_character() {
            assert_eq!(paint(&[0xD83E, 0x42]), "\u{FFFD}B");
            assert_eq!(paint(&[0xDD80, 0x42]), "\u{FFFD}B");
            assert_eq!(paint(&[0xD83E, 0xD83E, 0xDD80]), "\u{FFFD}\u{1F980}");
            let mut pending = None;
            assert!(join_surrogates(&mut pending, 0xD83E).is_empty());
            assert_eq!(pending, Some(0xD83E), "the high half waits for its partner");
        }
    }
}
