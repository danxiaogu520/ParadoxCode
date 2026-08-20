//! Mission-preview localisation latency benchmark.
//!
//! The `pdx/missionPreview` handler resolves every `{mission}_title` key through
//! `pdx_analysis::localisation_values_by_key` on each preview refresh (the VS Code
//! client refreshes on every edit, debounced). This fixture reproduces the symbol
//! volume of a real EU4 install (a dense localisation index of hundreds of
//! thousands of keys) and measures the cost of resolving the title keys, which used
//! to force a full-workspace semantic rebuild per request.

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pdx_analysis::{CancellationToken, localisation_values_by_key};
use pdx_engine::{AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
use pdx_game::eu4;

const DEFAULT_DENSE_FILES: usize = 100;
const DEFAULT_DENSE_ENTRIES: usize = 3_000;
const DEFAULT_TITLE_COUNT: usize = 40;

/// Dense localisation fixture reproducing the symbol volume of a real Vanilla
/// install (hundreds of thousands of localisation definitions).
struct DenseLocalisation {
    root: PathBuf,
}

impl DenseLocalisation {
    fn create(file_count: usize, entries_per_file: usize) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdx-analysis-preview-benchmark-{}-{nonce}",
            std::process::id()
        ));
        let localisation = root.join("localisation");
        fs::create_dir_all(&localisation).expect("create localisation directory");
        for file in 0..file_count {
            let mut text = String::with_capacity(entries_per_file.saturating_mul(24));
            text.push_str("l_english:\n");
            for entry in 0..entries_per_file {
                text.push_str(&format!("key_{file}_{entry}:0 \"Value {file} {entry}\"\n"));
            }
            fs::write(
                localisation.join(format!("loc_{file:04}_l_english.yml")),
                text,
            )
            .expect("write dense localisation file");
        }
        Self { root }
    }
}

impl Drop for DenseLocalisation {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!(
                "failed to clean benchmark fixture {}: {error}",
                self.root.display()
            );
        }
    }
}

fn dense_files() -> usize {
    std::env::var("PDX_BENCH_PREVIEW_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_DENSE_FILES)
}

fn dense_entries() -> usize {
    std::env::var("PDX_BENCH_PREVIEW_ENTRIES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_DENSE_ENTRIES)
}

fn title_count() -> usize {
    std::env::var("PDX_BENCH_PREVIEW_TITLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_TITLE_COUNT)
}

fn timed(query: impl FnOnce()) -> Duration {
    let started = Instant::now();
    query();
    started.elapsed()
}

fn main() {
    let files = dense_files();
    let entries = dense_entries();
    let title_keys = title_count();
    let fixture = DenseLocalisation::create(files, entries);

    let mut host = AnalysisHost::with_profile(
        eu4::first_party_rules().expect("first-party rules"),
        eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fixture.root.clone(),
    )]));
    let report = host
        .refresh_source_roots()
        .expect("scan dense localisation");
    assert_eq!(report.indexed_files, files);
    let snapshot = host.snapshot();

    // Resolve distinct title keys that exist in the index, plus one that is missing,
    // mirroring how a mission preview resolves `{mission}_title` keys.
    let mut keys = (0..title_keys)
        .map(|index| format!("key_{}_{}", index % files, (index * 37) % entries))
        .collect::<Vec<_>>();
    keys.push("no_such_mission_title".to_owned());
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    black_box(&key_refs);

    let total_keys = key_refs.len();
    let cold = timed(|| {
        let _ = localisation_values_by_key(&snapshot, &key_refs, &CancellationToken::new())
            .expect("resolve title keys");
    });
    let warm = timed(|| {
        let _ = localisation_values_by_key(&snapshot, &key_refs, &CancellationToken::new())
            .expect("resolve title keys");
    });

    println!(
        "mission title resolution: {total_keys} key(s) against a {files}-file x {entries}-entry localisation index ({}-definition workspace)",
        files * entries
    );
    println!(
        "cold (first) pass:  {:>10.3} ms",
        cold.as_secs_f64() * 1_000.0
    );
    println!(
        "warm (repeat) pass: {:>10.3} ms",
        warm.as_secs_f64() * 1_000.0
    );
}
