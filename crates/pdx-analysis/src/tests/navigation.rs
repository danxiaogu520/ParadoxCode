#![allow(unused_imports)]

use super::support::*;

#[test]
fn unresolved_symbol_is_diagnosed_without_a_definition() {
    let (host, id) = snapshot("event = missing.1\n");
    let diagnostics = diagnostics(&host.snapshot(), &id);
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == DiagnosticCode::UnknownSymbol)
    );
    assert!(definition(&host.snapshot(), &id, 8).is_empty());
}

#[test]
fn ambiguous_symbol_is_diagnosed_and_never_picks_a_definition() {
    let (host, id) = snapshot(
        "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = duplicate.1\n",
    );
    let snapshot = host.snapshot();
    assert!(
        diagnostics(&snapshot, &id)
            .iter()
            .any(|item| item.code == DiagnosticCode::AmbiguousSymbol)
    );
    assert!(definition(&snapshot, &id, 80).is_empty());
    let reference = u32::try_from(
        "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = "
            .len()
            + 1,
    )
    .expect("reference offset");
    let hover = hover(&snapshot, &id, reference).expect("ambiguous hover");
    assert!(hover.contents.contains("ambiguous event symbol"));
    assert!(hover.contents.contains("#### Candidates:\n\n- "));
    assert!(hover.contents.contains("Candidates:"));
}

#[test]
fn localisation_values_offer_indexed_localisation_symbols() {
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/localisation/test.yml");
    host.open_document(
        id.clone(),
        1,
        "l_english:\nfoo_name:0 \"Foo\"\nbar:0 \"\"\n".to_owned(),
        None,
    )
    .expect("open");
    let snapshot = host.snapshot();
    let result = complete(&snapshot, &id, 36);
    assert!(
        result
            .items
            .iter()
            .any(|item| item.label == "foo_name" && item.kind == CompletionKind::Localisation)
    );
}

