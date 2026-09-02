//! Process-wide deduplication pool for retained strings.
//!
//! Index shards, HIR properties, and analysis frontends retain one string per
//! definition, key, and reference. Kinds come from a small closed set and names
//! repeat heavily (the same event referenced across a mod, one localisation key
//! per language), so materialising a fresh `String` for every entry dominated
//! steady-state memory and allocation CPU. Interning through this pool stores
//! each distinct spelling once; entries keep it alive through plain `Arc<str>`
//! clones, so the pool itself may be dropped or reset at any time.
//!
//! The pool is sharded by hash (64 shards, one cache line each) to keep
//! parallel scan threads from contending on one lock — a single global write
//! lock made interning anti-scale in the CWTools F# implementation and its
//! Rust port. Reads take the shared lock; writes upgrade per shard.

use std::sync::{Arc, RwLock};

use rustc_hash::FxHashMap;

/// Keeps adjacent shards on separate cache lines; the byte itself is never read.
#[repr(align(64))]
struct ShardPadding(#[allow(dead_code)] u8);

#[repr(align(64))]
struct StringPoolShard {
    entries: RwLock<FxHashMap<Arc<str>, ()>>,
}

/// Interning pool shared by every shard builder in the process.
pub struct StringPool {
    shards: Vec<StringPoolShard>,
    _padding: ShardPadding,
}

impl Default for StringPool {
    fn default() -> Self {
        Self::new()
    }
}

impl StringPool {
    /// Creates an empty pool with one cache-aligned shard per hash bucket.
    #[must_use]
    pub fn new() -> Self {
        Self::with_shards(64)
    }

    /// Creates an empty pool with an explicit shard count.
    #[must_use]
    pub fn with_shards(shards: usize) -> Self {
        Self {
            shards: (0..shards.max(1))
                .map(|_| StringPoolShard {
                    entries: RwLock::new(FxHashMap::default()),
                })
                .collect(),
            _padding: ShardPadding(0),
        }
    }

    fn shard_for(&self, value: &str) -> &StringPoolShard {
        let hash = fnv1a(value.as_bytes());
        &self.shards[hash as usize % self.shards.len()]
    }

    /// Returns the shared copy of `value`, storing it on first sight.
    ///
    /// Concurrent interns of the same spelling return the same allocation.
    pub fn intern(&self, value: &str) -> Arc<str> {
        let shard = self.shard_for(value);
        if let Ok(entries) = shard.entries.read()
            && let Some((existing, ())) = entries.get_key_value(value)
        {
            return Arc::clone(existing);
        }
        let mut entries = shard.entries.write().expect("string pool lock poisoned");
        if let Some((existing, ())) = entries.get_key_value(value) {
            return Arc::clone(existing);
        }
        let interned: Arc<str> = Arc::from(value);
        entries.insert(Arc::clone(&interned), ());
        interned
    }

    /// Returns the number of distinct interned spellings (for tests and probes).
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .entries
                    .read()
                    .map(|entries| entries.len())
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Returns whether no spelling was interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// FNV-1a: cheap, stable, and dependency-free.
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Process-wide pool for retained strings, initialized on first intern.
static SHARD_STRING_POOL: std::sync::OnceLock<StringPool> = std::sync::OnceLock::new();

/// Interns `value` through the process-wide string pool.
pub fn intern_shard_string(value: &str) -> Arc<str> {
    SHARD_STRING_POOL.get_or_init(StringPool::new).intern(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_spellings_share_one_allocation() {
        let pool = StringPool::new();
        let first = pool.intern("flavor_tanflesi_events");
        let second = pool.intern("flavor_tanflesi_events");
        assert!(Arc::ptr_eq(&first, &second));
        let other = pool.intern("common\\events:file");
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn concurrent_interns_converge() {
        let pool = std::sync::Arc::new(StringPool::new());
        let mut handles = Vec::new();
        for thread in 0..8 {
            let pool = Arc::clone(&pool);
            handles.push(std::thread::spawn(move || {
                let spelling = format!("shared-{}", thread % 2);
                let interned = pool.intern(&spelling);
                std::thread::yield_now();
                interned
            }));
        }
        let results: Vec<Arc<str>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(Arc::ptr_eq(&results[0], &results[2]));
        assert!(Arc::ptr_eq(&results[1], &results[3]));
    }
}
