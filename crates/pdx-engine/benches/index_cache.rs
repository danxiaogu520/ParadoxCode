use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pdx_engine::{
    AnalysisHost, IndexCache, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
    WorkspaceScanToken,
};

const DEFAULT_FILE_COUNT: usize = 8_000;

struct SyntheticVanilla {
    root: PathBuf,
}

impl SyntheticVanilla {
    fn create(file_count: usize) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdx-engine-vanilla-benchmark-{}-{nonce}",
            std::process::id()
        ));
        let effects = root.join("common/scripted_effects");
        fs::create_dir_all(&effects).expect("create synthetic effect directory");
        for index in 0..file_count {
            fs::write(
                effects.join(format!("effect-{index:05}.txt")),
                format!(
                    "effect_{index} = {{ add_prestige = {} effect_{} = yes add_treasury = {} }}\n",
                    index % 7,
                    (index + 1) % file_count,
                    index % 11
                ),
            )
            .expect("write synthetic effect");
        }
        Self { root }
    }
}

impl Drop for SyntheticVanilla {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!(
                "failed to clean benchmark fixture {}: {error}",
                self.root.display()
            );
        }
    }
}

/// Dense localisation fixture that reproduces the symbol volume of a real Vanilla install
/// (hundreds of thousands to millions of definitions), so the `install_index_cache` merge —
/// which dominates LSP startup for EU4-scale data — is measurable rather than sub-millisecond.
struct DenseVanilla {
    root: PathBuf,
}

impl DenseVanilla {
    fn create(file_count: usize, entries_per_file: usize) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pdx-engine-dense-benchmark-{}-{nonce}",
            std::process::id()
        ));
        let localisation = root.join("localisation");
        fs::create_dir_all(&localisation).expect("create localisation directory");
        // Localisation keys are the dominant symbol population in EU4; one entry per line
        // keeps the parse HIR-heavy in the same way the real files are.
        for file in 0..file_count {
            let mut text = String::with_capacity(entries_per_file.saturating_mul(28));
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

impl Drop for DenseVanilla {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!(
                "failed to clean benchmark fixture {}: {error}",
                self.root.display()
            );
        }
    }
}

fn measured<T>(operation: impl FnOnce() -> T) -> (Duration, T) {
    let started = Instant::now();
    let result = operation();
    (started.elapsed(), result)
}

