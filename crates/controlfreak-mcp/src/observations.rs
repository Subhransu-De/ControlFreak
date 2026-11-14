//! Image-space pointer coordinates.
//!
//! Every returned screenshot gets an `observation_id` that records its source rectangle in
//! virtual-desktop pixels and its image size. Pointer tools can then take a point in that image
//! and the server maps it back, so clients never redo crop and scale arithmetic. Only geometry is
//! kept, never pixels, in a small bounded map with a short lifetime.
//!
//! An image of one window maps points only while that window is unchanged: its reference changes
//! when the window moves, resizes or closes, so a stale image is refused instead of mapped onto
//! whatever now lies under the old rectangle.

use controlfreak_core::{
    DisplayBounds, ObservationOptions, PlatformBackend, PlatformError, WaitForWindowRequest,
};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The alias every `display_id` parameter accepts for the primary display.
pub(super) const PRIMARY_DISPLAY: &str = "primary";

/// Unit tests share one process-wide store across parallel threads, so they get headroom that
/// no burst of registrations from other tests can use up before a test reads its own.
const MAX_OBSERVATIONS: usize = if cfg!(test) { 4096 } else { 64 };
const OBSERVATION_TTL: Duration = Duration::from_mins(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageMapping {
    source: DisplayBounds,
    image_width: u32,
    image_height: u32,
}

struct Observation {
    id: String,
    created: Instant,
    mapping: ImageMapping,
    /// The window this image shows, when it is an image of one window.
    window_id: Option<String>,
}

static OBSERVATIONS: Mutex<VecDeque<Observation>> = Mutex::new(VecDeque::new());
static NEXT_OBSERVATION: AtomicU64 = AtomicU64::new(1);

fn process_nonce() -> u64 {
    static NONCE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *NONCE.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos());
        u64::from(std::process::id()) << 32 | u64::from(nanos)
    })
}

/// Records a returned image's geometry and returns the id clients pass back with image points.
/// `window_id` is set when the image shows one window.
pub(super) fn register(
    source: DisplayBounds,
    image_width: u32,
    image_height: u32,
    window_id: Option<String>,
) -> String {
    let id = format!(
        "obs-{:016x}-{:x}",
        process_nonce(),
        NEXT_OBSERVATION.fetch_add(1, Ordering::Relaxed)
    );
    let mut observations = OBSERVATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    remember(
        &mut observations,
        Observation {
            id: id.clone(),
            created: Instant::now(),
            mapping: ImageMapping {
                source,
                image_width,
                image_height,
            },
            window_id,
        },
    );
    id
}

/// Drops expired entries, then the oldest ones, so the store never holds more than the bound.
fn remember(observations: &mut VecDeque<Observation>, observation: Observation) {
    observations.retain(|kept| kept.created.elapsed() < OBSERVATION_TTL);
    while observations.len() >= MAX_OBSERVATIONS {
        observations.pop_front();
    }
    observations.push_back(observation);
}

fn invalid(argument: &str, reason: impl Into<String>) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: argument.to_owned(),
        reason: reason.into(),
    }
}

fn lookup(id: &str) -> Result<(ImageMapping, Option<String>), PlatformError> {
    OBSERVATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|observation| observation.id == id && observation.created.elapsed() < OBSERVATION_TTL)
        .map(|observation| (observation.mapping, observation.window_id.clone()))
        .ok_or_else(|| {
            invalid(
                "observation_id",
                "observation was not found or has expired; capture again and use the new observation_id",
            )
        })
}

#[cfg(test)]
fn mapping(id: &str) -> Result<ImageMapping, PlatformError> {
    lookup(id).map(|(mapping, _)| mapping)
}

/// Resolves the `"primary"` alias to the primary display's ID. Other IDs pass through for the
/// backend to validate.
pub(super) fn resolve_display(
    backend: &dyn PlatformBackend,
    display_id: &str,
) -> Result<String, PlatformError> {
    if !display_id.eq_ignore_ascii_case(PRIMARY_DISPLAY) {
        return Ok(display_id.to_owned());
    }
    backend
        .list_displays()?
        .into_iter()
        .find(|display| display.is_primary)
        .map(|display| display.id)
        .ok_or_else(|| {
            invalid(
                "display_id",
                "no primary display was found; call list_displays and pass an ID",
            )
        })
}

