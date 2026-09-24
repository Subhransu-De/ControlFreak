use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use controlfreak_core::{
    ActionObservation, DisplayBounds, DisplayInfo, DisplayScreenshot, MousePosition, PlatformError,
    PointerActionResult,
};

use super::{
    ActivityArbitrator, ActivityIndicator, ArbitrationBusy, BeginSessionError, CaptureDisplayInput,
    FocusWindowInput, GlowTiming, IndicatorHealth, IndicatorRuntime, PressKeysInput,
    SafetyIndicator, handlers::run_platform_operation, parse_arguments, pointer_result,
    schema::json_object, tool_error, tool_execution_error, tools,
};

pub(super) async fn wait_until(expected: &str, timeout: Duration, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(timeout, async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {expected}"));
}

async fn wait_for_status(indicator: &SafetyIndicator, expected: &str) {
    wait_until(
        &format!("indicator status {expected}"),
        Duration::from_secs(5),
        || indicator.status() == expected,
    )
    .await;
}

// Keep scheduled deadlines queued so tests can deliver timeout callbacks in
// an exact order, without racing the OS clock or changing production timing.
fn manual_timeouts(runtime: &IndicatorRuntime) -> mpsc::Receiver<super::IdleCommand> {
    let (sender, receiver) = mpsc::channel();
    *runtime.idle_worker.lock().unwrap() = Some(super::IdleWorker {
        sender,
        thread: None,
    });
    receiver
}

fn expire_current_timeout(runtime: &IndicatorRuntime) {
    let generation = runtime.state.lock().unwrap().generation;
    runtime.close_if_idle(generation);
}

#[tokio::test]
async fn status_wait_yields_to_the_task_that_completes_the_transition() {
    let indicator = SafetyIndicator::dormant();
    let updated = indicator.clone();
    let task = tokio::spawn(async move { updated.mark_hidden() });
    wait_for_status(&indicator, "hidden").await;
    task.await.unwrap();
}

#[tokio::test]
#[should_panic(expected = "timed out after 0ns waiting for missing transition")]
async fn missing_transition_has_a_bounded_failure() {
    wait_until("missing transition", Duration::ZERO, || false).await;
}

struct RecordingIndicator {
    events: Arc<Mutex<Vec<&'static str>>>,
    fail_hide: bool,
}

struct FailingSecondShowIndicator {
    shows: usize,
}

struct HealthControlledIndicator {
    health: Arc<Mutex<IndicatorHealth>>,
}

struct ExclusiveArbitrator {
    owned: Arc<AtomicBool>,
}

impl ActivityArbitrator for ExclusiveArbitrator {
    fn try_acquire(&self) -> Result<(), ArbitrationBusy> {
        self.owned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ArbitrationBusy {
                owner_instance_id: Some("other-test-server".to_owned()),
                retry_after_ms: 25,
            })
    }

    fn release(&self) {
        self.owned.store(false, Ordering::Release);
    }
}

impl ActivityIndicator for FailingSecondShowIndicator {
    fn set_level(&mut self, _level: super::IndicatorLevel) -> Result<(), String> {
        self.shows += 1;
        if self.shows == 2 {
            Err("helper exited".to_owned())
        } else {
            Ok(())
        }
    }

    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl ActivityIndicator for HealthControlledIndicator {
    fn set_level(&mut self, _level: super::IndicatorLevel) -> Result<(), String> {
        Ok(())
    }

    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn health(&self) -> IndicatorHealth {
        self.health.lock().unwrap().clone()
    }
}

impl ActivityIndicator for RecordingIndicator {
    fn set_level(&mut self, level: super::IndicatorLevel) -> Result<(), String> {
        self.events.lock().unwrap().push(match level {
            super::IndicatorLevel::Armed => "armed",
            super::IndicatorLevel::Acting => "acting",
        });
        Ok(())
    }

    fn hide(&mut self) -> Result<(), String> {
        self.events.lock().unwrap().push("hide");
        if self.fail_hide {
            Err("hide failed".to_owned())
        } else {
            Ok(())
        }
    }

