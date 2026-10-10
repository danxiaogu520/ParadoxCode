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
//! Ordinary results use three invalidation lineages, each tracking its own revision.
//! Two single-slot input memos additionally retain source ownership or localisation files;
//! callers validate overlay identities, file stamps and policy before reusing those inputs.
//! Memo replacement rejects workers older than the current document watermark.
//! Document edits used to clear the whole cache, so every keystroke discarded
//! workspace-scale indexes (member-name lists, the localisation key index) and rebuilt them
//! from scratch. Index-domain entries now survive document revisions: an entry built from
//! index state revision `r` stays valid for every reader at revision `r` or later until the
//! index state itself advances, so a slow workspace-wide pass observing an older revision
//! keeps hitting views that a newer interactive reader already populated (and vice versa).
//! Documents-domain entries are valid for exactly one document revision. Each domain
//! overflows independently, so cheap boolean probes no longer evict the large shared
//! indexes they share a map with.
//!
//! The definitions domain covers entries derived from the set of dynamic
//! definitions (index files plus overlay documents that declare one): editing a
//! document that only *calls* scripted definitions leaves those entries valid,
//! while a commit that changes a declaring document advances the domain. Its
//! watermark advances on index changes too, matching the index domain's rules.

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
    /// Heavy parsed/lowered frontends, sharing document invalidation but a
    /// separate small capacity so workspace sweeps do not retain every HIR.
    Frontends,
    /// Derived from the dynamic-definition set (index plus declaring overlays);
    /// invalidated when a declaring document commits or the index advances.
    Definitions,
    /// One disk text corpus; index changes and caller watch epochs invalidate it.
    SearchTexts,
    /// One full-value localisation corpus, invalidated with document revisions.
    SearchLocalisations,
    /// One ownership view, keyed by immutable index state and validated overlay paths.
    SourceOwnership,
    /// One previous localisation corpus used only after callers validate every source input.
    /// Retained across revisions to share unchanged file data, never served as an unchecked result.
    SearchLocalisationReuse,
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
    /// Revision of the dynamic-definition set the `definitions` entries were built
    /// from; only declaring-document commits and index advances move it.
    definitions_revision: Option<u64>,
    index: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    documents: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    frontends: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    definitions: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    search_texts: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    search_localisations: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    search_localisation_reuse: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
    source_ownership: FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>>,
}

