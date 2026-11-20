//! A cancelled `begin_control_session` undoes only what that request did: a cancelled renewal
//! leaves the live session as it was, and a cancelled first begin releases the desktop it took.

use super::stop_tests::WaitingBackend;
use super::tests::{ExclusiveArbitrator, wait_until};
use super::*;

const TARGET: &str = "target-00000000-0000-0000-0000-000000000003";

/// Runs once, inside the next `set_level`, while the begin holds the lifecycle lock.
type Hook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

struct HookedIndicator(Hook);

impl ActivityIndicator for HookedIndicator {
    fn set_level(&mut self, _: IndicatorLevel) -> Result<(), String> {
        let hook = self.0.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(())
    }
    fn hide(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    server: ControlFreakServer,
    runtime: Arc<SessionRuntime>,
    owned: Arc<AtomicBool>,
    hook: Hook,
}

impl Fixture {
    fn new() -> Self {
        let hook: Hook = Arc::default();
        let owned = Arc::new(AtomicBool::new(false));
        let runtime = Arc::new(SessionRuntime::new_with_timing(
            SafetyIndicator::dormant(),
            {
                let hook = Arc::clone(&hook);
                move || Ok(HookedIndicator(Arc::clone(&hook)))
            },
            Arc::new(ExclusiveArbitrator {
                owned: Arc::clone(&owned),
            }),
            GlowTiming {
                short_hold: Duration::from_secs(1),
                session_hold: Duration::from_secs(1),
                max_hold: Duration::from_mins(1),
            },
        ));
        let server = ControlFreakServer::with_indicator(
            Arc::new(WaitingBackend::idle()),
            SafetyIndicator::dormant(),
            Some(Arc::clone(&runtime)),
        );
        Self {
            server,
            runtime,
            owned,
            hook,
        }
    }

    fn on_next_set_level(&self, hook: impl FnOnce() + Send + 'static) {
        *self.hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn work(&self) -> stop::Work {
        stop::Work::new(&self.runtime.stop).unwrap()
    }

    /// Calls `begin_control_session` as the request `work`, whose cancellation the test holds.
    async fn begin(&self, expected_seconds: u64, work: stop::Work) -> CallToolResult {
        let arguments = json!({"target_ref": TARGET, "expected_seconds": expected_seconds});
        complete(
            stop::WORK
                .scope(
                    work,
                    self.server.begin_session(arguments.as_object().cloned()),
                )
                .await
                .unwrap(),
        )
    }

    /// The `session` object of `get_server_status`.
    fn session(&self) -> Value {
        let status = complete(self.server.server_status(None).unwrap());
        status.structured_content.unwrap()["session"].clone()
    }
}

fn complete(response: CallToolResponse) -> CallToolResult {
    let CallToolResponse::Complete(result) = response else {
        panic!("complete result expected");
    };
    result
}

fn until_close_ms(session: &Value) -> u64 {
    session["time_until_close_ms"].as_u64().unwrap()
}

#[tokio::test]
async fn a_cancelled_renewal_keeps_the_live_session_and_its_deadline() {
    let fixture = Fixture::new();
    let first = fixture.begin(2, fixture.work()).await;
    assert_ne!(first.is_error, Some(true), "{first:?}");
    let before = fixture.session();
    assert_eq!(before["state"], "armed");

    let renewal = fixture.work();
    let cancel = renewal.control.clone();
    fixture.on_next_set_level(move || cancel.cancel());
    let renewed = fixture.begin(60, renewal).await;
    assert_eq!(renewed.is_error, Some(true), "{renewed:?}");

    let after = fixture.session();
    assert_eq!(after["state"], "armed", "{after}");
    assert_eq!(after["owns_arbitration"], true, "{after}");
    assert_eq!(after["hold_ms"], 2_000, "{after}");
    assert!(until_close_ms(&after) <= until_close_ms(&before), "{after}");
    assert!(fixture.owned.load(Ordering::SeqCst));

    // The first session's close is still scheduled, so the hold still ends it.
    wait_until(
        "the first session's hold to end it",
        Duration::from_secs(10),
        || fixture.session()["state"] == "dormant",
    )
    .await;
    let closed = fixture.session();
    assert_eq!(closed["last_cleanup_reason"], "timeout", "{closed}");
    assert_eq!(closed["owns_arbitration"], false, "{closed}");
    assert!(!fixture.owned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_cancelled_first_begin_releases_the_desktop_it_took() {
    let fixture = Fixture::new();
    let work = fixture.work();
    let cancel = work.control.clone();
    let acquired = Arc::new(AtomicBool::new(false));
    fixture.on_next_set_level({
        let owned = Arc::clone(&fixture.owned);
        let acquired = Arc::clone(&acquired);
        move || {
            acquired.store(owned.load(Ordering::SeqCst), Ordering::SeqCst);
            cancel.cancel();
        }
    });
    let result = fixture.begin(60, work).await;
    assert_eq!(result.is_error, Some(true), "{result:?}");

    assert!(
        acquired.load(Ordering::SeqCst),
        "admission took the desktop"
    );
    assert!(!fixture.owned.load(Ordering::SeqCst), "and released it");
    let session = fixture.session();
    assert_eq!(session["state"], "dormant", "{session}");
    assert_eq!(session["owns_arbitration"], false, "{session}");
    assert_eq!(session["last_cleanup_reason"], "admission_failure");
    assert!(session["time_until_close_ms"].is_null(), "{session}");
}

#[tokio::test]
async fn a_begin_cancelled_by_a_user_stop_reports_the_stop() {
    for renewal in [false, true] {
        let fixture = Fixture::new();
        if renewal {
            let first = fixture.begin(60, fixture.work()).await;
            assert_ne!(first.is_error, Some(true), "{first:?}");
        }
        let stop = fixture.runtime.stop.clone();
        fixture.on_next_set_level(move || assert!(stop.stop_by_user()));
        let result = fixture.begin(60, fixture.work()).await;

        assert_eq!(result.is_error, Some(true), "{result:?}");
        let error = &result.structured_content.unwrap()["error"];
        assert_eq!(error["code"], "desktop_work_stopped", "renewal={renewal}");
        assert_eq!(error["retryable"], false);
        assert!(
            error["details"]["recovery"]["action"].is_string(),
            "the refusal carries the recovery step: {error}"
        );
        let session = fixture.session();
        assert_eq!(session["owns_arbitration"], false, "{session}");
        assert!(!fixture.owned.load(Ordering::SeqCst));
    }
}
