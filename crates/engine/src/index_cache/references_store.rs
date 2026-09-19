//! Lazy symbol-reference store over an installed index cache file.
//!
//! Cache installs materialize only dynamic-definition-kind references (the
//! dynamic call graph needs those eagerly). Every other kind — path maps,
//! localisation keys, province ids and the like, ~three quarters of the
//! reference rows on EU4 vanilla — is served on demand from the SQLite file
//! with a small bounded memo, keeping the resident set bounded while
//! find-references/rename answers stay identical.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Reference;

impl std::fmt::Debug for ReferenceIndexStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReferenceIndexStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Maximum memoized `(kind, name)` lookups; exceeding it drops the memo
/// wholesale — reference queries are interactive, so a rare rebuild beats an
/// LRU's bookkeeping.
const MEMO_CAP: usize = 4096;

/// Memo value: all references for one `(kind, name)` pair.
type MemoEntry = Arc<Vec<Reference>>;
type MemoKey = (Box<str>, Box<str>);

pub struct ReferenceIndexStore {
    path: PathBuf,
    memo: Mutex<HashMap<MemoKey, MemoEntry>>,
}

impl ReferenceIndexStore {
    /// Attaches a store to a cache file, creating the `(kind, name)` index
    /// once if the cache predates it. Errors are deferred to queries so a
    /// read-only cache location never blocks installation.
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            memo: Mutex::new(HashMap::new()),
        }
    }

    /// All references for one `(kind, name)` pair, case-insensitive.
    pub fn references_for(&self, kind: &str, name: &str) -> Arc<Vec<Reference>> {
        let key = (
            kind.to_ascii_lowercase().into_boxed_str(),
            name.to_ascii_lowercase().into_boxed_str(),
        );
        if let Some(hit) = self.memo.lock().expect("reference store lock").get(&key) {
            return Arc::clone(hit);
        }
        let loaded = Arc::new(self.query(&key.0, &key.1));
        let mut memo = self.memo.lock().expect("reference store lock");
        if memo.len() >= MEMO_CAP {
            memo.clear();
        }
        memo.insert(key, Arc::clone(&loaded));
        loaded
    }

    fn query(&self, kind: &str, name: &str) -> Vec<Reference> {
        let Ok(connection) = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return Vec::new();
        };
        // Best-effort index backfill for caches written before it existed;
        // failures simply fall back to a full scan inside SQLite.
        let _ = connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_symbol_references_kind_name
             ON symbol_references(kind COLLATE NOCASE, name COLLATE NOCASE)",
            [],
        );
        let Ok(mut statement) = connection.prepare(
            "SELECT file_id, kind, name, range_start, range_end
             FROM symbol_references
             WHERE kind = ? COLLATE NOCASE AND name = ? COLLATE NOCASE
             ORDER BY file_id, ordinal",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = statement.query_map([kind, name], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        }) else {
            return Vec::new();
        };
        let mut references = Vec::new();
        for row in rows.flatten() {
            let (file_id, ref_kind, ref_name, start, end) = row;
            // Undecodable rows are skipped rather than failing the query;
            // the cache reader enforces strict validation at install time.
            let Ok(file) = super::codec::decode_file_id(&file_id) else {
                continue;
            };
            let Ok(range) = super::codec::decode_range(start, end) else {
                continue;
            };
            references.push(Reference {
                kind: vfs::intern_shard_string(&ref_kind),
                name: vfs::intern_shard_string(&ref_name),
                file_id: file,
                range,
            });
        }
        references
    }
}