impl CacheState {
    fn map(&mut self, domain: CacheDomain) -> &mut FxHashMap<Box<str>, Arc<dyn Any + Send + Sync>> {
        match domain {
            CacheDomain::Index => &mut self.index,
            CacheDomain::Documents => &mut self.documents,
            CacheDomain::Frontends => &mut self.frontends,
            CacheDomain::Definitions => &mut self.definitions,
            CacheDomain::SearchTexts => &mut self.search_texts,
            CacheDomain::SearchLocalisations => &mut self.search_localisations,
            CacheDomain::SearchLocalisationReuse => &mut self.search_localisation_reuse,
            CacheDomain::SourceOwnership => &mut self.source_ownership,
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
                definitions_revision: None,
                index: FxHashMap::default(),
                documents: FxHashMap::default(),
                frontends: FxHashMap::default(),
                definitions: FxHashMap::default(),
                search_texts: FxHashMap::default(),
                search_localisations: FxHashMap::default(),
                search_localisation_reuse: FxHashMap::default(),
                source_ownership: FxHashMap::default(),
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
    /// Documents-domain entries answer only at their exact revision. Index- and
    /// definitions-domain entries answer at their revision and every later one:
    /// only an advance of their own domain can clear them, so a reader still
    /// observing an older revision (a slow workspace-wide pass) hits views built
    /// for the same underlying state. Input memo domains require callers to validate
    /// source stamps or overlay identities before using their retained data.
    pub fn get<T: Send + Sync + 'static>(&self, revision: u64, key: &str) -> Option<Arc<T>> {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        if let Some(value) = state.search_localisation_reuse.get(key) {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state.documents_revision == Some(revision)
            && let Some(value) = state.documents.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state.documents_revision == Some(revision)
            && let Some(value) = state.frontends.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state.documents_revision == Some(revision)
            && let Some(value) = state.search_localisations.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state
            .index_revision
            .is_some_and(|current| revision >= current)
            && let Some(value) = state.source_ownership.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state
            .index_revision
            .is_some_and(|current| revision >= current)
            && let Some(value) = state.search_texts.get(key)
        {
            return Arc::clone(value).downcast::<T>().ok();
        }
        if state
            .definitions_revision
            .is_some_and(|current| revision >= current)
            && let Some(value) = state.definitions.get(key)
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

    /// Stores `value` under `(revision, domain, key)`. Ordinary query entries are immutable;
    /// validated single-slot input memos replace their previous value at the same key.
    ///
    /// An insert from a revision the domain has already advanced past is dropped: a
    /// stale worker finishing late must not repopulate the cache with results derived
    /// from superseded state. An Index insert at a newer revision does not clear the
    /// domain — reaching a newer revision without an index advance proves the index
    /// state did not change, so the older entries remain valid. Definitions-domain
    /// inserts follow the same lineage rule as the index domain.
    pub fn insert<T: Send + Sync + 'static>(
        &self,
        revision: u64,
        domain: CacheDomain,
        key: String,
        value: Arc<T>,
    ) {
        let mut state = self.write();
        match domain {
            CacheDomain::SourceOwnership => {
                if state
                    .documents_revision
                    .is_some_and(|current| revision < current)
                {
                    return;
                }
                state.index_revision.get_or_insert(revision);
            }
            CacheDomain::SearchLocalisationReuse => {
                if state
                    .documents_revision
                    .is_some_and(|current| revision < current)
                {
                    return;
                }
            }
            CacheDomain::Documents | CacheDomain::Frontends | CacheDomain::SearchLocalisations => {
                match state.documents_revision {
                    Some(current) if revision < current => return,
                    Some(current) if revision > current => {
                        state.documents.clear();
                        state.frontends.clear();
                        state.search_localisations.clear();
                        state.documents_revision = Some(revision);
                    }
                    None => state.documents_revision = Some(revision),
                    _ => {}
                }
            }
            CacheDomain::Index | CacheDomain::SearchTexts => match state.index_revision {
                Some(current) if revision < current => return,
                // An insert above `current` proves no index advance happened since
                // (an advance clears the domain and moves the watermark), so the
                // entry joins the same index-state lineage without touching the
                // watermark — readers still at `current` keep hitting every entry.
                Some(_) => {}
                None => state.index_revision = Some(revision),
            },
            CacheDomain::Definitions => match state.definitions_revision {
                Some(current) if revision < current => return,
                // Same lineage argument as the index domain: reaching a newer
                // revision without a definitions advance proves the declaring
                // documents did not change, so the entry joins the lineage.
                Some(_) => {}
                None => state.definitions_revision = Some(revision),
            },
        }
        let capacity = match domain {
            CacheDomain::Frontends => self.capacity.min(32),
            CacheDomain::SearchTexts
            | CacheDomain::SearchLocalisations
            | CacheDomain::SearchLocalisationReuse
            | CacheDomain::SourceOwnership => 1,
            _ => self.capacity,
        };
        let entries = state.map(domain);
        if entries.len() >= capacity && !entries.contains_key(key.as_str()) {
            entries.clear();
        }
        if matches!(
            domain,
            CacheDomain::SourceOwnership | CacheDomain::SearchLocalisationReuse
        ) {
            entries.insert(Box::from(key), value);
            return;
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
            state.search_texts.clear();
            state.source_ownership.clear();
            state.index_revision = Some(revision);
        }
        if state
            .definitions_revision
            .is_none_or(|current| revision > current)
        {
            state.definitions.clear();
            state.definitions_revision = Some(revision);
        }
        if state
            .documents_revision
            .is_none_or(|current| revision > current)
        {
            state.documents.clear();
            state.frontends.clear();
            state.search_localisations.clear();
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
            state.frontends.clear();
            state.search_localisations.clear();
            state.documents_revision = Some(revision);
        }
    }

    /// Advances to a revision that changed the dynamic-definition set.
    ///
    /// A committed document that declares dynamic definitions (or the close of one)
    /// invalidates definition-derived entries while leaving both the index domain
    /// and plain document entries alone: callers advance the documents domain
    /// themselves for the same revision.
    pub fn advance_definitions(&self, revision: u64) {
        let mut state = self.write();
        if state
            .definitions_revision
            .is_none_or(|current| revision > current)
        {
            state.definitions.clear();
            state.definitions_revision = Some(revision);
        }
    }

    /// True when `revision` observes state the domain has already advanced past.
    ///
    /// Long-running workers on superseded snapshots use this to skip rebuilding
    /// domain-derived results whose insert would be dropped anyway: rebuilding
    /// would only burn a core repeating work the advancing reader will redo.
    #[must_use]
    pub fn is_superseded(&self, domain: CacheDomain, revision: u64) -> bool {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        let watermark = match domain {
            CacheDomain::Index | CacheDomain::SearchTexts => state.index_revision,
            CacheDomain::Documents
            | CacheDomain::Frontends
            | CacheDomain::SearchLocalisations
            | CacheDomain::SearchLocalisationReuse
            | CacheDomain::SourceOwnership => state.documents_revision,
            CacheDomain::Definitions => state.definitions_revision,
        };
        watermark.is_some_and(|current| revision < current)
    }

    /// Returns the number of cached entries (for diagnostics and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        let state = self
            .state
            .read()
            .expect("snapshot query cache lock poisoned");
        state.index.len()
            + state.documents.len()
            + state.frontends.len()
            + state.definitions.len()
            + state.search_texts.len()
            + state.search_localisations.len()
            + state.search_localisation_reuse.len()
            + state.source_ownership.len()
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
    fn frontend_overflow_does_not_evict_lightweight_document_views() {
        let cache = SnapshotQueryCache::new();
        cache.insert(1, CacheDomain::Documents, "cheap".into(), Arc::new(7_u32));
        let first = Arc::new(vec![1_u32]);
        let weak = Arc::downgrade(&first);
        cache.insert(1, CacheDomain::Frontends, "frontend-0".into(), first);
        for index in 1_u32..=32 {
            cache.insert(
                1,
                CacheDomain::Frontends,
                format!("frontend-{index}"),
                Arc::new(vec![index]),
            );
        }
        assert!(weak.upgrade().is_none(), "old frontends must be released");
        assert_eq!(*cache.get::<u32>(1, "cheap").unwrap(), 7);
        assert_eq!(cache.len(), 2, "one frontend and the cheap view survive");
        assert_eq!(*cache.get::<Vec<u32>>(1, "frontend-32").unwrap(), vec![32]);
    }

    #[test]
    fn frontends_follow_document_revisions_and_reject_stale_workers() {
        let cache = SnapshotQueryCache::new();
        cache.insert(
            1,
            CacheDomain::Frontends,
            "frontend".into(),
            Arc::new(3_u32),
        );
        cache.insert(1, CacheDomain::Index, "index".into(), Arc::new(5_u32));
        cache.advance_documents(2);
        assert!(cache.get::<u32>(1, "frontend").is_none());
        assert!(cache.get::<u32>(2, "frontend").is_none());
        assert!(cache.get::<u32>(2, "index").is_some());
        cache.insert(1, CacheDomain::Frontends, "stale".into(), Arc::new(9_u32));
        assert!(cache.get::<u32>(2, "stale").is_none());
        assert!(cache.is_superseded(CacheDomain::Frontends, 1));
        cache.insert(2, CacheDomain::Frontends, "fresh".into(), Arc::new(11_u32));
        cache.advance_to(3);
        assert!(cache.get::<u32>(3, "fresh").is_none());
    }

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

    /// Definition-derived entries must survive edits of documents that only
    /// call scripted definitions (plain document advances), move when a
    /// declaring document commits, and answer superseded readers so a stale
    /// worker degrades instead of looping on dropped inserts.
    #[test]
    fn definitions_entries_track_declaring_documents_only() {
        let cache = SnapshotQueryCache::with_capacity(8);
        cache.insert(
            1,
            CacheDomain::Definitions,
            "dynamic-rule-rows".to_owned(),
            Arc::new(vec![1_u32]),
        );
        // A keystroke in a non-declaring document keeps the report valid for
        // readers at both revisions.
        cache.advance_documents(2);
        assert!(cache.get::<Vec<u32>>(1, "dynamic-rule-rows").is_some());
        assert!(cache.get::<Vec<u32>>(2, "dynamic-rule-rows").is_some());
        assert!(!cache.is_superseded(CacheDomain::Definitions, 1));
        // A declaring-document commit moves the domain: the older reader is
        // superseded and its insert would be dropped.
        cache.advance_definitions(4);
        assert!(cache.get::<Vec<u32>>(1, "dynamic-rule-rows").is_none());
        assert!(cache.get::<Vec<u32>>(4, "dynamic-rule-rows").is_none());
        assert!(cache.is_superseded(CacheDomain::Definitions, 1));
        assert!(!cache.is_superseded(CacheDomain::Definitions, 4));
        cache.insert(
            1,
            CacheDomain::Definitions,
            "stale".to_owned(),
            Arc::new(vec![2_u32]),
        );
        assert!(cache.get::<Vec<u32>>(4, "stale").is_none());
        // A current worker populates and later readers at the same lineage hit it.
        cache.insert(
            4,
            CacheDomain::Definitions,
            "dynamic-rule-rows".to_owned(),
            Arc::new(vec![3_u32]),
        );
        assert_eq!(
            cache
                .get::<Vec<u32>>(4, "dynamic-rule-rows")
                .expect("rebuilt")[0],
            3
        );
        assert_eq!(
            cache
                .get::<Vec<u32>>(6, "dynamic-rule-rows")
                .expect("later reader")[0],
            3
        );
        // An index advance clears the domain too: index files declare.
        cache.advance_to(7);
        assert!(cache.get::<Vec<u32>>(6, "dynamic-rule-rows").is_none());
    }
}

#[cfg(test)]
mod search_text_cache_tests {
    use super::*;

