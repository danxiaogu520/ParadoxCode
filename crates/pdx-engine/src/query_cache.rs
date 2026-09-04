//! Snapshot-scoped lazy cache for higher-layer query results.
//!
//! `AnalysisSnapshot` values are immutable and cheaply cloned across worker threads, but the
//! analysis layer recomputes per-document semantic extraction on every request. This module
//! provides a bounded, revision-keyed cache that is owned by the snapshot infrastructure and
//! shared by all clones, so results are computed once per (revision, key) and reused by every
//! query worker observing that same revision.
//!
//! The engine intentionally stores opaque values (`Arc<dyn Any>`): the cache is a mechanism
//! only. Contents belong to higher layers (currently `pdx-analysis`). Entries from older
//! revisions are discarded as soon as a newer revision is observed; an old worker that finishes
//! later cannot repopulate the cache with stale data.
//!
//! Entries live in one of two invalidation domains. Document edits used to clear the whole
//! cache, so every keystroke discarded workspace-scale indexes (member-name lists, the
//! localisation key index) and rebuilt them from scratch. Index-domain entries now survive
//! document revisions; each domain overflows independently, so cheap boolean probes no longer
//! evict the large shared indexes they share a map with.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;

/// Invalidation scope of one cache entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheDomain {
    /// Derived from workspace index state; invalidated when shards or rules change.
    Index,
    /// Derived from open overlay documents; invalidated by every document edit.
    Documents,
}

/// Bounded snapshot-scoped cache keyed by `(revision, domain, key)`.
///
/// Entries are immutable: a key is only ever inserted once per revision, and the owning
/// snapshot guarantees that all callers observing that revision see the same inputs. When a
/// domain exceeds its capacity that domain is cleared wholesale. Only entries for the newest
/// observed revision are retained, because an older immutable snapshot can always recompute a
/// miss.
pub struct SnapshotQueryCache {
    // Reads dominate: parallel validation workers probe shared per-revision views under the
    // read lock, while inserts upgrade per revision. FxHashMap keeps probes allocation-free
    // and cheap enough that sharding is unnecessary at current worker counts.
    state: RwLock<CacheState>,
    capacity: usize,
    /// Identity of the analysis state that owns this cache. Each `AnalysisHost`
    /// allocates its own cache, so this id distinguishes hosts that happen to
    /// reach the same revision number — analysis-layer thread-local fast paths
    /// key on it to avoid serving one host's view to another.
    id: u64,
}

/// Monotonic source of [`SnapshotQueryCache::id`] values. Allocations are
/// never reused while a cache lives, so ids are unique among live hosts.
static NEXT_CACHE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct CacheState {
    revision: Option<u64>,
    index: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    documents: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
}

impl CacheState {
    fn map(&mut self, domain: CacheDomain) -> &mut FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>> {
        match domain {
            CacheDomain::Index => &mut self.index,
            CacheDomain::Documents => &mut self.documents,
        }
    }
}

