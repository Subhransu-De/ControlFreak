//! Image-space pointer coordinates.
//!
//! Every returned screenshot gets an `observation_id` that records its source rectangle in
//! virtual-desktop pixels and its image size. Pointer tools can then take a point in that image
//! and the server maps it back, so clients never redo crop and scale arithmetic. Only geometry is
//! kept, never pixels, in a small bounded map with a short lifetime.
//!
//! An image of one window maps points only while that window is unchanged: the same reference
//! with the same bounds. A window keeps its reference when it only moves or is resized, so the
//! bounds are compared too, and a stale image is refused instead of mapped onto
//! whatever now lies under the old rectangle.
//!
//! Every image also records the display layout it was captured on (display IDs and bounds). A
//! point maps only while that layout is unchanged: after a display was added, removed or moved,
//! the old virtual rectangle may lie on another display, so the point is refused instead.

use controlfreak_core::{
    DisplayBounds, DisplayLayout, ObservationOptions, PlatformBackend, PlatformError,
    WaitForWindowRequest, WindowBounds,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageMapping {
    source: DisplayBounds,
    image_width: u32,
    image_height: u32,
    /// The display layout `source` belongs to.
    layout: DisplayLayout,
}

struct Observation {
    id: String,
    created: Instant,
    mapping: ImageMapping,
    /// The window this image shows, when it is an image of one window.
    window: Option<ShownWindow>,
}

/// The window an image shows, as it was when the image was captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ShownWindow {
    pub(super) id: String,
    pub(super) bounds: WindowBounds,
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
/// `layout` is the display layout the image was captured on; `window` is set when the image shows
/// one window.
pub(super) fn register(
    source: DisplayBounds,
    image_width: u32,
    image_height: u32,
    layout: DisplayLayout,
    window: Option<ShownWindow>,
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
                layout,
            },
            window,
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

fn lookup(id: &str) -> Result<(ImageMapping, Option<ShownWindow>), PlatformError> {
    OBSERVATIONS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|observation| observation.id == id && observation.created.elapsed() < OBSERVATION_TTL)
        .map(|observation| (observation.mapping.clone(), observation.window.clone()))
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

/// Maps an image pixel to the virtual-desktop pixel under its centre, the same
/// `(2 * image_x + 1) * source_width / (2 * image_width)` arithmetic as the published
/// crop-mapping recipe. A resized image pixel covers several source pixels; its centre is the
/// point it shows best, and it brings a source pixel taken to the image pixel that covers it and
/// back within one source pixel of where it started. An unresized image maps one to one.
fn to_virtual(
    mapping: &ImageMapping,
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
        (2 * u64::from(image) + 1) * u64::from(source_size) / (2 * u64::from(image_size))
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
    /// through their observation only while the display layout is the one it was captured on.
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
        let (mapping, shown) = lookup(observation_id)?;
        // The same list locates the point below, so the layout check costs no extra call.
        let displays = backend.list_displays()?;
        if DisplayLayout::from_displays(&displays) != mapping.layout {
            return Err(invalid(
                "observation_id",
                "the display layout changed since this screenshot; capture again",
            ));
        }
        // Ask for this one window's reference directly: `list_windows` is capped, so a window
        // missing from that list would otherwise look closed.
        if let Some(shown) = shown {
            let unchanged = backend
                .wait_for_window(&WaitForWindowRequest {
                    window_id: Some(shown.id.clone()),
                    title_contains: None,
                    class_name: None,
                    process_id: None,
                    is_foreground: None,
                    timeout_ms: 0,
                })?
                .window
                .is_some_and(|window| window.id == shown.id && window.bounds == shown.bounds);
            if !unchanged {
                return Err(invalid(
                    "observation_id",
                    "the window in this image moved, resized or closed since the capture; capture it again",
                ));
            }
        }
        let (x, y) = to_virtual(&mapping, image_x, image_y)?;
        let display = displays
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

    fn image(source: DisplayBounds, image_width: u32, image_height: u32) -> ImageMapping {
        ImageMapping {
            source,
            image_width,
            image_height,
            layout: DisplayLayout::from_displays(&[]),
        }
    }

    #[test]
    fn image_points_map_through_crop_and_fractional_scale() {
        // A 1920x1080 crop at a negative origin, returned as a 1280x720 image.
        let mapping = image(bounds(-1920, -200, 1920, 1080), 1280, 720);
        assert_eq!(to_virtual(&mapping, 0, 0).unwrap(), (-1920, -200));
        assert_eq!(
            to_virtual(&mapping, 640, 360).unwrap(),
            (-1920 + 960, -200 + 540)
        );
        // The centre of the last image pixel, which covers 1.5 source pixels on each axis.
        assert_eq!(to_virtual(&mapping, 1279, 719).unwrap(), (-1, 879));
        assert!(to_virtual(&mapping, 1280, 0).is_err());
        assert!(to_virtual(&mapping, 0, 720).is_err());
        let native = image(bounds(100, 60, 400, 200), 400, 200);
        assert_eq!(to_virtual(&native, 17, 23).unwrap(), (117, 83));
    }

    #[test]
    fn an_odd_image_size_round_trips_within_one_source_pixel_on_each_axis() {
        // 1920x1080 at a negative origin returned as 1279x719: neither axis has a whole scale.
        let (left, top) = (-1920, -1080);
        let mapping = image(bounds(left, top, 1920, 1080), 1279, 719);
        // The image pixel that covers a source pixel: how the image was made, and how the cursor
        // marker is placed.
        let to_image = |offset: u32, source: u32, image: u32| {
            u32::try_from(u64::from(offset) * u64::from(image) / u64::from(source)).unwrap()
        };
        let offset = |mapped: i64, origin: i32| u32::try_from(mapped - i64::from(origin)).unwrap();
        // Each axis on its own, with the other axis held at the image's last pixel.
        for x in 0..1920 {
            let (virtual_x, virtual_y) =
                to_virtual(&mapping, to_image(x, 1920, 1279), 718).unwrap();
            let error = i64::from(left) + i64::from(x) - virtual_x;
            assert!(
                error.abs() <= 1,
                "source x {x} comes back {error} pixels away"
            );
            assert_eq!(virtual_y, i64::from(top) + 1079);
        }
        for y in 0..1080 {
            let (virtual_x, virtual_y) =
                to_virtual(&mapping, 1278, to_image(y, 1080, 719)).unwrap();
            let error = i64::from(top) + i64::from(y) - virtual_y;
            assert!(
                error.abs() <= 1,
                "source y {y} comes back {error} pixels away"
            );
            assert_eq!(virtual_x, i64::from(left) + 1919);
        }
        // Every image pixel maps inside the source, onto a source pixel that overlaps the source
        // span `[image * source / size, (image + 1) * source / size)` it shows. With a scale that
        // is not whole, that source pixel can straddle two image pixels.
        let overlaps = |image: u32, mapped: u32, source: u32, size: u32| {
            let (image, mapped, source, size) = (
                u64::from(image),
                u64::from(mapped),
                u64::from(source),
                u64::from(size),
            );
            mapped < source
                && (mapped + 1) * size > image * source
                && mapped * size < (image + 1) * source
        };
        for image_x in 0..1279 {
            let (virtual_x, _) = to_virtual(&mapping, image_x, 0).unwrap();
            assert!(
                overlaps(image_x, offset(virtual_x, left), 1920, 1279),
                "x {image_x}"
            );
        }
        for image_y in 0..719 {
            let (_, virtual_y) = to_virtual(&mapping, 0, image_y).unwrap();
            assert!(
                overlaps(image_y, offset(virtual_y, top), 1080, 719),
                "y {image_y}"
            );
        }
        assert!(to_virtual(&mapping, 1279, 0).is_err());
        assert!(to_virtual(&mapping, 0, 719).is_err());
    }

    #[test]
    fn a_layout_is_compared_by_display_ids_and_bounds_in_any_order() {
        let display = |id: &str, left: i32, is_primary: bool| controlfreak_core::DisplayInfo {
            id: id.to_owned(),
            name: format!("name of {id}"),
            bounds: bounds(left, 0, 1920, 1080),
            is_primary,
        };
        let layout =
            DisplayLayout::from_displays(&[display("a", 0, true), display("b", 1920, false)]);
        let mut renamed = display("b", 1920, false);
        renamed.name = "another name".to_owned();
        assert_eq!(
            DisplayLayout::from_displays(&[renamed, display("a", 0, true)]),
            layout
        );
        // One display moved to the other side.
        assert_ne!(
            DisplayLayout::from_displays(&[display("a", 0, true), display("b", -1920, false)]),
            layout
        );
        assert_ne!(
            DisplayLayout::from_displays(&[display("a", 0, true)]),
            layout
        );
    }

    /// Every `max_width` budget a capture accepts, and none, sizes the image with the same
    /// arithmetic the backend uses. Each image pixel then maps onto a source pixel inside the
    /// source rectangle that overlaps the span it shows, and a source pixel comes back at most
    /// half an image pixel's footprint (plus one pixel of rounding) away on each axis.
    #[test]
    fn image_points_round_trip_for_every_max_width_budget() {
        use controlfreak_core::{limits::MAX_CAPTURE_WIDTH, scaled_dimensions};
        let sources = [
            bounds(-1920, -200, 1920, 1080), // left of a primary display, above its top
            bounds(0, 0, 3840, 2160),
            bounds(-1280, -1024, 1280, 1024),
            bounds(2560, 0, 1366, 768),
            bounds(37, -11, 401, 3), // a thin crop with odd sizes
            bounds(0, 0, 7680, 4320),
        ];
        // The image pixel that covers a source pixel, as the image was made.
        let to_image = |offset: u32, source: u32, image: u32| {
            u32::try_from(u64::from(offset) * u64::from(image) / u64::from(source)).unwrap()
        };
        // `mapped` lies in the source and overlaps `[image * source / size, (image + 1) * source / size)`.
        let overlaps = |image: u32, mapped: i64, source: u32, size: u32| {
            let Ok(mapped) = u64::try_from(mapped) else {
                return false;
            };
            let (image, source, size) = (u64::from(image), u64::from(source), u64::from(size));
            mapped < source
                && (mapped + 1) * size > image * source
                && mapped * size < (image + 1) * source
        };
        for budget in (1..=MAX_CAPTURE_WIDTH).map(Some).chain([None]) {
            for source in sources {
                let (width, height) = (source.width, source.height);
                let (image_width, image_height) = scaled_dimensions(width, height, budget);
                let context = format!("{budget:?} {source:?} -> {image_width}x{image_height}");
                assert_eq!(
                    image_width,
                    budget.map_or(width, |budget| budget.min(width)),
                    "{context}"
                );
                assert!((1..=height).contains(&image_height), "{context}");
                let mapping = image(source, image_width, image_height);
                let (left, top) = (i64::from(source.left), i64::from(source.top));
                for (image_x, image_y) in [
                    (0, 0),
                    (image_width / 3, image_height * 2 / 3),
                    (image_width / 2, image_height / 2),
                    (image_width - 1, image_height - 1),
                ] {
                    let (virtual_x, virtual_y) = to_virtual(&mapping, image_x, image_y).unwrap();
                    assert!(
                        overlaps(image_x, virtual_x - left, width, image_width)
                            && overlaps(image_y, virtual_y - top, height, image_height),
                        "{context}: image ({image_x}, {image_y}) -> ({virtual_x}, {virtual_y})"
                    );
                }
                assert!(to_virtual(&mapping, image_width, 0).is_err(), "{context}");
                assert!(to_virtual(&mapping, 0, image_height).is_err(), "{context}");
                for (source_x, source_y) in [
                    (0, 0),
                    (width / 3, height * 2 / 3),
                    (width / 2, height / 2),
                    (width - 1, height - 1),
                ] {
                    let (virtual_x, virtual_y) = to_virtual(
                        &mapping,
                        to_image(source_x, width, image_width),
                        to_image(source_y, height, image_height),
                    )
                    .unwrap();
                    // |error| < footprint / 2 + 1, kept in integers.
                    let close = |error: i64, source: u32, size: u32| {
                        2 * error.abs() * i64::from(size) < i64::from(source) + 2 * i64::from(size)
                    };
                    let error_x = left + i64::from(source_x) - virtual_x;
                    let error_y = top + i64::from(source_y) - virtual_y;
                    assert!(
                        close(error_x, width, image_width) && close(error_y, height, image_height),
                        "{context}: ({source_x}, {source_y}) came back ({error_x}, {error_y}) away"
                    );
                    if image_width == width && image_height == height {
                        assert_eq!((error_x, error_y), (0, 0), "{context}");
                    }
                }
            }
        }
    }

    #[test]
    fn observations_are_bounded_and_unknown_ids_are_refused() {
        // A local store: the process-wide one is shared with every parallel test, and filling it
        // here would evict their observations.
        let mut store = VecDeque::new();
        let entry = |id: String| Observation {
            id,
            created: Instant::now(),
            mapping: image(bounds(0, 0, 10, 10), 10, 10),
            window: None,
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
