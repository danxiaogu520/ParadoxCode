use super::{
    CURRENT_SCHEMA_VERSION, FileCategory, FileMatcher, FileResolutionPolicy, GameProfile,
    KeyMatcher, ParserKind, ProfileMatchMode, ProfileTextMatcher, RuleRecord, RuleSet, RuleShape,
    RulesModel, ScriptedMacroDescriptor, ScriptedMacroUsage, SemanticRule, TypeDescriptor,
    ValueMatcher,
};
use pdx_text::LogicalPath;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn empty_rules_have_a_stable_schema_identity() {
    let rules = RuleSet::empty();
    assert_eq!(rules.schema_version(), CURRENT_SCHEMA_VERSION);
    assert_eq!(rules.rule_hash().as_bytes(), [0; 32]);
}

#[test]
fn profile_text_matchers_are_case_insensitive_and_bounded() {
    let suffix = ProfileTextMatcher::insensitive(ProfileMatchMode::Suffix, "_event");
    let contains = ProfileTextMatcher::insensitive(ProfileMatchMode::Contains, "events/");
    let directory = ProfileTextMatcher::insensitive(ProfileMatchMode::Directory, "common/cultures");

    assert!(suffix.matches("COUNTRY_EVENT"));
    assert!(!suffix.matches("event_target"));
    assert!(contains.matches("COMMON/EVENTS/example.txt"));
    assert!(!contains.matches("common/event_modifiers/example.txt"));
    assert!(directory.matches("COMMON/CULTURES/example.txt"));
    assert!(!directory.matches("common/cultures/nested/example.txt"));
    assert!(ProfileTextMatcher::any().matches("anything"));
}

#[test]
fn file_matcher_path_prefix_is_directory_bounded() {
    let matcher = FileMatcher {
        path_prefix: Some("localisation".to_owned()),
        path_exact: None,
        extensions: vec!["yml".to_owned()],
        path_suffix: None,
        path_exclude_prefixes: Vec::new(),
        case_sensitive: false,
    };

    assert!(matcher.matches(&LogicalPath::parse("localisation/main.yml").expect("path")));
    assert!(matcher.matches(&LogicalPath::parse("localisation/events/main.yml").expect("path")));
    assert!(matcher.matches(&LogicalPath::parse("LOCALISATION/events/MAIN.YML").expect("path")));
    assert!(!matcher.matches(&LogicalPath::parse("localisation_extra/main.yml").expect("path")));
    assert!(!matcher.matches(&LogicalPath::parse("common/main.yml").expect("path")));
}

#[test]
fn file_matcher_supports_exact_paths_and_excluded_directories() {
    let exact = FileMatcher {
        path_prefix: None,
        path_exact: Some("common/technology.txt".to_owned()),
        extensions: vec!["txt".to_owned()],
        path_suffix: None,
        path_exclude_prefixes: Vec::new(),
        case_sensitive: false,
    };
    assert!(exact.matches(&LogicalPath::parse("COMMON/TECHNOLOGY.TXT").expect("path")));
    assert!(!exact.matches(&LogicalPath::parse("common/technology_extra.txt").expect("path")));

    let script = FileMatcher {
        path_prefix: None,
        path_exact: None,
        extensions: vec!["txt".to_owned()],
        path_suffix: None,
        path_exclude_prefixes: vec!["common".to_owned()],
        case_sensitive: false,
    };
    assert!(!script.matches(&LogicalPath::parse("common/unknown.txt").expect("path")));
    assert!(script.matches(&LogicalPath::parse("common_extra/unknown.txt").expect("path")));
    assert!(script.matches(&LogicalPath::parse("events/unknown.txt").expect("path")));
}

#[test]
fn classify_prefers_an_exact_path_over_a_broad_prefix() {
    let category = |id: &str, path_prefix: Option<&str>, path_exact: Option<&str>| FileCategory {
        id: id.to_owned(),
        parser: ParserKind::Script,
        resolution: FileResolutionPolicy::Merge,
        matcher: FileMatcher {
            path_prefix: path_prefix.map(str::to_owned),
            path_exact: path_exact.map(str::to_owned),
            extensions: vec!["txt".to_owned()],
            path_suffix: None,
            path_exclude_prefixes: Vec::new(),
            case_sensitive: false,
        },
    };
    let rules = RulesModel {
        file_categories: vec![
            category("script", None, None),
            category("common-root", Some("common"), None),
            category("common-technology", None, Some("common/technology.txt")),
        ],
        ..RulesModel::default()
    };
    let path = LogicalPath::parse("common/technology.txt").expect("path");
    assert_eq!(
        rules.classify(&path).map(|item| item.id.as_str()),
        Some("common-technology")
    );
}

#[test]
fn profile_scan_roots_are_directory_bounded() {
    let mut profile = GameProfile::empty("test");
    profile.scan_roots = vec!["common".to_owned(), "events".to_owned()];

    assert!(profile.allows_scan_path("common/events/example.txt"));
    assert!(profile.allows_scan_path("events/example.txt"));
    assert!(!profile.allows_scan_path("common_extra/example.txt"));
    assert!(!profile.allows_scan_path("root_level.txt"));
}

