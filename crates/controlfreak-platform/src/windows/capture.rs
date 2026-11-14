#![allow(unsafe_code)]

use controlfreak_core::timing;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
    UI::WindowsAndMessaging::{GetWindowRect, IsHungAppWindow},
};

use super::{
    ActionObservation, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, BitmapPixelFormat, CAPTUREBLT,
    CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DataWriter, DeleteDC, DeleteObject,
    DisplayBounds, DisplayInfo, DisplayScreenshot, Duration, GetDC, GetDIBits, HBITMAP, HDC,
    HGDIOBJ, HSTRING, Instant, Language, MAX_ACTION_IMAGE_WIDTH, MAX_CAPTURE_BYTES,
    MAX_CAPTURE_WIDTH, MAX_WAIT_MS, ObservationMode, ObservationOptions, OcrEngine, OcrLine,
    OcrRegionRequest, OcrResult, OcrWord, OnceLock, PlatformError, ReleaseDC, SRCCOPY,
    SelectObject, SoftwareBitmap, SystemTime, UNIX_EPOCH, VISUAL_COMPARE_WIDTH, VISUAL_POLL_MS,
    VisualChangeResult, WaitForVisualChangeRequest, WindowBounds, WindowInfo, c_void,
    current_cursor_position, ensure_dpi_awareness, enumerate_displays, find_display,
    foreground_window_handle, initialize_com, last_win32_error, size_of, win32_error, window_info,
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
    let target_before = super::target::observe();
    let (png, image_width, image_height, downscale_factor, cursor_marker) =
        capture_bounds(source_bounds, max_width, include_cursor_marker)?;
    Ok(DisplayScreenshot {
        target_ref: super::target::finish_observation(target_before),
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
    let _timer = timing::start("observe");
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
    let _timer = timing::start("observe");
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

fn foreground_window_info() -> Option<WindowInfo> {
    let hwnd = foreground_window_handle();
    (!hwnd.is_invalid())
        .then(|| window_info(hwnd).ok())
        .flatten()
}

/// Bitmap dimensions and byte count of a 32-bit capture, refused beyond Win32 and safety limits.
fn capture_geometry(width: u32, height: u32) -> Result<(i32, i32, usize), PlatformError> {
    let signed_width = i32::try_from(width).map_err(|_| PlatformError::OperationFailed {
        operation: "capture_display".to_owned(),
        reason: "display width exceeds the Win32 bitmap limit".to_owned(),
    })?;
    let signed_height = i32::try_from(height).map_err(|_| PlatformError::OperationFailed {
        operation: "capture_display".to_owned(),
        reason: "display height exceeds the Win32 bitmap limit".to_owned(),
    })?;
    let byte_count = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_CAPTURE_BYTES)
        .ok_or_else(|| PlatformError::OperationFailed {
            operation: "capture_display".to_owned(),
            reason: format!("capture buffer exceeds the {MAX_CAPTURE_BYTES}-byte safety limit"),
        })?;
    Ok((signed_width, signed_height, byte_count))
}

/// Reads a bitmap that is not selected into any DC as top-down 32-bit BGRA into `buffer`.
fn read_bitmap_bgra(
    memory_dc: &MemoryDc,
    bitmap: &Bitmap,
    width: i32,
    height: i32,
    buffer: &mut [u8],
) -> Result<(), PlatformError> {
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
    debug_assert_eq!(
        buffer.len(),
        usize::try_from(width).unwrap_or(0) * usize::try_from(height).unwrap_or(0) * 4
    );
    // SAFETY: `buffer` has exactly the size of the requested 32-bit image (checked by
    // `capture_geometry` for the callers), BITMAPINFO is initialized for that layout, the bitmap
    // is not selected into a DC, and all GDI handles remain live through the call.
    let copied_lines = unsafe {
        GetDIBits(
            memory_dc.0,
            bitmap.0,
            0,
            u32::try_from(height).unwrap_or(0),
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            &raw mut bitmap_info,
            DIB_RGB_COLORS,
        )
    };
    if copied_lines == height {
        Ok(())
    } else {
        Err(PlatformError::OperationFailed {
            operation: "GetDIBits".to_owned(),
            reason: format!("copied {copied_lines} of {height} scan lines"),
        })
    }
}

/// Reads one screen rectangle repeatedly. The device contexts, the bitmap and the pixel buffer
/// are created once, so each further read is one `BitBlt` and one `GetDIBits` with no allocation.
struct FrameGrabber {
    screen_dc: ScreenDc,
    memory_dc: MemoryDc,
    bitmap: Bitmap,
    bounds: DisplayBounds,
    width: i32,
    height: i32,
    /// The latest read: top-down BGRA, the byte after each pixel's red byte unspecified.
    buffer: Vec<u8>,
}

impl FrameGrabber {
    fn new(bounds: DisplayBounds) -> Result<Self, PlatformError> {
        let (width, height, byte_count) = capture_geometry(bounds.width, bounds.height)?;
        let screen_dc = ScreenDc::acquire()?;
        let memory_dc = MemoryDc::create(screen_dc.0)?;
        let bitmap = Bitmap::create(screen_dc.0, width, height)?;
        Ok(Self {
            screen_dc,
            memory_dc,
            bitmap,
            bounds,
            width,
            height,
            buffer: vec![0_u8; byte_count],
        })
    }

    /// Replaces `self.buffer` with the rectangle's current pixels.
    fn grab(&mut self) -> Result<(), PlatformError> {
        {
            // GetDIBits needs the bitmap unselected, so it is selected only around the copy.
            let _selection = SelectedObject::select(self.memory_dc.0, self.bitmap.0.into())?;
            // SAFETY: Both DCs and the selected bitmap are live, coordinates and dimensions were
            // validated when the grabber was created, and BitBlt retains no Rust references.
            unsafe {
                BitBlt(
                    self.memory_dc.0,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.screen_dc.0),
                    self.bounds.left,
                    self.bounds.top,
                    SRCCOPY | CAPTUREBLT,
                )
            }
            .map_err(|error| win32_error("BitBlt", &error))?;
        }
        read_bitmap_bgra(
            &self.memory_dc,
            &self.bitmap,
            self.width,
            self.height,
            &mut self.buffer,
        )
    }
}

/// Converts a BGRA frame to opaque RGBA in place.
fn bgra_to_rgba(frame: &mut [u8]) {
    for pixel in frame.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = u8::MAX;
    }
}

