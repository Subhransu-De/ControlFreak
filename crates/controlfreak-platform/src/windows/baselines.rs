//! Visual baselines, removed when they expire.
//!
//! A baseline holds screen pixels, and PRIVACY.md promises that each one is kept for at most its
//! time to live. Lookups refuse expired entries, but an idle server makes no lookups, so a sweeper
//! thread removes each entry at its expiry. The sweeper exists only while the store holds an entry:
//! the insert that finds no sweeper starts one, and it exits when the store is empty or dropped.

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The time source of a store, injected so tests can move time without sleeping.
pub(super) trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;

    /// How long the sweeper waits, in real time, before it checks again for `deadline`, which is
    /// in this clock's time. A notification for shutdown can end the wait early.
    fn wait_for(&self, deadline: Instant) -> Duration;
}

pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait_for(&self, deadline: Instant) -> Duration {
        deadline.saturating_duration_since(Instant::now())
    }
}

struct Entry<T> {
    id: String,
    created_at: Instant,
    value: T,
}

struct State<T> {
    /// In insertion order, so the first entry expires first.
    entries: Vec<Entry<T>>,
    sweeper: Option<JoinHandle<()>>,
    closed: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    capacity: usize,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remove_expired(&self, state: &mut State<T>) {
        let now = self.clock.now();
        state
            .entries
            .retain(|entry| now.saturating_duration_since(entry.created_at) < self.ttl);
    }
}

/// At most `capacity` entries, each kept for at most `ttl`.
pub(super) struct BaselineStore<T: Send + 'static> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone + Send + 'static> BaselineStore<T> {
    pub(super) fn new(ttl: Duration, capacity: usize, clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    entries: Vec::new(),
                    sweeper: None,
                    closed: false,
                }),
                wake: Condvar::new(),
                clock,
                ttl,
                capacity,
            }),
        }
    }

    /// Stores `value`, evicting the oldest entry when the store is full. Fails, storing nothing,
    /// if the sweeper cannot start: the entry could then outlive its time to live.
    pub(super) fn insert(&self, id: String, value: T) -> std::io::Result<()> {
        let mut state = self.shared.lock();
        self.shared.remove_expired(&mut state);
        if state.sweeper.is_none() {
            let shared = Arc::clone(&self.shared);
            state.sweeper = Some(
                thread::Builder::new()
                    .name("controlfreak-baseline-expiry".to_owned())
                    .spawn(move || sweep(&shared))?,
            );
        }
        if state.entries.len() >= self.shared.capacity {
            state.entries.remove(0);
        }
        let created_at = self.shared.clock.now();
        state.entries.push(Entry {
            id,
            created_at,
            value,
        });
        Ok(())
    }

    /// The entry `id`, or `None` if it was never stored, was evicted, or has expired.
    pub(super) fn get(&self, id: &str) -> Option<T> {
        let mut state = self.shared.lock();
        self.shared.remove_expired(&mut state);
        state
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.value.clone())
    }

    #[cfg(test)]
    fn has_sweeper(&self) -> bool {
        self.shared.lock().sweeper.is_some()
    }
}

impl<T: Send + 'static> Drop for BaselineStore<T> {
    fn drop(&mut self) {
        let sweeper = {
            let mut state = self.shared.lock();
            state.closed = true;
            state.entries.clear();
            state.sweeper.take()
        };
        self.shared.wake.notify_all();
        // The sweeper only waits on `wake` or briefly for the lock, so it exits at once.
        if let Some(sweeper) = sweeper {
            let _ = sweeper.join();
        }
    }
}