#[test]
fn profile_scan_extensions_are_case_insensitive_and_directory_bounded() {
    let mut profile = GameProfile::empty("test");
    profile.scan_roots = vec!["events".to_owned()];
    profile.scan_extensions = vec!["txt".to_owned(), "gfx".to_owned(), "yml".to_owned()];

    assert!(profile.allows_scan_file("events/example.TXT"));
    assert!(profile.allows_scan_file("events/example.gfx"));
    assert!(profile.allows_scan_file("events/example.yml"));
    assert!(!profile.allows_scan_file("events/example.gui"));
    assert!(!profile.allows_scan_file("events/example.txt.bak"));
    assert!(!profile.allows_scan_file("events_extra/example.txt"));
    assert!(!profile.allows_scan_file("root_level.txt"));
}

#[test]
fn profile_scan_root_depth_can_limit_directories_without_affecting_other_roots() {
    let mut profile = GameProfile::empty("test");
    profile.scan_roots = vec![
        "common".to_owned(),
        "common/countries".to_owned(),
        "events".to_owned(),
    ];
    profile.scan_root_max_depths.insert("common".to_owned(), 0);
    profile
        .scan_root_max_depths
        .insert("common/countries".to_owned(), 0);
    profile
        .scan_root_files
        .insert("common".to_owned(), vec!["technology.txt".to_owned()]);

    assert!(profile.allows_scan_file("common/technology.txt"));
    assert!(!profile.allows_scan_file("common/other.txt"));
    assert!(!profile.allows_scan_file("common/other/file.txt"));
    assert!(profile.allows_scan_file("common/countries/file.txt"));
    assert!(!profile.allows_scan_file("common/countries/nested/file.txt"));
    assert!(profile.allows_scan_file("events/nested/file.txt"));

    profile.scan_roots.push("map".to_owned());
    profile.scan_root_max_depths.insert("map".to_owned(), 1);
    profile.scan_root_files.insert(
        "map".to_owned(),
        vec!["area.txt".to_owned(), "lakes/00_lakes.txt".to_owned()],
    );
    assert!(profile.allows_scan_file("map/area.txt"));
    assert!(profile.allows_scan_file("map/lakes/00_lakes.txt"));
    assert!(!profile.allows_scan_file("map/unknown.txt"));
    assert!(!profile.allows_scan_file("map/lakes/unknown.txt"));
    assert!(!profile.allows_scan_file("map/random/area.txt"));
}

#[test]
fn profile_shape_matching_ignores_extensions_but_keeps_common_whitelist() {
    let mut profile = GameProfile::empty("test");
    profile.scan_roots = vec!["common".to_owned(), "interface".to_owned()];
    profile.scan_root_max_depths.insert("common".to_owned(), 0);
    profile
        .scan_root_max_depths
        .insert("interface".to_owned(), 0);
    profile
        .scan_root_files
        .insert("common".to_owned(), vec!["technology.txt".to_owned()]);

    assert!(profile.allows_profile_path("common/technology.txt"));
    assert!(!profile.allows_profile_path("common/unknown.txt"));
    assert!(!profile.allows_profile_path("common/nested/file.txt"));
    assert!(profile.allows_profile_path("interface/window.gui"));
    assert!(profile.rejects_unlisted_root_file("common/unknown.txt"));
    assert!(!profile.rejects_unlisted_root_file("common/nested/file.txt"));
}

#[test]
fn canonical_hash_is_independent_of_record_insertion_order() {
    let mut first = RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    };
    let records = [
        RuleRecord {
            table: "types".to_owned(),
            logical_id: "a".to_owned(),
            source_order: 1,
            fields: BTreeMap::from([(String::from("name"), String::from("event"))]),
        },
        RuleRecord {
            table: "enums".to_owned(),
            logical_id: "b".to_owned(),
            source_order: 0,
            fields: BTreeMap::from([(String::from("name"), String::from("scope"))]),
        },
    ];
    first.records.extend(records.clone());
    let mut second = RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    };
    second.records.extend(records.into_iter().rev());
    assert_eq!(
        RuleSet::from_model(first).rule_hash(),
        RuleSet::from_model(second).rule_hash()
    );
}

#[test]
fn exact_semantic_rule_index_is_case_insensitive_and_excludes_dynamic_matchers() {
    let semantic_rule = |id: &str, key: KeyMatcher| SemanticRule {
        id: id.to_owned(),
        context: "effect".to_owned(),
        parent_path: Vec::new(),
        key,
        operator: None,
        value: ValueMatcher::AnyScalar,
        shape: RuleShape::Leaf,
        child_context: None,
        alternative_id: None,
        severity: None,
        required: false,
        deprecated: false,
        documentation: Vec::new(),
        allowed_scopes: Vec::new(),
        push_scope: None,
        replace_scope: Vec::new(),
        min_occurs: None,
        strict_min: false,
        max_occurs: None,
        source_file: String::new(),
        line: 0,
    };
    let mut model = RulesModel::default();
    model.semantic.rules = vec![
        semantic_rule("exact", KeyMatcher::Exact("Owner".to_owned())),
        semantic_rule("dynamic", KeyMatcher::Dynamic("scope".to_owned())),
    ];
    let rules = RuleSet::from_model(model);

    assert_eq!(
        rules
            .exact_semantic_rules("OWNER")
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        vec!["exact"]
    );
    assert!(rules.exact_semantic_rules("missing").next().is_none());
    assert_eq!(
        rules
            .semantic_rules_for_context("EFFECT")
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        vec!["dynamic", "exact"]
    );
    assert!(rules.semantic_rules_for_context("trigger").next().is_none());
}

