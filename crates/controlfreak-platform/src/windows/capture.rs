#![allow(unsafe_code)]

use super::{
    ActionObservation, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, BitmapPixelFormat, CAPTUREBLT,
    ComApartment, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DataWriter, DeleteDC,
    DeleteObject, DisplayBounds, DisplayInfo, DisplayScreenshot, Duration, GetDC, GetDIBits,
    HBITMAP, HDC, HGDIOBJ, HSTRING, Instant, Language, MAX_ACTION_IMAGE_WIDTH, MAX_CAPTURE_BYTES,
    MAX_CAPTURE_WIDTH, MAX_WAIT_MS, ObservationMode, ObservationOptions, OcrEngine, OcrLine,
    OcrRegionRequest, OcrResult, OcrWord, OnceLock, PlatformError, ReleaseDC, SRCCOPY,
    SelectObject, SoftwareBitmap, SystemTime, UNIX_EPOCH, VISUAL_COMPARE_WIDTH, VISUAL_POLL_MS,
    VisualChangeResult, WaitForVisualChangeRequest, WindowBounds, WindowInfo, c_void,
    current_cursor_position, ensure_dpi_awareness, enumerate_displays, find_display,
    foreground_window_handle, last_win32_error, size_of, thread, win32_error, window_info,
};

struct ScreenDc(HDC);

