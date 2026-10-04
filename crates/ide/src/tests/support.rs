pub(crate) use crate::{
    CancellationToken, Cancelled, CompletionKind, DiagnosticCode, RenameError, RenameFailure,
    complete, complete_with_cancellation, definition, diagnostics, diagnostics_with_cancellation,
    document_symbols, hover, input_for_document, prepare_rename, references, rename,
    rename_with_cancellation, scope_inlay_hints_with_cancellation, workspace_symbols,
    workspace_symbols_with_cancellation,
};
pub(crate) use engine::{
    AnalysisHost, AnalysisSnapshot, DocumentId, IndexCache, SourceRoot, SourceRootId,
    SourceRootKind, WorkspaceChange,
};
pub(crate) use rules::RuleSet;
pub(crate) use text::{LogicalPath, TextRange};

pub(crate) fn eu4_host(_catalog: RuleSet) -> AnalysisHost {
    let ir = game::eu4::first_party_ir().expect("embedded IR");
    AnalysisHost::with_ir(RuleSet::from_ir_catalog(&ir), ir.game.profile.clone(), ir)
}

/// Checks the actual compiled vocabulary at the cursor, without rebuilding
/// the legacy context/parent-path representation on the IR path.
pub(crate) fn assert_ir_context_keys(
    snapshot: &AnalysisSnapshot,
    input: &crate::support::ParsedInput,
    position: u32,
    keys: &[&str],
) {
    let hir = input.hir.as_ref().expect("HIR");
    assert!(hir.uses_ir());
    let fact = hir.schema_at(position).expect("compiled cursor schema");
    let ir = snapshot.ir();
    for key in keys {
        assert!(ir.fields(fact.schema).iter().any(|id| {
            matches!(ir.matcher(ir.field(*id).key), rules::ir::Matcher::Literal(name) if ir.strings().resolve(*name).eq_ignore_ascii_case(key))
        }), "missing `{key}` in compiled schema `{}`", ir.strings().resolve(ir.schema(fact.schema).name));
    }
}

/// Creates an isolated fixture root under the system temp directory. Cleanup
/// stays with the caller (`fs::remove_dir_all`), matching the existing tests.
pub(crate) fn temp_root(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ide-{tag}-{nonce}"));
    std::fs::create_dir_all(&root).expect("fixture root");
    root
}

pub(crate) fn snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    let mut host = eu4_host(game::eu4::bootstrap_rules());
    let id = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    (host, id)
}

pub(crate) fn semantic_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    semantic_snapshot_with_constraints(text, None, None, Some(1))
}

pub(crate) fn semantic_snapshot_with_severity(
    text: &str,
    severity: Option<u8>,
) -> (AnalysisHost, DocumentId) {
    semantic_snapshot_with_constraints(text, severity, None, Some(1))
}