impl SnapshotQueryCache {
    /// Creates a cache with a conservative per-domain entry bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(32_768)
    }

    /// Creates a cache with an explicit per-domain entry bound.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: RwLock::new(CacheState {
                revision: None,
                index: FxHashMap::default(),
                documents: FxHashMap::default(),
            }),
            capacity,
            id: NEXT_CACHE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Returns the identity of the owning analysis state.
    ///
    /// Two snapshots share an id if and only if they come from the same
    /// `AnalysisHost`; distinct hosts never do. Higher layers key per-snapshot
    /// thread-local fast paths on this together with the revision so a second
    /// host reaching the same revision number cannot observe the first host's
    /// cached views.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, CacheState> {
        self.state
            .write()
            .expect("snapshot query cache lock poisoned")
    }

    /// Returns the cached value for `(revision, key)` when it was inserted as `T`.
    pub fn get<T: Send + Sync + 'static>(&self, revision: u64, key: &str) -> Option<Arc<T>> {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        if state.revision != Some(revision) {
            return None;
        }
        state
            .documents
            .get(key)
            .or_else(|| state.index.get(key))
            .and_then(|value| Arc::clone(value).downcast::<T>().ok())
    }

    /// Stores `value` under `(revision, domain, key)`; an existing key is never replaced.
    pub fn insert<T: Send + Sync + 'static>(
        &self,
        revision: u64,
        domain: CacheDomain,
        key: String,
        value: Arc<T>,
    ) {
        let mut state = self.write();
        match state.revision {
            Some(current) if revision < current => return,
            Some(current) if revision != current => {
                state.index.clear();
                state.documents.clear();
                state.revision = Some(revision);
            }
            None => state.revision = Some(revision),
            _ => {}
        }
        let entries = state.map(domain);
        if entries.len() >= self.capacity && !entries.contains_key(key.as_str()) {
            entries.clear();
        }
        entries
            .entry(Box::from(key))
            .or_insert_with(|| value.clone());
    }

    /// Advances the cache to a committed workspace revision and drops all query results.
    pub fn advance_to(&self, revision: u64) {
        let mut state = self.write();
        match state.revision {
            Some(current) if revision > current => {
                state.index.clear();
                state.documents.clear();
                state.revision = Some(revision);
            }
            None => state.revision = Some(revision),
            _ => {}
        }
    }

    /// Advances to a document-only revision, keeping index-derived entries.
    ///
    /// Overlay edits and closes change per-document query results but leave the workspace
    /// index untouched, so the expensive index-domain indexes stay valid across keystrokes.
    pub fn advance_documents(&self, revision: u64) {
        let mut state = self.write();
        match state.revision {
            Some(current) if revision > current => {
                state.documents.clear();
                state.revision = Some(revision);
            }
            None => state.revision = Some(revision),
            _ => {}
        }
    }

    /// Returns the number of cached entries (for diagnostics and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        state.index.len() + state.documents.len()
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
        cache.insert(1, CacheDomain::Index, "key".to_owned(), Arc::new(7_u32));
        assert_eq!(*cache.get::<u32>(1, "key").expect("cached"), 7);
        // Inserting a different type under the same key must not collide or replace.
        cache.insert(
            1,
            CacheDomain::Index,
            "key".to_owned(),
            Arc::new("replacement"),
        );
        assert_eq!(*cache.get::<u32>(1, "key").expect("cached"), 7);
        // Moving to a newer revision drops the old snapshot's entries.
        assert!(cache.get::<u32>(2, "key").is_none());
        cache.insert(2, CacheDomain::Index, "other".to_owned(), Arc::new(9_u32));
        assert_eq!(cache.len(), 1);
        assert!(cache.get::<u32>(1, "key").is_none());
        // A stale worker cannot repopulate the cache after the revision advanced.
        cache.insert(1, CacheDomain::Index, "stale".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 1);
        assert!(cache.get::<u32>(1, "stale").is_none());
        cache.insert(2, CacheDomain::Index, "third".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 2);
        // Overflow clears that domain wholesale and the newest entry survives.
        cache.insert(2, CacheDomain::Index, "fourth".to_owned(), Arc::new(13_u32));
        assert_eq!(cache.len(), 1);
        assert_eq!(*cache.get::<u32>(2, "fourth").expect("newest entry"), 13);
    }

    #[test]
    fn document_revisions_keep_index_entries() {
        let cache = SnapshotQueryCache::with_capacity(8);
        cache.insert(
            1,
            CacheDomain::Index,
            "workspace-member-names:event".to_owned(),
            Arc::new(Vec::<String>::new()),
        );
        cache.insert(
            1,
            CacheDomain::Documents,
            "file:///events/a.txt".to_owned(),
            Arc::new(1_u32),
        );
        cache.advance_documents(2);
        assert!(
            cache
                .get::<Vec<String>>(2, "workspace-member-names:event")
                .is_some(),
            "index entries survive document revisions"
        );
        assert!(cache.get::<u32>(2, "file:///events/a.txt").is_none());
        // A full advance drops both domains.
        cache.advance_to(3);
        assert!(
            cache
                .get::<Vec<String>>(3, "workspace-member-names:event")
                .is_none()
        );
    }
}
