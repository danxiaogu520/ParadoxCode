//! Process-wide deduplication pool for retained shard strings.
//!
//! Index shards retain one `kind`/`name` string pair per definition and
//! reference. Kinds come from a small closed set and names repeat heavily
//! (the same event referenced across a mod, one localisation key per
//! language), so materialising a fresh `String` for every entry dominated
//! steady-state memory. Interning through this pool stores each distinct
//! spelling once; entries keep it alive through plain `Arc<str>` clones, so
//! the pool itself may be dropped or reset at any time.
//!
//! The pool is sharded by hash to keep parallel scan threads from
//! contending on one lock — a single global write lock made interning
//! anti-scale in the CWTools F# implementation and its Rust port.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const SHARD_COUNT: usize = 16;

#[derive(Default)]
struct StringPoolShard {
    entries: RwLock<HashMap<Arc<str>, ()>>,
}

/// Interning pool shared by every shard builder in the process.
#[derive(Default)]
pub struct StringPool {
    shards: [StringPoolShard; SHARD_COUNT],
}

impl StringPool {
    /// Creates an empty pool.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn shard_for(&self, value: &str) -> &StringPoolShard {
        let hash = fnv1a(value.as_bytes());
        &self.shards[hash as usize % SHARD_COUNT]
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

/// Process-wide pool for shard strings, initialized on first intern.
static SHARD_STRING_POOL: std::sync::OnceLock<StringPool> = std::sync::OnceLock::new();

/// Interns `value` through the process-wide shard string pool.
pub fn intern_shard_string(value: &str) -> Arc<str> {
    SHARD_STRING_POOL.get_or_init(StringPool::new).intern(value)
}
