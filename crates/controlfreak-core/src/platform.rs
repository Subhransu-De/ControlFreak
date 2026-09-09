use serde::{Deserialize, Serialize};

use crate::{
    CapabilityDescriptor, CapabilityReport, CaptureDisplayRequest, CaptureRegionRequest,
    CaptureWindowRequest, ClickTextRequest, ClickTextResult, DisplayInfo, DisplayScreenshot,
    FindTextRequest, FindTextResult, FocusWindowRequest, KeyChordRequest, KeyboardActionResult,
    MouseClickRequest, MouseDragRequest, MouseMoveRequest, MouseScrollRequest, MutationControl,
    OcrRegionRequest, OcrResult, PermissionDescriptor, PlatformError, PointerActionResult,
    SecurityContext, TextInputRequest, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForVisualChangeRequest, WaitForWindowRequest, WindowFocusResult,
    WindowInfo, WindowScreenshot, WindowWaitResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayServer {
    Dwm,
    Quartz,
    X11,
    Wayland,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub platform: Platform,
    pub display_server: DisplayServer,
    pub backend_id: String,
}

/// Identifies a backend and reports the capabilities it exposes.
pub trait BackendMetadata: Send + Sync + 'static {
    fn identity(&self) -> BackendIdentity;
    fn capabilities(&self) -> Vec<CapabilityDescriptor>;
    fn permissions(&self) -> Vec<PermissionDescriptor>;

    fn security_context(&self) -> SecurityContext {
        SecurityContext::default()
    }

    fn report(&self) -> CapabilityReport {
        CapabilityReport::new(
            self.identity(),
            self.capabilities(),
            self.permissions(),
            self.security_context(),
        )
    }
}

/// Captures displays and tracks visual changes.
pub trait DisplayBackend {
    fn list_displays(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        Err(PlatformError::unsupported(
            "list_displays",
            "the active platform backend has no display implementation",
        ))
    }

    fn capture_display(
        &self,
        _request: &CaptureDisplayRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_display",
            "the active platform backend has no screen-capture implementation",
        ))
    }

    fn capture_region(
        &self,
        _request: &CaptureRegionRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_region",
            "the active platform backend has no region-capture implementation",
        ))
    }

    fn wait_for_visual_change(
        &self,
        _request: &WaitForVisualChangeRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_visual_change",
            "the active platform backend has no visual-wait implementation",
        ))
    }

    fn capture_visual_baseline(
        &self,
        _request: &VisualBaselineRequest,
    ) -> Result<VisualBaseline, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_visual_baseline",
            "the active platform backend has no visual-baseline implementation",
        ))
    }

    fn wait_for_change_since(
        &self,
        _request: &WaitForChangeSinceRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_change_since",
            "the active platform backend has no visual-baseline implementation",
        ))
    }
}

/// Performs local OCR and semantic text interaction.
pub trait OcrBackend {
    fn read_text_in_region(&self, _request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        Err(PlatformError::unsupported(
            "read_text_in_region",
            "the active platform backend has no OCR implementation",
        ))
    }

    fn find_text_on_screen(
        &self,
        _request: &FindTextRequest,
    ) -> Result<FindTextResult, PlatformError> {
        Err(PlatformError::unsupported(
            "find_text_on_screen",
            "the active platform backend has no OCR implementation",
        ))
    }

    fn click_text(&self, _request: &ClickTextRequest) -> Result<ClickTextResult, PlatformError> {
        Err(PlatformError::unsupported(
            "click_text",
            "the active platform backend has no OCR interaction implementation",
        ))
    }

    fn click_text_controlled(
        &self,
        request: &ClickTextRequest,
        control: &MutationControl,
    ) -> Result<ClickTextResult, PlatformError> {
        control.check("click_text")?;
        self.click_text(request)
    }
}