fn capture_rgba(bounds: DisplayBounds) -> Result<Vec<u8>, PlatformError> {
    let mut grabber = FrameGrabber::new(bounds)?;
    grabber.grab()?;
    let mut frame = std::mem::take(&mut grabber.buffer);
    bgra_to_rgba(&mut frame);
    Ok(frame)
}

/// How a window screenshot was produced; reported to clients as `capture_method`.
pub(super) const CAPTURE_METHOD_PRINT_WINDOW: &str = "windows_print_window";
pub(super) const CAPTURE_METHOD_SCREEN: &str = "windows_gdi";

/// `PW_RENDERFULLCONTENT`: ask the window (and DWM) to render all of its content, including
/// layered and hardware-accelerated child content, even when it is covered by other windows.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

/// Copies the part of a window-sized BGRA frame that starts at (`left`, `top`) and is
/// `width` x `height` as opaque RGBA. `None` if that part is not inside the frame.
fn crop_bgra_to_rgba(
    frame: &[u8],
    frame_width: u32,
    frame_height: u32,
    left: i64,
    top: i64,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let right = left.checked_add(i64::from(width))?;
    let bottom = top.checked_add(i64::from(height))?;
    if left < 0
        || top < 0
        || right > i64::from(frame_width)
        || bottom > i64::from(frame_height)
        || frame.len()
            != usize::try_from(u64::from(frame_width) * u64::from(frame_height) * 4).ok()?
    {
        return None;
    }
    let stride = usize::try_from(frame_width).ok()? * 4;
    let row_bytes = usize::try_from(width).ok()? * 4;
    let mut rgba = Vec::with_capacity(row_bytes * usize::try_from(height).ok()?);
    for row in 0..usize::try_from(height).ok()? {
        let start = (usize::try_from(top).ok()? + row) * stride + usize::try_from(left).ok()? * 4;
        rgba.extend_from_slice(&frame[start..start + row_bytes]);
    }
    bgra_to_rgba(&mut rgba);
    Some(rgba)
}

/// A window that failed to render itself comes back fully black; real windows are not.
fn is_blank_frame(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4)
        .all(|pixel| pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0)
}

/// Renders the window itself into a bitmap, so what covers it on screen is irrelevant, and
/// returns the part inside `visible` (the frame bounds that coordinates are reported in) as
/// RGBA. `None` when the window cannot render itself, returns nothing, or its pixels do not line
/// up with `visible`; the caller then copies the screen instead.
fn print_window_rgba(hwnd: HWND, visible: DisplayBounds) -> Option<Vec<u8>> {
    let mut rect = RECT::default();
    // SAFETY: rect is writable for the synchronous call, which retains no references.
    unsafe { GetWindowRect(hwnd, &raw mut rect) }.ok()?;
    let full_width = u32::try_from(rect.right.checked_sub(rect.left)?).ok()?;
    let full_height = u32::try_from(rect.bottom.checked_sub(rect.top)?).ok()?;
    if full_width == 0 || full_height == 0 {
        return None;
    }
    let (width, height, byte_count) = capture_geometry(full_width, full_height).ok()?;
    let screen_dc = ScreenDc::acquire().ok()?;
    let memory_dc = MemoryDc::create(screen_dc.0).ok()?;
    let bitmap = Bitmap::create(screen_dc.0, width, height).ok()?;
    {
        let _selection = SelectedObject::select(memory_dc.0, bitmap.0.into()).ok()?;
        // SAFETY: hwnd is a window handle checked by the caller, the memory DC has the bitmap
        // selected, and PrintWindow retains no references after returning.
        if !unsafe { PrintWindow(hwnd, memory_dc.0, PW_RENDERFULLCONTENT) }.as_bool() {
            return None;
        }
    }
    let mut frame = vec![0_u8; byte_count];
    read_bitmap_bgra(&memory_dc, &bitmap, width, height, &mut frame).ok()?;
    let rgba = crop_bgra_to_rgba(
        &frame,
        full_width,
        full_height,
        i64::from(visible.left) - i64::from(rect.left),
        i64::from(visible.top) - i64::from(rect.top),
        visible.width,
        visible.height,
    )?;
    (!is_blank_frame(&rgba)).then_some(rgba)
}

/// How long a window may take to render itself. `PrintWindow` waits for the target application
/// to handle `WM_PRINT`, and an application that stops pumping messages would hold the call
/// forever. A window that is flagged as hung is not asked at all; for any other, the render runs
/// on its own thread and the caller waits at most this long before copying the screen instead.
pub(super) const PRINT_WINDOW_DEADLINE: Duration = Duration::from_secs(2);

/// Set while a render thread has not returned yet. At most one render exists at a time: another
/// window capture waits for the slot, within the same deadline as its own render, and copies the
/// screen only if the slot does not come free in time. A window that never answers therefore
/// costs one parked thread and its GDI objects rather than one per call, and two captures in
/// parallel both get their window's own rendering.
/// How often a capture that is waiting for the render slot looks again.
const RENDER_SLOT_POLL: Duration = Duration::from_millis(5);