fn file_count() -> usize {
    std::env::var("PDX_BENCH_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_FILE_COUNT)
}

const DEFAULT_DENSE_FILES: usize = 200;
const DEFAULT_DENSE_ENTRIES: usize = 5_000;
const DEFAULT_MIXED_CURRENT_FILES: usize = 64;
const DEFAULT_MIXED_CURRENT_ENTRIES: usize = 2_000;

fn dense_file_count() -> usize {
    std::env::var("PDX_BENCH_DENSE_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_DENSE_FILES)
}

fn dense_entries() -> usize {
    std::env::var("PDX_BENCH_DENSE_ENTRIES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_DENSE_ENTRIES)
}

fn mixed_current_file_count() -> usize {
    std::env::var("PDX_BENCH_MIXED_CURRENT_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_MIXED_CURRENT_FILES)
}

fn mixed_current_entries() -> usize {
    std::env::var("PDX_BENCH_MIXED_CURRENT_ENTRIES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_MIXED_CURRENT_ENTRIES)
}

fn display_millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn main() {
    let count = file_count();
    let fixture = SyntheticVanilla::create(count);
    let mut builder =
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile());
    builder.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        fixture.root.clone(),
    )]));
    let (scan, report) = measured(|| builder.refresh_source_roots().expect("scan fixture"));
    assert_eq!(report.indexed_files, count);
    black_box(builder.snapshot());

    let cache_path = fixture.root.join("vanilla.pdxindex");
    let (build, cache) =
        measured(|| IndexCache::from_snapshot(&builder.snapshot()).expect("build cache"));
    let (save, _) = measured(|| cache.save(&cache_path).expect("save cache"));
    let position_entries = cache.index().position_ranges().len();
    drop(cache);
    drop(builder);

    // Position payload compaction versus the naive six-i64-per-entry column layout.
    let connection = rusqlite::Connection::open(&cache_path).expect("open cache for metrics");
    let position_bytes: i64 = connection
        .query_row(
            "SELECT COALESCE(SUM(length(payload)), 0) FROM navigation_positions",
            [],
            |row| row.get(0),
        )
        .expect("position payload bytes");
    drop(connection);
    let naive_bytes = position_entries.saturating_mul(48);

    let (load, cache) = measured(|| {
        IndexCache::load_cancellable_for_install(&cache_path, &WorkspaceScanToken::new())
            .expect("load cache")
    });
    let mut host =
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile());
    let (install, _) = measured(|| host.install_index_cache(cache).expect("install cache"));
    black_box(host.snapshot());

    println!("vanilla cache: {count} files");
    println!("scan fixture:       {:>10.3} ms", display_millis(scan));
    println!("build cache:        {:>10.3} ms", display_millis(build));
    println!("save cache:         {:>10.3} ms", display_millis(save));
    println!("load cache:         {:>10.3} ms", display_millis(load));
    println!("install cache:      {:>10.3} ms", display_millis(install));
    println!("position entries:   {position_entries}");
    println!(
        "position payload:   {position_bytes} bytes ({:.1}% of the naive 6-column layout)",
        if naive_bytes == 0 {
            0.0
        } else {
            position_bytes as f64 * 100.0 / naive_bytes as f64
        }
    );

    // EU4-scale dense fixture: isolates the `install_index_cache` merge cost.
    let dense_files = dense_file_count();
    let dense_entries = dense_entries();
    let dense_fixture = DenseVanilla::create(dense_files, dense_entries);
    let mut dense_builder =
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile());
    dense_builder.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        dense_fixture.root.clone(),
    )]));
    let (dense_scan, dense_report) = measured(|| {
        dense_builder
            .refresh_source_roots()
            .expect("scan dense fixture")
    });
    assert_eq!(dense_report.indexed_files, dense_files);
    black_box(dense_builder.snapshot());
    let dense_cache_path = dense_fixture.root.join("vanilla.pdxindex");
    let (dense_build, dense_cache) = measured(|| {
        IndexCache::from_snapshot(&dense_builder.snapshot()).expect("build dense cache")
    });
    let (dense_save, _) = measured(|| {
        dense_cache
            .save(&dense_cache_path)
            .expect("save dense cache")
    });
    let dense_positions = dense_cache.index().position_ranges().len();
    drop(dense_cache);
    drop(dense_builder);
    let (dense_load, dense_cache) = measured(|| {
        IndexCache::load_cancellable_for_install(&dense_cache_path, &WorkspaceScanToken::new())
            .expect("load dense cache")
    });
    let mut dense_host =
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile());
    let (dense_install, _) = measured(|| {
        dense_host
            .install_index_cache(dense_cache)
            .expect("install dense cache")
    });
    black_box(dense_host.snapshot());

    println!(
        "\ndense localisation: {dense_files} file(s) x {dense_entries} entries = {dense_positions} position(s)"
    );
    println!(
        "dense scan fixture:    {:>10.3} ms",
        display_millis(dense_scan)
    );
    println!(
        "dense build cache:     {:>10.3} ms",
        display_millis(dense_build)
    );
    println!(
        "dense save cache:      {:>10.3} ms",
        display_millis(dense_save)
    );
    println!(
        "dense load cache:      {:>10.3} ms",
        display_millis(dense_load)
    );
    println!(
        "dense install cache:   {:>10.3} ms",
        display_millis(dense_install)
    );

    // Mixed fixture: an already-indexed Current Mod plus a dense Vanilla cache.  This is the
    // scenario in which per-file position replacement used to rescan the complete cache map.
    let mixed_current_files = mixed_current_file_count();
    let mixed_current_entries = mixed_current_entries();
    let mixed_current_fixture = DenseVanilla::create(mixed_current_files, mixed_current_entries);
    let mut mixed_host =
        AnalysisHost::with_profile(pdx_game::eu4::bootstrap_rules(), pdx_game::eu4::profile());
    mixed_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(u32::MAX),
        SourceRootKind::CurrentMod,
        mixed_current_fixture.root.clone(),
    )]));
    let (mixed_scan, mixed_report) = measured(|| {
        mixed_host
            .refresh_source_roots()
            .expect("scan mixed Current Mod fixture")
    });
    assert_eq!(mixed_report.indexed_files, mixed_current_files);
    let mixed_current_positions = mixed_host.snapshot().index().position_ranges().len();
    let expected_positions = dense_positions.saturating_add(mixed_current_positions);
    let (mixed_load, mixed_cache) = measured(|| {
        IndexCache::load_cancellable_for_install(&dense_cache_path, &WorkspaceScanToken::new())
            .expect("load mixed Vanilla cache")
    });
    let (mixed_install, _) = measured(|| {
        mixed_host
            .install_index_cache(mixed_cache)
            .expect("install mixed Vanilla cache")
    });
    let mixed_snapshot = mixed_host.snapshot();
    assert_eq!(
        mixed_snapshot.index().position_ranges().len(),
        expected_positions,
        "mixed install must retain Current Mod and Vanilla positions"
    );
    black_box(mixed_snapshot);
    println!(
        "\nmixed workspace: {mixed_current_files} Current Mod file(s) x {mixed_current_entries} entries + {dense_files} Vanilla file(s) x {dense_entries} entries"
    );
    println!(
        "mixed Current Mod positions: {mixed_current_positions}; expected merged positions: {expected_positions}"
    );
    println!(
        "mixed scan Current Mod: {:>10.3} ms",
        display_millis(mixed_scan)
    );
    println!(
        "mixed load Vanilla cache: {:>10.3} ms",
        display_millis(mixed_load)
    );
    println!(
        "mixed install cache:       {:>10.3} ms",
        display_millis(mixed_install)
    );
}
