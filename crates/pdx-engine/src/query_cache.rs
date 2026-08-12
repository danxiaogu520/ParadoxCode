//! Snapshot-scoped lazy cache for higher-layer query results.
//!
//! `AnalysisSnapshot` values are immutable and cheaply cloned across worker threads, but the
//! analysis layer recomputes per-document semantic extraction on every request. This module
//! provides a bounded, revision-keyed cache that is owned by the snapshot infrastructure and
//! shared by all clones, so results are computed once per (revision, key) and reused by every
//! query worker observing the same revision.
//!
//! The engine intentionally stores opaque values (`Arc<dyn Any>`): the cache is a mechanism
//! only. Contents belong to higher layers (currently `pdx-analysis`), which are free to evict
//! stale entries by using a fresh key whenever their inputs change.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Bounded snapshot-scoped cache keyed by `(revision, key)`.
///
/// Entries are immutable: a key is only ever inserted once per revision, and the owning
/// snapshot guarantees that all callers observing that revision see the same inputs. When the
/// capacity is exceeded the cache is cleared wholesale; revisions advance frequently enough
/// that older entries are unlikely to be reused anyway.
pub struct SnapshotQueryCache {
    entries: Mutex<BTreeMap<(u64, String), Arc<dyn Any + Send + Sync>>>,
    capacity: usize,
}

impl SnapshotQueryCache {
    /// Creates a cache with a conservative entry bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(4096)
    }

    /// Creates a cache with an explicit entry bound.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            capacity,
        }
    }

    /// Returns the cached value for `(revision, key)` when it was inserted as `T`.
    pub fn get<T: Send + Sync + 'static>(&self, revision: u64, key: &str) -> Option<Arc<T>> {
        let entries = self
            .entries
            .lock()
            .expect("snapshot query cache lock poisoned");
        entries
            .get(&(revision, key.to_owned()))
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// Stores `value` under `(revision, key)`; a key that already exists is never replaced.
    pub fn insert<T: Send + Sync + 'static>(&self, revision: u64, key: String, value: Arc<T>) {
        let mut entries = self
            .entries
            .lock()
            .expect("snapshot query cache lock poisoned");
        if entries.len() >= self.capacity && !entries.contains_key(&(revision, key.clone())) {
            entries.clear();
        }
        entries.entry((revision, key)).or_insert(value);
    }

    /// Returns the number of cached entries (for diagnostics and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("snapshot query cache lock poisoned")
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
        // Revisions are independent.
        assert!(cache.get::<u32>(2, "key").is_none());
        cache.insert(2, "other".to_owned(), Arc::new(9_u32));
        assert_eq!(cache.len(), 2);
        // Overflow clears wholesale and the newest entry survives.
        cache.insert(1, "third".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 1);
        assert_eq!(*cache.get::<u32>(1, "third").expect("newest entry"), 11);
    }
}
