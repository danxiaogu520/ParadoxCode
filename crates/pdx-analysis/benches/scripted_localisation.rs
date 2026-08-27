//! Scripted-localisation registry benchmark.
//!
//! The fixture deliberately contains many ordinary script definitions and only a small
//! scripted-localisation subtree.  The cold query therefore makes the path-partitioning
//! optimization visible, while the warm query exercises the snapshot cache used by editor
//! diagnostics and completion.

use std::hint::black_box;
use std::time::{Duration, Instant};

use pdx_analysis::{complete, diagnostics, scripted_localisation_names};
use pdx_engine::{
    AnalysisHost, DocumentId, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};

const EVENT_FILES: usize = 2_000;
const SCRIPTED_FILES: usize = 16;
const NAMES_PER_FILE: usize = 100;

fn timed(mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    query();
    started.elapsed()
}

fn measured(iterations: usize, mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        query();
    }
    started.elapsed()
}

fn main() {
    let root = std::env::temp_dir().join(format!(
        "pdx-bench-scripted-localisation-{}",
        std::process::id()
    ));
    let events = root.join("events");
    let scripted = root.join("common/scripted_localisation");
    let localisation = root.join("localisation");
    std::fs::create_dir_all(&events).expect("create events fixture");
    std::fs::create_dir_all(&scripted).expect("create scripted-localisation fixture");
    std::fs::create_dir_all(&localisation).expect("create localisation fixture");

    for index in 0..EVENT_FILES {
        std::fs::write(
            events.join(format!("bench_{index:05}.txt")),
            format!("country_event = {{ id = bench.{index} }}\n"),
        )
        .expect("write event fixture");
    }
    for file_index in 0..SCRIPTED_FILES {
        let body = (0..NAMES_PER_FILE)
            .map(|name_index| {
                format!(
                    "defined_text = {{ name = Scripted.{file_index:02}.{name_index:03} text = yes }}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(scripted.join(format!("defs_{file_index:02}.txt")), body)
            .expect("write scripted-localisation fixture");
    }

    let mut host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().expect("first-party rules"),
        pdx_game::eu4::profile(),
    );
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    let scan = host.refresh_source_roots().expect("scan fixture");
    let names_cold = timed(|| {
        black_box(scripted_localisation_names(&host.snapshot()));
    });
    let snapshot = host.snapshot();
    let names_warm = measured(20, || {
        black_box(scripted_localisation_names(&snapshot));
    });

    let document = DocumentId::new("file:///bench/localisation/scripted.yml");
    let text = "l_english:\nentry: \"[ROOT.Scripted.00.000] [ROOT.MissingScripted]\"\n";
    host.open_document(
        document.clone(),
        1,
        text.to_owned(),
        Some(localisation.join("scripted.yml")),
    )
    .expect("open localisation overlay");
    let snapshot = host.snapshot();
    let diagnostics_elapsed = timed(|| {
        black_box(diagnostics(&snapshot, &document));
    });
    let completion_position = u32::try_from(text.find("Scripted.00.000").expect("command") + 13)
        .expect("bounded position");
    let completion_elapsed = measured(20, || {
        black_box(complete(&snapshot, &document, completion_position));
    });

    println!(
        "scripted localisation: {} names across {} files + {} ordinary files ({} indexed files)",
        SCRIPTED_FILES * NAMES_PER_FILE,
        SCRIPTED_FILES,
        EVENT_FILES,
        scan.indexed_files
    );
    println!(
        "registry: cold {:.3} ms, warm {:.3} ms/query",
        names_cold.as_secs_f64() * 1_000.0,
        names_warm.as_secs_f64() * 1_000.0 / 20.0
    );
    println!(
        "localisation diagnostics: {:.3} ms, completion: {:.3} ms/query",
        diagnostics_elapsed.as_secs_f64() * 1_000.0,
        completion_elapsed.as_secs_f64() * 1_000.0 / 20.0
    );

    std::fs::remove_dir_all(root).expect("cleanup fixture");
}
