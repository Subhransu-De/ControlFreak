//! Visual baselines, removed when they expire.
//!
//! A baseline holds screen pixels, and PRIVACY.md promises that each one is kept for at most its
//! time to live. Lookups refuse expired entries, but an idle server makes no lookups, so a sweeper
//! thread removes each entry at its expiry. The sweeper exists only while the store holds an entry:
//! the insert that finds no sweeper starts one, and it exits when the store is empty or dropped.
//!
//! A removed entry leaves a tombstone, its ID and why it went (expired or evicted) and nothing
//! else, so a later lookup can say why the ID no longer works. The newest tombstones are kept, at
//! most a fixed number, and an ID with no entry and no tombstone is unknown.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use controlfreak_core::BaselineFailure;

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

/// What the store holds after an insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Usage {
    pub(super) count: usize,
    /// The sum of the `bytes` given for each held entry.
    pub(super) bytes: u64,
}

struct Entry<T> {
    id: String,
    created_at: Instant,
    bytes: u64,
    value: T,
}

/// A removed entry's ID and why it was removed. It never holds the entry's value.
struct Tombstone {
    id: String,
    cause: BaselineFailure,
}

struct State<T> {
    /// In insertion order, so the first entry expires first.
    entries: Vec<Entry<T>>,
    /// Oldest first, at most `Shared::tombstones` long.
    tombstones: VecDeque<Tombstone>,
    sweeper: Option<JoinHandle<()>>,
    closed: bool,
}

impl<T> State<T> {
    fn bury(&mut self, id: String, cause: BaselineFailure, capacity: usize) {
        if capacity == 0 {
            return;
        }
        while self.tombstones.len() >= capacity {
            self.tombstones.pop_front();
        }
        self.tombstones.push_back(Tombstone { id, cause });
    }
}

struct Shared<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    capacity: usize,
    tombstones: usize,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remove_expired(&self, state: &mut State<T>) {
        let now = self.clock.now();
        // Entries are in insertion order and the clock does not go back, so the expired ones
        // are a prefix.
        let expired = state
            .entries
            .iter()
            .take_while(|entry| now.saturating_duration_since(entry.created_at) >= self.ttl)
            .count();
        let removed: Vec<Entry<T>> = state.entries.drain(..expired).collect();
        for entry in removed {
            state.bury(entry.id, BaselineFailure::Expired, self.tombstones);
        }
    }
}

/// At most `capacity` entries, each kept for at most `ttl`, and tombstones for at most
/// `tombstones` removed entries.
pub(super) struct BaselineStore<T: Send + 'static> {
    shared: Arc<Shared<T>>,
}

impl<T: Clone + Send + 'static> BaselineStore<T> {
    pub(super) fn new(
        ttl: Duration,
        capacity: usize,
        tombstones: usize,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    entries: Vec::new(),
                    tombstones: VecDeque::with_capacity(tombstones),
                    sweeper: None,
                    closed: false,
                }),
                wake: Condvar::new(),
                clock,
                ttl,
                capacity,
                tombstones,
            }),
        }
    }

    /// Stores `value`, which holds `bytes` bytes, evicting the oldest entry when the store is
    /// full, and returns what the store then holds. Fails, storing nothing, if the sweeper
    /// cannot start: the entry could then outlive its time to live.
    pub(super) fn insert(&self, id: String, value: T, bytes: u64) -> std::io::Result<Usage> {
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
        if state.entries.len() >= self.shared.capacity && !state.entries.is_empty() {
            let evicted = state.entries.remove(0);
            state.bury(evicted.id, BaselineFailure::Evicted, self.shared.tombstones);
        }
        let created_at = self.shared.clock.now();
        state.entries.push(Entry {
            id,
            created_at,
            bytes,
            value,
        });
        Ok(Usage {
            count: state.entries.len(),
            bytes: state.entries.iter().map(|entry| entry.bytes).sum(),
        })
    }

    /// The entry `id`, or why there is none: it expired, it was evicted, or the store has no
    /// record of it.
    pub(super) fn get(&self, id: &str) -> Result<T, BaselineFailure> {
        let mut state = self.shared.lock();
        self.shared.remove_expired(&mut state);
        if let Some(entry) = state.entries.iter().find(|entry| entry.id == id) {
            return Ok(entry.value.clone());
        }
        Err(state
            .tombstones
            .iter()
            .find(|tombstone| tombstone.id == id)
            .map_or(BaselineFailure::Unknown, |tombstone| tombstone.cause))
    }

    #[cfg(test)]
    fn has_sweeper(&self) -> bool {
        self.shared.lock().sweeper.is_some()
    }

    #[cfg(test)]
    fn tombstone_count(&self) -> usize {
        self.shared.lock().tombstones.len()
    }
}

