//! Cache of finished translations: a bounded map, optionally over a directory
//! that survives the process.
//!
//! Every LSP request is answered from here: running the engine takes hundreds
//! of milliseconds, and a request handler is not allowed to wait that long
//! (AGENTS.md). The cache is what turns "translate on demand" into "look up
//! what a background task has already produced".
//!
//! IMPORTANT: this crate must keep building without an async runtime, so the
//! shared state is a `std::sync::Mutex` and never a `tokio` one. The lock is
//! only ever held for a hash lookup - never across the store's file IO - so no
//! `.await` can happen underneath it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{GlossKey, GlossStore};

/// How many translations to keep before the least recently used one is dropped.
///
/// A comment block is a sentence or two, so a few thousand entries cover every
/// file a reader is likely to have open while staying small in memory.
pub const DEFAULT_CAPACITY: usize = 4096;

/// A bounded, thread-safe map from [`GlossKey`] to translated text.
///
/// The key already carries the model version, so entries produced by a
/// different engine or a different set of weights can never be served after a
/// swap: they simply hash elsewhere.
///
/// Values are `Arc<str>` so that handing one to a request handler costs a
/// refcount bump rather than a copy.
///
/// With a [`GlossStore`] behind it the map becomes the hot half of a two-level
/// cache: a miss consults the directory, and what is found there is promoted
/// into the map so the next lookup is a hash lookup again. Both halves stay
/// infallible - a store that cannot be read or written is a miss, never an
/// error a request handler has to deal with.
#[derive(Debug)]
pub struct GlossCache {
    entries: Mutex<Entries>,
    capacity: usize,
    /// Where glosses outlive the process, when the server was given somewhere
    /// to put them.
    store: Option<GlossStore>,
}

#[derive(Debug, Default)]
struct Entries {
    map: HashMap<GlossKey, Entry>,
    /// Monotonic counter standing in for a clock. Comparing ticks is what
    /// makes "least recently used" meaningful without pulling in `Instant`,
    /// which is not available on every target this crate has to build for.
    tick: u64,
}

#[derive(Debug)]
struct Entry {
    value: Arc<str>,
    used_at: u64,
}

