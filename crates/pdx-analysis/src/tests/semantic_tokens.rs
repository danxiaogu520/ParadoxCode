use crate::tests::support::*;
use crate::{
    CancellationToken, SemanticTokenType, semantic_tokens,
    semantic_tokens_in_range_with_cancellation,
};
use pdx_engine::DocumentId;
use pdx_text::TextRange;

type Spelling = (String, SemanticTokenType, bool);

fn token_spellings(host: &AnalysisHost, id: &DocumentId, text: &str) -> Vec<Spelling> {
    let snapshot = host.snapshot();
    semantic_tokens(&snapshot, id)
        .into_iter()
        .map(|token| {
            let range = TextRange::new(token.range.start(), token.range.end()).expect("range");
            let start = usize::try_from(range.start()).expect("start");
            let end = usize::try_from(range.end()).expect("end");
            (
                text[start..end].to_owned(),
                token.token_type,
                token.definition,
            )
        })
        .collect()
}

#[test]
fn script_tokens_cover_comments_operators_keys_and_scalars() {
    let text = "# note\ncountry_event = { id = test.1 quux = 3.5 whatever = yes }\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    assert_eq!(
        tokens,
        vec![
            ("# note".to_owned(), SemanticTokenType::Comment, false),
            (
                "country_event".to_owned(),
                SemanticTokenType::Function,
                false
            ),
            ("=".to_owned(), SemanticTokenType::Operator, false),
            ("id".to_owned(), SemanticTokenType::Function, false),
            ("=".to_owned(), SemanticTokenType::Operator, false),
            ("test.1".to_owned(), SemanticTokenType::String, false),
            ("quux".to_owned(), SemanticTokenType::Property, false),
            ("=".to_owned(), SemanticTokenType::Operator, false),
            ("3.5".to_owned(), SemanticTokenType::Number, false),
            ("whatever".to_owned(), SemanticTokenType::Property, false),
            ("=".to_owned(), SemanticTokenType::Operator, false),
            ("yes".to_owned(), SemanticTokenType::Boolean, false),
        ]
    );
}

#[test]
fn indexed_scripted_macro_names_use_the_function_token_color() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let root = std::env::temp_dir().join(format!(
        "pdx-analysis-semantic-macro-colors-{}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("common/scripted_effects")).expect("effect directory");
    fs::create_dir_all(root.join("common/scripted_triggers")).expect("trigger directory");
    fs::write(
        root.join("common/scripted_effects/00_test.txt"),
        "apply = { add_prestige = 1 }\n",
    )
    .expect("scripted effect definition");
    fs::write(
        root.join("common/scripted_triggers/00_test.txt"),
        "check = { always = yes }\n",
    )
    .expect("scripted trigger definition");

    let mut host = eu4_host(pdx_game::eu4::first_party_rules().expect("first-party rules"));
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("index scripted macros");

    let id = DocumentId::new("file:///tmp/events/semantic-macros.txt");
    let text = "country_event = { immediate = { apply = yes } trigger = { check = yes } }\n";
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open source document");
    let tokens = token_spellings(&host, &id, text);
    assert!(
        tokens.contains(&("apply".to_owned(), SemanticTokenType::Function, false)),
        "scripted_effect calls should use the function token: {tokens:?}"
    );
    assert!(
        tokens.contains(&("check".to_owned(), SemanticTokenType::Function, false)),
        "scripted_trigger calls should use the function token: {tokens:?}"
    );

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn variable_definitions_and_parameter_usages_are_distinguished() {
    let text = "@cost = 100\nlimit = { has_manpower = @cost }\n$name$ = { trigger = $name$ }\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    let positions = tokens
        .iter()
        .map(|(spelling, token_type, definition)| (spelling.as_str(), *token_type, *definition))
        .collect::<Vec<_>>();
    assert!(positions.contains(&("@cost", SemanticTokenType::Variable, true)));
    assert!(positions.contains(&("100", SemanticTokenType::Number, false)));
    assert!(positions.contains(&("@cost", SemanticTokenType::Variable, false)));
    assert!(positions.contains(&("$name$", SemanticTokenType::Parameter, false)));
}

#[test]
fn boolean_spellings_match_the_syntax_layer() {
    // The TextMate grammar colours true/false as booleans; the semantic layer must agree so
    // highlighting does not change when the language server becomes ready.
    let text = "a = true b = false c = yes d = no\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    for spelling in ["true", "false", "yes", "no"] {
        assert!(
            tokens.contains(&(spelling.to_owned(), SemanticTokenType::Boolean, false)),
            "{spelling} should be a boolean token"
        );
    }
}

#[test]
fn headers_and_parameter_conditions_use_type_and_parameter_tokens() {
    let text = "rgb { 1 2 3 }\n[[!country] foo = bar ]\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    assert!(tokens.contains(&("rgb".to_owned(), SemanticTokenType::Type, false)));
    assert!(tokens.contains(&("1".to_owned(), SemanticTokenType::Number, false)));
    assert!(tokens.contains(&("country".to_owned(), SemanticTokenType::Parameter, false)));
    assert!(tokens.contains(&("foo".to_owned(), SemanticTokenType::Property, false)));
    assert!(tokens.contains(&("bar".to_owned(), SemanticTokenType::String, false)));
}

#[test]
fn control_flow_keys_are_keywords_above_functions() {
    // Profile control-flow keys take precedence over the rule-known Function classification so
    // the script skeleton (if/limit/not/...) reads differently from effects and triggers.
    let text = "country_event = {\n  id = test.1\n  trigger = { NOT = { has_dlc = \"x\" } }\n  immediate = { limit = { always = yes } }\n}\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    for spelling in ["trigger", "NOT", "immediate", "limit"] {
        assert!(
            tokens.contains(&(spelling.to_owned(), SemanticTokenType::Keyword, false)),
            "{spelling} should be a keyword token"
        );
    }
    // `always` is a profile fallback key, so it stays Function-colored.
    assert!(tokens.contains(&("always".to_owned(), SemanticTokenType::Function, false)));
}