pub(crate) fn semantic_snapshot_with_constraints(
    text: &str,
    severity: Option<u8>,
    min_occurs: Option<u32>,
    max_occurs: Option<u32>,
) -> (AnalysisHost, DocumentId) {
    let mut field = serde_json::json!({"value":"bool", "card":format!("{}..{}", min_occurs.unwrap_or(0), max_occurs.map_or("*".to_owned(),|n|n.to_string()))});
    if let Some(severity) = severity {
        field["severity"] = serde_json::json!(match severity {
            1 => "error",
            2 => "warning",
            _ => "info",
        });
    }
    let mut host =
        fixture_host(serde_json::json!({"schemas":{"trigger":{"fields":{"foo":field}}}}));
    let id = DocumentId::new("file:///tmp/events/test.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    (host, id)
}

pub(crate) fn quoted_script_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    let mut host = fixture_host(serde_json::json!({
        "traits":{"Template":{}},
        "types":{"fixture_template":{"impl":{"Template":{"body":"trigger"}},"resolution":"replace"}},
        "schemas":{
            "fixture_root":{"fields":{"__templates":{"body":"template_definitions","card":"0..*"}}},
            "template_definitions":{"map":{"key":"def<fixture_template>","body":"trigger"}},
            "template_arguments":{"map":{"key":"scalar","value":"scalar"}},
            "trigger":{"fields":{"foo":{"value":"bool","card":"0..*"}},
                "patterns":[{"key":"ref<fixture_template>","body":"template_arguments","card":"0..*"}]}
        }
    }));
    host.open_document(
        DocumentId::new("file:///tmp/template-definitions.txt"),
        1,
        "__templates = { embedded = { $BODY$ } nested = { $BODY$ } }".to_owned(),
        None,
    )
    .unwrap();
    let id = DocumentId::new("file:///tmp/events/quoted-script.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .expect("open");
    (host, id)
}

/// Compiles a small declarative rule package for generic consumer tests.
pub(crate) fn fixture_host(patch: serde_json::Value) -> AnalysisHost {
    fn merge(target: &mut serde_json::Value, patch: serde_json::Value) {
        match (target, patch) {
            (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
                for (key, value) in patch {
                    merge(target.entry(key).or_insert(serde_json::Value::Null), value);
                }
            }
            (target, patch) => *target = patch,
        }
    }
    let mut source = serde_json::json!({
        "files":{"fixture":{"path":"","ext":"txt","root":"fixture_root"}},
        "schemas":{
            "fixture_root":{"fields":{
                "trigger":{"body":"trigger","card":"0..*"},
                "country_event":{"body":"fixture_event","card":"0..*","scope":{"push":"country"}},
                "province_event":{"body":"fixture_event","card":"0..*","scope":{"push":"province"}}
            }},
            "fixture_event":{"fields":{
                "id":{"value":"scalar","card":"0..1"},
                "trigger":{"body":"trigger","card":"0..1"},
                "immediate":{"body":"effect","card":"0..1"}
            }},
            "trigger":{"fields":{
                "OR":{"body":"self","card":"0..*","control":{"kind":"logic","op":"OR"}},
                "AND":{"body":"self","card":"0..*","control":{"kind":"logic","op":"AND"}},
                "NOT":{"body":"self","card":"0..*","control":{"kind":"logic","op":"NOT"}}
            }},"effect":{"fields":{}}
        },
        "scopes":{"types":["country","province","unit"],"registers":{
            "root":{"role":"root"},"this":{"role":"current"},
            "prev":{"role":"previous","chain":true},"from":{"role":"from","chain":true}
        },"links":{"capital":{"from":["country"],"to":"province"},"owner":{"from":["province"],"to":"country"}}}
    });
    merge(&mut source, patch);
    explicit_fixture_cards(&mut source);
    let file = serde_json::from_value(source).expect("fixture source shape");
    let config = rules::ir::GameConfig {
        profile: rules::GameProfile::empty("fixture"),
    };
    let ir = std::sync::Arc::new(
        rules::lower::lower(&[("fixture.json".to_owned(), file)], config)
            .expect("checked fixture IR"),
    );
    AnalysisHost::with_ir(RuleSet::from_ir_catalog(&ir), ir.game.profile.clone(), ir)
}

/// Changes declared fields in a full first-party source fixture, retaining checked IR composition.
pub(crate) fn eu4_fixture_host(
    schema: &str,
    fields: serde_json::Value,
    profile: rules::GameProfile,
) -> AnalysisHost {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4");
    let mut bundle = rules::bundle::load_directory(&path).unwrap();
    let (_, file) = bundle
        .files
        .iter_mut()
        .find(|(_, file)| file.schemas.contains_key(schema))
        .expect("fixture schema");
    let mut source = serde_json::to_value(&*file).unwrap();
    source["schemas"][schema]["fields"]
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    explicit_fixture_cards(&mut source);
    *file = serde_json::from_value(source).unwrap();
    bundle.game.profile = profile;
    let ir = std::sync::Arc::new(rules::lower::lower(&bundle.files, bundle.game).unwrap());
    AnalysisHost::with_ir(RuleSet::from_ir_catalog(&ir), ir.game.profile.clone(), ir)
}

/// Test builder materializes old unbounded fixture cardinalities explicitly.
/// Production source validation still requires every authored field to declare `card`.
fn explicit_fixture_cards(source: &mut serde_json::Value) {
    fn field(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    field(item);
                }
            }
            serde_json::Value::Object(fields) => {
                fields.entry("card").or_insert(serde_json::json!("0..*"));
            }
            _ => {}
        }
    }
    if let Some(schemas) = source
        .get_mut("schemas")
        .and_then(serde_json::Value::as_object_mut)
    {
        for schema in schemas.values_mut() {
            if let Some(fields) = schema
                .get_mut("fields")
                .and_then(serde_json::Value::as_object_mut)
            {
                for item in fields.values_mut() {
                    field(item);
                }
            }
            if let Some(patterns) = schema
                .get_mut("patterns")
                .and_then(serde_json::Value::as_array_mut)
            {
                for item in patterns {
                    field(item);
                }
            }
        }
    }
}

pub(crate) fn scope_fixture_snapshot(text: &str) -> (AnalysisHost, DocumentId) {
    let mut host = fixture_host(
        serde_json::json!({"schemas":{"trigger":{"fields":{"scope":{"value":"scope<country>","card":"0..1"}}}}}),
    );
    let id = DocumentId::new("file:///tmp/scope.txt");
    host.open_document(id.clone(), 1, text.to_owned(), None)
        .unwrap();
    (host, id)
}
