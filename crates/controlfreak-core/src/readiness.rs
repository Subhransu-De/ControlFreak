//! Runtime readiness: whether OCR and the helper processes work now, the hard request limits and
//! the tool families the server never offers. [`crate::CapabilityState`] says what a backend
//! implements; [`Readiness`] says whether a part works at the moment it was checked.

use serde::{Deserialize, Serialize};

use crate::{CapabilityDescriptor, limits};

/// Most OCR language tags a report lists. Windows offers far fewer OCR languages.
pub const MAX_REPORTED_OCR_LANGUAGES: usize = 64;
/// Longest OCR language tag a report repeats. Longer values are dropped, not shortened.
const MAX_LANGUAGE_TAG_LEN: usize = 35;

/// Whether a part of the server works now.
///
/// `Unsupported` does not change while the server runs: this build or configuration does not
/// offer the part. `Unavailable` is offered but cannot work now and may recover. `Degraded` works
/// with a limitation that `reason` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    Degraded { reason: ReadinessReason },
    Unavailable { reason: ReadinessReason },
    Unsupported { reason: ReadinessReason },
}

/// Stable reason codes for [`Readiness`]. Adding a code is compatible; renaming one is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    /// The backend has no implementation of this part.
    NotImplemented,
    /// The server was started without this helper.
    NotConfigured,
    /// The helper is configured but has not run yet, so whether it works is not known. It starts
    /// with the first call that needs it.
    NotVerified,
    /// No Windows OCR language pack is installed.
    NoOcrLanguage,
    /// OCR language packs are installed, but none matches a language in the Windows language
    /// list, so OCR works only with an explicit `language`.
    NoDefaultOcrLanguage,
    /// The Windows OCR runtime failed when it was asked for its languages.
    OcrRuntimeFailed,
    /// The helper process could not start, exited or answered with something invalid.
    HelperFailed,
    /// The check did not finish before its deadline.
    ProbeTimedOut,
    /// The session indicator failed, so sessions are refused.
    IndicatorFailed,
}

/// OCR readiness and the installed OCR language tags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcrReadiness {
    #[serde(flatten)]
    readiness: Readiness,
    /// BCP-47 tags of the installed OCR languages, at most [`MAX_REPORTED_OCR_LANGUAGES`].
    languages: Vec<String>,
    /// The language OCR uses when a request gives no `language`, if any.
    default_language: Option<String>,
}

impl OcrReadiness {
    /// OCR that was not or could not be checked.
    #[must_use]
    pub const fn without_languages(readiness: Readiness) -> Self {
        Self {
            readiness,
            languages: Vec::new(),
            default_language: None,
        }
    }

    /// The readiness the installed languages give. Tags that are not plain BCP-47 (ASCII
    /// letters, digits and `-`, at most 35 characters) are dropped, so a report never repeats
    /// anything else Windows might return.
    #[must_use]
    pub fn from_languages(languages: Vec<String>, default_language: Option<String>) -> Self {
        let mut kept: Vec<String> = Vec::new();
        for tag in languages {
            if kept.len() == MAX_REPORTED_OCR_LANGUAGES {
                break;
            }
            if is_language_tag(&tag) && !kept.contains(&tag) {
                kept.push(tag);
            }
        }
        let languages = kept;
        let default_language = default_language.filter(|tag| is_language_tag(tag));
        let readiness = if languages.is_empty() {
            Readiness::Unavailable {
                reason: ReadinessReason::NoOcrLanguage,
            }
        } else if default_language.is_none() {
            Readiness::Degraded {
                reason: ReadinessReason::NoDefaultOcrLanguage,
            }
        } else {
            Readiness::Ready
        };
        Self {
            readiness,
            languages,
            default_language,
        }
    }
}

fn is_language_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= MAX_LANGUAGE_TAG_LEN
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Whether each helper process the server starts can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperReadiness {
    /// The isolated OCR process. `ready` means it answered the probe.
    pub ocr: Readiness,
    /// The one-shot `focus_window` activation helper. It is started only when Windows refuses the
    /// server's own foreground request, so `ready` means it is configured, not that it ran.
    pub activation: Readiness,
    /// The capture-excluded session lamp. It starts with a session, so `ready` means it is
    /// configured and the indicator has not failed.
    pub lamp: Readiness,
    /// The UI Automation helper behind `find_elements`. It is not started for the report: it is
    /// `ready` once it has answered a query, and `degraded` with `not_verified` before the first.
    pub ui_automation: Readiness,
}

/// What a backend's bounded readiness probe found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeReadiness {
    pub ocr: OcrReadiness,
    pub helpers: HelperReadiness,
}

impl Default for RuntimeReadiness {
    /// A backend without a probe: OCR is not implemented and no helper is configured.
    fn default() -> Self {
        let not_configured = Readiness::Unsupported {
            reason: ReadinessReason::NotConfigured,
        };
        Self {
            ocr: OcrReadiness::without_languages(Readiness::Unsupported {
                reason: ReadinessReason::NotImplemented,
            }),
            helpers: HelperReadiness {
                ocr: not_configured,
                activation: not_configured,
                lamp: not_configured,
                ui_automation: not_configured,
            },
        }
    }
}

