//! Snapshot-scoped lazy cache for higher-layer query results.
//!
//! `AnalysisSnapshot` values are immutable and cheaply cloned across worker threads, but the
//! analysis layer recomputes per-document semantic extraction on every request. This module
//! provides a bounded, revision-keyed cache that is owned by the snapshot infrastructure and
//! shared by all clones, so results are computed once per (revision, key) and reused by every
//! query worker observing that same revision.
//!
//! The engine intentionally stores opaque values (`Arc<dyn Any>`): the cache is a mechanism
//! only. Contents belong to higher layers (currently `ide`). Entries from older
//! revisions are discarded as soon as a newer revision is observed; an old worker that finishes
//! later cannot repopulate the cache with stale data.
//!
//! Entries live in one of two invalidation domains, each tracking its own revision.
//! Document edits used to clear the whole cache, so every keystroke discarded
//! workspace-scale indexes (member-name lists, the localisation key index) and rebuilt them
//! from scratch. Index-domain entries now survive document revisions: an entry built from
//! index state revision `r` stays valid for every reader at revision `r` or later until the
//! index state itself advances, so a slow workspace-wide pass observing an older revision
//! keeps hitting views that a newer interactive reader already populated (and vice versa).
//! Documents-domain entries are valid for exactly one document revision. Each domain
//! overflows independently, so cheap boolean probes no longer evict the large shared
//! indexes they share a map with.

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
    /// Revision of the index state the `index` entries were built from. Entries stay
    /// valid for every revision at or after it until an index advance clears them.
    index_revision: Option<u64>,
    /// Revision of the `documents` entries; document revisions change per keystroke.
    documents_revision: Option<u64>,
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
                index_revision: None,
                documents_revision: None,
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
    ///
    /// Documents-domain entries answer only at their exact revision. Index-domain
    /// entries answer at their revision and every later one: only an index advance
    /// can clear them, so a reader still observing an older revision (a slow
    /// workspace-wide pass) hits views built for the same index state.
    pub fn get<T: Send + Sync + 'static>(&self, revision: u64, key: &str) -> Option<Arc<T>> {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        if state.documents_revision == Some(revision)
            && let Some(value) = state.documents.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state
            .index_revision
            .is_some_and(|current| revision >= current)
            && let Some(value) = state.index.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        None
    }

    /// Stores `value` under `(revision, domain, key)`; an existing key is never replaced.
    ///
    /// An insert from a revision the domain has already advanced past is dropped: a
    /// stale worker finishing late must not repopulate the cache with results derived
    /// from superseded state. An Index insert at a newer revision does not clear the
    /// domain — reaching a newer revision without an index advance proves the index
    /// state did not change, so the older entries remain valid.
    pub fn insert<T: Send + Sync + 'static>(
        &self,
        revision: u64,
        domain: CacheDomain,
        key: String,
        value: Arc<T>,
    ) {
        let mut state = self.write();
        match domain {
            CacheDomain::Documents => match state.documents_revision {
                Some(current) if revision < current => return,
                Some(current) if revision > current => {
                    state.documents.clear();
                    state.documents_revision = Some(revision);
                }
                None => state.documents_revision = Some(revision),
                _ => {}
            },
            CacheDomain::Index => match state.index_revision {
                Some(current) if revision < current => return,
                // An insert above `current` proves no index advance happened since
                // (an advance clears the domain and moves the watermark), so the
                // entry joins the same index-state lineage without touching the
                // watermark — readers still at `current` keep hitting every entry.
                Some(_) => {}
                None => state.index_revision = Some(revision),
            },
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
        if state
            .index_revision
            .is_none_or(|current| revision > current)
        {
            state.index.clear();
            state.index_revision = Some(revision);
        }
        if state
            .documents_revision
            .is_none_or(|current| revision > current)
        {
            state.documents.clear();
            state.documents_revision = Some(revision);
        }
    }

    /// Advances to a document-only revision, keeping index-derived entries.
    ///
    /// Overlay edits and closes change per-document query results but leave the workspace
    /// index untouched, so the expensive index-domain indexes stay valid across keystrokes
    /// and across readers still observing an older revision.
    pub fn advance_documents(&self, revision: u64) {
        let mut state = self.write();
        if state
            .documents_revision
            .is_none_or(|current| revision > current)
        {
            state.documents.clear();
            state.documents_revision = Some(revision);
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
        // Document revisions keep index entries valid for readers at both revisions.
        cache.advance_documents(2);
        assert!(cache.get::<u32>(1, "key").is_some());
        assert!(cache.get::<u32>(2, "key").is_some());
        // A full advance drops the entries.
        cache.advance_to(3);
        assert!(cache.get::<u32>(1, "key").is_none());
        assert!(cache.get::<u32>(3, "key").is_none());
        // A stale worker cannot repopulate the cache after the revision advanced.
        cache.insert(1, CacheDomain::Index, "stale".to_owned(), Arc::new(11_u32));
        assert!(cache.get::<u32>(1, "stale").is_none());
        assert!(cache.get::<u32>(3, "stale").is_none());
        cache.insert(3, CacheDomain::Index, "third".to_owned(), Arc::new(11_u32));
        assert_eq!(cache.len(), 1);
        // Overflow clears that domain wholesale and the newest entry survives.
        cache.insert(3, CacheDomain::Index, "fourth".to_owned(), Arc::new(13_u32));
        assert_eq!(cache.len(), 2);
        cache.insert(3, CacheDomain::Index, "fifth".to_owned(), Arc::new(17_u32));
        assert_eq!(cache.len(), 1);
        assert_eq!(*cache.get::<u32>(3, "fifth").expect("newest entry"), 17);
    }

    /// A slow workspace-wide pass observes an older revision while interactive edits
    /// advance the document revision. Index-derived views must keep answering the
    /// older reader; document-derived entries must not.
    #[test]
    fn index_entries_survive_document_advances_for_older_readers() {
        let cache = SnapshotQueryCache::with_capacity(8);
        cache.insert(
            5,
            CacheDomain::Index,
            "context-rule-view:effect".to_owned(),
            Arc::new(vec![1_u32]),
        );
        cache.advance_documents(7);
        // The pass worker still at revision 5 hits the same index state.
        assert!(
            cache
                .get::<Vec<u32>>(5, "context-rule-view:effect")
                .is_some()
        );
        // So does the newer interactive reader.
        assert!(
            cache
                .get::<Vec<u32>>(7, "context-rule-view:effect")
                .is_some()
        );
        // A newer reader can extend the lineage; the older one hits the new entry too.
        cache.insert(
            7,
            CacheDomain::Index,
            "context-rule-view:trigger".to_owned(),
            Arc::new(vec![2_u32]),
        );
        assert!(
            cache
                .get::<Vec<u32>>(5, "context-rule-view:trigger")
                .is_some()
        );
        // Document-derived entries answer only at their own revision.
        cache.insert(
            7,
            CacheDomain::Documents,
            "dynamic-definition:e:x".to_owned(),
            Arc::new(1_u32),
        );
        assert!(cache.get::<u32>(7, "dynamic-definition:e:x").is_some());
        assert!(cache.get::<u32>(5, "dynamic-definition:e:x").is_none());
        // An index advance supersedes every older reader's entries.
        cache.advance_to(9);
        assert!(
            cache
                .get::<Vec<u32>>(5, "context-rule-view:effect")
                .is_none()
        );
        assert!(
            cache
                .get::<Vec<u32>>(7, "context-rule-view:effect")
                .is_none()
        );
        assert!(
            cache
                .get::<Vec<u32>>(9, "context-rule-view:effect")
                .is_none()
        );
        // A stale worker cannot repopulate after the advance.
        cache.insert(5, CacheDomain::Index, "stale".to_owned(), Arc::new(9_u32));
        assert!(cache.get::<u32>(9, "stale").is_none());
        assert!(cache.get::<u32>(5, "stale").is_none());
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