/// Removes entries as they expire, sleeping until the earliest expiry, and exits when the store
/// is empty or closed. Inserts never make the earliest expiry sooner, so they need not wake it.
fn sweep<T>(shared: &Shared<T>) {
    let mut state = shared.lock();
    loop {
        if state.closed {
            return;
        }
        shared.remove_expired(&mut state);
        let Some(first) = state.entries.first() else {
            // Detaches this thread's own handle; the next insert starts a new sweeper.
            state.sweeper = None;
            return;
        };
        let wait = first
            .created_at
            .checked_add(shared.ttl)
            .map_or(shared.ttl, |deadline| shared.clock.wait_for(deadline));
        state = shared
            .wake
            .wait_timeout(state, wait)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::{BaselineStore, Clock, SystemClock};
    use std::{
        sync::{Arc, Mutex, PoisonError, Weak},
        thread,
        time::{Duration, Instant},
    };

    const TTL: Duration = Duration::from_mins(5);
    const JUST_BEFORE_TTL: Duration = Duration::from_millis(299_999);
    const BOUND: Duration = Duration::from_secs(5);

    /// Time moves only when a test advances it. The sweeper rechecks it every millisecond of real
    /// time, so a test sees a drop soon after advancing past an expiry.
    struct FakeClock(Mutex<Instant>);

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(Mutex::new(Instant::now())))
        }

        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) += by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn wait_for(&self, _deadline: Instant) -> Duration {
            Duration::from_millis(1)
        }
    }

    fn store(clock: &Arc<FakeClock>) -> BaselineStore<Arc<Vec<u8>>> {
        BaselineStore::new(TTL, 16, Arc::clone(clock) as Arc<dyn Clock>)
    }

    /// Stores a frame and returns a `Weak` that dies when the store drops the frame.
    fn insert_frame(store: &BaselineStore<Arc<Vec<u8>>>, id: &str) -> Weak<Vec<u8>> {
        let frame = Arc::new(vec![0_u8; 1024]);
        let weak = Arc::downgrade(&frame);
        store.insert(id.to_owned(), frame).expect("store a frame");
        weak
    }

    fn eventually(condition: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < BOUND {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        condition()
    }

    #[test]
    fn an_expired_frame_is_dropped_without_another_call() {
        let clock = FakeClock::new();
        let store = store(&clock);
        let frame = insert_frame(&store, "a");

        clock.advance(JUST_BEFORE_TTL);
        thread::sleep(Duration::from_millis(50));
        assert!(frame.upgrade().is_some(), "dropped before its expiry");

        clock.advance(Duration::from_millis(1));
        assert!(
            eventually(|| frame.upgrade().is_none()),
            "kept after its expiry"
        );
        assert!(eventually(|| !store.has_sweeper()), "sweeper kept running");
    }

    #[test]
    fn each_frame_is_dropped_at_its_own_expiry() {
        let clock = FakeClock::new();
        let store = store(&clock);
        let first = insert_frame(&store, "a");
        clock.advance(Duration::from_mins(1));
        let second = insert_frame(&store, "b");

        clock.advance(Duration::from_mins(4));
        assert!(eventually(|| first.upgrade().is_none()));
        thread::sleep(Duration::from_millis(50));
        assert!(second.upgrade().is_some(), "dropped before its expiry");
        assert!(store.has_sweeper(), "sweeper stopped with an entry left");

        clock.advance(Duration::from_mins(1));
        assert!(eventually(|| second.upgrade().is_none()));
        assert!(eventually(|| !store.has_sweeper()));
    }

    #[test]
    fn the_sweeper_runs_only_while_the_store_holds_an_entry() {
        let clock = FakeClock::new();
        let store = store(&clock);
        assert!(!store.has_sweeper());

        insert_frame(&store, "a");
        assert!(store.has_sweeper());
        clock.advance(TTL);
        assert!(eventually(|| !store.has_sweeper()));

        // A new entry arms a new sweeper.
        let frame = insert_frame(&store, "b");
        assert!(store.has_sweeper());
        clock.advance(TTL);
        assert!(eventually(|| frame.upgrade().is_none()));
        assert!(eventually(|| !store.has_sweeper()));
    }

    #[test]
    fn expired_and_unknown_ids_are_not_found() {
        let clock = FakeClock::new();
        let store = store(&clock);
        insert_frame(&store, "a");
        assert!(store.get("a").is_some());
        assert!(store.get("b").is_none());

        clock.advance(TTL);
        assert!(store.get("a").is_none());
    }

    #[test]
    fn a_full_store_evicts_its_oldest_frame() {
        let clock = FakeClock::new();
        let store = BaselineStore::new(TTL, 2, Arc::clone(&clock) as Arc<dyn Clock>);
        let oldest = insert_frame(&store, "a");
        insert_frame(&store, "b");
        insert_frame(&store, "c");

        assert!(oldest.upgrade().is_none());
        assert!(store.get("a").is_none());
        assert!(store.get("b").is_some());
        assert!(store.get("c").is_some());
    }

    #[test]
    fn dropping_the_store_stops_the_sweeper_and_drops_its_frames_at_once() {
        let store = BaselineStore::new(TTL, 16, Arc::new(SystemClock));
        let frame = insert_frame(&store, "a");
        let started = Instant::now();

        drop(store);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(frame.upgrade().is_none());
    }
}
