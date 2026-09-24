#![allow(unsafe_code)]

use controlfreak_core::StopController;
use std::{
    cell::{Cell, RefCell},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateFontIndirectW, DC_BRUSH, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
            DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, GetStockObject, HBRUSH,
            HFONT, InvalidateRect, LOGFONTW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
            MonitorFromPoint, PAINTSTRUCT, SelectObject, SetBkMode, SetDCBrushColor, SetTextColor,
            TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE},
            WindowsAndMessaging::{
                CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GetClientRect, HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
                PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE,
                SWP_NOACTIVATE, SetWindowDisplayAffinity, SetWindowPos, SetWindowsHookExW,
                ShowWindow, UnhookWindowsHookEx, UnregisterClassW, WDA_EXCLUDEFROMCAPTURE,
                WH_KEYBOARD_LL, WM_CLOSE, WM_KEYDOWN, WM_KEYUP, WM_PAINT, WM_SYSKEYDOWN,
                WM_SYSKEYUP, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP,
            },
        },
    },
    core::w,
};

const BAR_TEXT: &str = "ControlFreak is controlling your system (Use Esc to stop)";
const CLASS_NAME: windows::core::PCWSTR = w!("ControlFreakUserStop");
const NAVY: COLORREF = COLORREF(0x0080_0000); // RGB #000080; COLORREF is BGR.

#[derive(Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    Released,
    PassedThrough,
    Consumed,
}

thread_local! {
    static CONTROLLER: RefCell<Option<StopController>> = const { RefCell::new(None) };
    static ESC_STATE: Cell<EscapeState> = const { Cell::new(EscapeState::Released) };
    static BAR_FONT: Cell<HFONT> = const { Cell::new(HFONT(std::ptr::null_mut())) };
}

/// Independent native session bar and physical Esc monitor. Never calls a provider.
pub struct UserStop {
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl UserStop {
    pub fn start(controller: StopController) -> Result<Self, String> {
        super::ensure_dpi_awareness().map_err(|error| error.to_string())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let exiting = Arc::clone(&shutdown);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("controlfreak-user-stop".to_owned())
            .spawn(move || {
                CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
                if let Err(error) = run(&controller, &exiting, &sender) {
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
struct NativeBar {
    window: HWND,
    hook: HHOOK,
    font: HFONT,
    instance: HINSTANCE,
    bounds: RECT,
    font_height: i32,
}

impl NativeBar {
    fn create() -> windows::core::Result<Self> {
        // SAFETY: This UI thread owns the class, hidden window and hook. The guard
        // releases every successfully created resource if a later call fails.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None)?.into();
            let descriptor = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            if RegisterClassW(&raw const descriptor) == 0 {
                return Err(windows::core::Error::from_thread());
            }
            let mut bar = Self {
                window: HWND::default(),
                hook: HHOOK::default(),
                font: HFONT::default(),
                instance,
                bounds: RECT::default(),
                font_height: 0,
            };
            bar.window = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("ControlFreak session"),
                WS_POPUP,
                0,
                0,
                600,
                48,
                None,
                None,
                Some(instance),
                None,
            )?;
            SetWindowDisplayAffinity(bar.window, WDA_EXCLUDEFROMCAPTURE)?;
            // A key already held before monitoring must retain its matching release.
            ESC_STATE.with(|state| {
                state.set(if GetAsyncKeyState(i32::from(VK_ESCAPE.0)) < 0 {
                    EscapeState::PassedThrough
                } else {
                    EscapeState::Released
                });
            });
            bar.hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance), 0)?;
            Ok(bar)
        }
    }

    fn update_layout(&mut self) -> windows::core::Result<()> {
        // SAFETY: The window and font belong to this UI thread. Monitor info uses
        // a correctly sized writable struct; no GDI objects remain selected here.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or(0),
                ..Default::default()
            };
            GetMonitorInfoW(monitor, &raw mut info).ok()?;
            let bounds = bar_bounds(info.rcMonitor, GetDpiForWindow(self.window).max(96));
            if bounds != self.bounds {
                SetWindowPos(
                    self.window,
                    Some(HWND_TOPMOST),
                    bounds.left,
                    bounds.top,
                    bounds.right - bounds.left,
                    bounds.bottom - bounds.top,
                    SWP_NOACTIVATE,
                )?;
                self.bounds = bounds;
            }
            // Scale text down with the bar on narrow or highly scaled displays.
            let height = ((bounds.right - bounds.left) * 16 / 600).max(1);
            if height != self.font_height {
                let mut font = LOGFONTW {
                    lfHeight: -height,
                    ..Default::default()
                };
                for (slot, value) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
                    *slot = value;
                }
                let new_font = CreateFontIndirectW(&raw const font);
                if new_font.is_invalid() {
                    return Err(windows::core::Error::from_thread());
                }
                BAR_FONT.with(|font| font.set(new_font));
                if !self.font.is_invalid() {
                    let _ = DeleteObject(self.font.into());
                }
                self.font = new_font;
                self.font_height = height;
                let _ = InvalidateRect(Some(self.window), None, false);
            }
        }
        Ok(())
    }
}