static RENDER_OUTSTANDING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Marks the single outstanding render finished when dropped, on success, failure or unwinding.
struct OutstandingRender(&'static std::sync::atomic::AtomicBool);

impl Drop for OutstandingRender {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Runs `render` on a worker thread and waits at most `deadline` for its answer. `None` means
/// the render failed, or the slot and the render together took too long, because `outstanding`
/// says an earlier render still has not returned. The worker owns everything it creates and clears `outstanding`
/// only when it really finishes, so an abandoned render cleans up after itself whenever the
/// application finally answers.
fn render_within<T: Send + 'static>(
    deadline: Duration,
    outstanding: &'static std::sync::atomic::AtomicBool,
    render: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    use std::sync::atomic::Ordering;
    let started = Instant::now();
    while outstanding.swap(true, Ordering::AcqRel) {
        if started.elapsed() >= deadline {
            return None;
        }
        std::thread::sleep(RENDER_SLOT_POLL);
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("controlfreak-print-window".to_owned())
        .spawn(move || {
            // Clears the flag even if `render` panics, so one failed render never turns every
            // later window capture into a screen copy.
            let slot = OutstandingRender(outstanding);
            let rendered = render();
            drop(slot);
            // The receiver is gone after a timeout; the result is then dropped here.
            let _ = sender.send(rendered);
        });
    if spawned.is_err() {
        outstanding.store(false, Ordering::Release);
        return None;
    }
    receiver
        .recv_timeout(deadline.saturating_sub(started.elapsed()))
        .ok()
        .flatten()
}

/// `print_window_rgba` with the bounds described on [`PRINT_WINDOW_DEADLINE`].
fn print_window_bounded(hwnd: HWND, visible: DisplayBounds) -> Option<Vec<u8>> {
    // SAFETY: IsHungAppWindow only reads the window's responsiveness state for a handle value.
    if unsafe { IsHungAppWindow(hwnd) }.as_bool() {
        return None;
    }
    // A raw handle value crosses the thread boundary; the worker re-validates it through the
    // window APIs, which fail for a handle that no longer exists.
    let raw = hwnd.0 as isize;
    render_within(PRINT_WINDOW_DEADLINE, &RENDER_OUTSTANDING, move || {
        print_window_rgba(HWND(raw as *mut c_void), visible)
    })
}

/// Picks the pixels for a window screenshot: the window's own rendering if there is one, else a
/// copy of its screen rectangle.
fn window_pixels(
    printed: Option<Vec<u8>>,
    copy_screen: impl FnOnce() -> Result<Vec<u8>, PlatformError>,
) -> Result<(Vec<u8>, &'static str), PlatformError> {
    match printed {
        Some(rgba) => Ok((rgba, CAPTURE_METHOD_PRINT_WINDOW)),
        None => copy_screen().map(|rgba| (rgba, CAPTURE_METHOD_SCREEN)),
    }
}

/// A window screenshot ready to return.
pub(super) struct WindowCapture {
    pub(super) png: Vec<u8>,
    pub(super) image_width: u32,
    pub(super) image_height: u32,
    pub(super) downscale_factor: u32,
    pub(super) cursor_marker: bool,
    pub(super) capture_method: &'static str,
}

/// Captures `hwnd` by rendering the window, so an occluded window shows its own content rather
/// than whatever covers it, and falls back to a screen copy of `bounds` if it cannot. Either way
/// the image covers exactly `bounds`, so mapping image points to screen points is unchanged.
pub(super) fn capture_window_bounds(
    hwnd: HWND,
    bounds: DisplayBounds,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<WindowCapture, PlatformError> {
    let printed = timing::time("capture_print_window", || {
        print_window_bounded(hwnd, bounds)
    });
    let (rgba, capture_method) = window_pixels(printed, || {
        timing::time("capture_gdi", || capture_rgba(bounds))
    })?;
    let (png, image_width, image_height, downscale_factor, cursor_marker) =
        finish_image(rgba, bounds, max_width, include_cursor_marker)?;
    Ok(WindowCapture {
        png,
        image_width,
        image_height,
        downscale_factor,
        cursor_marker,
        capture_method,
    })
}

pub(super) fn capture_bounds(
    bounds: DisplayBounds,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<(Vec<u8>, u32, u32, u32, bool), PlatformError> {
    let rgba = timing::time("capture_gdi", || capture_rgba(bounds))?;
    finish_image(rgba, bounds, max_width, include_cursor_marker)
}

/// Resizes, marks the cursor on and PNG-encodes a captured frame of `bounds`.
fn finish_image(
    bgra: Vec<u8>,
    bounds: DisplayBounds,
    max_width: Option<u32>,
    include_cursor_marker: bool,
) -> Result<(Vec<u8>, u32, u32, u32, bool), PlatformError> {
    let (image_width, image_height, downscale_factor) =
        scaled_dimensions(bounds.width, bounds.height, max_width);
    let mut rgba = if image_width == bounds.width && image_height == bounds.height {
        bgra
    } else {
        timing::time("resize", || {
            resize_rgba_area(
                &bgra,
                bounds.width,
                bounds.height,
                image_width,
                image_height,
            )
        })
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

    let png = timing::time("encode_png", || {
        encode_png(image_width, image_height, &rgba)
    })?;
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

/// Changed pixels needed before a repaint counts as the application's response. A blinking
/// caret changes fewer pixels than this; one typed glyph or a pressed button changes more.
const SETTLE_MIN_CHANGED_PIXELS: usize = 48;
/// Largest window area watched for settling; bigger windows fall back to the fixed wait because
/// each read of them costs about as much as the wait itself.
const SETTLE_MAX_PIXELS: u64 = 2_560 * 1_600;
const SETTLE_POLL: Duration = Duration::from_millis(4);
/// How long the window must stay identical to count as settled. Applications often repaint in
/// phases, for example a busy state and then the result; a short window returns between phases
/// and the evidence shows the intermediate state. Live tests saw a second phase 65 ms after the
/// first, so the window covers gaps up to 60 ms while still returning well under the 150 ms cap.
const SETTLE_STABLE: Duration = Duration::from_millis(60);

/// Watches the approved window across an input action, so the action returns as soon as the
/// window has visibly responded and stopped changing, instead of always sleeping the full cap.
pub(super) struct SettleWatch {
    /// Reads the window with reused GDI objects and one pixel buffer.
    grabber: FrameGrabber,
    /// The window before input, as BGRA.
    before: Vec<u8>,
}

/// How a settle wait ended; recorded as a timing stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettleOutcome {
    /// The window changed and then held still for one read.
    Settled,
    /// The cap elapsed first; the result is the same as the previous fixed wait.
    Capped,
}

impl SettleWatch {
    /// Reads the window before input. Returns `None` when it is unknown, too large, or unreadable,
    /// in which case the caller keeps the fixed wait.
    pub(super) fn begin(bounds: Option<DisplayBounds>) -> Option<Self> {
        let bounds = bounds?;
        if bounds.width == 0
            || bounds.height == 0
            || u64::from(bounds.width) * u64::from(bounds.height) > SETTLE_MAX_PIXELS
        {
            return None;
        }
        let mut grabber = FrameGrabber::new(bounds).ok()?;
        timing::time("settle_capture", || grabber.grab()).ok()?;
        let before = grabber.buffer.clone();
        Some(Self { grabber, before })
    }

    /// Waits until the window changed and then stayed identical for at least `SETTLE_STABLE`,
    /// or until `cap`. Frames are compared as the BGRA the screen read produces, with no
    /// conversion or allocation per read.
    pub(super) fn wait(mut self, cap: Duration) -> SettleOutcome {
        let started = Instant::now();
        // The latest frame that differed from its predecessor, and when it was read. Swapping
        // it with the grabber's buffer keeps both buffers at full size without copying.
        let mut held = vec![0_u8; self.before.len()];
        let mut changed_at: Option<Instant> = None;
        loop {
            let remaining = cap.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return SettleOutcome::Capped;
            }
            std::thread::sleep(SETTLE_POLL.min(remaining));
            let read_at = Instant::now();
            if timing::time("settle_capture", || self.grabber.grab()).is_err() {
                std::thread::sleep(cap.saturating_sub(started.elapsed()));
                return SettleOutcome::Capped;
            }
            let frame = &self.grabber.buffer;
            match changed_at {
                Some(since) if changed_pixels(&held, frame, 1) == 0 => {
                    if read_at.saturating_duration_since(since) >= SETTLE_STABLE {
                        return SettleOutcome::Settled;
                    }
                }
                Some(_) => {
                    std::mem::swap(&mut held, &mut self.grabber.buffer);
                    changed_at = Some(read_at);
                }
                None if changed_pixels(&self.before, frame, SETTLE_MIN_CHANGED_PIXELS)
                    >= SETTLE_MIN_CHANGED_PIXELS =>
                {
                    std::mem::swap(&mut held, &mut self.grabber.buffer);
                    changed_at = Some(read_at);
                }
                None => {}
            }
        }
    }
}

/// Counts pixels whose three colour bytes differ between two equally sized 4-byte-per-pixel
/// frames, stopping at `limit`. The fourth byte is ignored: a screen read leaves it unspecified,
/// so BGRA frames and RGBA frames with a constant alpha both compare correctly.
fn changed_pixels(left: &[u8], right: &[u8], limit: usize) -> usize {
    if left.len() != right.len() {
        return limit;
    }
    left.chunks_exact(4)
        .zip(right.chunks_exact(4))
        .filter(|(left, right)| left[..3] != right[..3])
        .take(limit)
        .count()
}

/// Reads a screen region and fingerprints its pixels, reusing the GDI objects and pixel buffer
/// for every read. `wait_for_text` compares this with the fingerprint of the exact frame that
/// recognition read (`recognize_text_fingerprinted`), so a result is reused only for pixels it was
/// really computed from.
pub(super) struct RegionChange {
    grabber: FrameGrabber,
}

impl RegionChange {
    pub(super) fn new(bounds: DisplayBounds) -> Result<Self, PlatformError> {
        Ok(Self {
            grabber: FrameGrabber::new(bounds)?,
        })
    }

    pub(super) fn fingerprint(&mut self) -> Result<u64, PlatformError> {
        self.grabber.grab()?;
        Ok(pixel_fingerprint(&self.grabber.buffer, false))
    }
}

/// Hashes the three colour bytes of every pixel and ignores the fourth, which a screen read
/// leaves unspecified. `red_first` selects RGBA input; otherwise the frame is BGRA, so both
/// layouts of the same pixels give the same value.
pub(super) fn pixel_fingerprint(frame: &[u8], red_first: bool) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for pixel in frame.chunks_exact(4) {
        let (blue, red) = if red_first {
            (pixel[2], pixel[0])
        } else {
            (pixel[0], pixel[2])
        };
        let colour = u64::from(blue) | u64::from(pixel[1]) << 8 | u64::from(red) << 16;
        hash = (hash ^ colour).wrapping_mul(PRIME);
    }
    hash ^ (frame.len() as u64)
}

pub(super) fn capture_comparison_frame(bounds: DisplayBounds) -> Result<Vec<u8>, PlatformError> {
    let rgba = timing::time("capture_gdi", || capture_rgba(bounds))?;
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
    control: &controlfreak_core::MutationControl,
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
        control.wait(
            "wait_for_change",
            remaining.min(Duration::from_millis(VISUAL_POLL_MS)),
        )?;
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

    control.check("wait_for_change")?;
    let screenshot = capture_display_bounds(display, bounds, Some(MAX_ACTION_IMAGE_WIDTH), true)?;
    Ok(VisualChangeResult {
        changed,
        timed_out: !settled,
        difference,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        screenshot,
    })
}

/// Recognizes text and also returns the fingerprint of the exact frame that was recognized.
pub(super) fn recognize_text_fingerprinted(
    request: &OcrRegionRequest,
    control: &controlfreak_core::MutationControl,
) -> Result<(OcrResult, u64), PlatformError> {
    control.check("windows_ocr")?;
    let _timer = timing::start("ocr_recognize");
    ensure_dpi_awareness()?;
    let _apartment = initialize_com()?;
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
    let rgba = timing::time("capture_gdi", || capture_rgba(source_bounds))?;
    let fingerprint = pixel_fingerprint(&rgba, true);
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
    let engine = timing::time("ocr_engine_create", || {
        ocr_engine(request.language.as_deref())
    })?;
    let language = engine
        .RecognizerLanguage()
        .and_then(|language| language.LanguageTag())
        .map(|tag| tag.to_string())
        .map_err(ocr_error)?;
    control.check("windows_ocr")?;
    let operation = engine.RecognizeAsync(&bitmap).map_err(ocr_error)?;
    // WinRT AsyncStatus::Started is zero. Request cancellation once, then drain
    // the operation; a provider that ignores cancellation remains visibly draining.
    while operation.Status().map_err(ocr_error)?.0 == 0 {
        if control.is_cancelled() {
            let _ = operation.Cancel();
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let recognized = operation.join().map_err(ocr_error)?;
    control.check("windows_ocr")?;
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
    Ok((
        OcrResult {
            target_ref: None,
            display,
            source_bounds,
            language,
            text: recognized.Text().map_err(ocr_error)?.to_string(),
            lines,
        },
        fingerprint,
    ))
}

fn ocr_engine(language: Option<&str>) -> Result<OcrEngine, PlatformError> {
    let Some(tag) = language else {
        return OcrEngine::TryCreateFromUserProfileLanguages().map_err(ocr_error);
    };
    let language = Language::CreateLanguage(&HSTRING::from(tag)).map_err(ocr_error)?;
    if !OcrEngine::IsLanguageSupported(&language).map_err(ocr_error)? {
        return Err(PlatformError::Unavailable {
            reason: format!("OCR language '{tag}' is not installed"),
        });
    }
    OcrEngine::TryCreateFromLanguage(&language).map_err(ocr_error)
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

/// Fixed-point scale for resampling weights; one source pixel's full coverage.
const AREA_WEIGHT_BITS: u32 = 14;
const AREA_WEIGHT_ONE: u32 = 1 << AREA_WEIGHT_BITS;

/// Coverage of each output pixel along one axis: `spans[i]` is the first source pixel and the
/// range of `weights` for output pixel `i`. Each output pixel's weights sum to `AREA_WEIGHT_ONE`.
struct AreaWeights {
    spans: Vec<(usize, std::ops::Range<usize>)>,
    weights: Vec<u32>,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn area_weights(source: u32, target: u32) -> AreaWeights {
    let scale = f64::from(source) / f64::from(target);
    let mut spans = Vec::with_capacity(target as usize);
    let mut weights = Vec::with_capacity(target as usize * (scale.ceil() as usize + 1));
    for index in 0..target {
        let start = f64::from(index) * scale;
        let end = (f64::from(index + 1) * scale).min(f64::from(source));
        let first = (start.floor() as u32).min(source - 1);
        let last = (end.ceil() as u32).max(first + 1).min(source);
        let span = (end - start).max(f64::MIN_POSITIVE);
        let begin = weights.len();
        weights.extend((first..last).map(|pixel| {
            let overlap = (f64::from(pixel + 1).min(end) - f64::from(pixel).max(start)).max(0.0);
            (overlap / span * f64::from(AREA_WEIGHT_ONE)).round() as u32
        }));
        // Rounding can leave the total slightly off; correct it on the largest weight.
        let output = &mut weights[begin..];
        let total: u32 = output.iter().sum();
        if let Some(largest) = output.iter_mut().max() {
            *largest = (*largest + AREA_WEIGHT_ONE).saturating_sub(total);
        }
        spans.push((first as usize, begin..weights.len()));
    }
    AreaWeights { spans, weights }
}

/// Downscales RGBA by averaging the exact source area under each output pixel. Thin strokes such
/// as text survive large reductions, unlike point or bilinear sampling of a few source pixels.
fn resize_rgba_area(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    // After the vertical pass a value is at most 255 << 14; dropping 6 bits keeps the horizontal
    // products below 2^32 while retaining 8 fractional bits.
    const VERTICAL_SHIFT: u32 = 6;
    let columns = area_weights(source_width, target_width);
    let rows = area_weights(source_height, target_height);
    let source_stride = source_width as usize * 4;
    let mut target = vec![0_u8; target_width as usize * target_height as usize * 4];
    let mut vertical = vec![0_u32; source_stride];
    let horizontal_bits = AREA_WEIGHT_BITS * 2 - VERTICAL_SHIFT;
    let rounding = 1_u32 << (horizontal_bits - 1);
    for (target_row, (first_row, row_range)) in target
        .chunks_exact_mut(target_width as usize * 4)
        .zip(&rows.spans)
    {
        vertical.fill(0);
        for (offset, weight) in rows.weights[row_range.clone()].iter().enumerate() {
            let start = (first_row + offset) * source_stride;
            for (sum, value) in vertical
                .iter_mut()
                .zip(&source[start..start + source_stride])
            {
                *sum += u32::from(*value) * weight;
            }
        }
        for sum in &mut vertical {
            *sum >>= VERTICAL_SHIFT;
        }
        for (pixel, (first_column, column_range)) in
            target_row.chunks_exact_mut(4).zip(&columns.spans)
        {
            let mut sums = [rounding; 4];
            let taps = &vertical[first_column * 4..];
            for (tap, weight) in taps
                .chunks_exact(4)
                .zip(&columns.weights[column_range.clone()])
            {
                for channel in 0..4 {
                    sums[channel] += tap[channel] * weight;
                }
            }
            for channel in 0..4 {
                pixel[channel] = u8::try_from(sums[channel] >> horizontal_bits).unwrap_or(u8::MAX);
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

/// Encodes opaque RGBA pixels as an RGB PNG; dropping the constant alpha channel makes the image
/// smaller and faster to encode without changing what the client sees. Rows are converted one at
/// a time as they are written, so no second full-frame buffer is built.
pub(super) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PlatformError> {
    use std::io::Write as _;
    let failed = |reason: String| PlatformError::OperationFailed {
        operation: "encode_png".to_owned(),
        reason,
    };
    let row_pixels = usize::try_from(width).map_err(|error| failed(error.to_string()))?;
    if rgba.len()
        != row_pixels
            .saturating_mul(4)
            .saturating_mul(usize::try_from(height).map_err(|error| failed(error.to_string()))?)
    {
        return Err(failed(
            "pixel data does not match the image dimensions".to_owned(),
        ));
    }
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // On real 1280-pixel screenshots, zlib level 2 encodes about four times faster than the
    // default level for roughly 8% more bytes. Images do not cost more model tokens by size.
    encoder.set_deflate_compression(png::DeflateCompression::Level(2));
    let mut writer = encoder
        .write_header()
        .map_err(|error| failed(error.to_string()))?;
    {
        let mut stream = writer
            .stream_writer()
            .map_err(|error| failed(error.to_string()))?;
        let mut row = Vec::with_capacity(row_pixels * 3);
        if row_pixels > 0 {
            for source in rgba.chunks_exact(row_pixels * 4) {
                row.clear();
                row.extend(
                    source
                        .chunks_exact(4)
                        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]),
                );
                stream
                    .write_all(&row)
                    .map_err(|error| failed(error.to_string()))?;
            }
        }
        stream.finish().map_err(|error| failed(error.to_string()))?;
    }
    writer.finish().map_err(|error| failed(error.to_string()))?;
    Ok(output)
}

#[cfg(test)]
mod benchmarks {
    use super::*;

    /// A desktop-like synthetic frame: flat panels, borders, and dense text-like strokes.
    fn synthetic_frame(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let panel: u8 = if x < width / 5 { 32 } else { 245 };
                let text = (y % 24 < 12) && (x % 9 < 5) && ((x / 9 + y / 24) % 7 != 0);
                let value = if x % 400 == 0 || y % 300 == 0 {
                    128
                } else if text && panel > 128 {
                    24
                } else {
                    panel
                };
                rgba.extend_from_slice(&[value, value, value.saturating_add(8), 255]);
            }
        }
        rgba
    }

    /// The pre-RGB encoder, kept only so the benchmark can show what dropping alpha saves.
    fn encode_png_rgba_reference(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_deflate_compression(png::DeflateCompression::Level(2));
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
        writer.finish().unwrap();
        output
    }

    fn median_ms(mut samples: Vec<f64>) -> f64 {
        samples.sort_by(f64::total_cmp);
        samples[samples.len() / 2]
    }

    fn measure(runs: usize, mut operation: impl FnMut()) -> f64 {
        median_ms(
            (0..runs)
                .map(|_| {
                    let started = Instant::now();
                    operation();
                    started.elapsed().as_secs_f64() * 1_000.0
                })
                .collect(),
        )
    }

    /// Run with `cargo test -p controlfreak-platform --release --lib benchmarks -- --ignored --nocapture`.
    #[test]
    #[ignore = "synthetic latency benchmark; run explicitly"]
    fn synthetic_capture_pipeline_latency() {
        for (width, height) in [(1920_u32, 1080_u32), (2560, 1440), (3840, 2160)] {
            let frame = synthetic_frame(width, height);
            let (image_width, image_height, _) = scaled_dimensions(width, height, Some(1280));
            let mut resized = Vec::new();
            let resize = measure(9, || {
                resized = resize_rgba_area(&frame, width, height, image_width, image_height);
            });
            let encode_full = measure(5, || {
                encode_png(width, height, &frame).unwrap();
            });
            let png_bytes = encode_png(width, height, &frame).unwrap().len();
            let encode_full_rgba = measure(5, || {
                encode_png_rgba_reference(width, height, &frame);
            });
            let png_bytes_rgba = encode_png_rgba_reference(width, height, &frame).len();
            let encode_action = measure(9, || {
                encode_png(image_width, image_height, &resized).unwrap();
            });
            let factor = width.div_ceil(VISUAL_COMPARE_WIDTH).max(1);
            let mut compare = Vec::new();
            let downscale_compare = measure(9, || {
                compare = resize_rgba_box(&frame, width, height, factor);
            });
            let difference = measure(9, || {
                image_difference(&compare, &compare).unwrap();
            });
            eprintln!(
                "{width}x{height}: resize_to_1280={resize:.1}ms encode_png_full={encode_full:.1}ms ({png_bytes} bytes, RGB) encode_png_full_rgba_reference={encode_full_rgba:.1}ms ({png_bytes_rgba} bytes) encode_png_1280={encode_action:.1}ms compare_downscale={downscale_compare:.1}ms compare_difference={difference:.2}ms"
            );
        }
    }
}

#[cfg(test)]
mod png_tests {
    use super::*;

    #[test]
    fn encoded_png_is_eight_bit_rgb_and_round_trips_every_pixel() {
        let (width, height) = (37_u32, 23_u32);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                let value = u8::try_from(index % 251).unwrap();
                [
                    value,
                    value.wrapping_mul(3),
                    255 - value,
                    255, // captures are opaque
                ]
            })
            .collect();
        let encoded = encode_png(width, height, &rgba).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(encoded))
            .read_info()
            .unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (width, height));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut decoded).unwrap();
        let expected: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        assert_eq!(&decoded[..frame.buffer_size()], expected.as_slice());
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    fn gray(width: u32, height: u32, value: impl Fn(u32, u32) -> u8) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let level = value(x, y);
                [level, level, level, 255]
            })
            .collect()
    }

    #[test]
    fn area_weights_cover_every_output_pixel_exactly_once() {
        for (source, target) in [(1920, 1280), (3840, 1280), (7, 3), (5, 5), (1, 1), (3, 5)] {
            let weights = area_weights(source, target);
            assert_eq!(weights.spans.len(), target as usize);
            for (first, range) in &weights.spans {
                assert!(*first + range.len() <= source as usize);
                assert_eq!(
                    weights.weights[range.clone()].iter().sum::<u32>(),
                    AREA_WEIGHT_ONE
                );
            }
        }
    }

    #[test]
    fn area_resize_keeps_flat_color_and_averages_integer_blocks() {
        let flat = gray(1920, 1080, |_, _| 77);
        let resized = resize_rgba_area(&flat, 1920, 1080, 1280, 720);
        assert_eq!(resized.len(), 1280 * 720 * 4);
        assert!(
            resized
                .chunks_exact(4)
                .all(|pixel| pixel == [77, 77, 77, 255])
        );
        let source = [
            0, 0, 0, 255, 100, 0, 0, 255, 0, 100, 0, 255, 100, 100, 0, 255,
        ];
        assert_eq!(resize_rgba_area(&source, 2, 2, 1, 1), [50, 50, 0, 255]);
    }

    #[test]
    fn one_pixel_strokes_survive_a_three_to_one_reduction() {
        // A dark one-pixel vertical stroke every 9 columns, like thin text on a light page.
        let source = gray(90, 30, |x, _| if x % 9 == 4 { 0 } else { 255 });
        let resized = resize_rgba_area(&source, 90, 30, 30, 10);
        let darkest = resized.chunks_exact(4).map(|pixel| pixel[0]).min().unwrap();
        // Area averaging keeps a third of the stroke's contrast instead of skipping it.
        assert!((165..=175).contains(&darkest), "{darkest}");
    }
}

