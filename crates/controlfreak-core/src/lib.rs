#![forbid(unsafe_code)]

mod build_info;
mod capability;
mod desktop;
mod display;
mod dpi;
mod error;
mod keyboard;
pub mod limits;
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

pub use build_info::{BUILD_INFO_SCHEMA_VERSION, BuildInfo, UNKNOWN_REVISION};
pub use capability::{Capability, CapabilityDescriptor, CapabilityState};
pub use desktop::{
    VirtualDesktopDirection, VirtualDesktopInfo, VirtualDesktopList, VirtualDesktopSwitchRequest,
    VirtualDesktopSwitchResult, VirtualDesktopTransition,
};
pub use display::{
    ActionObservation, CaptureDisplayRequest, CaptureRegionRequest, DisplayBounds, DisplayInfo,
    DisplayLayout, DisplayScreenshot, MouseButton, MouseClickRequest, MouseDragRequest,
    MouseMoveRequest, MousePosition, MouseScrollRequest, ObservationMode, ObservationOptions,
    ObservationRegion, PointerActionResult, SettleOutcome, SettleReport, VisualBaseline,
    VisualBaselineRequest, VisualChangeResult, WaitForChangeSinceRequest,
    WaitForVisualChangeRequest, scaled_dimensions,
};
pub use dpi::{DpiAwareness, DpiAwarenessStatus};
pub use error::{FailureCause, PlatformError, RefusalCause};
pub use keyboard::{
    Key, KeyChordRequest, KeyboardActionResult, TextInputRequest, validate_text_timeout,
};
pub use mutation::MutationControl;
pub use ocr::{
    ClickTextRequest, ClickTextResult, FindTextRequest, FindTextResult, OcrLine, OcrMatchDetails,
    OcrRegionRequest, OcrResult, OcrWord, TextCondition, TextMatch, TextMatchMode, TextMatchTier,
    WaitForTextRequest, WaitForTextResult,
};
pub use outcome::{ActivationHelperOutcome, CleanupStatus, InputOutcome, MutationProgress};
pub use permission::{PermissionDescriptor, PermissionId, PermissionState};
pub use platform::{
    BackendIdentity, BackendMetadata, DisplayBackend, DisplayServer, KeyboardBackend, OcrBackend,
    Platform, PlatformBackend, PointerBackend, WindowBackend, wait_for_text_with,
};
pub use report::CapabilityReport;
pub use security::{IntegrityLevel, SecurityContext};
pub use stop::{
    Backoff, EscHook, RearmRefusal, StopController, StopRecovery, StopRegistration, StopSnapshot,
    retry_with_backoff,
};
pub use text_matching::{discover_text, select_text_candidate, validate_text_discovery};
pub use window::{
    CaptureWindowRequest, FocusWindowRequest, ForegroundWindow, UnlistedWindow,
    WaitForWindowRequest, WindowBounds, WindowFocusResult, WindowInfo, WindowList,
    WindowScreenshot, WindowWaitResult,
};