    #[test]
    fn reusable_file_memos_survive_edits_but_reject_late_workers() {
        let cache = SnapshotQueryCache::new();
        cache.advance_to(1);
        cache.insert(
            1,
            CacheDomain::SearchLocalisationReuse,
            "reuse".into(),
            Arc::new(1_u32),
        );
        cache.advance_documents(2);
        assert_eq!(*cache.get::<u32>(2, "reuse").unwrap(), 1);
        cache.insert(
            2,
            CacheDomain::SearchLocalisationReuse,
            "reuse".into(),
            Arc::new(2_u32),
        );
        assert_eq!(
            *cache.get::<u32>(2, "reuse").unwrap(),
            2,
            "the validated memo advances at the same key"
        );
        cache.advance_to(3);
        cache.insert(
            3,
            CacheDomain::SearchLocalisationReuse,
            "new-reuse".into(),
            Arc::new(3_u32),
        );
        cache.insert(
            1,
            CacheDomain::SearchLocalisationReuse,
            "late".into(),
            Arc::new(7_u32),
        );
        assert!(
            cache.get::<u32>(3, "reuse").is_none(),
            "only one file memo is retained"
        );
        assert!(cache.get::<u32>(3, "late").is_none());
        assert_eq!(*cache.get::<u32>(3, "new-reuse").unwrap(), 3);
    }

