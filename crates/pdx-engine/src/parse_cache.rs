//! Bounded persistent per-file syntax-tree cache.
//!
//! The cache mirrors the useful part of CWTools Rust's `.cwb` parse cache without persisting
//! semantic HIR, source text, or source-root state. Entries are keyed by stable file identity and
//! validated by parser schema, frontend format, source digest, and CST range safety before they
//! are reused. The compact postcard payload is zstd-compressed to keep the disk cache cheap while
//! decompression remains bounded by the same 64 MiB safety limit. Cache entries use the maintained
//! postcard serde adapter and are versioned so older payloads are ordinary misses.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pdx_parser::{FileFormat, ParsedFile, ParsedFileCache};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::SourceFile;

/// Current on-disk syntax-tree cache schema.
pub const CURRENT_PARSE_CACHE_SCHEMA_VERSION: u32 = 5;

const MAX_PARSE_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const CACHE_NAMESPACE: &[u8] = b"paradoxcode/parse-cache/v5\0";
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A user-local directory containing independent syntax-tree cache entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseCache {
    directory: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct ParseCacheEntry {
    schema_version: u32,
    format: FileFormat,
    source_sha256: [u8; 32],
    parsed: ParsedFileCache,
}

/// Errors returned when a parsed tree cannot be persisted.
#[derive(Debug)]
pub enum ParseCacheError {
    /// Cache directory or temporary file I/O failed.
    Io(std::io::Error),
    /// The parsed tree could not be encoded.
    Encode(String),
    /// The encoded entry would exceed the safety limit.
    TooLarge { bytes: usize, limit: u64 },
    /// The caller attempted to persist a tree that does not match its source.
    InvalidEntry,
}

impl fmt::Display for ParseCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "parse cache I/O error: {error}"),
            Self::Encode(error) => write!(formatter, "parse cache encode error: {error}"),
            Self::TooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "parse cache entry is too large: {bytes} bytes (limit {limit})"
                )
            }
            Self::InvalidEntry => formatter.write_str("parse cache entry does not match source"),
        }
    }
}

impl std::error::Error for ParseCacheError {}

impl From<std::io::Error> for ParseCacheError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl ParseCache {
    /// Creates a cache rooted at `directory`. The directory is created lazily on the first write.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// Returns the configured cache directory without touching the filesystem.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.directory.as_path()
    }

    /// Loads a matching parsed frontend, returning `None` for a miss or invalid entry.
    ///
    /// Cache failures deliberately do not fail a workspace scan. The caller can parse the source
    /// normally and overwrite the bad entry on the next store.
    pub fn load(&self, file: &SourceFile, format: FileFormat, source: &str) -> Option<ParsedFile> {
        let path = self.entry_path(file);
        let metadata = fs::metadata(&path).ok()?;
        if metadata.len() > MAX_PARSE_CACHE_BYTES {
            return None;
        }
        let mut handle = fs::File::open(path).ok()?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
        std::io::Read::take(&mut handle, MAX_PARSE_CACHE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if u64::try_from(bytes.len()).ok()? > MAX_PARSE_CACHE_BYTES {
            return None;
        }
        let decompressed =
            zstd::bulk::decompress(&bytes, usize::try_from(MAX_PARSE_CACHE_BYTES).ok()?).ok()?;
        let (entry, remaining) =
            postcard::take_from_bytes::<ParseCacheEntry>(&decompressed).ok()?;
        if !remaining.is_empty() {
            return None;
        }
        if entry.schema_version != CURRENT_PARSE_CACHE_SCHEMA_VERSION
            || entry.format != format
            || entry.source_sha256 != digest(source)
            || entry.parsed.format != format
        {
            return None;
        }
        let parsed = ParsedFile::from_cache_data(entry.parsed, source);
        parsed.is_valid_for(format, source).then_some(parsed)
    }

    /// Atomically stores one validated parsed frontend. Write errors are reported to the caller;
    /// they are normally non-fatal to indexing and may be ignored by a background scan.
    pub fn store(
        &self,
        file: &SourceFile,
        format: FileFormat,
        source: &str,
        parsed: &ParsedFile,
    ) -> Result<(), ParseCacheError> {
        if !parsed.is_valid_for(format, source) {
            return Err(ParseCacheError::InvalidEntry);
        }
        let entry = ParseCacheEntry {
            schema_version: CURRENT_PARSE_CACHE_SCHEMA_VERSION,
            format,
            source_sha256: digest(source),
            parsed: parsed.cache_data(),
        };
        let encoded = postcard::to_allocvec(&entry)
            .map_err(|error| ParseCacheError::Encode(error.to_string()))?;
        if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > MAX_PARSE_CACHE_BYTES {
            return Err(ParseCacheError::TooLarge {
                bytes: encoded.len(),
                limit: MAX_PARSE_CACHE_BYTES,
            });
        }
        let bytes = zstd::bulk::compress(&encoded, 3)
            .map_err(|error| ParseCacheError::Encode(error.to_string()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PARSE_CACHE_BYTES {
            return Err(ParseCacheError::TooLarge {
                bytes: bytes.len(),
                limit: MAX_PARSE_CACHE_BYTES,
            });
        }
        fs::create_dir_all(&self.directory)?;
        let destination = self.entry_path(file);
        let sequence = WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self.directory.join(format!(
            ".{}.{}-{sequence}.tmp",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("entry.pdxast"),
            std::process::id()
        ));
        let result = (|| {
            let mut handle = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            handle.write_all(&bytes)?;
            handle.sync_all()?;
            drop(handle);
            // A concurrent writer may have published an equivalent entry first. Preserve the
            // winner rather than deleting a valid cache file from another scan.
            match fs::rename(&temporary, &destination) {
                Ok(()) => Ok(()),
                Err(_error) if destination.exists() => Ok(()),
                Err(error) => Err(error),
            }
        })();
        if temporary.exists() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(ParseCacheError::Io)
    }

    fn entry_path(&self, file: &SourceFile) -> PathBuf {
        self.directory.join(format!("{}.pdxast", file_key(file)))
    }
}

fn digest(source: &str) -> [u8; 32] {
    Sha256::digest(source.as_bytes()).into()
}

fn file_key(file: &SourceFile) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_NAMESPACE);
    hasher.update(file.root_id.get().to_le_bytes());
    put_string(&mut hasher, file.logical_path.as_str());
    put_string(&mut hasher, &file.physical_path.to_string_lossy());
    hex_digest(hasher.finalize())
}