impl Drop for NativeBar {
    fn drop(&mut self) {
        // SAFETY: These handles were created on this thread and are released once.
        // Remove the callback before destroying its window and thread-local font.
        unsafe {
            if !self.hook.is_invalid() {
                let _ = UnhookWindowsHookEx(self.hook);
            }
            if !self.window.is_invalid() {
                let _ = DestroyWindow(self.window);
            }
            BAR_FONT.with(|font| font.set(HFONT::default()));
            if !self.font.is_invalid() {
                let _ = DeleteObject(self.font.into());
            }
            let _ = UnregisterClassW(CLASS_NAME, Some(self.instance));
        }
    }
}

fn bar_bounds(monitor: RECT, dpi: u32) -> RECT {
    let scale = |value: i32| value.saturating_mul(i32::try_from(dpi).unwrap_or(96)) / 96;
    let available = (monitor.right - monitor.left).max(1);
    let width = scale(600).min((available - scale(32)).max(1));
    let left = monitor.left + (available - width) / 2;
    let top = monitor.top + scale(16);
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + scale(48),
    }
}

fn bar_text(controller: &StopController) -> &'static str {
    match controller.status() {
        "cleanup_failed" => "ControlFreak stopped. Input cleanup needs attention.",
        "draining" | "stopped" => "ControlFreak stopped. Finishing input cleanup...",
        _ => BAR_TEXT,
    }
}

fn run(
    controller: &StopController,
    exiting: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> windows::core::Result<()> {
    let mut bar = NativeBar::create()?;
    let _ = ready.send(Ok(()));
    let mut visible = false;
    let mut previous_text = "";
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
        let active = controller.session_active();
        if active {
            bar.update_layout()?;
            let text = bar_text(controller);
            if text != previous_text || !visible {
                // SAFETY: The owned window remains live throughout this loop.
                unsafe {
                    let _ = InvalidateRect(Some(bar.window), None, false);
                }
                previous_text = text;
            }
        }
        if active != visible {
            // SAFETY: Show or hide our owned window without taking foreground focus.
            unsafe {
                let _ = ShowWindow(bar.window, if active { SW_SHOWNOACTIVATE } else { SW_HIDE });
            }
            visible = active;
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
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
    if message == WM_PAINT {
        // SAFETY: WM_PAINT is for this live window; pair BeginPaint/EndPaint and
        // restore the selected font before the owning thread can delete it.
        unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &raw mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(window, &raw mut rect);
            SetDCBrushColor(dc, NAVY);
            FillRect(dc, &raw const rect, HBRUSH(GetStockObject(DC_BRUSH).0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00ff_ffff));
            let old = BAR_FONT.with(|font| SelectObject(dc, font.get().into()));
            let mut text: Vec<u16> = CONTROLLER
                .with(|slot| slot.borrow().as_ref().map_or(BAR_TEXT, bar_text))
                .encode_utf16()
                .collect();
            DrawTextW(
                dc,
                &mut text,
                &raw mut rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(dc, old);
            let _ = EndPaint(window, &raw const paint);
        }
        return LRESULT(0);
    }
    if message == WM_CLOSE {
        return LRESULT(0);
    }
    // SAFETY: Forward unchanged Win32 callback arguments to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_is_centered_with_scaled_top_gap_even_at_negative_origins() {
        for dpi in [96, 144, 192] {
            let monitor = RECT {
                left: -1920,
                top: -1080,
                right: 0,
                bottom: 0,
            };
            let bounds = bar_bounds(monitor, dpi);
            assert_eq!(bounds.left + bounds.right, monitor.left + monitor.right);
            assert_eq!(bounds.top - monitor.top, i32::try_from(dpi).unwrap() / 6);
            assert!(bounds.left >= monitor.left && bounds.right <= monitor.right);
        }
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
