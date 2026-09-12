use super::*;
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
        assert_eq!(first.control.is_cancelled(), reason != "explicit_end");
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
            assert!(runtime.begin_session(None).is_err());
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
        while !lease.control.is_cancelled() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(lease.control.is_cancelled());
        assert!(runtime.begin_session(None).is_err());
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
        runtime.begin_session(None).unwrap();
        runtime.close_session("shutdown", true).unwrap();
    }
}

#[test]
fn stale_timeout_cannot_close_a_new_session() {
    let fixture = Arc::new(Fixture::default());
    let runtime = fixture.runtime();
    runtime.begin_session(None).unwrap();
    let generation = runtime.state.lock().unwrap().generation;
    runtime.begin_session(None).unwrap();
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
    runtime.begin_session(None).unwrap();
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
    assert!(lease.control.is_cancelled());
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
            assert!(runtime.begin_session(None).is_err());
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
    assert!(lease.control.is_cancelled());
    drop(lease);
    assert_eq!(runtime.status()["last_cleanup_reason"], "disconnect");
    assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
}