#[cfg(test)]
mod live_capture_benchmarks {
    use super::*;

    /// Times screen reads only; no pixels are stored or written anywhere.
    #[test]
    #[ignore = "reads the live desktop; run only on an approved disposable desktop"]
    fn live_gdi_capture_latency() {
        ensure_dpi_awareness().unwrap();
        for (width, height) in [(64_u32, 64_u32), (400, 300), (760, 560), (1920, 1080)] {
            let bounds = DisplayBounds {
                left: 0,
                top: 0,
                width,
                height,
            };
            let mut samples: Vec<f64> = (0..15)
                .map(|_| {
                    let started = Instant::now();
                    capture_rgba(bounds).unwrap();
                    started.elapsed().as_secs_f64() * 1_000.0
                })
                .collect();
            samples.sort_by(f64::total_cmp);
            eprintln!(
                "{width}x{height}: median={:.1}ms min={:.1}ms",
                samples[samples.len() / 2],
                samples[0]
            );
        }
    }
}

#[cfg(test)]
mod settle_tests {
    use super::*;

    #[test]
    fn changed_pixel_counts_stop_at_the_limit_and_ignore_identical_pixels() {
        let base = vec![10_u8; 4 * 100];
        let mut caret = base.clone();
        for pixel in caret
            .chunks_exact_mut(4)
            .take(SETTLE_MIN_CHANGED_PIXELS - 1)
        {
            pixel[0] = 0;
        }
        assert_eq!(changed_pixels(&base, &base, 1), 0);
        assert_eq!(
            changed_pixels(&base, &caret, SETTLE_MIN_CHANGED_PIXELS),
            SETTLE_MIN_CHANGED_PIXELS - 1
        );
        let glyph = vec![0_u8; 4 * 100];
        assert_eq!(changed_pixels(&base, &glyph, 7), 7);
        assert_eq!(changed_pixels(&base, &base[..4], 5), 5);
    }