#[test]
fn date_key_matcher_accepts_campaign_dates_only() {
    let matcher = KeyMatcher::Date;
    assert!(matcher.matches("1444.11.11", |_, _| false, |_, _| false));
    assert!(matcher.matches("1444.11", |_, _| false, |_, _| false));
    assert!(!matcher.matches("date_field", |_, _| false, |_, _| false));
    assert!(!matcher.matches("1444.13.40.extra", |_, _| false, |_, _| false));
}

#[test]
fn canonical_hash_includes_scripted_macro_metadata() {
    let descriptor = TypeDescriptor {
        name: "scripted_effect".to_owned(),
        scripted_macro: Some(ScriptedMacroDescriptor {
            body_context: "effect".to_owned(),
            macro_enabled: true,
            usage: ScriptedMacroUsage {
                replacement: true,
                condition: true,
                dynamic_key: true,
                opaque_text: true,
            },
        }),
        ..TypeDescriptor::default()
    };
    let mut first = RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    };
    first
        .semantic
        .type_descriptors
        .insert(descriptor.name.clone(), descriptor.clone());
    let mut second = first.clone();
    second
        .semantic
        .type_descriptors
        .get_mut("scripted_effect")
        .expect("scripted descriptor")
        .scripted_macro
        .as_mut()
        .expect("macro metadata")
        .usage
        .opaque_text = false;

    assert_ne!(
        RuleSet::from_model(first).rule_hash(),
        RuleSet::from_model(second).rule_hash()
    );
}

#[test]
fn canonical_hash_includes_game_profile_data() {
    let mut first = RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    };
    first.profile.scan_roots.push("common".to_owned());
    let mut second = first.clone();
    second.profile.scan_extensions.push("txt".to_owned());

    assert_ne!(
        RuleSet::from_model(first).rule_hash(),
        RuleSet::from_model(second).rule_hash()
    );
}

#[test]
fn type_descriptor_macro_metadata_is_strict_and_old_source_defaults_it() {
    let old_source = r#"{
        "name": "legacy",
        "path": null,
        "path_file": null,
        "path_extension": null,
        "path_strict": false,
        "type_per_file": false,
        "skip_root_paths": [],
        "name_field": null,
        "name_from_file": false,
        "starts_with": null,
        "type_key_filter": null
    }"#;
    let descriptor: TypeDescriptor = serde_json::from_str(old_source).expect("legacy descriptor");
    assert!(descriptor.scripted_macro.is_none());
    assert!(
        serde_json::to_value(&descriptor)
            .expect("serialize legacy descriptor")
            .get("scripted_macro")
            .is_none()
    );

    let unknown_field = r#"{
        "name": "scripted_effect",
        "path": null,
        "path_file": null,
        "path_extension": null,
        "path_strict": false,
        "type_per_file": false,
        "skip_root_paths": [],
        "name_field": null,
        "name_from_file": false,
        "starts_with": null,
        "type_key_filter": null,
        "scripted_macro": {
            "body_context": "effect",
            "macro_enabled": true,
            "usage": {
                "replacement": true,
                "condition": false,
                "dynamic_key": false,
                "opaque_text": false,
                "not_a_usage": true
            }
        }
    }"#;
    assert!(serde_json::from_str::<TypeDescriptor>(unknown_field).is_err());
}

#[test]
fn sqlite_round_trip_validates_logical_hash() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("paradoxcode-rules-{nonce}.pdxrules"));
    let mut model = RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    };
    model.profile.scan_roots.push("common".to_owned());
    model.semantic.type_descriptors.insert(
        "scripted_trigger".to_owned(),
        TypeDescriptor {
            name: "scripted_trigger".to_owned(),
            scripted_macro: Some(ScriptedMacroDescriptor {
                body_context: "trigger".to_owned(),
                macro_enabled: true,
                usage: ScriptedMacroUsage {
                    replacement: true,
                    condition: true,
                    dynamic_key: true,
                    opaque_text: true,
                },
            }),
            ..TypeDescriptor::default()
        },
    );
    let rules = RuleSet::from_model(model);
    rules.write_sqlite(&path).expect("write rules");
    let loaded = RuleSet::load(&path).expect("load rules");
    assert_eq!(loaded, rules);
    assert_eq!(loaded.game_id(), "test-game");
    assert!(loaded.ensure_game("test-game").is_ok());
    assert!(matches!(
        loaded.ensure_game("another-game"),
        Err(super::RulesError::GameMismatch { .. })
    ));
    std::fs::remove_file(path).expect("remove temporary rules");
}