impl<T: Send + 'static> Drop for BaselineStore<T> {
    fn drop(&mut self) {
        let sweeper = {
            let mut state = self.shared.lock();
            state.closed = true;
            state.entries.clear();
            state.tombstones.clear();
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

/// Time moves only when a test advances it. The sweeper rechecks it every millisecond of real
/// time, so a test sees a drop soon after advancing past an expiry.
#[cfg(test)]
pub(super) struct FakeClock(Mutex<Instant>);

#[cfg(test)]
impl FakeClock {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Instant::now())))
    }

    pub(super) fn advance(&self, by: Duration) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_for(&self, _deadline: Instant) -> Duration {
        Duration::from_millis(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{BaselineStore, Clock, FakeClock, SystemClock, Usage};
    use controlfreak_core::BaselineFailure;
    use std::{
        sync::{Arc, Weak},
        thread,
        time::{Duration, Instant},
    };

    const TTL: Duration = Duration::from_mins(5);
    const JUST_BEFORE_TTL: Duration = Duration::from_millis(299_999);
    const BOUND: Duration = Duration::from_secs(5);
    const FRAME_BYTES: usize = 1024;
    const TOMBSTONES: usize = 64;

    fn store(clock: &Arc<FakeClock>) -> BaselineStore<Arc<Vec<u8>>> {
        BaselineStore::new(TTL, 16, TOMBSTONES, Arc::clone(clock) as Arc<dyn Clock>)
    }

    /// Stores a frame and returns a `Weak` that dies when the store drops the frame.
    fn insert_frame(store: &BaselineStore<Arc<Vec<u8>>>, id: &str) -> Weak<Vec<u8>> {
        insert_counted(store, id).0
    }

    fn insert_counted(store: &BaselineStore<Arc<Vec<u8>>>, id: &str) -> (Weak<Vec<u8>>, Usage) {
        let frame = Arc::new(vec![0_u8; FRAME_BYTES]);
        let weak = Arc::downgrade(&frame);
        let usage = store
            .insert(id.to_owned(), frame, FRAME_BYTES as u64)
            .expect("store a frame");
        (weak, usage)
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
        // Only the sweeper removed it, and the lookup still knows why.
        assert_eq!(store.get("a").err(), Some(BaselineFailure::Expired));
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
    fn an_id_older_than_the_lifetime_is_expired_and_an_unissued_one_is_unknown() {
        let clock = FakeClock::new();
        let store = store(&clock);
        insert_frame(&store, "a");
        assert!(store.get("a").is_ok());
        assert_eq!(store.get("b").err(), Some(BaselineFailure::Unknown));

        clock.advance(JUST_BEFORE_TTL);
        assert!(store.get("a").is_ok(), "refused before its expiry");
        clock.advance(Duration::from_millis(1));
        assert_eq!(store.get("a").err(), Some(BaselineFailure::Expired));
        assert_eq!(store.get("b").err(), Some(BaselineFailure::Unknown));
    }

    #[test]
    fn the_seventeenth_capture_evicts_the_first() {
        let clock = FakeClock::new();
        let store = store(&clock);
        let ids: Vec<String> = (1..=17).map(|n| format!("id-{n}")).collect();
        let mut frames = Vec::new();
        for (n, id) in ids.iter().enumerate().take(16) {
            let (frame, usage) = insert_counted(&store, id);
            frames.push(frame);
            assert_eq!(usage.count, n + 1);
            assert_eq!(usage.bytes, ((n + 1) * FRAME_BYTES) as u64);
        }
        assert!(store.get(&ids[0]).is_ok());

        let (_, usage) = insert_counted(&store, &ids[16]);
        assert_eq!(usage.count, 16, "the count stays at capacity");
        assert_eq!(usage.bytes, (16 * FRAME_BYTES) as u64);
        assert!(
            frames[0].upgrade().is_none(),
            "the evicted pixels were kept"
        );
        assert_eq!(store.get(&ids[0]).err(), Some(BaselineFailure::Evicted));
        for id in &ids[1..] {
            assert!(store.get(id).is_ok(), "{id}");
        }

        // An evicted ID stays evicted after the time it would have expired.
        clock.advance(TTL);
        assert_eq!(store.get(&ids[0]).err(), Some(BaselineFailure::Evicted));
        assert_eq!(store.get(&ids[1]).err(), Some(BaselineFailure::Expired));
    }

    #[test]
    fn tombstones_are_bounded_and_keep_no_pixels() {
        let clock = FakeClock::new();
        let store = store(&clock);
        let mut frames = Vec::new();
        for n in 1..=100 {
            frames.push(insert_frame(&store, &format!("id-{n}")));
        }
        // 84 were evicted; only the newest 64 tombstones remain.
        assert_eq!(store.tombstone_count(), TOMBSTONES);
        assert!(frames[..84].iter().all(|frame| frame.upgrade().is_none()));
        assert_eq!(store.get("id-20").err(), Some(BaselineFailure::Unknown));
        assert_eq!(store.get("id-21").err(), Some(BaselineFailure::Evicted));
        assert_eq!(store.get("id-84").err(), Some(BaselineFailure::Evicted));
        assert!(store.get("id-85").is_ok());

        clock.advance(TTL);
        assert!(eventually(|| frames
            .iter()
            .all(|frame| frame.upgrade().is_none())));
        assert_eq!(store.tombstone_count(), TOMBSTONES);
        assert_eq!(store.get("id-85").err(), Some(BaselineFailure::Expired));
        assert_eq!(store.get("id-36").err(), Some(BaselineFailure::Unknown));
        assert_eq!(store.get("id-37").err(), Some(BaselineFailure::Evicted));
    }

    #[test]
    fn dropping_the_store_stops_the_sweeper_and_drops_its_frames_at_once() {
        let store = BaselineStore::new(TTL, 16, TOMBSTONES, Arc::new(SystemClock));
        let frame = insert_frame(&store, "a");
        let started = Instant::now();

        drop(store);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(frame.upgrade().is_none());
    }
}
