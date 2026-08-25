#![allow(unused_imports)]

use super::support::*;

#[test]
fn rename_updates_definition_and_resolved_references() {
    let text = "country_event = { id = test.1 }\nevent = test.1\n";
    let (mut host, id) = snapshot(text);
    let position = u32::try_from(text.rfind("test.1").expect("reference")).expect("offset");
    let prepared = prepare_rename(&host.snapshot(), &id, position).expect("prepare rename");
    assert_eq!(prepared.placeholder, "test.1");
    assert_eq!(prepared.range.len(), 6);

    let plan = rename(&host.snapshot(), &id, position, "renamed.1").expect("rename");
    assert_eq!(plan.edits.len(), 2);
    assert!(plan.edits[0].location.range.start() > plan.edits[1].location.range.start());
    let mut changed = text.to_owned();
    for edit in &plan.edits {
        let start = usize::try_from(edit.location.range.start()).expect("start");
        let end = usize::try_from(edit.location.range.end()).expect("end");
        changed.replace_range(start..end, &edit.new_text);
    }
    host.apply_document_changes(&id, 2, &[pdx_engine::TextChange::full(changed)])
        .expect("apply rename");
    assert!(diagnostics(&host.snapshot(), &id).iter().all(|item| {
        item.code != DiagnosticCode::UnknownSymbol && item.code != DiagnosticCode::AmbiguousSymbol
    }));
}

#[test]
fn rename_rejects_invalid_names_ambiguous_symbols_and_conflicts() {
    let (host, id) = snapshot(
        "country_event = { id = old.1 }\ncountry_event = { id = other.1 }\nevent = old.1\n",
    );
    let current_snapshot = host.snapshot();
    let old_position = u32::try_from("country_event = { id = ".len()).expect("offset");
    assert_eq!(
        rename(&current_snapshot, &id, old_position, "not a name").expect_err("invalid name"),
        RenameError::InvalidName
    );
    assert_eq!(
        rename(&current_snapshot, &id, old_position, "other.1").expect_err("conflict"),
        RenameError::Conflict
    );

    let ambiguous_text = "country_event = { id = duplicate.1 }\ncountry_event = { id = duplicate.1 }\nevent = duplicate.1\n";
    let (ambiguous_host, ambiguous_id) = snapshot(ambiguous_text);
    let reference =
        u32::try_from(ambiguous_text.rfind("duplicate.1").expect("reference")).expect("offset");
    assert_eq!(
        prepare_rename(&ambiguous_host.snapshot(), &ambiguous_id, reference)
            .expect_err("ambiguous symbol"),
        RenameError::Ambiguous
    );
}

#[test]
fn rename_rejects_dependency_and_vanilla_definitions() {
    use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
    use std::fs;

    let nonce = std::process::id();
    let root = std::env::temp_dir().join(format!("pdx-analysis-rename-{nonce}"));
    let dependency = root.join("dependency/events");
    fs::create_dir_all(&dependency).expect("dependency directory");
    let path = dependency.join("events.txt");
    fs::write(&path, "country_event = { id = read_only.1 }\n").expect("dependency event");

    let mut host = eu4_host(pdx_game::eu4::bootstrap_rules());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot {
        id: SourceRootId::new(1),
        kind: SourceRootKind::Dependency,
        path: root.join("dependency"),
        order: 0,
        writable: false,
    }]));
    host.refresh_source_roots().expect("scan dependency");
    let id = DocumentId::new("file:///dependency/events.txt");
    let text = "country_event = { id = read_only.1 }\n";
    host.open_document(id.clone(), 1, text.to_owned(), Some(path.clone()))
        .expect("open dependency overlay");
    let position = u32::try_from(text.find("read_only.1").expect("definition")).expect("offset");
    assert_eq!(
        prepare_rename(&host.snapshot(), &id, position).expect_err("read-only definition"),
        RenameError::ReadOnly
    );
    fs::remove_dir_all(root).expect("cleanup");
}