    fn shutdown(&mut self) -> Result<(), String> {
        self.events.lock().unwrap().push("shutdown");
        Ok(())
    }
}

#[test]
fn safety_indicator_transitions_are_explicit() {
    let indicator = SafetyIndicator::dormant();
    assert_eq!(indicator.status(), "dormant");

    indicator.mark_starting();
    assert_eq!(indicator.status(), "starting");

    indicator.mark_failed("glow failed");
    assert_eq!(indicator.status(), "failed");
    assert_eq!(indicator.failure_reason().as_deref(), Some("glow failed"));

    indicator.mark_visible();
    assert_eq!(indicator.status(), "visible");
    assert_eq!(indicator.failure_reason(), None);

    indicator.mark_idle_pending();
    assert_eq!(indicator.status(), "idle_pending");
    indicator.mark_hidden();
    assert_eq!(indicator.status(), "hidden");
    indicator.mark_stopping();
    assert_eq!(indicator.status(), "stopping");
}

#[tokio::test]
async fn operation_leases_start_once_and_hide_after_the_last_call() {
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_for_runtime = Arc::clone(&starts);
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            starts_for_runtime.fetch_add(1, Ordering::Relaxed);
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(20),
    ));
    let _timeouts = manual_timeouts(&runtime);

    assert_eq!(starts.load(Ordering::Relaxed), 0);
    assert_eq!(indicator.status(), "dormant");

    let first_lease = runtime.acquire().await.unwrap();
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    assert_eq!(indicator.status(), "visible");
    assert_eq!(*events.lock().unwrap(), ["acting"]);

    let second_lease = runtime.acquire().await.unwrap();
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

    drop(first_lease);
    expire_current_timeout(&runtime);
    assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

    drop(second_lease);
    assert_eq!(indicator.status(), "idle_pending");
    expire_current_timeout(&runtime);
    wait_for_status(&indicator, "hidden").await;
    assert_eq!(
        *events.lock().unwrap(),
        ["acting", "acting", "armed", "hide"]
    );
}

#[tokio::test]
async fn final_mutation_preserves_the_helper_failure_reason() {
    let health = Arc::new(Mutex::new(IndicatorHealth::Healthy));
    let health_for_runtime = Arc::clone(&health);
    let indicator = SafetyIndicator::dormant();
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(HealthControlledIndicator {
                health: Arc::clone(&health_for_runtime),
            })
        },
        Duration::from_millis(20),
    ));

    let lease = runtime.acquire().await.unwrap();
    *health.lock().unwrap() = IndicatorHealth::Dead("heartbeat was lost".to_owned());
    drop(lease);

    assert_eq!(indicator.status(), "failed");
    assert_eq!(
        indicator.failure_reason().as_deref(),
        Some("heartbeat was lost")
    );
    assert_eq!(runtime.status()["state"], "dormant");
}

#[tokio::test]
async fn a_new_operation_cancels_the_pending_hide() {
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(40),
    ));
    let _timeouts = manual_timeouts(&runtime);

    let first_lease = runtime.acquire().await.unwrap();
    drop(first_lease);
    let stale_generation = runtime.state.lock().unwrap().generation;

    let second_lease = runtime.acquire().await.unwrap();
    runtime.close_if_idle(stale_generation);
    expire_current_timeout(&runtime);
    assert_eq!(indicator.status(), "visible");
    assert_eq!(*events.lock().unwrap(), ["acting", "armed", "acting"]);

    drop(second_lease);
    runtime.close_if_idle(stale_generation);
    assert_eq!(indicator.status(), "idle_pending");
    expire_current_timeout(&runtime);
    wait_for_status(&indicator, "hidden").await;
    assert_eq!(
        *events.lock().unwrap(),
        ["acting", "armed", "acting", "armed", "hide"]
    );
}

#[tokio::test]
async fn sequential_operations_reuse_one_idle_worker() {
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(20),
    ));

    for _ in 0..20 {
        drop(runtime.acquire().await.unwrap());
        wait_for_status(&indicator, "hidden").await;
    }
    assert_eq!(runtime.idle_worker_start_count(), 1);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == "hide")
            .count(),
        20
    );
}

#[tokio::test]
async fn a_hide_failure_shuts_down_the_helper_and_degrades_status() {
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: true,
            })
        },
        Duration::from_millis(10),
    ));

    let lease = runtime.acquire().await.unwrap();
    drop(lease);
    wait_for_status(&indicator, "failed").await;
    assert_eq!(indicator.failure_reason().as_deref(), Some("hide failed"));
    assert_eq!(
        *events.lock().unwrap(),
        ["acting", "armed", "hide", "shutdown"]
    );
}

