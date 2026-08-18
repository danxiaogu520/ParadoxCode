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
}
