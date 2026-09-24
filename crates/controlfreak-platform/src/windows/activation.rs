use controlfreak_core::PlatformError;

pub(super) trait Activation {
    /// Validate identity, desktop and both target and foreground integrity on every poll.
    fn validate(&mut self) -> Result<bool, PlatformError>;
    fn activate(&mut self) -> Result<bool, PlatformError>;
    fn elapsed_ms(&self) -> u64;
    fn wait(&mut self, milliseconds: u64);
}

/// Poll every 10 ms; retry at 0, 50, 150, 350 ms, then every 200 ms.
/// Never retry after an accepted activation, or after validation fails.
pub(super) fn focus(
    driver: &mut impl Activation,
    timeout_ms: u32,
) -> Result<(u32, u64), PlatformError> {
    if !(100..=5_000).contains(&timeout_ms) {
        return Err(PlatformError::InvalidArgument {
            argument: "timeout_ms".into(),
            reason: "must be between 100 and 5000".into(),
        });
    }
    let mut attempts = 0;
    let mut accepted = false;
    let mut next_attempt = 0;
    let mut backoff = 50;
    loop {
        if driver.validate()? {
            return Ok((attempts, driver.elapsed_ms()));
        }
        let elapsed_ms = driver.elapsed_ms();
        if elapsed_ms >= u64::from(timeout_ms) {
            return Err(PlatformError::ActivationFailed {
                reason: if accepted {
                    "settle_timeout"
                } else {
                    "activation_refused"
                }
                .into(),
                attempts,
                elapsed_ms,
            });
        }
        if !accepted && elapsed_ms >= next_attempt {
            attempts += 1;
            accepted = driver.activate()?;
            next_attempt = driver.elapsed_ms() + backoff;
            backoff = (backoff * 2).min(200);
        }
        driver.wait(10.min(u64::from(timeout_ms).saturating_sub(driver.elapsed_ms())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        time: u64,
        calls: Vec<u64>,
        accept_at: u64,
        foreground_at: u64,
        invalid_at: u64,
    }
    impl Activation for Fake {
        fn validate(&mut self) -> Result<bool, PlatformError> {
            if self.time >= self.invalid_at {
                return Err(PlatformError::TargetInvalidated {
                    reason: "synthetic focus theft".into(),
                });
            }
            Ok(self.time >= self.foreground_at)
        }
        fn activate(&mut self) -> Result<bool, PlatformError> {
            self.calls.push(self.time);
            Ok(self.time >= self.accept_at)
        }
        fn elapsed_ms(&self) -> u64 {
            self.time
        }
        fn wait(&mut self, milliseconds: u64) {
            self.time += milliseconds;
        }
    }
    fn fixture() -> Fake {
        Fake {
            time: 0,
            calls: vec![],
            accept_at: 150,
            foreground_at: 700,
            invalid_at: u64::MAX,
        }
    }
    #[test]
    fn retries_refusal_then_waits_for_slow_activation() {
        let mut driver = fixture();
        assert_eq!(focus(&mut driver, 1_000).unwrap(), (3, 700));
        assert_eq!(driver.calls, [0, 50, 150]);
    }
    #[test]
    fn bounded_refusal_and_settle_timeout_are_distinct() {
        for accepted in [false, true] {
            let mut driver = fixture();
            driver.accept_at = if accepted { 0 } else { u64::MAX };
            driver.foreground_at = u64::MAX;
            let error = focus(&mut driver, 500).unwrap_err();
            assert!(
                matches!(error, PlatformError::ActivationFailed { reason, elapsed_ms: 500, .. }
                if reason == if accepted { "settle_timeout" } else { "activation_refused" })
            );
            assert_eq!(driver.calls.len(), if accepted { 1 } else { 4 });
        }
    }
    #[test]
    fn invalidation_stops_before_another_activation() {
        for invalid_at in [0, 40, 100, 400] {
            let mut driver = fixture();
            driver.invalid_at = invalid_at;
            assert!(matches!(
                focus(&mut driver, 1_000),
                Err(PlatformError::TargetInvalidated { .. })
            ));
            assert!(driver.calls.iter().all(|time| *time < invalid_at));
        }
    }
    #[test]
    fn validates_budget_before_any_attempt() {
        for timeout in [0, 99, 5_001, u32::MAX] {
            let mut driver = fixture();
            assert!(focus(&mut driver, timeout).is_err());
            assert!(driver.calls.is_empty());
        }
    }
}
