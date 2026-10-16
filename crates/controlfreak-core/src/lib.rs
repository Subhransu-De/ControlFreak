#![forbid(unsafe_code)]

mod capability;
mod desktop;
mod display;
mod error;
mod keyboard;
mod mutation;
mod ocr;
mod outcome;
mod permission;
mod platform;
mod report;
mod security;
mod stop;
mod text_matching;
pub mod timing;
mod window;

pub use capability::{Capability, CapabilityDescriptor, CapabilityState};
pub use desktop::{
    VirtualDesktopDirection, VirtualDesktopInfo, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult,
};
pub use display::{
    ActionObservation, CaptureDisplayRequest, CaptureRegionRequest, DisplayBounds, DisplayInfo,
    DisplayScreenshot, MouseButton, MouseClickRequest, MouseDragRequest, MouseMoveRequest,
    MousePosition, MouseScrollRequest, ObservationMode, ObservationOptions, ObservationRegion,
    PointerActionResult, VisualBaseline, VisualBaselineRequest, VisualChangeResult,
    WaitForChangeSinceRequest, WaitForVisualChangeRequest,
};
pub use error::PlatformError;
pub use keyboard::{Key, KeyChordRequest, KeyboardActionResult, TextInputRequest};
pub use mutation::MutationControl;
pub use ocr::{
    ClickTextRequest, ClickTextResult, FindTextRequest, FindTextResult, MAX_TEXT_WAIT_MS, OcrLine,
    OcrMatchDetails, OcrRegionRequest, OcrResult, OcrWord, TextCondition, TextMatch, TextMatchMode,
    TextMatchTier, WaitForTextRequest, WaitForTextResult,
};
pub use outcome::{CleanupStatus, InputOutcome, MutationProgress};
pub use permission::{PermissionDescriptor, PermissionId, PermissionState};
pub use platform::{
    BackendIdentity, BackendMetadata, DisplayBackend, DisplayServer, KeyboardBackend, OcrBackend,
    Platform, PlatformBackend, PointerBackend, WindowBackend,
};
pub use report::CapabilityReport;
pub use security::{IntegrityLevel, SecurityContext};
pub use stop::{StopController, StopRegistration};
pub use text_matching::{discover_text, select_text_candidate, validate_text_discovery};
pub use window::{
    CaptureWindowRequest, FocusWindowRequest, WaitForWindowRequest, WindowBounds,
    WindowFocusResult, WindowInfo, WindowList, WindowScreenshot, WindowWaitResult,
};