impl ScreenDc {
    fn acquire() -> Result<Self, PlatformError> {
        // SAFETY: A null HWND requests the desktop DC; the returned handle is released by Drop.
        let dc = unsafe { GetDC(None) };
        if dc.is_invalid() {
            Err(last_win32_error("GetDC"))
        } else {
            Ok(Self(dc))
        }
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: This DC was acquired by GetDC(None) and is released exactly once here.
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

struct MemoryDc(HDC);

impl MemoryDc {
    fn create(compatible_with: HDC) -> Result<Self, PlatformError> {
        // SAFETY: `compatible_with` is a live screen DC held by ScreenDc for this call.
        let dc = unsafe { CreateCompatibleDC(Some(compatible_with)) };
        if dc.is_invalid() {
            Err(last_win32_error("CreateCompatibleDC"))
        } else {
            Ok(Self(dc))
        }
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: This memory DC was created by CreateCompatibleDC and is deleted exactly once.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);

impl Bitmap {
    fn create(compatible_with: HDC, width: i32, height: i32) -> Result<Self, PlatformError> {
        // SAFETY: The DC is live and the caller validated positive, bounded dimensions.
        let bitmap = unsafe { CreateCompatibleBitmap(compatible_with, width, height) };
        if bitmap.is_invalid() {
            Err(last_win32_error("CreateCompatibleBitmap"))
        } else {
            Ok(Self(bitmap))
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: This bitmap was created by CreateCompatibleBitmap and is deleted exactly once
        // after SelectedObject has restored the previous object.
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

struct SelectedObject {
    dc: HDC,
    previous: HGDIOBJ,
}

impl SelectedObject {
    fn select(dc: HDC, object: HGDIOBJ) -> Result<Self, PlatformError> {
        // SAFETY: Both handles are live and compatible; Drop restores the returned prior object.
        let previous = unsafe { SelectObject(dc, object) };
        if previous.is_invalid() {
            Err(last_win32_error("SelectObject"))
        } else {
            Ok(Self { dc, previous })
        }
    }
}

impl Drop for SelectedObject {
    fn drop(&mut self) {
        // SAFETY: `previous` came from SelectObject on this same live memory DC.
        unsafe {
            SelectObject(self.dc, self.previous);
        }
    }
}

pub(super) fn capture_display_image(
    display: DisplayInfo,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<DisplayScreenshot, PlatformError> {
    let bounds = display.bounds;
    capture_display_bounds(display, bounds, max_width, include_cursor_marker)
}

pub(super) fn capture_display_bounds(
    display: DisplayInfo,
    source_bounds: DisplayBounds,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<DisplayScreenshot, PlatformError> {
    let (png, image_width, image_height, downscale_factor, cursor_marker) =
        capture_bounds(source_bounds, max_width, include_cursor_marker)?;
    Ok(DisplayScreenshot {
        display,
        source_bounds,
        png,
        image_width,
        image_height,
        downscale_factor,
        cursor_marker,
    })
}

pub(super) fn display_region_bounds(
    display: &DisplayInfo,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<DisplayBounds, PlatformError> {
    if width == 0 || height == 0 {
        return Err(PlatformError::InvalidArgument {
            argument: "width/height".to_owned(),
            reason: "region dimensions must be greater than zero".to_owned(),
        });
    }
    let right = x
        .checked_add(width)
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "x/width".to_owned(),
            reason: "region coordinate overflow".to_owned(),
        })?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| PlatformError::InvalidArgument {
            argument: "y/height".to_owned(),
            reason: "region coordinate overflow".to_owned(),
        })?;
    if right > display.bounds.width || bottom > display.bounds.height {
        return Err(PlatformError::InvalidArgument {
            argument: "region".to_owned(),
            reason: format!(
                "local region ({x}, {y}, {width}, {height}) is outside {}x{} display bounds",
                display.bounds.width, display.bounds.height
            ),
        });
    }
    let (left, top) = display.bounds.to_virtual(x, y)?;
    Ok(DisplayBounds {
        left,
        top,
        width,
        height,
    })
}

pub(super) const fn display_bounds_from_window(bounds: WindowBounds) -> DisplayBounds {
    DisplayBounds {
        left: bounds.left,
        top: bounds.top,
        width: bounds.width,
        height: bounds.height,
    }
}

pub(super) fn validate_max_width(max_width: Option<u32>) -> Result<(), PlatformError> {
    if max_width.is_some_and(|width| width == 0 || width > MAX_CAPTURE_WIDTH) {
        return Err(PlatformError::InvalidArgument {
            argument: "max_width".to_owned(),
            reason: format!("must be between 1 and {MAX_CAPTURE_WIDTH}"),
        });
    }
    Ok(())
}

pub(super) fn observe_display(
    display_id: &str,
    options: &ObservationOptions,
) -> Result<ActionObservation, PlatformError> {
    validate_max_width(options.max_width)?;
    if options.mode == ObservationMode::None {
        return Ok(ActionObservation {
            foreground_window: None,
            screenshot: None,
        });
    }
    let display = find_display(display_id)?;
    let foreground_window = foreground_window_info();
    let screenshot = if options.mode == ObservationMode::Screenshot {
        Some(capture_observation_screenshot(display, options)?)
    } else {
        None
    };
    Ok(ActionObservation {
        foreground_window,
        screenshot,
    })
}

pub(super) fn observe_foreground(
    options: &ObservationOptions,
) -> Result<ActionObservation, PlatformError> {
    validate_max_width(options.max_width)?;
    if options.mode == ObservationMode::None {
        return Ok(ActionObservation {
            foreground_window: None,
            screenshot: None,
        });
    }
    let foreground_window = foreground_window_info();
    let display = if let Some(window) = &foreground_window {
        find_display(&window.display_id)?
    } else {
        enumerate_displays()?
            .into_iter()
            .find(|display| display.is_primary)
            .ok_or_else(|| PlatformError::OperationFailed {
                operation: "post_action_observation".to_owned(),
                reason: "Windows reported no foreground window or primary display".to_owned(),
            })?
    };
    let screenshot = if options.mode == ObservationMode::Screenshot {
        Some(capture_observation_screenshot(display, options)?)
    } else {
        None
    };
    Ok(ActionObservation {
        foreground_window,
        screenshot,
    })
}

fn capture_observation_screenshot(
    default_display: DisplayInfo,
    options: &ObservationOptions,
) -> Result<DisplayScreenshot, PlatformError> {
    let max_width = Some(options.max_width.unwrap_or(MAX_ACTION_IMAGE_WIDTH));
    if let Some(region) = &options.region {
        let display = find_display(&region.display_id)?;
        let bounds =
            display_region_bounds(&display, region.x, region.y, region.width, region.height)?;
        capture_display_bounds(display, bounds, max_width, options.include_cursor)
    } else {
        capture_display_image(default_display, max_width, options.include_cursor)
    }
}

pub(super) fn foreground_window_info() -> Option<WindowInfo> {
    let hwnd = foreground_window_handle();
    (!hwnd.is_invalid())
        .then(|| window_info(hwnd).ok())
        .flatten()
}

fn capture_rgba(bounds: DisplayBounds) -> Result<Vec<u8>, PlatformError> {
    let width = i32::try_from(bounds.width).map_err(|_| PlatformError::OperationFailed {
        operation: "capture_display".to_owned(),
        reason: "display width exceeds the Win32 bitmap limit".to_owned(),
    })?;
    let height = i32::try_from(bounds.height).map_err(|_| PlatformError::OperationFailed {
        operation: "capture_display".to_owned(),
        reason: "display height exceeds the Win32 bitmap limit".to_owned(),
    })?;
    let byte_count = usize::try_from(bounds.width)
        .ok()
        .and_then(|width| {
            usize::try_from(bounds.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_CAPTURE_BYTES)
        .ok_or_else(|| PlatformError::OperationFailed {
            operation: "capture_display".to_owned(),
            reason: format!("capture buffer exceeds the {MAX_CAPTURE_BYTES}-byte safety limit"),
        })?;

    let screen_dc = ScreenDc::acquire()?;
    let memory_dc = MemoryDc::create(screen_dc.0)?;
    let bitmap = Bitmap::create(screen_dc.0, width, height)?;

    {
        let _selection = SelectedObject::select(memory_dc.0, bitmap.0.into())?;
        // SAFETY: Both DCs and the selected bitmap are live, coordinates/dimensions were validated,
        // and BitBlt retains no Rust references after returning.
        unsafe {
            BitBlt(
                memory_dc.0,
                0,
                0,
                width,
                height,
                Some(screen_dc.0),
                bounds.left,
                bounds.top,
                SRCCOPY | CAPTUREBLT,
            )
        }
        .map_err(|error| win32_error("BitBlt", &error))?;
    }

    let mut bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).map_err(|_| {
                PlatformError::OperationFailed {
                    operation: "GetDIBits".to_owned(),
                    reason: "BITMAPINFOHEADER size exceeds the Win32 field width".to_owned(),
                }
            })?,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0_u8; byte_count];
    // SAFETY: `bgra` has the checked size required for the requested 32-bit image, BITMAPINFO is
    // initialized for that layout, and all GDI handles remain live through the call.
    let copied_lines = unsafe {
        GetDIBits(
            memory_dc.0,
            bitmap.0,
            0,
            bounds.height,
            Some(bgra.as_mut_ptr().cast::<c_void>()),
            &raw mut bitmap_info,
            DIB_RGB_COLORS,
        )
    };
    if copied_lines != height {
        return Err(PlatformError::OperationFailed {
            operation: "GetDIBits".to_owned(),
            reason: format!("copied {copied_lines} of {height} scan lines"),
        });
    }

    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = u8::MAX;
    }
    Ok(bgra)
}

pub(super) fn capture_bounds(
    bounds: DisplayBounds,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<(Vec<u8>, u32, u32, u32, bool), PlatformError> {
    let bgra = capture_rgba(bounds)?;
    let (image_width, image_height, downscale_factor) =
        scaled_dimensions(bounds.width, bounds.height, max_width);
    let mut rgba = if image_width == bounds.width && image_height == bounds.height {
        bgra
    } else {
        resize_rgba_bilinear(
            &bgra,
            bounds.width,
            bounds.height,
            image_width,
            image_height,
        )
    };
    let cursor_marker = include_cursor_marker
        && cursor_position_in_bounds(bounds).is_some_and(|(local_x, local_y)| {
            draw_cursor_marker(
                &mut rgba,
                image_width,
                image_height,
                u32::try_from(
                    u64::from(local_x) * u64::from(image_width) / u64::from(bounds.width),
                )
                .unwrap_or(0),
                u32::try_from(
                    u64::from(local_y) * u64::from(image_height) / u64::from(bounds.height),
                )
                .unwrap_or(0),
            );
            true
        });

    let png = encode_png(image_width, image_height, &rgba)?;
    Ok((
        png,
        image_width,
        image_height,
        downscale_factor,
        cursor_marker,
    ))
}

pub(super) fn scaled_dimensions(
    source_width: u32,
    source_height: u32,
    max_width: Option<u32>,
) -> (u32, u32, u32) {
    let image_width = max_width.map_or(source_width, |width| width.min(source_width));
    let image_height = u32::try_from(
        (u64::from(source_height) * u64::from(image_width) + u64::from(source_width / 2))
            / u64::from(source_width),
    )
    .unwrap_or(source_height)
    .max(1);
    let legacy_downscale_factor = source_width.div_ceil(image_width);
    (image_width, image_height, legacy_downscale_factor)
}

pub(super) fn capture_comparison_frame(bounds: DisplayBounds) -> Result<Vec<u8>, PlatformError> {
    let rgba = capture_rgba(bounds)?;
    let factor = bounds.width.div_ceil(VISUAL_COMPARE_WIDTH).max(1);
    if factor == 1 {
        Ok(rgba)
    } else {
        Ok(resize_rgba_box(&rgba, bounds.width, bounds.height, factor))
    }
}

pub(super) fn frame_fingerprint(frame: &[u8]) -> String {
    let hash = frame.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64-rgba320:{hash:016x}")
}

pub(super) fn baseline_process_nonce() -> u128 {
    static NONCE: OnceLock<u128> = OnceLock::new();
    *NONCE.get_or_init(|| {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        timestamp ^ u128::from(std::process::id())
    })
}

pub(super) fn wait_for_change(
    display: DisplayInfo,
    bounds: DisplayBounds,
    baseline: &[u8],
    timeout_ms: u32,
    stable_ms: u32,
    difference_threshold: f64,
) -> Result<VisualChangeResult, PlatformError> {
    validate_wait_values(timeout_ms, stable_ms, difference_threshold)?;
    let started = Instant::now();
    let mut previous = baseline.to_owned();
    let mut difference = 0.0_f64;
    let mut changed = false;
    let mut settled = false;
    let mut stable_since: Option<Instant> = None;
    let deadline = started + Duration::from_millis(u64::from(timeout_ms));

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(VISUAL_POLL_MS)));
        let frame = capture_comparison_frame(bounds)?;
        difference = image_difference(baseline, &frame)?;
        if difference >= difference_threshold {
            changed = true;
        }
        if changed {
            let consecutive_difference = image_difference(&previous, &frame)?;
            let stable_threshold = (difference_threshold / 4.0).max(0.001);
            if consecutive_difference < stable_threshold {
                let since = *stable_since.get_or_insert_with(Instant::now);
                if Instant::now().saturating_duration_since(since)
                    >= Duration::from_millis(u64::from(stable_ms))
                {
                    settled = true;
                    break;
                }
            } else {
                stable_since = None;
            }
        }
        previous = frame;
    }

    let screenshot = capture_display_bounds(display, bounds, Some(MAX_ACTION_IMAGE_WIDTH), true)?;
    Ok(VisualChangeResult {
        changed,
        timed_out: !settled,
        difference,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        screenshot,
    })
}

pub(super) fn recognize_text(request: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
    ensure_dpi_awareness()?;
    let _apartment = ComApartment::initialize()?;
    let display = find_display(&request.display_id)?;
    let source_bounds = display_region_bounds(
        &display,
        request.x,
        request.y,
        request.width,
        request.height,
    )?;
    let max_dimension = OcrEngine::MaxImageDimension().map_err(ocr_error)?;
    if request.width > max_dimension || request.height > max_dimension {
        return Err(PlatformError::InvalidArgument {
            argument: "region".to_owned(),
            reason: format!(
                "Windows OCR supports at most {max_dimension} pixels per dimension; use a smaller region"
            ),
        });
    }
    let rgba = capture_rgba(source_bounds)?;
    let writer = DataWriter::new().map_err(ocr_error)?;
    writer.WriteBytes(&rgba).map_err(ocr_error)?;
    let buffer = writer.DetachBuffer().map_err(ocr_error)?;
    let width = i32::try_from(request.width).map_err(|_| PlatformError::InvalidArgument {
        argument: "width".to_owned(),
        reason: "exceeds the Windows OCR bitmap limit".to_owned(),
    })?;
    let height = i32::try_from(request.height).map_err(|_| PlatformError::InvalidArgument {
        argument: "height".to_owned(),
        reason: "exceeds the Windows OCR bitmap limit".to_owned(),
    })?;
    let bitmap =
        SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Rgba8, width, height)
            .map_err(ocr_error)?;
    let engine = if let Some(tag) = request.language.as_deref() {
        let language = Language::CreateLanguage(&HSTRING::from(tag)).map_err(ocr_error)?;
        if !OcrEngine::IsLanguageSupported(&language).map_err(ocr_error)? {
            return Err(PlatformError::Unavailable {
                reason: format!("OCR language '{tag}' is not installed"),
            });
        }
        OcrEngine::TryCreateFromLanguage(&language).map_err(ocr_error)?
    } else {
        OcrEngine::TryCreateFromUserProfileLanguages().map_err(ocr_error)?
    };
    let language = engine
        .RecognizerLanguage()
        .and_then(|language| language.LanguageTag())
        .map(|tag| tag.to_string())
        .map_err(ocr_error)?;
    let recognized = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.join())
        .map_err(ocr_error)?;
    let recognized_lines = recognized.Lines().map_err(ocr_error)?;
    let mut lines = Vec::with_capacity(recognized_lines.Size().map_err(ocr_error)? as usize);
    for index in 0..recognized_lines.Size().map_err(ocr_error)? {
        let line = recognized_lines.GetAt(index).map_err(ocr_error)?;
        let recognized_words = line.Words().map_err(ocr_error)?;
        let mut words = Vec::with_capacity(recognized_words.Size().map_err(ocr_error)? as usize);
        for word_index in 0..recognized_words.Size().map_err(ocr_error)? {
            let word = recognized_words.GetAt(word_index).map_err(ocr_error)?;
            let bounds = word.BoundingRect().map_err(ocr_error)?;
            let (x, y, width, height) = ocr_rect(bounds, request.x, request.y);
            words.push(OcrWord {
                text: word.Text().map_err(ocr_error)?.to_string(),
                x,
                y,
                width,
                height,
            });
        }
        let (x, y, width, height) = word_union(&words).unwrap_or((request.x, request.y, 0, 0));
        lines.push(OcrLine {
            text: line.Text().map_err(ocr_error)?.to_string(),
            x,
            y,
            width,
            height,
            words,
        });
    }
    Ok(OcrResult {
        display,
        source_bounds,
        language,
        text: recognized.Text().map_err(ocr_error)?.to_string(),
        lines,
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn ocr_rect(rect: windows::Foundation::Rect, offset_x: u32, offset_y: u32) -> (u32, u32, u32, u32) {
    let x = rect.X.max(0.0).floor() as u32;
    let y = rect.Y.max(0.0).floor() as u32;
    let right = (rect.X + rect.Width).max(rect.X).ceil() as u32;
    let bottom = (rect.Y + rect.Height).max(rect.Y).ceil() as u32;
    (
        offset_x.saturating_add(x),
        offset_y.saturating_add(y),
        right.saturating_sub(x),
        bottom.saturating_sub(y),
    )
}

pub(super) fn word_union(words: &[OcrWord]) -> Option<(u32, u32, u32, u32)> {
    let left = words.iter().map(|word| word.x).min()?;
    let top = words.iter().map(|word| word.y).min()?;
    let right = words
        .iter()
        .map(|word| word.x.saturating_add(word.width))
        .max()?;
    let bottom = words
        .iter()
        .map(|word| word.y.saturating_add(word.height))
        .max()?;
    Some((
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    ))
}

fn ocr_error(error: impl std::fmt::Display) -> PlatformError {
    PlatformError::Unavailable {
        reason: format!(
            "{error}; install a Windows OCR language pack if no recognizer is available"
        ),
    }
}

pub(super) fn image_difference(left: &[u8], right: &[u8]) -> Result<f64, PlatformError> {
    if left.len() != right.len() || left.is_empty() {
        return Err(PlatformError::OperationFailed {
            operation: "wait_for_visual_change".to_owned(),
            reason: "comparison frames had incompatible dimensions".to_owned(),
        });
    }
    let (total, channels) = left.chunks_exact(4).zip(right.chunks_exact(4)).fold(
        (0.0_f64, 0.0_f64),
        |(total, channels), (left, right)| {
            (
                total
                    + f64::from(left[0].abs_diff(right[0]))
                    + f64::from(left[1].abs_diff(right[1]))
                    + f64::from(left[2].abs_diff(right[2])),
                channels + 3.0,
            )
        },
    );
    Ok(total / (channels * 255.0))
}

pub(super) fn validate_visual_wait(
    request: &WaitForVisualChangeRequest,
) -> Result<(), PlatformError> {
    validate_wait_values(
        request.timeout_ms,
        request.stable_ms,
        request.difference_threshold,
    )
}

pub(super) fn validate_wait_values(
    timeout_ms: u32,
    stable_ms: u32,
    difference_threshold: f64,
) -> Result<(), PlatformError> {
    if timeout_ms == 0 || timeout_ms > MAX_WAIT_MS {
        return Err(PlatformError::InvalidArgument {
            argument: "timeout_ms".to_owned(),
            reason: format!("must be between 1 and {MAX_WAIT_MS}"),
        });
    }
    if stable_ms > timeout_ms {
        return Err(PlatformError::InvalidArgument {
            argument: "stable_ms".to_owned(),
            reason: "must not exceed timeout_ms".to_owned(),
        });
    }
    if !difference_threshold.is_finite()
        || difference_threshold <= 0.0
        || difference_threshold > 1.0
    {
        return Err(PlatformError::InvalidArgument {
            argument: "difference_threshold".to_owned(),
            reason: "must be greater than 0 and at most 1".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn resize_rgba_box(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    factor: u32,
) -> Vec<u8> {
    let target_width = source_width.div_ceil(factor);
    let target_height = source_height.div_ceil(factor);
    let target_length = usize::try_from(target_width)
        .ok()
        .and_then(|width| {
            usize::try_from(target_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(0);
    let mut target = vec![0_u8; target_length];
    for target_y in 0..target_height {
        for target_x in 0..target_width {
            let source_left = target_x * factor;
            let source_top = target_y * factor;
            let source_right = source_left.saturating_add(factor).min(source_width);
            let source_bottom = source_top.saturating_add(factor).min(source_height);
            let mut totals = [0_u64; 4];
            let mut count = 0_u64;
            for source_y in source_top..source_bottom {
                for source_x in source_left..source_right {
                    let source_index = usize::try_from(
                        (u64::from(source_y) * u64::from(source_width) + u64::from(source_x)) * 4,
                    )
                    .unwrap_or(0);
                    for channel in 0..4 {
                        totals[channel] += u64::from(source[source_index + channel]);
                    }
                    count += 1;
                }
            }
            let target_index = usize::try_from(
                (u64::from(target_y) * u64::from(target_width) + u64::from(target_x)) * 4,
            )
            .unwrap_or(0);
            for channel in 0..4 {
                target[target_index + channel] = u8::try_from(totals[channel] / count).unwrap_or(0);
            }
        }
    }
    target
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn resize_rgba_bilinear(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let target_length = usize::try_from(target_width)
        .ok()
        .and_then(|width| {
            usize::try_from(target_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(0);
    let mut target = vec![0_u8; target_length];
    for target_y in 0..target_height {
        let source_y = if target_height == 1 {
            0.0
        } else {
            f64::from(target_y) * f64::from(source_height.saturating_sub(1))
                / f64::from(target_height - 1)
        };
        let top = source_y.floor() as u32;
        let bottom = top.saturating_add(1).min(source_height - 1);
        let y_weight = source_y - f64::from(top);
        for target_x in 0..target_width {
            let source_x = if target_width == 1 {
                0.0
            } else {
                f64::from(target_x) * f64::from(source_width.saturating_sub(1))
                    / f64::from(target_width - 1)
            };
            let left = source_x.floor() as u32;
            let right = left.saturating_add(1).min(source_width - 1);
            let x_weight = source_x - f64::from(left);
            let target_index = usize::try_from(
                (u64::from(target_y) * u64::from(target_width) + u64::from(target_x)) * 4,
            )
            .unwrap_or(0);
            for channel in 0..4_u64 {
                let pixel = |x: u32, y: u32| {
                    let index = usize::try_from(
                        (u64::from(y) * u64::from(source_width) + u64::from(x)) * 4 + channel,
                    )
                    .unwrap_or(0);
                    f64::from(source[index])
                };
                let top_value = pixel(left, top) * (1.0 - x_weight) + pixel(right, top) * x_weight;
                let bottom_value =
                    pixel(left, bottom) * (1.0 - x_weight) + pixel(right, bottom) * x_weight;
                target[target_index + usize::try_from(channel).unwrap_or(0)] =
                    (top_value * (1.0 - y_weight) + bottom_value * y_weight).round() as u8;
            }
        }
    }
    target
}

fn cursor_position_in_bounds(bounds: DisplayBounds) -> Option<(u32, u32)> {
    let cursor = current_cursor_position().ok()?;
    let local_x = cursor.x.checked_sub(bounds.left).and_then(|value| {
        u32::try_from(value)
            .ok()
            .filter(|coordinate| *coordinate < bounds.width)
    })?;
    let local_y = cursor.y.checked_sub(bounds.top).and_then(|value| {
        u32::try_from(value)
            .ok()
            .filter(|coordinate| *coordinate < bounds.height)
    })?;
    Some((local_x, local_y))
}

fn draw_cursor_marker(rgba: &mut [u8], width: u32, height: u32, x: u32, y: u32) {
    const RADIUS: i32 = 10;
    for offset in -RADIUS..=RADIUS {
        set_marker_pixel(
            rgba,
            width,
            height,
            i64::from(x) + i64::from(offset),
            i64::from(y),
        );
        set_marker_pixel(
            rgba,
            width,
            height,
            i64::from(x),
            i64::from(y) + i64::from(offset),
        );
    }
}

fn set_marker_pixel(rgba: &mut [u8], width: u32, height: u32, x: i64, y: i64) {
    if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
        return;
    }
    let index = usize::try_from((y * i64::from(width) + x) * 4).unwrap_or(0);
    rgba[index..index + 4].copy_from_slice(&[255, 0, 255, 255]);
}

pub(super) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PlatformError> {
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| PlatformError::OperationFailed {
            operation: "encode_png".to_owned(),
            reason: error.to_string(),
        })?;
    writer
        .write_image_data(rgba)
        .map_err(|error| PlatformError::OperationFailed {
            operation: "encode_png".to_owned(),
            reason: error.to_string(),
        })?;
    writer
        .finish()
        .map_err(|error| PlatformError::OperationFailed {
            operation: "encode_png".to_owned(),
            reason: error.to_string(),
        })?;
    Ok(output)
}
