use super::*;

#[test]
fn stop_and_status_do_not_wait_for_a_provider_holding_runtime_state() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    let _state = runtime.state.lock().unwrap();
    runtime.stop.stop();
    assert_eq!(runtime.try_status()["state"], "busy");
    assert_eq!(runtime.try_status()["draining"], true);
}

#[tokio::test]
async fn cancelled_response_stays_draining_until_indicator_cleanup_returns() {
    struct BlockingHide {
        entered: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        finish: Arc<Mutex<mpsc::Receiver<()>>>,
    }
    impl ActivityIndicator for BlockingHide {
        fn set_level(&mut self, _: IndicatorLevel) -> Result<(), String> {
            Ok(())
        }
        fn hide(&mut self) -> Result<(), String> {
            self.entered
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            self.finish
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(())
        }
        fn shutdown(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
    let (entered, hiding) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let (finish, receiver) = mpsc::channel();
    let receiver = Arc::new(Mutex::new(receiver));
    let fixture = Arc::new(Fixture::default());
    let runtime = Arc::new(IndicatorRuntime::new_with_timing(
        SafetyIndicator::dormant(),
        move || {
            Ok(BlockingHide {
                entered: Arc::clone(&entered),
                finish: Arc::clone(&receiver),
            })
        },
        fixture.clone(),
        GlowTiming {
            short_hold: Duration::from_secs(10),
            session_hold: Duration::from_secs(10),
            max_hold: Duration::from_secs(10),
        },
    ));
    let lease = runtime.acquire_mutation_blocking().unwrap();
    let work = stop::Work::new(&runtime.stop).unwrap();
    let stopping = runtime.stop.clone();
    let task = tokio::spawn(stop::WORK.scope(work, async move {
        handlers::run_mutation_operation(
            Some(lease),
            "fixture".into(),
            Arc::new(stop_tests::WaitingBackend::idle()),
            move |_| {
                stopping.stop();
                Ok(())
            },
        )
        .await
    }));
    tokio::time::timeout(Duration::from_secs(5), hiding)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    let _ = task.await;
    assert_eq!(runtime.stop.status(), "draining");
    assert!(fixture.owned.load(Ordering::SeqCst));
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.stop.status() == "draining" {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.owned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn stop_keeps_blocked_worker_ownership_until_cleanup_finishes() {
    for cleanup_failed in [false, true] {
        let fixture = Arc::new(Fixture::default());
        let runtime = fixture.runtime();
        let lease = runtime.acquire_mutation_blocking().unwrap();
        let work = stop::Work::new(&runtime.stop).unwrap();
        let control = work.control.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, drain) = mpsc::channel();
        let task = tokio::spawn(stop::WORK.scope(work, async move {
            let _cancel = stop::CancelOnDrop(control);
            handlers::run_mutation_operation(
                Some(lease),
                "fixture".into(),
                Arc::new(stop_tests::WaitingBackend::idle()),
                move |control| {
                    control.dispatch_accepted(1);
                    control.cleanup_status(controlfreak_core::CleanupStatus::Unknown);
                    started.send(()).unwrap();
                    drain.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(control.is_cancelled());
                    if !cleanup_failed {
                        control.cleanup_status(controlfreak_core::CleanupStatus::Succeeded);
                    }
                    control.check("synthetic_drag")
                },
            )
            .await
        }));
        ready.await.unwrap();
        assert!(runtime.stop.session_active());
        assert!(runtime.stop.stop_by_user());
        assert_eq!(runtime.stop.status(), "draining");
        assert!(runtime.acquire_mutation_blocking().is_err());
        assert!(fixture.owned.load(Ordering::SeqCst));
        // Simulate rmcp dropping a cancelled request future while its native call drains.
        task.abort();
        let _ = task.await;
        assert_eq!(runtime.stop.status(), "draining");
        finish.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while runtime.stop.status() == "draining" {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        runtime.check_lifecycle();
        assert_eq!(
            runtime.stop.status(),
            if cleanup_failed {
                "cleanup_failed"
            } else {
                "stopped"
            }
        );
        assert_eq!(fixture.owned.load(Ordering::SeqCst), cleanup_failed);
        assert_eq!(runtime.stop.session_active(), cleanup_failed);
        assert!(runtime.stop.user_stopped());
        assert!(runtime.begin_session(None, "fixture").is_err());
    }
}

#[test]
fn stop_during_helper_startup_refuses_admission_after_helper_returns() {
    let fixture = Arc::new(Fixture::default());
    let mut runtime = fixture.runtime();
    let stop = runtime.stop.clone();
    let indicator_fixture = Arc::clone(&fixture);
    Arc::get_mut(&mut runtime).unwrap().starter = Arc::new(move || {
        stop.stop();
        Ok(Box::new(Indicator(Arc::clone(&indicator_fixture))))
    });
    assert!(runtime.acquire_mutation_blocking().is_err());
    assert_eq!(runtime.status()["active_mutations"], 0);
    assert!(!fixture.owned.load(Ordering::SeqCst));
}
use controlfreak_core::{
    BackendMetadata, DisplayBackend, KeyboardBackend, OcrBackend, PointerBackend, WindowBackend,
};

struct Environment(Arc<Mutex<Option<&'static str>>>);
impl BackendMetadata for Environment {
    fn identity(&self) -> controlfreak_core::BackendIdentity {
        unreachable!()
    }
    fn capabilities(&self) -> Vec<controlfreak_core::CapabilityDescriptor> {
        vec![]
    }
    fn permissions(&self) -> Vec<controlfreak_core::PermissionDescriptor> {
        vec![]
    }
    fn check_control_environment(&self) -> Result<(), PlatformError> {
        self.0.lock().unwrap().map_or(Ok(()), |reason| {
            Err(PlatformError::Unavailable {
                reason: reason.to_owned(),
            })
        })
    }
}
impl DisplayBackend for Environment {}
impl KeyboardBackend for Environment {}
impl OcrBackend for Environment {}
impl PointerBackend for Environment {}
impl WindowBackend for Environment {}

#[derive(Default)]
struct Fixture {
    environment: Arc<Mutex<Option<&'static str>>>,
    shutdown_failed: Arc<AtomicBool>,
    owned: Arc<AtomicBool>,
    releases: AtomicUsize,
    shutdowns: AtomicUsize,
}
impl ActivityArbitrator for Fixture {
    fn try_acquire(&self) -> Result<(), ArbitrationBusy> {
        assert!(!self.owned.swap(true, Ordering::SeqCst));
        Ok(())
    }
    fn release(&self) {
        assert!(self.owned.swap(false, Ordering::SeqCst));
        self.releases.fetch_add(1, Ordering::SeqCst);
    }
}
struct Indicator(Arc<Fixture>);
impl ActivityIndicator for Indicator {
    fn set_level(&mut self, _: IndicatorLevel) -> Result<(), String> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), String> {
        self.0.shutdowns.fetch_add(1, Ordering::SeqCst);
        if self.0.shutdown_failed.load(Ordering::SeqCst) {
            Err("still draining".to_owned())
        } else {
            Ok(())
        }
    }
}
impl Fixture {
    fn runtime(self: &Arc<Self>) -> Arc<IndicatorRuntime> {
        let fixture = Arc::clone(self);
        let mut runtime = IndicatorRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            move || Ok(Indicator(Arc::clone(&fixture))),
            Arc::clone(self) as Arc<dyn ActivityArbitrator>,
            GlowTiming {
                short_hold: Duration::from_mins(1),
                session_hold: Duration::from_mins(1),
                max_hold: Duration::from_mins(1),
            },
        );
        runtime.environment = Some(Arc::new(Environment(Arc::clone(&self.environment))));
        Arc::new(runtime)
    }
}

#[test]
fn concurrent_cleanup_waits_for_every_mutation_and_preserves_the_reason() {
    for reason in ["disconnect", "shutdown", "explicit_end"] {
        let fixture = Arc::new(Fixture::default());
        let runtime = fixture.runtime();
        let first = runtime.acquire_mutation_blocking().unwrap();
        let second = runtime.acquire_mutation_blocking().unwrap();
        thread::scope(|scope| {
            for _ in 0..4 {
                let runtime = &runtime;
                scope.spawn(move || {
                    runtime
                        .close_session(reason, reason != "explicit_end")
                        .unwrap();
                });
            }
        });
        assert_eq!(runtime.status()["draining"], true);
        assert!(runtime.acquire_mutation_blocking().is_err());
        assert_eq!(
            first.mutation_control().is_cancelled(),
            reason != "explicit_end"
        );
        drop(first);
        assert!(fixture.owned.load(Ordering::SeqCst));
        // This guard represents backend work, including compensating releases.
        drop(second);
        assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.status()["draining"], false);
        assert_eq!(runtime.status()["last_cleanup_reason"], reason);
        runtime.end_session().unwrap();
        assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
        if reason != "explicit_end" {
            assert!(runtime.begin_session(None, "synthetic").is_err());
        }
    }
}

#[test]
fn desktop_changes_cancel_active_work_and_refuse_new_admission() {
    for reason in [
        "locked",
        "inactive",
        "non-default desktop",
        "environment query failed",
    ] {
        let fixture = Arc::new(Fixture::default());
        let runtime = fixture.runtime();
        let lease = runtime.acquire_mutation_blocking().unwrap();
        *fixture.environment.lock().unwrap() = Some(reason);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !lease.mutation_control().is_cancelled() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(lease.mutation_control().is_cancelled());
        assert!(runtime.begin_session(None, "synthetic").is_err());
        assert!(runtime.acquire_mutation_blocking().is_err());
        assert_eq!(runtime.status()["draining"], true);
        drop(lease);
        assert!(!fixture.owned.load(Ordering::SeqCst));
        assert!(
            runtime.status()["last_cleanup_reason"]
                .as_str()
                .unwrap()
                .contains(reason)
        );
        *fixture.environment.lock().unwrap() = None;
        runtime.begin_session(None, "synthetic").unwrap();
        runtime.close_session("shutdown", true).unwrap();
    }
}

#[test]
fn stale_timeout_cannot_close_a_new_session() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    runtime.begin_session(None, "synthetic").unwrap();
    let generation = runtime.state.lock().unwrap().generation;
    runtime.begin_session(None, "synthetic").unwrap();
    runtime.close_if_idle(generation);
    assert!(fixture.owned.load(Ordering::SeqCst));
    let generation = runtime.state.lock().unwrap().generation;
    runtime.close_if_idle(generation);
    assert_eq!(runtime.status()["last_cleanup_reason"], "timeout");
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_shutdown_keeps_arbitration_until_retry_is_safe() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    runtime.begin_session(None, "synthetic").unwrap();
    fixture.shutdown_failed.store(true, Ordering::SeqCst);
    assert!(runtime.close_session("shutdown", true).is_err());
    assert_eq!(runtime.status()["draining"], true);
    assert!(fixture.owned.load(Ordering::SeqCst));
    assert!(runtime.acquire_mutation_blocking().is_err());
    fixture.shutdown_failed.store(false, Ordering::SeqCst);
    runtime.check_lifecycle();
    assert_eq!(runtime.status()["draining"], false);
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
    assert!(fixture.shutdowns.load(Ordering::SeqCst) >= 2);
}

#[test]
fn dropping_server_cleanup_closes_admission_before_worker_completion() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    let lease = runtime.acquire_mutation_blocking().unwrap();
    drop(ServerCleanup(Some(Arc::clone(&runtime))));
    assert!(lease.mutation_control().is_cancelled());
    assert_eq!(runtime.status()["last_cleanup_reason"], "shutdown");
    assert!(runtime.acquire_mutation_blocking().is_err());
    drop(lease);
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn desktop_is_revalidated_after_indicator_startup() {
    for explicit in [false, true] {
        let fixture = Arc::new(Fixture::default());
        let mut runtime = fixture.runtime();
        let fixture_for_start = Arc::clone(&fixture);
        Arc::get_mut(&mut runtime).unwrap().starter = Arc::new(move || {
            *fixture_for_start.environment.lock().unwrap() = Some("locked during startup");
            Ok(Box::new(Indicator(Arc::clone(&fixture_for_start))))
        });
        if explicit {
            assert!(runtime.begin_session(None, "synthetic").is_err());
        } else {
            assert!(runtime.acquire_mutation_blocking().is_err());
        }
        assert_eq!(runtime.status()["active_mutations"], 0);
        assert!(!fixture.owned.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn eof_closes_admission_before_response_drain() {
    use tokio::io::AsyncReadExt;
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    let lease = runtime.acquire_mutation_blocking().unwrap();
    let mut reader = DisconnectReader {
        inner: &b"x"[..],
        runtime: Some(Arc::clone(&runtime)),
    };
    assert_eq!(reader.read(&mut []).await.unwrap(), 0);
    assert_eq!(reader.read(&mut [0_u8; 1]).await.unwrap(), 1);
    assert!(!runtime.terminated.load(Ordering::Acquire));
    assert_eq!(reader.read(&mut [0_u8; 1]).await.unwrap(), 0);
    assert!(runtime.acquire_mutation_blocking().is_err());
    // Response draining can continue while the mutation retains ownership.
    assert!(fixture.owned.load(Ordering::SeqCst));
    runtime.close_session("disconnect", true).unwrap();
    assert!(lease.mutation_control().is_cancelled());
    drop(lease);
    assert_eq!(runtime.status()["last_cleanup_reason"], "disconnect");
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn persistent_shutdown_failure_has_a_bounded_wait_without_releasing_ownership() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    runtime.begin_session(None, "synthetic").unwrap();
    fixture.shutdown_failed.store(true, Ordering::SeqCst);
    assert!(runtime.close_session("shutdown", true).is_err());
    let error = wait_for_cleanup(&runtime, Duration::from_millis(1))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let (dropped, receiver) = tokio::sync::oneshot::channel();
    thread::spawn(move || {
        drop(runtime);
        dropped.send(()).unwrap();
    });
    let result = tokio::time::timeout(Duration::from_secs(5), receiver).await;
    let retained = fixture.owned.load(Ordering::SeqCst);
    // Always let cleanup finish, including when an assertion fails.
    fixture.shutdown_failed.store(false, Ordering::SeqCst);
    assert!(
        matches!(result, Ok(Ok(()))),
        "runtime Drop blocked on failed indicator shutdown"
    );
    assert!(retained);
    tests::wait_until(
        "cleanup to release desktop ownership",
        Duration::from_secs(5),
        || !fixture.owned.load(Ordering::SeqCst),
    )
    .await;
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
}