/// Controls the system pointer.
pub trait PointerBackend {
    fn move_mouse(
        &self,
        _request: &MouseMoveRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "move_mouse",
            "the active platform backend has no pointer-movement implementation",
        ))
    }

    fn move_mouse_controlled(
        &self,
        request: &MouseMoveRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("move_mouse")?;
        self.move_mouse(request)
    }

    fn click_mouse(
        &self,
        _request: &MouseClickRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "click_mouse",
            "the active platform backend has no pointer-click implementation",
        ))
    }

    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("click_mouse")?;
        self.click_mouse(request)
    }

    fn drag_mouse(
        &self,
        _request: &MouseDragRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "drag_mouse",
            "the active platform backend has no pointer-drag implementation",
        ))
    }

    fn drag_mouse_controlled(
        &self,
        request: &MouseDragRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("drag_mouse")?;
        self.drag_mouse(request)
    }

    fn scroll_mouse(
        &self,
        _request: &MouseScrollRequest,
    ) -> Result<PointerActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "scroll_mouse",
            "the active platform backend has no pointer-scroll implementation",
        ))
    }

    fn scroll_mouse_controlled(
        &self,
        request: &MouseScrollRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        control.check("scroll_mouse")?;
        self.scroll_mouse(request)
    }
}

/// Enumerates, observes, and focuses windows and virtual desktops.
pub trait WindowBackend {
    fn list_windows(&self) -> Result<Vec<WindowInfo>, PlatformError> {
        Err(PlatformError::unsupported(
            "list_windows",
            "the active platform backend has no window-enumeration implementation",
        ))
    }

    fn list_virtual_desktops(&self) -> Result<VirtualDesktopList, PlatformError> {
        Err(PlatformError::unsupported(
            "list_virtual_desktops",
            "the active platform backend has no virtual-desktop discovery implementation",
        ))
    }

    fn switch_virtual_desktop(
        &self,
        _request: &VirtualDesktopSwitchRequest,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        Err(PlatformError::unsupported(
            "switch_virtual_desktop",
            "the active platform backend has no virtual-desktop switching implementation",
        ))
    }

    fn switch_virtual_desktop_controlled(
        &self,
        request: &VirtualDesktopSwitchRequest,
        control: &MutationControl,
    ) -> Result<VirtualDesktopSwitchResult, PlatformError> {
        control.check("switch_virtual_desktop")?;
        self.switch_virtual_desktop(request)
    }

    fn focus_window(
        &self,
        _request: &FocusWindowRequest,
    ) -> Result<WindowFocusResult, PlatformError> {
        Err(PlatformError::unsupported(
            "focus_window",
            "the active platform backend has no window-focus implementation",
        ))
    }

    fn focus_window_controlled(
        &self,
        request: &FocusWindowRequest,
        control: &MutationControl,
    ) -> Result<WindowFocusResult, PlatformError> {
        control.check("focus_window")?;
        self.focus_window(request)
    }

    fn capture_window(
        &self,
        _request: &CaptureWindowRequest,
    ) -> Result<WindowScreenshot, PlatformError> {
        Err(PlatformError::unsupported(
            "capture_window",
            "the active platform backend has no window-capture implementation",
        ))
    }

    fn wait_for_window(
        &self,
        _request: &WaitForWindowRequest,
    ) -> Result<WindowWaitResult, PlatformError> {
        Err(PlatformError::unsupported(
            "wait_for_window",
            "the active platform backend has no window-wait implementation",
        ))
    }
}

/// Sends keyboard shortcuts and Unicode text.
pub trait KeyboardBackend {
    fn press_keys(
        &self,
        _request: &KeyChordRequest,
    ) -> Result<KeyboardActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "press_keys",
            "the active platform backend has no keyboard implementation",
        ))
    }

    fn press_keys_controlled(
        &self,
        request: &KeyChordRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        control.check("press_keys")?;
        self.press_keys(request)
    }

    fn type_text(
        &self,
        _request: &TextInputRequest,
    ) -> Result<KeyboardActionResult, PlatformError> {
        Err(PlatformError::unsupported(
            "type_text",
            "the active platform backend has no text-input implementation",
        ))
    }

    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        control.check("type_text")?;
        self.type_text(request)
    }
}

/// Complete backend contract consumed by the MCP adapter.
pub trait PlatformBackend:
    BackendMetadata + DisplayBackend + OcrBackend + PointerBackend + WindowBackend + KeyboardBackend
{
}

impl<T> PlatformBackend for T where
    T: BackendMetadata
        + DisplayBackend
        + OcrBackend
        + PointerBackend
        + WindowBackend
        + KeyboardBackend
{
}