/// Resolves the display alias in an observation's crop region.
pub(super) fn resolve_observation(
    backend: &dyn PlatformBackend,
    options: &mut ObservationOptions,
) -> Result<(), PlatformError> {
    if let Some(region) = &mut options.region {
        region.display_id = resolve_display(backend, &region.display_id)?;
    }
    Ok(())
}

/// Maps an image pixel to the first virtual-desktop pixel it covers, the same
/// `image_x * source_width / image_width` arithmetic as the published crop-mapping recipe.
fn to_virtual(
    mapping: ImageMapping,
    image_x: u32,
    image_y: u32,
) -> Result<(i64, i64), PlatformError> {
    if image_x >= mapping.image_width || image_y >= mapping.image_height {
        return Err(invalid(
            "image_x/image_y",
            format!(
                "must lie inside the {}x{} image",
                mapping.image_width, mapping.image_height
            ),
        ));
    }
    let scale = |image: u32, image_size: u32, source_size: u32| {
        u64::from(image) * u64::from(source_size) / u64::from(image_size)
    };
    let offset_x = scale(image_x, mapping.image_width, mapping.source.width);
    let offset_y = scale(image_y, mapping.image_height, mapping.source.height);
    Ok((
        i64::from(mapping.source.left) + i64::try_from(offset_x).unwrap_or(i64::MAX),
        i64::from(mapping.source.top) + i64::try_from(offset_y).unwrap_or(i64::MAX),
    ))
}

/// A pointer position given either directly or as a point in a returned image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PointerTarget {
    Display {
        display_id: String,
        x: u32,
        y: u32,
    },
    Image {
        observation_id: String,
        image_x: u32,
        image_y: u32,
    },
}

impl PointerTarget {
    /// Builds a target from optional tool arguments, requiring exactly one complete form.
    pub(super) fn from_arguments(
        display: (Option<String>, Option<u32>, Option<u32>),
        image: (Option<&str>, Option<u32>, Option<u32>),
        names: &str,
    ) -> Result<Self, PlatformError> {
        match (display, image) {
            ((Some(display_id), Some(x), Some(y)), (None, None, None)) => {
                Ok(Self::Display { display_id, x, y })
            }
            // No display means the primary display.
            ((None, Some(x), Some(y)), (None, None, None)) => Ok(Self::Display {
                display_id: PRIMARY_DISPLAY.to_owned(),
                x,
                y,
            }),
            ((None, None, None), (Some(observation_id), Some(image_x), Some(image_y))) => {
                Ok(Self::Image {
                    observation_id: observation_id.to_owned(),
                    image_x,
                    image_y,
                })
            }
            _ => Err(invalid(
                names,
                "provide x and y (display_id is optional), or observation_id with image_x and image_y",
            )),
        }
    }