fn put_string(hasher: &mut Sha256, value: &str) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceFileId, SourceRootId};
    use pdx_rules::FileResolutionPolicy;
    use pdx_text::LogicalPath;

    fn file(root: &Path) -> SourceFile {
        SourceFile {
            id: SourceFileId::new(7),
            root_id: SourceRootId::new(3),
            physical_path: root.join("events/test.txt"),
            logical_path: LogicalPath::parse("events/test.txt").expect("logical path"),
            category_id: Some("script".to_owned()),
            resolution: FileResolutionPolicy::ReplaceByRelativePath,
        }
    }

    #[test]
    fn cache_round_trip_reuses_only_matching_source() {
        let directory = test_directory("round-trip");
        let source = "country_event = { id = cached.1 }\n";
        let parsed = pdx_parser::parse(FileFormat::Script, source);
        let cache = ParseCache::new(directory.join("parse-cache"));
        let source_file = file(&directory);
        cache
            .store(&source_file, FileFormat::Script, source, &parsed)
            .expect("store parse cache");
        assert_eq!(
            cache
                .load(&source_file, FileFormat::Script, source)
                .expect("cache hit"),
            parsed
        );
        assert!(
            cache
                .load(
                    &source_file,
                    FileFormat::Script,
                    "country_event = { id = changed.1 }\n"
                )
                .is_none()
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn cache_rejects_corrupt_entries_without_failing_the_scan() {
        let directory = test_directory("corrupt");
        let cache = ParseCache::new(directory.join("parse-cache"));
        let source_file = file(&directory);
        fs::create_dir_all(cache.directory()).expect("cache directory");
        fs::write(cache.entry_path(&source_file), b"not-a-cache").expect("corrupt cache");
        assert!(
            cache
                .load(
                    &source_file,
                    FileFormat::Script,
                    "country_event = { id = cached.1 }\n"
                )
                .is_none()
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn cache_payload_does_not_duplicate_source_text() {
        let directory = test_directory("source-elision");
        let source = "country_event = { id = cached.1 }\n".repeat(32);
        let parsed = pdx_parser::parse(FileFormat::Script, &source);
        let cache = ParseCache::new(directory.join("parse-cache"));
        let source_file = file(&directory);
        cache
            .store(&source_file, FileFormat::Script, &source, &parsed)
            .expect("store parse cache");

        let bytes = fs::read(cache.entry_path(&source_file)).expect("read cache entry");
        assert_eq!(
            bytes.get(..4),
            Some([0x28, 0xb5, 0x2f, 0xfd].as_slice()),
            "parse cache entries are compressed"
        );
        let bytes = zstd::bulk::decompress(&bytes, MAX_PARSE_CACHE_BYTES as usize)
            .expect("decompress cache entry");
        let (entry, remaining) =
            postcard::take_from_bytes::<ParseCacheEntry>(&bytes).expect("decode cache entry");
        assert!(remaining.is_empty());
        let compact = postcard::to_allocvec(&entry.parsed).expect("encode compact payload");
        let legacy = postcard::to_allocvec(&parsed).expect("encode source-bearing payload");
        assert!(compact.len() < legacy.len());
        assert!(
            cache
                .load(&source_file, FileFormat::Script, &source)
                .is_some(),
            "source text is reattached after the compact payload is loaded"
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    fn test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "pdx-engine-parse-cache-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("test directory");
        directory
    }
}
