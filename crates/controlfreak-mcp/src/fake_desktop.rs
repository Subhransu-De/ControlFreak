//! A small stateful desktop for MCP tests: windows with owners and process IDs, a foreground
//! window, popups, resizes and closes. It applies the same admission rules as the Windows
//! backend (`controlfreak-platform` `target.rs`), so tests can drive recovery paths through the
//! real tools and assert outcomes instead of reading private session state. The real rules are
//! covered by the platform tests; this model only keeps the two in step at the tool boundary.
//!
//! Rules modelled:
//! - A reference names one window revision. Resizing or closing makes it unknown, which is fatal
//!   for the session that approved it.
//! - Input is accepted while the foreground window belongs to the approved window's process. Any
//!   other foreground is refused for that action only; the session survives.
//! - Pointer input lands on the foreground window.

use controlfreak_core::{
    ActionObservation, BackendIdentity, BackendMetadata, CapabilityDescriptor,
    CaptureWindowRequest, DisplayBackend, DisplayBounds, DisplayInfo, FocusWindowRequest,
    KeyChordRequest, KeyboardActionResult, KeyboardBackend, MouseClickRequest, MousePosition,
    MutationControl, OcrBackend, PermissionDescriptor, Platform, PlatformError,
    PointerActionResult, PointerBackend, TextInputRequest, WaitForWindowRequest, WindowBackend,
    WindowBounds, WindowFocusResult, WindowInfo, WindowList, WindowScreenshot, WindowWaitResult,
};
use std::sync::Mutex;

/// What a window received, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Received {
    Click { window: u32, x: u32, y: u32 },
    Keys { window: u32, keys: Vec<String> },
    Text { window: u32, text: String },
    Focus { window: u32 },
}

#[derive(Clone)]
struct FakeWindow {
    key: u32,
    title: String,
    process_id: u32,
    owner: Option<u32>,
    bounds: WindowBounds,
    /// Bumped by every resize; part of the reference, as bounds are in the real one.
    revision: u32,
    open: bool,
}

#[derive(Default)]
struct State {
    windows: Vec<FakeWindow>,
    foreground: Option<u32>,
    received: Vec<Received>,
}

#[derive(Default)]
pub(super) struct FakeDesktop(Mutex<State>);

fn reference(window: &FakeWindow) -> String {
    format!(
        "target-{:08X}-0000-0000-0000-{:012X}",
        window.key, window.revision
    )
}

fn info(window: &FakeWindow, state: &State) -> WindowInfo {
    WindowInfo {
        id: reference(window),
        title: window.title.clone(),
        class_name: "FakeWindow".into(),
        process_id: window.process_id,
        bounds: window.bounds,
        display_id: "fake".into(),
        is_foreground: state.foreground == Some(window.key),
        is_minimized: false,
        owner: window
            .owner
            .and_then(|owner| state.window(owner))
            .map(reference),
    }
}

fn unknown_reference() -> PlatformError {
    PlatformError::TargetInvalidated {
        reason: "unknown, expired, or destroyed target reference; observe again".into(),
    }
}

impl State {
    fn window(&self, key: u32) -> Option<&FakeWindow> {
        self.windows
            .iter()
            .find(|window| window.key == key && window.open)
    }

    fn resolve(&self, id: &str) -> Option<&FakeWindow> {
        self.windows
            .iter()
            .find(|window| window.open && reference(window) == id)
    }

    fn add(&mut self, window: FakeWindow) -> u32 {
        let key = window.key;
        self.windows.push(window);
        self.foreground = Some(key);
        key
    }

    fn next_key(&self) -> u32 {
        u32::try_from(self.windows.len()).unwrap_or(u32::MAX) + 1
    }
}

fn display() -> DisplayInfo {
    DisplayInfo {
        id: "fake".into(),
        name: "Fake display".into(),
        bounds: DisplayBounds {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        },
        is_primary: true,
    }
}

impl FakeDesktop {
    pub(super) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Opens a top-level window that takes the foreground. Returns its key.
    pub(super) fn open(&self, title: &str, process_id: u32) -> u32 {
        let mut state = self.state();
        let key = state.next_key();
        state.add(FakeWindow {
            key,
            title: title.into(),
            process_id,
            owner: None,
            bounds: WindowBounds {
                left: 100,
                top: 100,
                width: 800,
                height: 600,
            },
            revision: 0,
            open: true,
        })
    }

    /// Opens a dialog or popup owned by `owner`, in the owner's process, in the foreground.
    pub(super) fn open_owned(&self, owner: u32, title: &str) -> u32 {
        let mut state = self.state();
        let key = state.next_key();
        let process_id = state.window(owner).expect("owner is open").process_id;
        state.add(FakeWindow {
            key,
            title: title.into(),
            process_id,
            owner: Some(owner),
            bounds: WindowBounds {
                left: 300,
                top: 250,
                width: 300,
                height: 150,
            },
            revision: 0,
            open: true,
        })
    }

    /// Changes the window's size; its old reference stops existing.
    pub(super) fn resize(&self, key: u32) {
        let mut state = self.state();
        let window = state
            .windows
            .iter_mut()
            .find(|window| window.key == key)
            .expect("window exists");
        window.bounds.width += 40;
        window.revision += 1;
    }

    /// Closes the window; the foreground falls to its owner when it has one.
    pub(super) fn close(&self, key: u32) {
        let mut state = self.state();
        let owner = state.window(key).and_then(|window| window.owner);
        if let Some(window) = state.windows.iter_mut().find(|window| window.key == key) {
            window.open = false;
        }
        if state.foreground == Some(key) {
            state.foreground = owner;
        }
    }

