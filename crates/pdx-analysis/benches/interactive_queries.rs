//! Interactive-query latency benchmark: hover, completion, and diagnostics against a
//! workspace with a Vanilla-sized file table and several open overlay documents.
//!
//! The fixture models the real EU4 workflow: tens of thousands of indexed files (Vanilla) plus
//! a handful of open mod files whose references must be resolved against both the index and
//! every open overlay. Reports the first (cold) call and the steady-state average separately,
//! because the snapshot query cache makes repeated interactions at one revision cheap.

use std::time::{Duration, Instant};

use pdx_analysis::{complete, diagnostics, hover};
use pdx_engine::{
    AnalysisHost, DocumentId, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};
use pdx_game::eu4;

const FILE_COUNT: usize = 4_000;
const OVERLAY_DOCS: usize = 4;
const REFERENCE_CALLS_PER_DOC: usize = 150;

fn measured(iterations: usize, mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        query();
    }
    started.elapsed()
}

fn main() {
    let root = std::env::temp_dir().join(format!("pdx-bench-interactive-{}", std::process::id()));
    let effects_dir = root.join("common/scripted_effects");
    std::fs::create_dir_all(&effects_dir).expect("create fixture directory");
    for index in 0..FILE_COUNT {
        let content = format!(
            "bench_effect_{index} = {{\n  add_prestige = {}\n  add_treasury = {}\n}}\n",
            index % 7,
            index % 11
        );
        std::fs::write(effects_dir.join(format!("bench_{index:05}.txt")), content)
            .expect("write fixture file");
    }
    let overlay_text = format!(
        "apply_effect = {{ add_prestige = 1 }}\n{}\n",
        (0..REFERENCE_CALLS_PER_DOC)
            .map(|index| format!(
                "wrapper_{index} = {{ apply_effect = yes add_prestige = {index} }}"
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
    for index in 0..OVERLAY_DOCS {
        std::fs::write(
            effects_dir.join(format!("overlay_{index}.txt")),
            &overlay_text,
        )
        .expect("write overlay fixture file");
    }

    let mut host = AnalysisHost::with_profile(
        eu4::first_party_rules().expect("first-party rules"),
        eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    let scan = host.refresh_source_roots().expect("scan fixture");
    println!("indexed {} files in the fixture", scan.indexed_files);

    let documents = (0..OVERLAY_DOCS)
        .map(|index| {
            let path = effects_dir.join(format!("overlay_{index}.txt"));
            let document = DocumentId::new(format!(
                "file:///bench/common/scripted_effects/overlay_{index}.txt"
            ));
            host.open_document(document.clone(), 1, overlay_text.clone(), Some(path))
                .expect("open overlay document");
            document
        })
        .collect::<Vec<_>>();
    let snapshot = host.snapshot();
    let current = &documents[0];

    let hover_position = u32::try_from(
        overlay_text
            .find("apply_effect = yes")
            .expect("hover target")
            + "apply_eff".len(),
    )
    .expect("bounded position");
    let completion_position = u32::try_from(
        overlay_text.find("wrapper_0").expect("completion target") + "wrapper_0 = { ".len(),
    )
    .expect("bounded position");

    let diagnostics_cold = timed(|| {
        let _ = diagnostics(&snapshot, current);
    });
    let diagnostics_warm = measured(10, || {
        let _ = diagnostics(&snapshot, current);
    });
    let hover_cold = timed(|| {
        let _ = hover(&snapshot, current, hover_position);
    });
    let hover_warm = measured(30, || {
        let _ = hover(&snapshot, current, hover_position);
    });
    let completion_cold = timed(|| {
        let _ = complete(&snapshot, current, completion_position);
    });
    let completion_warm = measured(30, || {
        let _ = complete(&snapshot, current, completion_position);
    });

    println!(
        "diagnostics: cold {:.3} ms, warm {:.3} ms/query ({} indexed files, {OVERLAY_DOCS} overlays, {REFERENCE_CALLS_PER_DOC} references per overlay)",
        diagnostics_cold.as_secs_f64() * 1_000.0,
        diagnostics_warm.as_secs_f64() * 1_000.0 / 10.0,
        scan.indexed_files,
    );
    println!(
        "hover:       cold {:.3} ms, warm {:.3} ms/query",
        hover_cold.as_secs_f64() * 1_000.0,
        hover_warm.as_secs_f64() * 1_000.0 / 30.0,
    );
    println!(
        "completion:  cold {:.3} ms, warm {:.3} ms/query",
        completion_cold.as_secs_f64() * 1_000.0,
        completion_warm.as_secs_f64() * 1_000.0 / 30.0,
    );

    std::fs::remove_dir_all(&root).expect("cleanup fixture");
}

fn timed(mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    query();
    started.elapsed()
}