#[test]
fn local_parameter_navigation_stays_within_its_scripted_definition() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-local-params-{nonce}"));
    let directory = root.join("common/scripted_effects");
    fs::create_dir_all(&directory).expect("scripted effect directory");
    let path = directory.join("parameters.txt");
    let text = concat!(
        "first = { value = $Amount$ again = $amount$ ",
        "[[optional] enabled = yes ] }\n",
        "second = { value = $amount$ }\n",
    );
    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    let id = DocumentId::new("file:///tmp/parameters.txt");
    host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
        .expect("open parameter document");
    let snapshot = host.snapshot();

    let second_use =
        u32::try_from(text.find("$amount$").expect("second use") + 1).expect("position");
    let first_name = TextRange::new(
        u32::try_from(text.find("Amount").expect("first definition")).expect("start"),
        u32::try_from(text.find("Amount").expect("first definition") + "Amount".len())
            .expect("end"),
    )
    .expect("first name range");
    assert_eq!(definition(&snapshot, &id, second_use)[0].range, first_name);

    let local_references = references(&snapshot, &id, second_use, true);
    assert_eq!(local_references.len(), 2);
    assert!(local_references.iter().all(|location| {
        location.range.end()
            < u32::try_from(text.find("second").expect("second definition")).expect("offset")
    }));

    let optional =
        u32::try_from(text.find("optional").expect("conditional parameter")).expect("offset");
    let hover = hover(&snapshot, &id, optional).expect("parameter hover");
    assert!(hover.contents.starts_with("### parameter `optional`"));
    assert!(hover.contents.contains("parameter `optional`"));
    assert!(hover.contents.contains("Arity: `optional`"));

    let preparation = prepare_rename(&snapshot, &id, second_use).expect("prepare local rename");
    assert_eq!(preparation.placeholder, "amount");
    let rename_plan = rename(&snapshot, &id, second_use, "total").expect("local rename");
    assert_eq!(rename_plan.edits.len(), 2);
    assert!(rename_plan.edits.iter().all(|edit| {
        edit.new_text == "total"
            && edit.location.range.end()
                < u32::try_from(text.find("second").expect("second definition")).expect("offset")
    }));
    assert_eq!(
        rename(&snapshot, &id, optional, "feature")
            .expect("conditional rename")
            .edits
            .len(),
        1
    );
    assert_eq!(
        rename(&snapshot, &id, second_use, "optional"),
        Err(RenameError::Conflict)
    );
    assert_eq!(
        rename(&snapshot, &id, second_use, "$invalid$"),
        Err(RenameError::InvalidName)
    );

    let parameter_symbols = document_symbols(&snapshot, &id)
        .into_iter()
        .filter(|symbol| symbol.kind == "parameter")
        .collect::<Vec<_>>();
    assert_eq!(
        parameter_symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Amount", "optional", "amount"]
    );
    assert_eq!(parameter_symbols[0].selection_range, first_name);
    assert!(
        workspace_symbols(&snapshot, "amount")
            .iter()
            .all(|symbol| symbol.kind != "parameter")
    );

    let second_owner_use =
        u32::try_from(text.rfind("$amount$").expect("second owner use") + 1).expect("position");
    let second_target = definition(&snapshot, &id, second_owner_use);
    assert_eq!(second_target.len(), 1);
    assert!(second_target[0].range.start() > first_name.end());

    let mut read_only = eu4_host(pdx_game::eu4::bootstrap_rules());
    read_only.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(2),
        SourceRootKind::Dependency,
        root.clone(),
    )]));
    let read_only_id = DocumentId::new("file:///tmp/read-only-parameters.txt");
    read_only
        .open_document(read_only_id.clone(), 1, text.to_owned(), Some(path))
        .expect("open dependency parameter document");
    let read_only_snapshot = read_only.snapshot();
    assert_eq!(
        prepare_rename(&read_only_snapshot, &read_only_id, second_use),
        Err(RenameError::ReadOnly)
    );
    assert_eq!(
        rename(&read_only_snapshot, &read_only_id, second_use, "total"),
        Err(RenameError::ReadOnly)
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn navigation_and_hover_use_local_event_definition() {
    let text = "country_event = { id = test.1 }\nevent = test.1\n";
    let (host, id) = snapshot(text);
    let snapshot = host.snapshot();
    let symbols = document_symbols(&snapshot, &id);
    assert_eq!(symbols.len(), 1);
    let definition_location = definition(&snapshot, &id, 40);
    assert_eq!(definition_location.len(), 1);
    let definition_name_start =
        u32::try_from(text.find("test.1").expect("definition name")).expect("offset");
    assert_eq!(
        definition_location[0].range,
        TextRange::new(definition_name_start, definition_name_start + 6)
            .expect("definition name range")
    );
    assert!(hover(&snapshot, &id, 40).is_some());
    let references = references(&snapshot, &id, 40, true);
    assert_eq!(references.len(), 2);
    assert!(references.iter().any(|location| {
        location.range
            == TextRange::new(definition_name_start, definition_name_start + 6)
                .expect("definition name range")
    }));
    assert!(!workspace_symbols(&snapshot, "test").is_empty());
    assert!(TextRange::new(0, 1).is_some());
}

#[test]
fn navigation_targets_the_name_in_an_indexed_definition() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("pdx-analysis-navigation-{nonce}"));
    let definitions = root.join("common/events");
    fs::create_dir_all(&definitions).expect("event directory");
    let definition_path = definitions.join("definitions.txt");
    let definition_text = "country_event = { id = indexed.1 }\n";
    fs::write(&definition_path, definition_text).expect("event definition");

    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::CurrentMod,
        root.clone(),
    )]));
    host.refresh_source_roots().expect("scan event definition");

    let id = DocumentId::new("file:///tmp/events/use.txt");
    let use_text = "event = indexed.1\n";
    let position =
        u32::try_from(use_text.find("indexed.1").expect("event reference")).expect("offset");
    host.open_document(id.clone(), 1, use_text.to_owned(), None)
        .expect("open event reference");

    let location = definition(&host.snapshot(), &id, position)
        .into_iter()
        .next()
        .expect("indexed definition location");
    let name_start =
        u32::try_from(definition_text.find("indexed.1").expect("definition name")).expect("offset");
    assert_eq!(
        location.range,
        TextRange::new(name_start, name_start + 9).expect("definition name range")
    );
    fs::remove_dir_all(root).expect("cleanup");
}
