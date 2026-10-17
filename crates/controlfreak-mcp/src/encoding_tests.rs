//! Base64 encoding of image data stays on blocking workers and honors cancellation.

use super::*;
use std::thread::ThreadId;

/// Records which thread asked for its PNG, which is when encoding happens.
struct Probe {
    reads: Arc<Mutex<Vec<ThreadId>>>,
}

impl CarriesPng for Probe {
    fn png(&self) -> Option<&[u8]> {
        self.reads.lock().unwrap().push(std::thread::current().id());
        Some(b"\x89PNG\r\n\x1a\n")
    }
}

#[tokio::test]
async fn image_encoding_runs_on_a_blocking_worker_not_the_async_thread() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe {
        reads: Arc::clone(&reads),
    };
    let encoded = handlers::run_image_operation(None, move || Ok(probe))
        .await
        .unwrap()
        .unwrap();
    let reads = reads.lock().unwrap();
    assert_eq!(reads.len(), 1, "the PNG is encoded exactly once");
    assert_ne!(
        reads[0],
        std::thread::current().id(),
        "encoding ran on the async thread"
    );
    drop(reads);
    // Turning the encoded value into a result is plain assembly and needs no PNG access.
    let _ = encoded;
}

#[tokio::test]
async fn cancellation_is_checked_before_any_image_is_encoded() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let work = stop::Work::default();
    let control = work.control.clone();
    let probe = Probe {
        reads: Arc::clone(&reads),
    };
    let result = stop::WORK
        .scope(work, async move {
            handlers::run_image_operation(None, move || {
                // The request is cancelled while the capture is in flight.
                control.cancel();
                Ok(probe)
            })
            .await
            .unwrap()
        })
        .await;
    assert!(result.is_err(), "a cancelled request must not succeed");
    assert!(
        reads.lock().unwrap().is_empty(),
        "a cancelled request must not be encoded"
    );
}

#[tokio::test]
async fn mutation_results_are_encoded_on_the_blocking_worker() {
    let reads = Arc::new(Mutex::new(Vec::new()));
    let probe = Probe {
        reads: Arc::clone(&reads),
    };
    let (result, _progress) = handlers::run_encoded_mutation(
        None,
        "fixture".into(),
        Arc::new(stop_tests::WaitingBackend::idle()),
        move |_| Ok(probe),
    )
    .await;
    assert!(result.is_ok());
    let reads = reads.lock().unwrap();
    assert_eq!(reads.len(), 1);
    assert_ne!(reads[0], std::thread::current().id());
}

#[test]
fn encoded_image_content_decodes_to_the_capture_png() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let png = b"\x89PNG\r\n\x1a\nsynthetic".to_vec();
    let result = results::screenshot_result(Encoded::new(controlfreak_core::DisplayScreenshot {
        target_ref: None,
        display: controlfreak_core::DisplayInfo {
            id: "synthetic".into(),
            name: "synthetic".into(),
            bounds: controlfreak_core::DisplayBounds {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
            },
            is_primary: true,
        },
        source_bounds: controlfreak_core::DisplayBounds {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
        },
        png: png.clone(),
        image_width: 1,
        image_height: 1,
        downscale_factor: 1,
        cursor_marker: false,
    }));
    let value = serde_json::to_value(result).unwrap();
    assert_eq!(value["content"][0]["type"], "image");
    assert_eq!(
        STANDARD
            .decode(value["content"][0]["data"].as_str().unwrap())
            .unwrap(),
        png
    );
    assert_eq!(
        value["structuredContent"]["byte_length"],
        u64::try_from(png.len()).unwrap()
    );
}

/// Run with `cargo test -p controlfreak-mcp --release --lib encoding_tests -- --ignored --nocapture`.
/// Sizes: the synthetic 3840x2160 desktop frame's PNG, a photographic 1280x720 PNG, and a
/// photographic 3840x2160 PNG from the platform benchmark.
#[test]
#[ignore = "synthetic latency benchmark; run explicitly"]
fn base64_encoding_latency_by_png_size() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    for bytes in [58_045_usize, 1_720_154, 8_000_000] {
        let png: Vec<u8> = (0..bytes)
            .map(|index| u8::try_from(index.wrapping_mul(2_654_435_761) >> 24 & 0xff).unwrap())
            .collect();
        let mut samples: Vec<f64> = (0..15)
            .map(|_| {
                let started = std::time::Instant::now();
                std::hint::black_box(STANDARD.encode(std::hint::black_box(&png)));
                started.elapsed().as_secs_f64() * 1_000.0
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        eprintln!("base64 of {bytes} bytes: median={:.3}ms", samples[7]);
    }
}