#[tokio::test]
async fn overlapping_operations_revalidate_helper_visibility() {
    let indicator = SafetyIndicator::dormant();
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        || Ok(FailingSecondShowIndicator { shows: 0 }),
        Duration::from_millis(10),
    ));

    let first_lease = runtime.acquire().await.unwrap();
    let second = runtime.acquire().await;

    assert!(second.is_err());
    assert_eq!(indicator.status(), "failed");
    assert_eq!(indicator.failure_reason().as_deref(), Some("helper exited"));
    drop(first_lease);
}

#[tokio::test]
async fn a_hide_failure_restarts_the_helper_on_the_next_mutation() {
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_for_runtime = Arc::clone(&starts);
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let indicator = SafetyIndicator::dormant();
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: starts_for_runtime.fetch_add(1, Ordering::Relaxed) == 0,
            })
        },
        Duration::from_millis(10),
    ));

    drop(runtime.acquire().await.unwrap());
    wait_for_status(&indicator, "failed").await;

    drop(runtime.acquire().await.unwrap());
    wait_for_status(&indicator, "hidden").await;
    assert_eq!(starts.load(Ordering::Relaxed), 2);
    assert_eq!(indicator.status(), "hidden");
}

#[test]
fn begin_clamps_expected_seconds_and_end_hides_immediately() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Arc::new(super::LocalArbitrator),
        GlowTiming {
            short_hold: Duration::from_millis(10),
            session_hold: Duration::from_secs(1),
            max_hold: Duration::from_secs(2),
        },
    ));

    assert!(runtime.begin_session(Some(99), "").is_err());
    assert_eq!(runtime.status()["owns_arbitration"], false);
    assert!(events.lock().unwrap().is_empty());
    runtime.begin_session(Some(99), "synthetic").unwrap();
    assert_eq!(runtime.status()["hold_ms"], 2_000);
    assert_eq!(runtime.status()["state"], "armed");
    let snapshot = || {
        let state = runtime.state.lock().unwrap();
        (
            state.session.hold,
            state.session.close_deadline,
            state.generation,
        )
    };
    let before = snapshot();
    std::thread::scope(|scope| {
        let callers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    assert!(matches!(
                        runtime.begin_session(Some(1), "different"),
                        Err(BeginSessionError::Target(
                            PlatformError::TargetInvalidated { .. }
                        ))
                    ));
                })
            })
            .collect();
        for caller in callers {
            caller.join().unwrap();
        }
    });
    assert_eq!(snapshot(), before);
    assert_eq!(runtime.status()["approved_target_ref"], "synthetic");
    assert_eq!(*events.lock().unwrap(), ["armed"]);
    runtime.end_session().unwrap();

    runtime.begin_session(Some(1), "synthetic").unwrap();
    assert_eq!(runtime.status()["hold_ms"], 1_000);
    runtime.end_session().unwrap();
    assert_eq!(*events.lock().unwrap(), ["armed", "hide", "armed", "hide"]);
}

#[tokio::test]
async fn begin_keeps_acting_while_a_mutation_is_in_flight() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        SafetyIndicator::dormant(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(20),
    ));

    let lease = runtime.acquire().await.unwrap();
    runtime.begin_session(None, "synthetic").unwrap();

    assert_eq!(runtime.status()["state"], "acting");
    assert_eq!(runtime.status()["active_mutations"], 1);
    assert!(runtime.status()["time_until_close_ms"].is_null());
    assert_eq!(*events.lock().unwrap(), ["acting", "acting"]);

    drop(lease);
    runtime.end_session().unwrap();
}

#[tokio::test]
async fn explicit_session_hold_survives_the_first_mutation() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Arc::new(super::LocalArbitrator),
        GlowTiming {
            short_hold: Duration::from_millis(10),
            session_hold: Duration::from_secs(1),
            max_hold: Duration::from_secs(2),
        },
    ));
    let _timeouts = manual_timeouts(&runtime);

    runtime.begin_session(Some(2), "synthetic").unwrap();
    drop(runtime.acquire().await.unwrap());

    assert_eq!(runtime.status()["state"], "armed");
    assert_eq!(runtime.status()["hold_ms"], 2_000);
    let state = runtime.state.lock().unwrap();
    assert!(
        state.session.close_deadline.unwrap()
            >= state.session.last_mutation_finished.unwrap() + state.session.hold
    );
    drop(state);

    runtime.end_session().unwrap();
}

