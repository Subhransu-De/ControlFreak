use super::*;
use controlfreak_core::{
    BackendIdentity, BackendMetadata, CapabilityDescriptor, DisplayBackend, DisplayBounds,
    DisplayInfo, DisplayServer, KeyboardBackend, MousePosition, OcrBackend, OcrResult,
    PermissionDescriptor, Platform, PointerBackend, WindowBackend,
};
use tokio::io::{AsyncWriteExt, BufReader};

#[derive(Default)]
struct Fixture(Mutex<FixtureState>);

#[derive(Default)]
struct FixtureState {
    text: Option<String>,
    baseline: Option<String>,
    mutations: usize,
}

fn display() -> DisplayInfo {
    DisplayInfo {
        id: "fixture".into(),
        name: "Synthetic secondary display".into(),
        bounds: DisplayBounds {
            left: -1920,
            top: -200,
            width: 1920,
            height: 1080,
        },
        is_primary: false,
    }
}

fn bounds() -> DisplayBounds {
    DisplayBounds {
        left: -1820,
        top: -140,
        width: 400,
        height: 200,
    }
}

fn screenshot() -> DisplayScreenshot {
    DisplayScreenshot {
        target_ref: Some("fixture-target".into()),
        display: display(),
        source_bounds: bounds(),
        png: include_bytes!("../../../examples/fixture.png").to_vec(),
        image_width: 200,
        image_height: 100,
        downscale_factor: 2,
        cursor_marker: false,
    }
}

impl Fixture {
    fn text(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .text
            .clone()
            .unwrap_or_else(|| "Ready".into())
    }

    fn pointer(
        &self,
        x: u32,
        y: u32,
        text: &str,
        control: &MutationControl,
    ) -> PointerActionResult {
        let (virtual_x, virtual_y) = display().bounds.to_virtual(x, y).unwrap();
        control.dispatch_started();
        control.dispatch_accepted(2);
        control.input_complete();
        let mut state = self.0.lock().unwrap();
        state.text = Some(text.into());
        state.mutations += 1;
        PointerActionResult {
            position: MousePosition {
                display_id: "fixture".into(),
                local_x: x,
                local_y: y,
                virtual_x,
                virtual_y,
            },
            observation: ActionObservation {
                foreground_window: None,
                screenshot: None,
            },
        }
    }
}

impl BackendMetadata for Fixture {
    fn validate_target_reference(&self, _target: &str) -> Result<(), PlatformError> {
        Ok(())
    }
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            platform: Platform::Windows,
            display_server: DisplayServer::Dwm,
            backend_id: "recipe-fixture".into(),
        }
    }
    fn capabilities(&self) -> Vec<CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<PermissionDescriptor> {
        vec![]
    }
}

impl DisplayBackend for Fixture {
    fn capture_region(
        &self,
        request: &CaptureRegionRequest,
    ) -> Result<DisplayScreenshot, PlatformError> {
        assert_eq!(
            (request.x, request.y, request.width, request.height),
            (100, 60, 400, 200)
        );
        assert_eq!(request.display_id, "fixture");
        assert_eq!(request.max_width, Some(200));
        Ok(screenshot())
    }

    fn capture_visual_baseline(
        &self,
        _: &VisualBaselineRequest,
    ) -> Result<VisualBaseline, PlatformError> {
        let text = self.text();
        self.0.lock().unwrap().baseline = Some(text);
        Ok(VisualBaseline {
            baseline_id: "vb-00000000000000000000000000000000-0000000000000001".into(),
            display: display(),
            source_bounds: bounds(),
            fingerprint: "fixture".into(),
            expires_in_ms: 300_000,
        })
    }

    fn wait_for_change_since(
        &self,
        request: &WaitForChangeSinceRequest,
    ) -> Result<VisualChangeResult, PlatformError> {
        assert_eq!(
            request.baseline_id,
            "vb-00000000000000000000000000000000-0000000000000001"
        );
        assert_eq!((request.timeout_ms, request.stable_ms), (500, 100));
        let text = self.text();
        let state = self.0.lock().unwrap();
        let changed = state
            .baseline
            .as_ref()
            .expect("capture baseline before waiting")
            != &text;
        Ok(VisualChangeResult {
            changed,
            timed_out: !changed,
            difference: if changed { 1.0 } else { 0.0 },
            elapsed_ms: if changed { 100 } else { 500 },
            screenshot: screenshot(),
        })
    }
}

impl OcrBackend for Fixture {
    fn find_text_on_screen(
        &self,
        request: &controlfreak_core::FindTextRequest,
    ) -> Result<controlfreak_core::FindTextResult, PlatformError> {
        let lines = vec![controlfreak_core::OcrLine {
            text: "Save\u{a0}changes".into(),
            x: 150,
            y: 80,
            width: 100,
            height: 20,
            words: vec![],
        }];
        let details = controlfreak_core::discover_text(
            &lines,
            &request.region,
            &request.query,
            request.case_sensitive,
            request.match_mode,
            request.ocr_confusions,
            request.max_results,
        )?;
        Ok(controlfreak_core::FindTextResult {
            target_ref: Some("fixture-target".into()),
            query: request.query.clone(),
            display: display(),
            source_bounds: bounds(),
            language: "en-US".into(),
            matches: details.candidates.clone(),
            details,
        })
    }
    fn read_text_in_region(&self, _: &OcrRegionRequest) -> Result<OcrResult, PlatformError> {
        Ok(OcrResult {
            target_ref: Some("fixture-target".into()),
            display: display(),
            source_bounds: bounds(),
            language: "en-US".into(),
            text: self.text(),
            lines: vec![],
        })
    }
}

