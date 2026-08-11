use std::hint::black_box;
use std::time::{Duration, Instant};

use pdx_analysis::{complete, diagnostics};
use pdx_engine::{AnalysisHost, DocumentId};
use pdx_parser::encode_quoted_script_text;

fn measured(iterations: usize, mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        query();
    }
    started.elapsed()
}

fn main() {
    let payload = (0..250)
        .map(|index| format!("add_prestige = {}", index % 5))
        .collect::<Vec<_>>()
        .join("\n");
    let encoded = encode_quoted_script_text(&payload);
    let source = format!(
        "country_event = {{ immediate = {{ for_variable_amount = {{ variable = bench effect = \"{encoded}\" }} }} }}\n"
    );
    let position = u32::try_from(
        source
            .find("add_prestige")
            .expect("quoted completion target")
            + "add_pre".len(),
    )
    .expect("bounded fixture");
    let mut host = AnalysisHost::with_profile(
        pdx_game::eu4::first_party_rules().expect("first-party rules"),
        pdx_game::eu4::profile(),
    );
    let document = DocumentId::new("file:///bench/events/quoted-query.txt");
    host.open_document(document.clone(), 1, source, None)
        .expect("open benchmark document");
    let snapshot = host.snapshot();

    let diagnostics_elapsed = measured(50, || {
        black_box(diagnostics(&snapshot, &document));
    });
    let completion_elapsed = measured(200, || {
        black_box(complete(&snapshot, &document, position));
    });

    println!(
        "quoted diagnostics: {:.3} ms/query (250 inner properties)",
        diagnostics_elapsed.as_secs_f64() * 1_000.0 / 50.0
    );
    println!(
        "quoted completion: {:.3} ms/query (250 inner properties)",
        completion_elapsed.as_secs_f64() * 1_000.0 / 200.0
    );
}
