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
mod text_matching;
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
    ClickTextRequest, ClickTextResult, FindTextRequest, FindTextResult, OcrLine, OcrMatchDetails,
    OcrRegionRequest, OcrResult, OcrWord, TextMatch, TextMatchMode, TextMatchTier,
};
pub use outcome::{CleanupStatus, InputOutcome, MutationProgress};
pub use permission::{PermissionDescriptor, PermissionId, PermissionState};
pub use platform::{
    BackendIdentity, BackendMetadata, DisplayBackend, DisplayServer, KeyboardBackend, OcrBackend,
    Platform, PlatformBackend, PointerBackend, WindowBackend,
};
pub use report::CapabilityReport;
pub use security::{IntegrityLevel, SecurityContext};
pub use text_matching::{discover_text, select_text_candidate};
pub use window::{
    CaptureWindowRequest, FocusWindowRequest, WaitForWindowRequest, WindowBounds,
    WindowFocusResult, WindowInfo, WindowScreenshot, WindowWaitResult,
};