    #[test]
    fn settle_watch_skips_unknown_empty_and_oversized_windows() {
        assert!(SettleWatch::begin(None).is_none());
        for (width, height) in [(0, 10), (10, 0), (7_680, 4_320)] {
            assert!(
                SettleWatch::begin(Some(DisplayBounds {
                    left: 0,
                    top: 0,
                    width,
                    height,
                }))
                .is_none()
            );
        }
    }
}

#[cfg(test)]
mod live_encode_benchmarks {
    use super::*;

    /// Times the shipped encoder against the previous default on the live primary display.
    /// Only sizes and times are printed; no pixels are kept.
    #[test]
    #[ignore = "reads the live desktop; run only on an approved disposable desktop"]
    fn live_png_encode_cost() {
        ensure_dpi_awareness().unwrap();
        let display = enumerate_displays()
            .unwrap()
            .into_iter()
            .find(|display| display.is_primary)
            .unwrap();
        let frame = capture_rgba(display.bounds).unwrap();
        let (width, height, _) =
            scaled_dimensions(display.bounds.width, display.bounds.height, Some(1280));
        let resized = resize_rgba_area(
            &frame,
            display.bounds.width,
            display.bounds.height,
            width,
            height,
        );
        let median = |encode: &dyn Fn() -> usize| {
            let mut size = 0;
            let mut samples: Vec<f64> = (0..7)
                .map(|_| {
                    let started = Instant::now();
                    size = encode();
                    started.elapsed().as_secs_f64() * 1_000.0
                })
                .collect();
            samples.sort_by(f64::total_cmp);
            (samples[samples.len() / 2], size)
        };
        let shipped = median(&|| encode_png(width, height, &resized).unwrap().len());
        let previous = median(&|| {
            let rgb: Vec<u8> = resized
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            let mut output = Vec::new();
            let mut encoder = png::Encoder::new(&mut output, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&rgb).unwrap();
            writer.finish().unwrap();
            output.len()
        });
        eprintln!(
            "{width}x{height}: shipped {:.1}ms {} bytes; balanced default {:.1}ms {} bytes",
            shipped.0, shipped.1, previous.0, previous.1
        );
    }
}