#[test]
fn quoted_values_stay_strings_and_quoted_keys_stay_properties() {
    let text = "monarch_names = { \"Friedrich #0\" = 100 }\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    assert!(tokens.contains(&(
        "\"Friedrich #0\"".to_owned(),
        SemanticTokenType::Property,
        false
    )));
    assert!(tokens.contains(&("100".to_owned(), SemanticTokenType::Number, false)));
}

#[test]
fn localisation_documents_produce_no_tokens() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/localisation/test_l_english.yml");
    host.open_document(
        id.clone(),
        1,
        "l_english:\nhello:0 \"Hi\"\n".to_owned(),
        None,
    )
    .expect("open");
    assert!(token_spellings(&host, &id, "l_english:\nhello:0 \"Hi\"\n").is_empty());
}

#[test]
fn unknown_documents_produce_no_tokens() {
    let (host, _) = snapshot("country_event = { }\n");
    let missing = DocumentId::new("file:///tmp/events/not-open.txt");
    assert!(token_spellings(&host, &missing, "").is_empty());
}

#[test]
fn syntax_errors_do_not_suppress_recoverable_tokens() {
    // An unterminated block produced a syntax diagnostic, but the recoverable property and
    // scalar inside it are still classified.
    let text = "country_event = { id = test.1 quux = 3\n";
    let (host, id) = snapshot(text);
    let tokens = token_spellings(&host, &id, text);
    assert!(tokens.contains(&(
        "country_event".to_owned(),
        SemanticTokenType::Function,
        false
    )));
    assert!(tokens.contains(&("quux".to_owned(), SemanticTokenType::Property, false)));
    assert!(tokens.contains(&("3".to_owned(), SemanticTokenType::Number, false)));
}

#[test]
fn ranged_semantic_tokens_skip_tokens_outside_the_viewport() {
    let text = "country_event = { id = first.1 }\ncountry_event = { id = second.1 }\n";
    let (host, id) = snapshot(text);
    let line_start = text
        .find("country_event = { id = second")
        .expect("second line");
    let range = TextRange::new(
        u32::try_from(line_start).expect("range start"),
        u32::try_from(text.len()).expect("range end"),
    )
    .expect("valid range");
    let tokens = semantic_tokens_in_range_with_cancellation(
        &host.snapshot(),
        &id,
        Some(range),
        &CancellationToken::new(),
    )
    .expect("range query");
    assert!(!tokens.is_empty());
    assert!(
        tokens
            .iter()
            .all(|token| token.range.start() >= range.start())
    );
    assert!(tokens.iter().any(|token| {
        let start = usize::try_from(token.range.start()).expect("token start");
        text[start..]
            .strip_prefix("country_event")
            .is_some_and(|_| start == line_start)
    }));
}