    /// Echoes the requested position in its original form for action results.
    pub(super) fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Display { display_id, x, y } => {
                serde_json::json!({ "display_id": display_id, "x": x, "y": y })
            }
            Self::Image {
                observation_id,
                image_x,
                image_y,
            } => serde_json::json!({
                "observation_id": observation_id,
                "image_x": image_x,
                "image_y": image_y,
            }),
        }
    }

    /// Resolves to a display ID and display-local physical pixels. Image points are mapped
    /// through their observation and located on the current display layout.
    pub(super) fn resolve(
        &self,
        backend: &dyn PlatformBackend,
    ) -> Result<(String, u32, u32), PlatformError> {
        let (observation_id, image_x, image_y) = match self {
            Self::Display { display_id, x, y } => {
                return Ok((resolve_display(backend, display_id)?, *x, *y));
            }
            Self::Image {
                observation_id,
                image_x,
                image_y,
            } => (observation_id, *image_x, *image_y),
        };
        let (mapping, window_id) = lookup(observation_id)?;
        // Ask for this one window's reference directly: `list_windows` is capped, so a window
        // missing from that list would otherwise look closed.
        if let Some(window_id) = window_id {
            let unchanged = backend
                .wait_for_window(&WaitForWindowRequest {
                    window_id: Some(window_id.clone()),
                    title_contains: None,
                    class_name: None,
                    process_id: None,
                    is_foreground: None,
                    timeout_ms: 0,
                })?
                .window
                .is_some_and(|window| window.id == window_id);
            if !unchanged {
                return Err(invalid(
                    "observation_id",
                    "the window in this image moved, resized or closed since the capture; capture it again",
                ));
            }
        }
        let (x, y) = to_virtual(mapping, image_x, image_y)?;
        let display = backend
            .list_displays()?
            .into_iter()
            .find(|display| {
                let bounds = display.bounds;
                x >= i64::from(bounds.left)
                    && x < i64::from(bounds.left) + i64::from(bounds.width)
                    && y >= i64::from(bounds.top)
                    && y < i64::from(bounds.top) + i64::from(bounds.height)
            })
            .ok_or_else(|| {
                invalid(
                    "observation_id",
                    "the image point is no longer on an active display; capture again",
                )
            })?;
        let local = |value: i64, origin: i32| u32::try_from(value - i64::from(origin)).unwrap_or(0);
        Ok((
            display.id,
            local(x, display.bounds.left),
            local(y, display.bounds.top),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(left: i32, top: i32, width: u32, height: u32) -> DisplayBounds {
        DisplayBounds {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn image_points_map_through_crop_and_fractional_scale() {
        // A 1920x1080 crop at a negative origin, returned as a 1280x720 image.
        let mapping = ImageMapping {
            source: bounds(-1920, -200, 1920, 1080),
            image_width: 1280,
            image_height: 720,
        };
        assert_eq!(to_virtual(mapping, 0, 0).unwrap(), (-1920, -200));
        assert_eq!(
            to_virtual(mapping, 640, 360).unwrap(),
            (-1920 + 960, -200 + 540)
        );
        assert_eq!(to_virtual(mapping, 1279, 719).unwrap(), (-2, 878));
        assert!(to_virtual(mapping, 1280, 0).is_err());
        assert!(to_virtual(mapping, 0, 720).is_err());
        let native = ImageMapping {
            source: bounds(100, 60, 400, 200),
            image_width: 400,
            image_height: 200,
        };
        assert_eq!(to_virtual(native, 17, 23).unwrap(), (117, 83));
    }

    #[test]
    fn observations_are_bounded_and_unknown_ids_are_refused() {
        // A local store: the process-wide one is shared with every parallel test, and filling it
        // here would evict their observations.
        let mut store = VecDeque::new();
        let entry = |id: String| Observation {
            id,
            created: Instant::now(),
            mapping: ImageMapping {
                source: bounds(0, 0, 10, 10),
                image_width: 10,
                image_height: 10,
            },
            window_id: None,
        };
        remember(&mut store, entry("first".to_owned()));
        for number in 0..MAX_OBSERVATIONS {
            remember(&mut store, entry(number.to_string()));
        }
        assert_eq!(store.len(), MAX_OBSERVATIONS);
        assert!(store.iter().all(|kept| kept.id != "first"));
        assert!(mapping("obs-not-issued").is_err());
    }

    #[test]
    fn exactly_one_complete_point_form_is_required() {
        let display = (Some("d".to_owned()), Some(1), Some(2));
        let none = (None, None, None);
        assert!(PointerTarget::from_arguments(display.clone(), (None, None, None), "x").is_ok());
        assert!(
            PointerTarget::from_arguments(none.clone(), (Some("obs"), Some(1), Some(2)), "x")
                .is_ok()
        );
        assert!(
            PointerTarget::from_arguments(display, (Some("obs"), Some(1), Some(2)), "x").is_err()
        );
        assert!(
            PointerTarget::from_arguments(none.clone(), (Some("obs"), Some(1), None), "x").is_err()
        );
        assert!(
            PointerTarget::from_arguments(
                (Some("d".to_owned()), None, Some(2)),
                (None, None, None),
                "x"
            )
            .is_err()
        );
        // A point with no display lies on the primary display.
        assert_eq!(
            PointerTarget::from_arguments((None, Some(1), Some(2)), (None, None, None), "x")
                .unwrap(),
            PointerTarget::Display {
                display_id: PRIMARY_DISPLAY.to_owned(),
                x: 1,
                y: 2
            }
        );
        assert!(PointerTarget::from_arguments(none, (None, None, None), "x").is_err());
    }
}
