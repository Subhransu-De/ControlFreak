//! Image-space pointer coordinates.
//!
//! Every returned screenshot gets an `observation_id` that records its source rectangle in
//! virtual-desktop pixels and its image size. Pointer tools can then take a point in that image
//! and the server maps it back, so clients never redo crop and scale arithmetic. Only geometry is
//! kept, never pixels, in a small bounded map with a short lifetime.

use controlfreak_core::{DisplayBounds, PlatformBackend, PlatformError};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_OBSERVATIONS: usize = 64;
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
pub(super) fn register(source: DisplayBounds, image_width: u32, image_height: u32) -> String {
    let id = format!(
        "obs-{:016x}-{:x}",
        process_nonce(),
        NEXT_OBSERVATION.fetch_add(1, Ordering::Relaxed)
    );
    let mut observations = OBSERVATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    observations.retain(|observation| observation.created.elapsed() < OBSERVATION_TTL);
    while observations.len() >= MAX_OBSERVATIONS {
        observations.pop_front();
    }
    observations.push_back(Observation {
        id: id.clone(),
        created: Instant::now(),
        mapping: ImageMapping {
            source,
            image_width,
            image_height,
        },
    });
    id
}

fn invalid(argument: &str, reason: impl Into<String>) -> PlatformError {
    PlatformError::InvalidArgument {
        argument: argument.to_owned(),
        reason: reason.into(),
    }
}

fn mapping(id: &str) -> Result<ImageMapping, PlatformError> {
    OBSERVATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|observation| observation.id == id && observation.created.elapsed() < OBSERVATION_TTL)
        .map(|observation| observation.mapping)
        .ok_or_else(|| {
            invalid(
                "observation_id",
                "observation was not found or has expired; capture again and use the new observation_id",
            )
        })
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
            ((None, None, None), (Some(observation_id), Some(image_x), Some(image_y))) => {
                Ok(Self::Image {
                    observation_id: observation_id.to_owned(),
                    image_x,
                    image_y,
                })
            }
            _ => Err(invalid(
                names,
                "provide either display_id with x and y, or observation_id with image_x and image_y",
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
            Self::Display { display_id, x, y } => return Ok((display_id.clone(), *x, *y)),
            Self::Image {
                observation_id,
                image_x,
                image_y,
            } => (observation_id, *image_x, *image_y),
        };
        let (x, y) = to_virtual(mapping(observation_id)?, image_x, image_y)?;
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
        let first = register(bounds(0, 0, 10, 10), 10, 10);
        assert!(mapping(&first).is_ok());
        for _ in 0..MAX_OBSERVATIONS {
            register(bounds(0, 0, 10, 10), 10, 10);
        }
        assert!(mapping(&first).is_err());
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
        assert!(PointerTarget::from_arguments(none, (None, None, None), "x").is_err());
    }
}