    #[test]
    fn materialized_corpora_have_one_slot_and_observe_their_input_revisions() {
        let cache = SnapshotQueryCache::new();
        let first = Arc::new(vec![1_u8]);
        let released = Arc::downgrade(&first);
        cache.insert(1, CacheDomain::SearchTexts, "roots-a".into(), first);
        cache.insert(1, CacheDomain::Documents, "cheap".into(), Arc::new(7_u8));
        cache.insert(
            1,
            CacheDomain::SearchTexts,
            "roots-b".into(),
            Arc::new(vec![2_u8]),
        );
        assert!(released.upgrade().is_none());
        assert!(cache.get::<Vec<u8>>(1, "roots-a").is_none());
        assert_eq!(*cache.get::<u8>(1, "cheap").unwrap(), 7);
        let localisation = Arc::new(vec![4_u8]);
        let old_localisation = Arc::downgrade(&localisation);
        cache.insert(
            1,
            CacheDomain::SearchLocalisations,
            "loc-a".into(),
            localisation,
        );
        cache.insert(
            1,
            CacheDomain::SearchLocalisations,
            "loc-b".into(),
            Arc::new(vec![5_u8]),
        );
        assert!(old_localisation.upgrade().is_none());
        assert!(cache.get::<Vec<u8>>(1, "roots-b").is_some());
        cache.advance_documents(2);
        assert!(
            cache.get::<Vec<u8>>(2, "roots-b").is_some(),
            "disk corpora survive buffer-only edits"
        );
        assert!(cache.get::<Vec<u8>>(2, "loc-b").is_none());
        cache.advance_to(3);
        assert!(cache.get::<Vec<u8>>(3, "roots-b").is_none());
        cache.insert(
            1,
            CacheDomain::SearchTexts,
            "stale".into(),
            Arc::new(vec![3_u8]),
        );
        assert!(cache.get::<Vec<u8>>(2, "stale").is_none());
    }
}