    /// The reference the tools use for this window right now.
    pub(super) fn id(&self, key: u32) -> String {
        let state = self.state();
        reference(state.window(key).expect("window is open"))
    }

    pub(super) fn received(&self) -> Vec<Received> {
        self.state().received.clone()
    }

    /// Admission for one input action: the approved window must still exist, and the foreground
    /// window must belong to its process. Returns the key of the window that receives input.
    fn admit(&self, control: &MutationControl, operation: &str) -> Result<u32, PlatformError> {
        control.check(operation)?;
        control.check_target()?;
        let approved =
            control
                .approved_target()
                .ok_or_else(|| PlatformError::TargetInvalidated {
                    reason: "an approved target reference is required".into(),
                })?;
        let state = self.state();
        let Some(target) = state.resolve(&approved) else {
            control.invalidate_target();
            return Err(PlatformError::TargetInvalidated {
                reason: "window, process lifetime, owner, or desktop changed".into(),
            });
        };
        state
            .foreground
            .and_then(|key| state.window(key))
            .filter(|window| window.process_id == target.process_id)
            .map(|window| window.key)
            .ok_or_else(|| PlatformError::TargetInvalidated {
                reason: "another process holds the foreground".into(),
            })
    }

    fn deliver(control: &MutationControl, events: u64) {
        control.dispatch_started();
        control.dispatch_accepted(events);
        control.input_complete();
    }
}

fn nothing() -> ActionObservation {
    ActionObservation {
        foreground_window: None,
        screenshot: None,
    }
}

impl BackendMetadata for FakeDesktop {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Windows,
            display_server: controlfreak_core::DisplayServer::Dwm,
            backend_id: "fake-desktop".into(),
        }
    }
    fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<PermissionDescriptor> {
        vec![]
    }
    fn validate_target_reference(&self, target: &str) -> Result<(), PlatformError> {
        self.state()
            .resolve(target)
            .map(|_| ())
            .ok_or_else(unknown_reference)
    }
}

impl DisplayBackend for FakeDesktop {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Ok(vec![display()])
    }
}

impl OcrBackend for FakeDesktop {}

impl WindowBackend for FakeDesktop {
    fn list_windows(&self) -> Result<WindowList, PlatformError> {
        let state = self.state();
        Ok(WindowList {
            windows: state
                .windows
                .iter()
                .filter(|window| window.open)
                .map(|window| info(window, &state))
                .collect(),
            truncated: false,
        })
    }

    fn wait_for_window(
        &self,
        request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        let state = self.state();
        let window = state
            .windows
            .iter()
            .filter(|window| window.open)
            .map(|window| info(window, &state))
            .find(|window| request.window_id.as_ref().is_none_or(|id| *id == window.id));
        Ok(WindowWaitResult {
            matched: window.is_some(),
            timed_out: window.is_none(),
            elapsed_ms: 0,
            window,
        })
    }

    fn capture_window(
        &self,
        request: &CaptureWindowRequest,
    ) -> Result<WindowScreenshot, PlatformError> {
        let state = self.state();
        let window = state
            .resolve(&request.window_id)
            .ok_or_else(unknown_reference)?;
        Ok(WindowScreenshot {
            window: info(window, &state),
            source_bounds: window.bounds,
            png: vec![1, 2, 3],
            image_width: window.bounds.width,
            image_height: window.bounds.height,
            downscale_factor: 1,
            cursor_marker: false,
            capture_method: "windows_print_window",
        })
    }

    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        control.check("focus_window")?;
        control.check_target()?;
        if control.approved_target().as_deref() != Some(request.window_id.as_str()) {
            return Err(PlatformError::TargetInvalidated {
                reason: "focus target differs from the approved session target".into(),
            });
        }
        let mut state = self.state();
        let Some(window) = state.resolve(&request.window_id).cloned() else {
            control.invalidate_target();
            return Err(PlatformError::TargetInvalidated {
                reason: "window, process lifetime, owner, or desktop changed".into(),
            });
        };
        state.foreground = Some(window.key);
        state.received.push(Received::Focus { window: window.key });
        Self::deliver(control, 1);
        Ok(WindowFocusResult {
            window: info(&window, &state),
            observation: nothing(),
            attempts: 1,
            elapsed_ms: 0,
        })
    }
}

impl PointerBackend for FakeDesktop {
    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        let window = self.admit(control, "click_mouse")?;
        self.state().received.push(Received::Click {
            window,
            x: request.x,
            y: request.y,
        });
        Self::deliver(control, 2);
        Ok(PointerActionResult {
            position: MousePosition {
                display_id: "fake".into(),
                local_x: request.x,
                local_y: request.y,
                virtual_x: i32::try_from(request.x).unwrap_or(i32::MAX),
                virtual_y: i32::try_from(request.y).unwrap_or(i32::MAX),
            },
            observation: nothing(),
        })
    }
}

impl KeyboardBackend for FakeDesktop {
    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        let window = self.admit(control, "press_keys")?;
        self.state().received.push(Received::Keys {
            window,
            keys: request.keys.iter().map(|key| format!("{key:?}")).collect(),
        });
        Self::deliver(control, 2);
        Ok(KeyboardActionResult {
            observation: nothing(),
        })
    }

    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        let window = self.admit(control, "type_text")?;
        self.state().received.push(Received::Text {
            window,
            text: request.text.clone(),
        });
        Self::deliver(control, 2);
        Ok(KeyboardActionResult {
            observation: nothing(),
        })
    }
}