/// Tool families the server deliberately never offers, whatever the platform supports. These
/// are a product boundary, not missing features: the host application does such work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenFamily {
    ApplicationLaunch,
    Shell,
    Filesystem,
    Registry,
    Network,
    Clipboard,
    BrowserDom,
}

impl ForbiddenFamily {
    pub(crate) const ALL: [Self; 7] = [
        Self::ApplicationLaunch,
        Self::Shell,
        Self::Filesystem,
        Self::Registry,
        Self::Network,
        Self::Clipboard,
        Self::BrowserDom,
    ];
}

/// The hard request limits from [`crate::limits`], as a report reads them.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardLimits {
    max_capture_width: u32,
    max_wait_ms: u32,
    max_settle_ms: u32,
    max_move_duration_ms: u32,
    max_scroll_delta: u32,
    max_text_utf16_units: usize,
    max_text_timeout_ms: u32,
    max_run_actions_wait_ms: u32,
    max_run_actions_total_wait_ms: u32,
    max_windows: usize,
    max_window_wait_candidates: usize,
    max_read_text_lines: usize,
    max_read_text_chars: usize,
    max_read_text_bytes: usize,
}

impl HardLimits {
    /// The limits this build enforces.
    pub const CURRENT: Self = Self {
        max_capture_width: limits::MAX_CAPTURE_WIDTH,
        max_wait_ms: limits::MAX_WAIT_MS,
        max_settle_ms: limits::MAX_SETTLE_MS,
        max_move_duration_ms: limits::MAX_MOVE_DURATION_MS,
        max_scroll_delta: limits::MAX_SCROLL_DELTA,
        max_text_utf16_units: limits::MAX_TEXT_UTF16_UNITS,
        max_text_timeout_ms: limits::MAX_TEXT_TIMEOUT_MS,
        max_run_actions_wait_ms: limits::MAX_RUN_ACTIONS_WAIT_MS,
        max_run_actions_total_wait_ms: limits::MAX_RUN_ACTIONS_TOTAL_WAIT_MS,
        max_windows: limits::MAX_WINDOWS,
        max_window_wait_candidates: limits::MAX_WINDOW_WAIT_CANDIDATES,
        max_read_text_lines: limits::MAX_READ_TEXT_LINES,
        max_read_text_chars: limits::MAX_READ_TEXT_CHARS,
        max_read_text_bytes: limits::MAX_READ_TEXT_BYTES,
    };
}

/// The capability part of the report that `get_server_status` carries as `capabilities`. The
/// identity, security and build fields of the full report are already status fields of their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    /// The `capabilities` list of the full report.
    features: Vec<CapabilityDescriptor>,
    ocr: OcrReadiness,
    helpers: HelperReadiness,
    limits: HardLimits,
    forbidden_families: Vec<ForbiddenFamily>,
}

impl RuntimeCapabilities {
    #[must_use]
    pub fn new(features: Vec<CapabilityDescriptor>, readiness: RuntimeReadiness) -> Self {
        Self {
            features,
            ocr: readiness.ocr,
            helpers: readiness.helpers,
            limits: HardLimits::CURRENT,
            forbidden_families: ForbiddenFamily::ALL.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn installed_languages_decide_ready_degraded_and_unavailable() {
        let ready = OcrReadiness::from_languages(tags(&["en-US", "de-DE"]), Some("en-US".into()));
        assert_eq!(ready.readiness, Readiness::Ready);
        assert_eq!(ready.languages, tags(&["en-US", "de-DE"]));

        let degraded = OcrReadiness::from_languages(tags(&["de-DE"]), None);
        assert_eq!(
            degraded.readiness,
            Readiness::Degraded {
                reason: ReadinessReason::NoDefaultOcrLanguage
            }
        );

        let unavailable = OcrReadiness::from_languages(Vec::new(), None);
        assert_eq!(
            unavailable.readiness,
            Readiness::Unavailable {
                reason: ReadinessReason::NoOcrLanguage
            }
        );
    }

    #[test]
    fn reported_tags_are_bounded_and_never_carry_other_text() {
        let mut many: Vec<String> = (0..100).map(|index| format!("x-{index}")).collect();
        many.push(r"C:\Users\someone".into());
        many.push("a".repeat(36));
        let report = OcrReadiness::from_languages(many, Some("en US".into()));
        assert_eq!(report.languages.len(), MAX_REPORTED_OCR_LANGUAGES);
        assert!(report.languages.iter().all(|tag| is_language_tag(tag)));
        assert_eq!(report.default_language, None);
    }

    #[test]
    fn states_serialize_as_distinct_values_with_stable_reason_codes() {
        let json = serde_json::to_value([
            Readiness::Ready,
            Readiness::Degraded {
                reason: ReadinessReason::NoDefaultOcrLanguage,
            },
            Readiness::Unavailable {
                reason: ReadinessReason::ProbeTimedOut,
            },
            Readiness::Unsupported {
                reason: ReadinessReason::NotConfigured,
            },
        ])
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {"state": "ready"},
                {"state": "degraded", "reason": "no_default_ocr_language"},
                {"state": "unavailable", "reason": "probe_timed_out"},
                {"state": "unsupported", "reason": "not_configured"},
            ])
        );
    }
}