#[cfg(test)]
mod window_capture_tests {
    use super::*;

    /// A 4x3 BGRA frame whose pixel (x, y) is blue = x, green = y, red = 100 + x + y.
    fn gradient() -> Vec<u8> {
        let mut frame = Vec::new();
        for y in 0..3_u8 {
            for x in 0..4_u8 {
                frame.extend_from_slice(&[x, y, 100 + x + y, 0]);
            }
        }
        frame
    }

    #[test]
    fn cropping_keeps_the_visible_frame_and_converts_bgra_to_opaque_rgba() {
        // The window is 4x3; the visible frame is the 2x2 block at offset (1, 1), as when the
        // window has invisible resize borders.
        let rgba = crop_bgra_to_rgba(&gradient(), 4, 3, 1, 1, 2, 2).unwrap();
        assert_eq!(
            rgba,
            [
                102, 1, 1, 255, // x=1, y=1
                103, 1, 2, 255, // x=2, y=1
                103, 2, 1, 255, // x=1, y=2
                104, 2, 2, 255, // x=2, y=2
            ]
        );
        let whole = crop_bgra_to_rgba(&gradient(), 4, 3, 0, 0, 4, 3).unwrap();
        assert_eq!(whole.len(), 48);
        assert_eq!(&whole[..4], [100, 0, 0, 255]);
    }

