#![allow(unsafe_code)]

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use controlfreak_core::StopController;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_PAUSE},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
                PeekMessageW, RegisterClassW, SetWindowTextW, WM_CLOSE, WM_COMMAND, WM_LBUTTONUP,
                WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_POPUP, WS_SYSMENU, WS_VISIBLE,
            },
        },
    },
    core::{HSTRING, w},
};

thread_local! {
    static CONTROLLER: RefCell<Option<StopController>> = const { RefCell::new(None) };
}

/// An independent native stop window and shortcut monitor. It never calls a provider.
pub struct UserStop {
    shutdown: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl UserStop {
    pub fn start(controller: StopController) -> Result<Self, String> {
        // Establish physical-pixel coordinates before creating the process's first window.
        super::ensure_dpi_awareness().map_err(|error| error.to_string())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let exiting = Arc::clone(&shutdown);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("controlfreak-user-stop".to_owned())
            .spawn(move || {
                CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
                match run(&controller, &exiting, &sender) {
                    Ok(()) => {}
                    Err(error) => {
                        controller.stop();
                        let _ = sender.send(Err(error.to_string()));
                    }
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

fn run(
    controller: &StopController,
    exiting: &AtomicBool,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> windows::core::Result<()> {
    // SAFETY: All windows and messages belong to this dedicated thread. The class
    // callback uses thread-local owned state and does not retain message pointers.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("ControlFreakUserStop");
        let descriptor = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&raw const descriptor) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("STOP ControlFreak: click here or Ctrl+Alt+Pause"),
            WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            16,
            16,
            470,
            70,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("BUTTON"),
            w!("STOP desktop work"),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            450,
            40,
            Some(window),
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ready.send(Ok(()));
        let mut previous = "";
        while !exiting.load(Ordering::Acquire) {
            let mut message = MSG::default();
            for _ in 0..64 {
                if !PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    break;
                }
                DispatchMessageW(&raw const message);
            }
            // Only the three stop-chord key states are sampled. No text or key history is retained.
            if GetAsyncKeyState(i32::from(VK_CONTROL.0)) < 0
                && GetAsyncKeyState(i32::from(VK_MENU.0)) < 0
                && GetAsyncKeyState(i32::from(VK_PAUSE.0)) < 0
            {
                controller.stop();
            }
            let status = controller.status();
            if status != previous {
                let title = if status == "ready" {
                    "STOP ControlFreak: click below or Ctrl+Alt+Pause".to_owned()
                } else {
                    format!("ControlFreak {status}: restart only when safe")
                };
                SetWindowTextW(window, &HSTRING::from(title))?;
                previous = status;
            }
            thread::sleep(Duration::from_millis(10));
        }
        DestroyWindow(window)?;
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if matches!(message, WM_LBUTTONUP | WM_CLOSE | WM_COMMAND) {
        CONTROLLER.with(|slot| {
            if let Some(controller) = slot.borrow().as_ref() {
                controller.stop();
            }
        });
        return LRESULT(0);
    }
    // SAFETY: Forward unchanged Win32 callback arguments to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
