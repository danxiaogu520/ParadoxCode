use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pdx_workspace::{
    AnalysisHost, DocumentId, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};

const DEFAULT_FILE_COUNT: usize = 2_000;

struct SyntheticWorkspace {
    root: PathBuf,
}

impl SyntheticWorkspace {
    fn create(file_count: usize) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("pdx-workspace-benchmark-{}-{nonce}", std::process::id()));
        let events = root.join("events");
        fs::create_dir_all(&events).expect("create synthetic event directory");
        for index in 0..file_count {
            fs::write(
                events.join(format!("event-{index:05}.txt")),
                format!(
                    "country_event = {{ id = synthetic.{index} immediate = {{ country_event = {{ id = synthetic.{} }} }} }}\n",
                    (index + 1) % file_count
                ),
            )
            .expect("write synthetic event");
        }
        Self { root }
    }

    fn event_path(&self, index: usize) -> PathBuf {
        self.root.join("events").join(format!("event-{index:05}.txt"))
    }
}

impl Drop for SyntheticWorkspace {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("failed to clean benchmark fixture {}: {error}", self.root.display());
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

fn document_id(path: &Path) -> DocumentId {
    DocumentId::new(format!("file://{}", path.display()))
}

fn main() {
    let count = file_count();
    let fixture = SyntheticWorkspace::create(count);
    let mut host =
        AnalysisHost::with_profile(pdx_game_eu4::bootstrap_rules(), pdx_game_eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        fixture.root.clone(),
    )]));

    let (initial_scan, report) = measured(|| host.refresh_source_roots().expect("initial scan"));
    assert_eq!(report.indexed_files, count);
    black_box(host.snapshot());

    let (unchanged_scan, report) =
        measured(|| host.refresh_source_roots().expect("unchanged scan"));
    assert_eq!(report.indexed_files, count);
    black_box(host.snapshot());

    fs::write(
        fixture.event_path(0),
        "country_event = { id = synthetic.changed immediate = { country_event = { id = synthetic.1 } } }\n",
    )
    .expect("change one synthetic event");
    let (single_disk_change, report) =
        measured(|| host.refresh_source_roots().expect("single-file disk refresh"));
    assert_eq!(report.indexed_files, count);
    black_box(host.snapshot());

    let path = fixture.event_path(0);
    let id = document_id(&path);
    host.stage_open_document(
        id.clone(),
        1,
        "country_event = { id = synthetic.changed }\n".to_owned(),
        Some(path),
    )
    .expect("stage overlay");
    let initial_overlay = host.snapshot().prepare_document(&id).expect("prepare overlay");
    assert!(host.commit_prepared_document(initial_overlay));
    let (single_overlay_edit, committed) = measured(|| {
        host.stage_document_text(
            &id,
            2,
            "country_event = { id = synthetic.overlay_changed }\n".to_owned(),
        )
        .expect("stage overlay edit");
        let prepared = host.snapshot().prepare_document(&id).expect("prepare overlay edit");
        host.commit_prepared_document(prepared)
    });
    assert!(committed);
    black_box(host.snapshot());

    println!("synthetic workspace: {count} EU4 event files");
    println!("initial scan/index:     {:>10.3} ms", display_millis(initial_scan));
    println!("unchanged full refresh: {:>10.3} ms", display_millis(unchanged_scan));
    println!("one disk-file refresh:  {:>10.3} ms", display_millis(single_disk_change));
    println!("one overlay edit:        {:>10.3} ms", display_millis(single_overlay_edit));
}