    #[test]
    fn cropping_refuses_a_visible_frame_that_does_not_line_up_with_the_pixels() {
        let frame = gradient();
        for (left, top, width, height) in [
            (-1, 0, 2, 2),
            (0, -1, 2, 2),
            (3, 0, 2, 2),
            (0, 2, 2, 2),
            (0, 0, 5, 3),
            (i64::MAX, 0, 2, 2),
        ] {
            assert!(
                crop_bgra_to_rgba(&frame, 4, 3, left, top, width, height).is_none(),
                "{left},{top},{width},{height}"
            );
        }
        // A frame of the wrong length is never trusted.
        assert!(crop_bgra_to_rgba(&frame[..40], 4, 3, 0, 0, 2, 2).is_none());
    }

    #[test]
    fn a_hung_renderer_is_given_up_on_and_only_one_render_can_be_outstanding() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        };
        static OUTSTANDING: AtomicBool = AtomicBool::new(false);
        let (release, hold) = mpsc::channel::<()>();
        let started = Instant::now();
        // A renderer that never answers until released, like a window that stops pumping messages.
        let result = render_within(Duration::from_millis(100), &OUTSTANDING, move || {
            let _ = hold.recv();
            Some(vec![1_u8])
        });
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(OUTSTANDING.load(Ordering::Acquire));

        // While it is parked no second render starts at all.
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&attempts);
        let second = render_within(Duration::from_millis(100), &OUTSTANDING, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Some(vec![2_u8])
        });
        assert!(second.is_none());
        assert_eq!(attempts.load(Ordering::SeqCst), 0);

        // Once the parked render returns, rendering works again.
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while OUTSTANDING.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "the abandoned render never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let third = render_within(Duration::from_secs(5), &OUTSTANDING, || Some(vec![3_u8]));
        assert_eq!(third, Some(vec![3_u8]));
    }

    #[test]
    fn a_second_render_waits_for_the_slot_instead_of_giving_up() {
        use std::sync::{atomic::AtomicBool, mpsc};
        static OUTSTANDING: AtomicBool = AtomicBool::new(false);
        let (release, hold) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        let first = std::thread::spawn(move || {
            render_within(Duration::from_secs(10), &OUTSTANDING, move || {
                started.send(()).unwrap();
                let _ = hold.recv();
                Some(1_u8)
            })
        });
        running.recv().unwrap();
        let second = std::thread::spawn(|| {
            render_within(Duration::from_secs(10), &OUTSTANDING, || Some(2_u8))
        });
        // The second capture has to be waiting, not already answered or abandoned.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!second.is_finished());
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap(), Some(1));
        assert_eq!(second.join().unwrap(), Some(2));
    }

    #[test]
    fn a_render_that_answers_in_time_is_returned_and_a_failed_one_is_not() {
        static OUTSTANDING: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        assert_eq!(
            render_within(Duration::from_secs(5), &OUTSTANDING, || Some(7_u8)),
            Some(7)
        );
        assert_eq!(
            render_within::<u8>(Duration::from_secs(5), &OUTSTANDING, || None),
            None
        );
    }

    #[test]
    fn a_render_that_panics_still_frees_the_render_slot() {
        static OUTSTANDING: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let panicked = render_within::<u8>(Duration::from_secs(5), &OUTSTANDING, || {
            panic!("synthetic render failure")
        });
        assert_eq!(panicked, None);
        // The worker clears the slot while unwinding; allow it a moment to finish doing so.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while OUTSTANDING.load(std::sync::atomic::Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            render_within(Duration::from_secs(5), &OUTSTANDING, || Some(9_u8)),
            Some(9)
        );
    }

    #[test]
    fn only_a_fully_black_render_counts_as_a_failed_print() {
        assert!(is_blank_frame(&[0, 0, 0, 255, 0, 0, 0, 0]));
        assert!(!is_blank_frame(&[0, 0, 0, 255, 0, 1, 0, 255]));
        assert!(is_blank_frame(&[]));
    }

    #[test]
    fn the_window_is_rendered_itself_and_the_screen_copy_is_only_a_fallback() {
        let rendered = vec![1, 2, 3, 255];
        let (pixels, method) = window_pixels(Some(rendered.clone()), || {
            panic!("an occluded window must not be copied from the screen")
        })
        .unwrap();
        assert_eq!((pixels, method), (rendered, "windows_print_window"));
        let (pixels, method) = window_pixels(None, || Ok(vec![9, 9, 9, 255])).unwrap();
        assert_eq!((pixels, method), (vec![9, 9, 9, 255], "windows_gdi"));
        assert!(
            window_pixels(None, || Err(PlatformError::OperationFailed {
                operation: "BitBlt".into(),
                reason: "synthetic".into(),
            }))
            .is_err()
        );
    }

    #[test]
    fn pixel_fingerprints_ignore_the_fourth_byte_and_the_channel_layout_but_not_any_colour() {
        let bgra = vec![30, 20, 10, 0, 60, 50, 40, 0];
        let same = vec![30, 20, 10, 255, 60, 50, 40, 7];
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 255];
        assert_eq!(
            pixel_fingerprint(&bgra, false),
            pixel_fingerprint(&same, false)
        );
        assert_eq!(
            pixel_fingerprint(&bgra, false),
            pixel_fingerprint(&rgba, true)
        );
        let mut other = same.clone();
        other[5] = 51;
        assert_ne!(
            pixel_fingerprint(&bgra, false),
            pixel_fingerprint(&other, false)
        );
        assert_ne!(
            pixel_fingerprint(&bgra, false),
            pixel_fingerprint(&bgra[..7], false),
            "different sizes never match"
        );
    }

    #[test]
    fn png_rows_are_written_without_a_full_frame_rgb_copy_and_round_trip() {
        let (width, height) = (5_u32, 4_u32);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                let value = u8::try_from(index * 11).unwrap();
                [value, value / 2, 255 - value, 255]
            })
            .collect();
        let encoded = encode_png(width, height, &rgba).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(encoded))
            .read_info()
            .unwrap();
        let mut pixels = vec![0_u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (width, height));
        let expected: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        assert_eq!(pixels, expected);
        // Mismatched pixel data is refused instead of writing a corrupt image.
        assert!(encode_png(width, height, &rgba[..rgba.len() - 4]).is_err());
        assert!(encode_png(width + 1, height, &rgba).is_err());
    }
}