impl GlossCache {
    /// A cache holding at most `capacity` entries. A capacity of zero is
    /// raised to one: a cache that stores nothing would silently turn every
    /// lookup into a miss and re-run the engine forever.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            capacity: capacity.max(1),
            store: None,
        }
    }

    /// A cache of `capacity` entries in memory, over glosses kept in `store`.
    ///
    /// The store is not bounded by `capacity`: it holds what it was opened
    /// with, and evicting from memory does not delete from disk. That is the
    /// point - the map is what makes a lookup fast, the directory is what
    /// makes a restart free.
    pub fn with_store(capacity: usize, store: GlossStore) -> Self {
        Self {
            store: Some(store),
            ..Self::new(capacity)
        }
    }

    /// The directory glosses are kept in, if there is one.
    pub fn store(&self) -> Option<&GlossStore> {
        self.store.as_ref()
    }

    /// The translation stored under `key`, marking it as recently used.
    ///
    /// A miss in memory falls through to the store, which is a file read - on
    /// the order of microseconds, against the hundreds of milliseconds not
    /// finding it would cost. What comes back is promoted into the map.
    pub fn get(&self, key: &GlossKey) -> Option<Arc<str>> {
        if let Some(value) = self.remembered(key) {
            return Some(value);
        }

        let value: Arc<str> = Arc::from(self.store.as_ref()?.get(key)?);
        self.remember(*key, Arc::clone(&value));
        Some(value)
    }

    /// Whether `key` has a translation, without disturbing the eviction order.
    ///
    /// This is the question the background worker asks before queueing work,
    /// and answering it must not make an entry look freshly read.
    pub fn contains(&self, key: &GlossKey) -> bool {
        self.lock().map.contains_key(key)
            || self.store.as_ref().is_some_and(|store| store.contains(key))
    }

    /// Stores a translation, evicting the least recently used entry if the
    /// cache is full.
    ///
    /// A store that refuses the write is ignored: the gloss is still in memory
    /// and still correct, and a read-only cache directory must not turn into
    /// an error on the path that has just produced a translation.
    pub fn insert(&self, key: GlossKey, value: Arc<str>) {
        if let Some(store) = &self.store {
            let _ = store.insert(&key, &value);
        }
        self.remember(key, value);
    }

    /// Puts a gloss in the map alone, leaving the store as it is.
    fn remember(&self, key: GlossKey, value: Arc<str>) {
        let mut entries = self.lock();
        let tick = entries.next_tick();

        if entries.map.len() >= self.capacity && !entries.map.contains_key(&key) {
            entries.evict_least_recently_used();
        }
        entries.map.insert(
            key,
            Entry {
                value,
                used_at: tick,
            },
        );
    }

    /// The gloss the map holds for `key`, marking it as recently used.
    fn remembered(&self, key: &GlossKey) -> Option<Arc<str>> {
        let mut entries = self.lock();
        let tick = entries.next_tick();
        let entry = entries.map.get_mut(key)?;
        entry.used_at = tick;
        Some(Arc::clone(&entry.value))
    }

    /// How many translations are held **in memory**. The store, if there is
    /// one, has its own [`len`](GlossStore::len).
    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// A poisoned lock is recovered from rather than propagated: the only code
    /// under the lock is a hash lookup, so a panic elsewhere leaves the map
    /// intact, and losing the cache would take the whole server's usefulness
    /// with it.
    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for GlossCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl Entries {
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Linear scan for the oldest entry.
    ///
    /// It runs only when the cache is full, and only on the path that has just
    /// finished a model inference: a few thousand comparisons are invisible
    /// next to that, and they buy a cache with no linked list to keep correct.
    fn evict_least_recently_used(&mut self) {
        let victim = self
            .map
            .iter()
            .min_by_key(|(_, entry)| entry.used_at)
            .map(|(key, _)| *key);

        if let Some(victim) = victim {
            self.map.remove(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> GlossKey {
        GlossKey::new("passthrough-1", "en", "ja", text)
    }

    #[test]
    fn a_missing_key_is_a_miss() {
        let cache = GlossCache::default();
        assert!(cache.get(&key("Return the cached user.")).is_none());
        assert!(!cache.contains(&key("Return the cached user.")));
        assert!(cache.is_empty());
    }

    #[test]
    fn what_was_inserted_comes_back() {
        let cache = GlossCache::default();
        cache.insert(
            key("Return the cached user."),
            Arc::from("キャッシュされたユーザーを返す。"),
        );

        assert_eq!(
            cache.get(&key("Return the cached user.")).as_deref(),
            Some("キャッシュされたユーザーを返す。")
        );
        assert!(cache.contains(&key("Return the cached user.")));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn inserting_the_same_key_twice_replaces_the_value() {
        let cache = GlossCache::default();
        cache.insert(key("a"), Arc::from("first"));
        cache.insert(key("a"), Arc::from("second"));

        assert_eq!(cache.get(&key("a")).as_deref(), Some("second"));
        assert_eq!(cache.len(), 1);
    }

    /// Entries from another model version must not be served after a swap.
    #[test]
    fn a_different_model_version_does_not_hit() {
        let cache = GlossCache::default();
        cache.insert(
            GlossKey::new("passthrough-1", "en", "ja", "text"),
            Arc::from("old"),
        );

        assert!(
            cache
                .get(&GlossKey::new("fugumt-en-ja@1", "en", "ja", "text"))
                .is_none()
        );
    }

    #[test]
    fn the_least_recently_used_entry_is_the_one_evicted() {
        let cache = GlossCache::new(2);
        cache.insert(key("a"), Arc::from("A"));
        cache.insert(key("b"), Arc::from("B"));

        // Reading `a` makes `b` the oldest, so `b` is what the third insert
        // pushes out.
        assert!(cache.get(&key("a")).is_some());
        cache.insert(key("c"), Arc::from("C"));

        assert_eq!(cache.len(), 2);
        assert!(cache.contains(&key("a")));
        assert!(!cache.contains(&key("b")));
        assert!(cache.contains(&key("c")));
    }

    #[test]
    fn contains_does_not_count_as_a_use() {
        let cache = GlossCache::new(2);
        cache.insert(key("a"), Arc::from("A"));
        cache.insert(key("b"), Arc::from("B"));

        // If `contains` bumped recency, `a` would survive and `b` would go.
        assert!(cache.contains(&key("a")));
        cache.insert(key("c"), Arc::from("C"));

        assert!(!cache.contains(&key("a")));
    }

    #[test]
    fn a_zero_capacity_still_stores_one_entry() {
        let cache = GlossCache::new(0);
        assert_eq!(cache.capacity(), 1);

        cache.insert(key("a"), Arc::from("A"));
        assert_eq!(cache.get(&key("a")).as_deref(), Some("A"));
    }

    /// A restart is what the store exists for: a fresh cache over the same
    /// directory answers without the engine.
    #[test]
    fn a_cache_over_a_store_answers_from_it_after_a_restart() {
        let directory = std::env::temp_dir().join(format!(
            "codegloss-cache-{}-{}",
            std::process::id(),
            "restart"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let store = || crate::GlossStore::open(&directory, 64).expect("the directory is writable");

        let first = GlossCache::with_store(DEFAULT_CAPACITY, store());
        first.insert(key("Return the cached user."), Arc::from("訳"));

        let second = GlossCache::with_store(DEFAULT_CAPACITY, store());
        assert!(second.is_empty(), "nothing is in memory yet");
        assert!(second.contains(&key("Return the cached user.")));
        assert_eq!(
            second.get(&key("Return the cached user.")).as_deref(),
            Some("訳")
        );
        // The hit was promoted, so the next one is a map lookup.
        assert_eq!(second.len(), 1);

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Evicting from memory must not delete from disk: the map is bounded, the
    /// directory is not.
    #[test]
    fn an_entry_evicted_from_memory_is_still_on_disk() {
        let directory = std::env::temp_dir().join(format!(
            "codegloss-cache-{}-{}",
            std::process::id(),
            "eviction"
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let cache = GlossCache::with_store(
            1,
            crate::GlossStore::open(&directory, 64).expect("the directory is writable"),
        );

        cache.insert(key("a"), Arc::from("A"));
        cache.insert(key("b"), Arc::from("B"));
        assert_eq!(cache.len(), 1, "the map holds one");
        assert_eq!(cache.get(&key("a")).as_deref(), Some("A"));

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The LSP handlers read the cache while a worker thread writes it, so
    /// sharing one across threads has to compile and has to stay consistent.
    #[test]
    fn the_cache_can_be_shared_across_threads() {
        let cache = Arc::new(GlossCache::default());
        let writers: Vec<_> = (0..4)
            .map(|worker| {
                let cache = Arc::clone(&cache);
                std::thread::spawn(move || {
                    for index in 0..64 {
                        let text = format!("{worker}-{index}");
                        cache.insert(key(&text), Arc::from(text.as_str()));
                    }
                })
            })
            .collect();

        for writer in writers {
            writer.join().expect("no writer panicked");
        }

        assert_eq!(cache.len(), 4 * 64);
        assert_eq!(cache.get(&key("3-63")).as_deref(), Some("3-63"));
    }
}
