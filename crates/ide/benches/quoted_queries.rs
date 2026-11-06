use std::hint::black_box;
use std::time::{Duration, Instant};

use engine::{AnalysisHost, DocumentId};
use ide::{complete, diagnostics};
use parser::encode_quoted_script_text;

fn measured(iterations: usize, mut query: impl FnMut()) -> Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        query();
    }
    started.elapsed()
}

fn main() {
    large_document_completion();
    large_payload_completion();
    let payload = (0..250)
        .map(|index| format!("add_prestige = {}", index % 5))
        .collect::<Vec<_>>()
        .join("\n");
    let encoded = encode_quoted_script_text(&payload);
    let source = format!(
        "country_event = {{ immediate = {{ bench_embed = {{ BODY = \"{encoded}\" }} }} }}\n"
    );
    let position = u32::try_from(
        source
            .find("add_prestige")
            .expect("quoted completion target")
            + "add_pre".len(),
    )
    .expect("bounded fixture");
    let mut host = AnalysisHost::with_ir(
        game::eu4::runtime_rules().expect("first-party rules"),
        game::eu4::profile(),
        game::eu4::first_party_ir().expect("first-party IR"),
    );
    open_definition(&mut host);
    let document = DocumentId::new("file:///bench/events/quoted-query.txt");
    host.open_document(document.clone(), 1, source, None)
        .expect("open benchmark document");
    let snapshot = host.snapshot();
    assert!(
        complete(&snapshot, &document, position)
            .items
            .iter()
            .any(|item| item.label == "add_prestige")
    );

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

/// An empty prefix offers many candidates; unrelated source must not be
/// reparsed and lowered once for every trial edit inside the script argument.
fn large_document_completion() {
    let mut expected = None;
    for unrelated in [0, 2_000] {
        let mut host = AnalysisHost::with_ir(
            game::eu4::runtime_rules().expect("first-party rules"),
            game::eu4::profile(),
            game::eu4::first_party_ir().expect("first-party IR"),
        );
        open_definition(&mut host);
        let mut source =
            "country_event = { immediate = { bench_embed = { BODY = \" \" } } }\n".to_owned();
        let position =
            (source.find("BODY = \"").expect("script argument") + "BODY = \"".len()) as u32;
        source
            .push_str(&"country_event = { immediate = { add_prestige = 1 } }\n".repeat(unrelated));
        let document = DocumentId::new("file:///bench/events/large-quoted-query.txt");
        host.open_document(document.clone(), 1, source, None)
            .expect("open large document");
        let snapshot = host.snapshot();
        let start = Instant::now();
        let result = complete(&snapshot, &document, position);
        assert!(result.items.iter().any(|item| item.label == "add_prestige"));
        let elapsed = start.elapsed();
        if let Some(directory) = std::env::var_os("PDC_COMPLETION_INVENTORY_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).expect("inventory output directory");
            std::fs::write(
                directory.join(format!("unrelated-{unrelated}.txt")),
                format!("{:#?}", result.items),
            )
            .expect("complete inventory before LSP truncation");
        }
        if let Some(expected) = &expected {
            assert_eq!(
                &result.items, expected,
                "unrelated events must not change completion"
            );
        } else {
            expected = Some(result.items.clone());
        }
        println!(
            "empty-prefix script completion ({unrelated} unrelated events): {:.3} ms",
            elapsed.as_secs_f64() * 1_000.0,
        );
    }
}

fn open_definition(host: &mut AnalysisHost) {
    host.open_document(
        DocumentId::new("file:///bench/common/scripted_effects/embed.txt"),
        1,
        "bench_embed = { $BODY$ }".to_owned(),
        None,
    )
    .expect("open script definition");
}

fn large_payload_completion() {
    for statements in [250, 2_000] {
        let mut host = AnalysisHost::with_ir(
            game::eu4::runtime_rules().expect("first-party rules"),
            game::eu4::profile(),
            game::eu4::first_party_ir().expect("first-party IR"),
        );
        open_definition(&mut host);
        let payload = format!("{}add_pre", "add_prestige = 1\n".repeat(statements));
        let source = format!(
            "country_event = {{ immediate = {{ bench_embed = {{ BODY = \"{}\" }} }} }}",
            encode_quoted_script_text(&payload)
        );
        let position = (source.rfind("add_pre").unwrap() + 7) as u32;
        let document = DocumentId::new("file:///bench/events/large-payload.txt");
        host.open_document(document.clone(), 1, source, None)
            .expect("open payload");
        let start = Instant::now();
        let result = complete(&host.snapshot(), &document, position);
        let elapsed = start.elapsed();
        assert!(result.items.iter().any(|item| item.label == "add_prestige"));
        println!(
            "script completion ({statements} statements inside argument): {:.3} ms",
            elapsed.as_secs_f64() * 1000.0
        );
    }
}