impl PointerBackend for Fixture {
    fn click_mouse_controlled(
        &self,
        request: &MouseClickRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        // The target is image pixel (50, 25), scaled into the cropped source and then display-local pixels.
        let shot = screenshot();
        let x = u32::try_from(shot.source_bounds.left - shot.display.bounds.left).unwrap()
            + 50 * shot.source_bounds.width / shot.image_width;
        let y = u32::try_from(shot.source_bounds.top - shot.display.bounds.top).unwrap()
            + 25 * shot.source_bounds.height / shot.image_height;
        assert_eq!((request.x, request.y), (x, y));
        assert_eq!(request.display_id, "fixture");
        Ok(self.pointer(request.x, request.y, "Saved", control))
    }

    fn drag_mouse_controlled(
        &self,
        request: &MouseDragRequest,
        control: &MutationControl,
    ) -> Result<PointerActionResult, PlatformError> {
        assert_eq!(
            (
                request.start_x,
                request.start_y,
                request.end_x,
                request.end_y
            ),
            (200, 110, 300, 160)
        );
        Ok(self.pointer(request.end_x, request.end_y, "Dropped", control))
    }
}

impl KeyboardBackend for Fixture {
    fn type_text_controlled(
        &self,
        request: &TextInputRequest,
        control: &MutationControl,
    ) -> Result<KeyboardActionResult, PlatformError> {
        assert_eq!(request.text, "ab");
        control.dispatch_started();
        control.dispatch_accepted(2);
        let mut state = self.0.lock().unwrap();
        state.text = Some("a".into());
        state.mutations += 1;
        Err(PlatformError::OperationFailed {
            operation: "type_text".into(),
            reason: "fixture stopped after the first character".into(),
        })
    }
}
impl WindowBackend for Fixture {}

struct FixtureIndicator;
impl ActivityIndicator for FixtureIndicator {
    fn set_level(&mut self, _: IndicatorLevel) -> Result<(), String> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn published_recipes_validate_and_run_on_a_synthetic_desktop() {
    let recipes: Value =
        serde_json::from_str(include_str!("../../../examples/recipes.json")).unwrap();
    for recipe in recipes.as_array().unwrap() {
        let backend = Arc::new(Fixture::default());
        let indicator = SafetyIndicator::dormant();
        let runtime = Arc::new(IndicatorRuntime::new_with_timing(
            indicator.clone(),
            || Ok(FixtureIndicator),
            Arc::new(LocalArbitrator),
            GlowTiming {
                short_hold: Duration::from_secs(30),
                session_hold: Duration::from_secs(30),
                max_hold: Duration::from_secs(30),
            },
        ));
        let server =
            ControlFreakServer::with_indicator(backend.clone(), indicator, Some(runtime.clone()));
        let (client_io, server_io) = tokio::io::duplex(256 * 1024);
        let serving = tokio::spawn(async move {
            server
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap()
        });
        let mut client = BufReader::new(client_io);
        let exchange = super::outcome_tests::exchange;
        exchange(&mut client, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"recipes","version":"1"}}})).await;
        client
            .get_mut()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        let listed = exchange(
            &mut client,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;
        assert_eq!(
            listed["result"]["tools"],
            serde_json::to_value(tool_definitions()).unwrap()
        );
        let tools = listed["result"]["tools"].as_array().unwrap();
        for step in recipe["steps"].as_array().unwrap() {
            let call = &step["call"];
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == call["name"])
                .expect("shipped tool");
            jsonschema::validate(&tool["inputSchema"], &call["arguments"]).unwrap();
            let response = exchange(
                &mut client,
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":call}),
            )
            .await;
            assert!(response.get("error").is_none(), "{response}");
            assert_ne!(response["result"]["isError"], true, "{response}");
            let content = &response["result"]["structuredContent"];
            if let Some(schema) = tool.get("outputSchema") {
                jsonschema::validate(schema, content).unwrap();
            }
            for (pointer, expected) in step["expect"].as_object().unwrap() {
                assert_eq!(
                    content.pointer(pointer),
                    Some(expected),
                    "{}: {}: {response}",
                    recipe["name"],
                    call["name"]
                );
            }
        }
        assert_eq!(
            backend.0.lock().unwrap().mutations,
            usize::from(
                recipe["name"] != "bounded-wait-timeout"
                    && recipe["name"] != "tolerant-ocr-discovery"
            )
        );
        assert_eq!(runtime.status()["state"], "dormant");
        assert_eq!(runtime.status()["owns_arbitration"], false);
        drop(client);
        tokio::time::timeout(Duration::from_secs(10), serving)
            .await
            .unwrap()
            .unwrap();
    }
}
