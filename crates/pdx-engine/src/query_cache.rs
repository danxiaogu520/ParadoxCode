//! Snapshot-scoped lazy cache for higher-layer query results.
//!
//! `AnalysisSnapshot` values are immutable and cheaply cloned across worker threads, but the
//! analysis layer recomputes per-document semantic extraction on every request. This module
//! provides a bounded, revision-keyed cache that is owned by the snapshot infrastructure and
//! shared by all clones, so results are computed once per (revision, key) and reused by every
//! query worker observing the same revision.
//!
//! The engine intentionally stores opaque values (`Arc<dyn Any>`): the cache is a mechanism
//! only. Contents belong to higher layers (currently `pdx-analysis`). Entries from older
//! revisions are discarded as soon as a newer revision is observed; an old worker that finishes
//! later cannot repopulate the cache with stale data.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Bounded snapshot-scoped cache keyed by `(revision, key)`.
///
/// Entries are immutable: a key is only ever inserted once per revision, and the owning
/// snapshot guarantees that all callers observing that revision see the same inputs. When the
/// capacity is exceeded the cache is cleared wholesale. Only entries for the newest observed
/// revision are retained, because an older immutable snapshot can always recompute a miss.
pub struct SnapshotQueryCache {
    state: Mutex<CacheState>,
    capacity: usize,
}

struct CacheState {
    revision: Option<u64>,
    entries: BTreeMap<String, Arc<dyn Any + Send + Sync>>,
}

impl SnapshotQueryCache {
    /// Creates a cache with a conservative entry bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    /// Creates a cache with an explicit entry bound.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: Mutex::new(CacheState {
                revision: None,
                entries: BTreeMap::new(),
            }),
            capacity,
        }
    }

    /// Returns the cached value for `(revision, key)` when it was inserted as `T`.
    pub fn get<T: Send + Sync + 'static>(&self, revision: u64, key: &str) -> Option<Arc<T>> {
        let state = self
            .state
            .lock()
            .expect("snapshot query cache lock poisoned");
        if state.revision != Some(revision) {
            return None;
        }
        state
            .entries
            .get(key)
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// Stores `value` under `(revision, key)`; a key that already exists is never replaced.
    pub fn insert<T: Send + Sync + 'static>(&self, revision: u64, key: String, value: Arc<T>) {
        let mut state = self
            .state
            .lock()
            .expect("snapshot query cache lock poisoned");
        match state.revision {
            Some(current) if revision < current => return,
            Some(current) if revision != current => {
                state.entries.clear();
                state.revision = Some(revision);
            }
            None => state.revision = Some(revision),
            _ => {}
        }
        if state.entries.len() >= self.capacity && !state.entries.contains_key(&key) {
            state.entries.clear();
        }
        state.entries.entry(key).or_insert(value);
    }

    /// Advances the cache to a committed workspace revision and drops older query results.
    pub fn advance_to(&self, revision: u64) {
        let mut state = self
            .state
            .lock()
            .expect("snapshot query cache lock poisoned");
        match state.revision {
            Some(current) if revision > current => {
                state.entries.clear();
                state.revision = Some(revision);
            }
            None => state.revision = Some(revision),
            _ => {}
        }
    }

    /// Returns the number of cached entries (for diagnostics and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("snapshot query cache lock poisoned")
            .entries
            .len()
    }

    /// Returns whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SnapshotQueryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SnapshotQueryCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotQueryCache")
            .field("entries", &self.len())
            .field("capacity", &self.capacity)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_immutable_and_capacity_is_bounded() {
        let cache = SnapshotQueryCache::with_capacity(2);
        assert!(cache.get::<u32>(1, "key").is_none());
        cache.insert(1, "key".to_owned(), Arc::new(7_u32));
        assert_eq!(*cache.get::<u32>(1, "key").expect("cached"), 7);
        // Inserting a different type under the same key must not collide or replace.
        cache.insert(1, "key".to_owned(), Arc::new("replacement"));
        assert_eq!(*cache.get::<u32>(1, "key").expect("cached"), 7);
        // Moving to a newer revision drops the old snapshot's entries.
        assert!(cache.get::<u32>(2, "key").is_none());
        cache.insert(2, "other".to_owned(), Arc::new(9_u32));
        assert_eq!(cache.len(), 1);
        assert!(cache.get::<u32>(1, "key").is_none());
        // A stale worker cannot repopulate the cache after the revision advanced.
        cache.insert(1, "stale".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 1);
        assert!(cache.get::<u32>(1, "stale").is_none());
        cache.insert(2, "third".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 2);
        // Overflow clears wholesale and the newest entry survives.
        cache.insert(2, "fourth".to_owned(), Arc::new(13_u32));
        assert_eq!(cache.len(), 1);
        assert_eq!(*cache.get::<u32>(2, "fourth").expect("newest entry"), 13);
    }
}