#[test]
fn refused_server_stays_dark_until_the_owner_ends() {
    let owned = Arc::new(AtomicBool::new(false));
    let first_events = Arc::new(Mutex::new(Vec::new()));
    let second_events = Arc::new(Mutex::new(Vec::new()));
    let second_starts = Arc::new(AtomicUsize::new(0));
    let first = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        {
            let events = Arc::clone(&first_events);
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events),
                    fail_hide: false,
                })
            }
        },
        Arc::new(ExclusiveArbitrator {
            owned: Arc::clone(&owned),
        }),
        GlowTiming {
            short_hold: Duration::from_millis(20),
            session_hold: Duration::from_millis(20),
            max_hold: Duration::from_secs(1),
        },
    ));
    let second = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        {
            let events = Arc::clone(&second_events);
            let starts = Arc::clone(&second_starts);
            move || {
                starts.fetch_add(1, Ordering::Relaxed);
                Ok(RecordingIndicator {
                    events: Arc::clone(&events),
                    fail_hide: false,
                })
            }
        },
        Arc::new(ExclusiveArbitrator {
            owned: Arc::clone(&owned),
        }),
        GlowTiming {
            short_hold: Duration::from_millis(20),
            session_hold: Duration::from_millis(20),
            max_hold: Duration::from_secs(1),
        },
    ));

    first.begin_session(None, "synthetic").unwrap();
    assert!(second.begin_session(None, "synthetic").is_err());
    assert_eq!(second_starts.load(Ordering::Relaxed), 0);
    assert!(second_events.lock().unwrap().is_empty());

    first.end_session().unwrap();
    second.begin_session(None, "synthetic").unwrap();
    assert_eq!(*second_events.lock().unwrap(), ["armed"]);
    second.end_session().unwrap();
}

#[test]
fn failed_helper_start_releases_arbitration_for_the_next_server() {
    let owned = Arc::new(AtomicBool::new(false));
    let failed_starts = Arc::new(AtomicUsize::new(0));
    let first = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        {
            let failed_starts = Arc::clone(&failed_starts);
            move || {
                failed_starts.fetch_add(1, Ordering::Relaxed);
                Err::<RecordingIndicator, _>("native indicator initialization failed".to_owned())
            }
        },
        Arc::new(ExclusiveArbitrator {
            owned: Arc::clone(&owned),
        }),
        GlowTiming {
            short_hold: Duration::from_millis(20),
            session_hold: Duration::from_millis(20),
            max_hold: Duration::from_secs(1),
        },
    ));
    let events = Arc::new(Mutex::new(Vec::new()));
    let second = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        {
            let events = Arc::clone(&events);
            move || {
                Ok(RecordingIndicator {
                    events: Arc::clone(&events),
                    fail_hide: false,
                })
            }
        },
        Arc::new(ExclusiveArbitrator {
            owned: Arc::clone(&owned),
        }),
        GlowTiming {
            short_hold: Duration::from_millis(20),
            session_hold: Duration::from_millis(20),
            max_hold: Duration::from_secs(1),
        },
    ));

    assert!(matches!(
        first.begin_session(None, "synthetic").unwrap_err(),
        BeginSessionError::Indicator(reason) if reason == "native indicator initialization failed"
    ));
    assert_eq!(failed_starts.load(Ordering::Relaxed), 3);
    assert_eq!(first.status()["owns_arbitration"], false);
    assert_eq!(first.status()["last_cleanup_reason"], "admission_failure");

    second.begin_session(None, "synthetic").unwrap();
    second.end_session().unwrap();
    assert_eq!(*events.lock().unwrap(), ["armed", "hide"]);
}

