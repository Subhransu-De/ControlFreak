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
//!                      [--with-dialog] [--lifetime-secs N]
//! controlfreak-fixture --stop-button-rect     # print the screen rectangle of the Stop agent button
//! controlfreak-fixture --activate-taskbar     # try to bring the shell taskbar to the foreground
//! ```
//!
//! Log lines, screen coordinates in physical pixels:
//!
//! ```text
//! ready client=L,T,R,B label=L,T,R,B edit=L,T,R,B button=L,T,R,B [dialog=L,T,R,B]
//! main keydown 0x0D | main keyup 0x0D | main char U+0041 | main activate 1
//! main mouse left_down X Y | dialog mouse left_down X Y | main button ping
//! ```

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
        path::PathBuf,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicU32, Ordering},
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
                Input::KeyboardAndMouse::SetFocus,
                WindowsAndMessaging::{
                    AdjustWindowRectEx, BN_CLICKED, BS_PUSHBUTTON, CreateWindowExW, DefWindowProcW,
                    DestroyWindow, DispatchMessageW, EnumWindows, FindWindowW, GetClassNameW,
                    GetForegroundWindow, GetMessageW, GetWindowRect, GetWindowTextW, HMENU,
                    IDC_ARROW, IsWindowVisible, LoadCursorW, MSG, PostQuitMessage, RegisterClassW,
                    SW_SHOWNORMAL, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_COMMAND,
                    WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
                    WM_PAINT, WM_PRINTCLIENT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
                    WM_SYSKEYUP, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_TOPMOST,
                    WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_VISIBLE,
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
    const BUTTON_ID: usize = 100;
    const LIFETIME_TIMER: usize = 1;
    const LABEL_TEXT: &str = "Banana";

    static LOG: OnceLock<PathBuf> = OnceLock::new();
    static TYPED: Mutex<String> = Mutex::new(String::new());
    /// COLORREF (0x00BBGGRR) of the window background.
    static BACKGROUND: AtomicU32 = AtomicU32::new(0x00D0_D0D0);

    struct Options {
        title: String,
        log: Option<PathBuf>,
        position: (i32, i32),
        topmost: bool,
        dialog: bool,
        /// Self-destruct bound so a crashed test run cannot leave a window behind.
        lifetime_secs: u32,
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
                "--stop-button-rect" => return print_stop_button_rect(),
                "--activate-taskbar" => return activate_taskbar(),
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
                if let Ok(mut typed) = TYPED.lock() {
                    match char::from_u32(unit) {
                        Some('\u{8}') => {
                            typed.pop();
                        }
                        Some(c) if !c.is_control() => typed.push(c),
                        _ => {}
                    }
                }
                // SAFETY: `hwnd` is live and owned by this thread.
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
            WM_ACTIVATE => log(&format!("{role} activate {}", wparam.0 & 0xFFFF)),
            _ => return None,
        }
        Some(LRESULT(0))
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

    struct Search {
        found: Option<RECT>,
    }

    unsafe extern "system" fn stop_button_callback(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the unique `Search` passed to the synchronous EnumWindows below.
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut class = [0_u16; 32];
        let mut title = [0_u16; 32];
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
        if visible && class.eq_ignore_ascii_case("Button") && title == "Stop agent" {
            let mut rect = RECT::default();
            // SAFETY: `rect` is writable for the synchronous call.
            if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_ok() {
                search.found = Some(rect);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    /// Prints `left top right bottom` of the visible Stop agent button, or exits with 1.
    fn print_stop_button_rect() -> i32 {
        let mut search = Search { found: None };
        // SAFETY: The LPARAM points at `search`, which outlives this synchronous enumeration.
        // EnumWindows reports an error when the callback stops it early, which is expected here.
        let _ = unsafe {
            EnumWindows(
                Some(stop_button_callback),
                LPARAM((&raw mut search).cast::<c_void>() as isize),
            )
        };
        match search.found {
            Some(rect) => {
                println!("{} {} {} {}", rect.left, rect.top, rect.right, rect.bottom);
                0
            }
            None => 1,
        }
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
}