#[tokio::test]
async fn end_waits_for_the_outstanding_mutation_guard() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        SafetyIndicator::dormant(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_secs(1),
    ));

    runtime.begin_session(None, "synthetic").unwrap();
    let lease = runtime.acquire().await.unwrap();
    runtime.end_session().unwrap();
    assert_eq!(*events.lock().unwrap(), ["armed", "acting"]);

    drop(lease);
    assert_eq!(*events.lock().unwrap(), ["armed", "acting", "hide"]);
    assert_eq!(runtime.status()["state"], "dormant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_acquisition_releases_the_completed_worker_lease() {
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let started = Arc::new(tokio::sync::Notify::new());
    let started_for_worker = Arc::clone(&started);
    let (continue_tx, continue_rx) = mpsc::sync_channel(1);
    let continue_rx = Arc::new(Mutex::new(continue_rx));
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            started_for_worker.notify_one();
            continue_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(10),
    ));

    let runtime_for_acquisition = Arc::clone(&runtime);
    let acquisition = tokio::spawn(async move { runtime_for_acquisition.acquire().await });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("timed out waiting for blocking worker startup");
    acquisition.abort();
    continue_tx.send(()).unwrap();
    let _ = acquisition.await;
    wait_for_status(&indicator, "hidden").await;
    assert_eq!(*events.lock().unwrap(), ["acting", "armed", "hide"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_request_keeps_the_glow_until_blocking_work_ends() {
    let indicator = SafetyIndicator::dormant();
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_for_runtime = Arc::clone(&events);
    let runtime = Arc::new(IndicatorRuntime::new_with_idle_delay(
        indicator.clone(),
        move || {
            Ok(RecordingIndicator {
                events: Arc::clone(&events_for_runtime),
                fail_hide: false,
            })
        },
        Duration::from_millis(10),
    ));
    let started = Arc::new(tokio::sync::Notify::new());
    let started_for_worker = Arc::clone(&started);
    let (finish_tx, finish_rx) = mpsc::sync_channel(1);

    let lease = runtime.acquire().await.unwrap();
    let request = tokio::spawn(run_platform_operation(Some(lease), move || {
        started_for_worker.notify_one();
        finish_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(())
    }));
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("timed out waiting for blocking worker startup");
    let retained_runtime = Arc::downgrade(&runtime);
    drop(runtime);
    request.abort();
    let _ = request.await;
    // Deliver a timeout while only the blocking work owns the runtime.
    expire_current_timeout(&retained_runtime.upgrade().unwrap());

    assert_eq!(indicator.status(), "visible");
    assert_eq!(*events.lock().unwrap(), ["acting"]);

    finish_tx.send(()).unwrap();
    wait_for_status(&indicator, "stopping").await;
    assert_eq!(*events.lock().unwrap(), ["acting", "armed", "shutdown"]);
}

#[test]
fn tool_surface_includes_windows_and_keyboard() {
    let names: Vec<String> = tools()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();

    assert_eq!(
        names,
        [
            "stop_desktop_work",
            "get_server_status",
            "begin_control_session",
            "end_control_session",
            "list_displays",
            "capture_display",
            "capture_region",
            "wait_for_visual_change",
            "capture_visual_baseline",
            "wait_for_change_since",
            "read_text_in_region",
            "find_text_on_screen",
            "click_text",
            "list_virtual_desktops",
            "switch_virtual_desktop",
            "list_windows",
            "focus_window",
            "capture_window",
            "wait_for_window",
            "move_mouse",
            "click_mouse",
            "drag_mouse",
            "scroll_mouse",
            "press_keys",
            "type_text",
        ]
    );
}

#[test]
fn improved_arguments_accept_bounded_capture_focus_observation_and_uppercase_keys() {
    let capture = parse_arguments::<CaptureDisplayInput>(Some(json_object(serde_json::json!({
        "display_id": "display",
        "max_width": 1600,
        "include_cursor": false
    }))))
    .unwrap();
    assert_eq!(capture.max_width, Some(1600));
    assert!(!capture.include_cursor);

    let focus = parse_arguments::<FocusWindowInput>(Some(json_object(serde_json::json!({
        "window_id": "target-12345678-1234-1234-1234-123456789ABC",
        "observation": {
            "mode": "screenshot",
            "region": {
                "display_id": "display",
                "x": 10,
                "y": 20,
                "width": 300,
                "height": 200
            }
        }
    }))))
    .unwrap();
    assert_eq!(focus.observation.region.unwrap().width, 300);

    let chord = parse_arguments::<PressKeysInput>(Some(json_object(serde_json::json!({
        "target_ref": "synthetic",
        "keys": ["CTRL", "L"]
    }))))
    .unwrap();
    assert_eq!(
        chord.keys,
        [controlfreak_core::Key::Ctrl, controlfreak_core::Key::L]
    );
}

#[test]
fn argument_errors_have_text_and_structured_content() {
    let result = tool_execution_error(&rmcp::ErrorData::invalid_params(
        "bad arguments",
        Some(serde_json::json!({ "reason": "unknown field" })),
    ));
    assert_eq!(result.is_error, Some(true));
    assert_eq!(result.content.len(), 1);
    assert_eq!(
        result.structured_content.unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[test]
fn pointer_result_contains_post_action_png() {
    let display = DisplayInfo {
        id: r"\\.\DISPLAY1".to_owned(),
        name: r"\\.\DISPLAY1".to_owned(),
        bounds: DisplayBounds {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
        },
        is_primary: true,
    };
    let action_result = PointerActionResult {
        position: MousePosition {
            display_id: display.id.clone(),
            local_x: 0,
            local_y: 0,
            virtual_x: 0,
            virtual_y: 0,
        },
        observation: ActionObservation {
            foreground_window: None,
            screenshot: Some(DisplayScreenshot {
                target_ref: None,
                source_bounds: display.bounds,
                display,
                png: b"\x89PNG\r\n\x1a\n".to_vec(),
                image_width: 1,
                image_height: 1,
                downscale_factor: 1,
                cursor_marker: false,
            }),
        },
    };
    let action = serde_json::json!({ "kind": "move" });
    let result = pointer_result(&action_result, &action);
    let serialized = serde_json::to_value(result).expect("serialize tool result");
    let encoded = serialized["content"][0]["data"]
        .as_str()
        .expect("image data");

    assert_eq!(serialized["content"][0]["type"], "image");
    assert_eq!(serialized["structuredContent"]["action"]["kind"], "move");
    assert_eq!(
        serialized["structuredContent"]["observation"]["screenshot"]["downscale_factor"],
        1
    );
    assert_eq!(
        serialized["structuredContent"]["observation"]["screenshot"]["source_bounds"]["width"],
        1
    );
    assert_eq!(
        STANDARD.decode(encoded).expect("valid base64"),
        b"\x89PNG\r\n\x1a\n"
    );
}

#[test]
fn pointer_result_without_screenshot_has_no_image_block() {
    let action_result = PointerActionResult {
        position: MousePosition {
            display_id: r"\\.\DISPLAY1".to_owned(),
            local_x: 10,
            local_y: 20,
            virtual_x: 10,
            virtual_y: 20,
        },
        observation: ActionObservation {
            foreground_window: None,
            screenshot: None,
        },
    };
    let result = pointer_result(&action_result, &serde_json::json!({ "kind": "move" }));
    let serialized = serde_json::to_value(result).expect("serialize tool result");

    assert!(
        serialized["content"]
            .as_array()
            .expect("content")
            .iter()
            .all(|content| content["type"] != "image")
    );
    assert_eq!(
        serialized["structuredContent"]["observation"]["screenshot"],
        serde_json::Value::Null
    );
}

#[test]
fn completed_action_with_failed_observation_is_not_retryable_error() {
    let result = tool_error(
        &controlfreak_core::PlatformError::PostActionObservationFailed {
            operation: "click_mouse".to_owned(),
            reason: "capture failed".to_owned(),
        },
    );
    let serialized = serde_json::to_value(result).expect("serialize tool result");

    assert_ne!(serialized["isError"], true);
    assert_eq!(
        serialized["structuredContent"]["status"],
        "completed_unverified"
    );
    assert_eq!(serialized["structuredContent"]["retry_action"], false);
}

#[test]
fn higher_integrity_targets_have_a_dedicated_structured_code() {
    let result = tool_error(&controlfreak_core::PlatformError::HigherIntegrityTarget {
        operation: "press_keys".to_owned(),
        process_id: 42,
        server_integrity: controlfreak_core::IntegrityLevel::Medium,
        target_integrity: controlfreak_core::IntegrityLevel::High,
    });
    let serialized = serde_json::to_value(result).expect("serialize tool result");

    assert_eq!(serialized["isError"], true);
    assert_eq!(
        serialized["structuredContent"]["error"]["code"],
        "higher_integrity_target"
    );
}
