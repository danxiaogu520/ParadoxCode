use super::{
    CURRENT_SCHEMA_VERSION, FileMatcher, GameProfile, KeyMatcher, ProfileMatchMode,
    ProfileTextMatcher, RuleRecord, RuleSet, RuleShape, RulesModel, SemanticRule, ValueMatcher,
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
        extensions: vec!["yml".to_owned()],
        path_suffix: None,
        case_sensitive: false,
    };

    assert!(matcher.matches(&LogicalPath::new("localisation/main.yml")));
    assert!(matcher.matches(&LogicalPath::new("localisation/events/main.yml")));
    assert!(matcher.matches(&LogicalPath::new("LOCALISATION/events/MAIN.YML")));
    assert!(!matcher.matches(&LogicalPath::new("localisation_extra/main.yml")));
    assert!(!matcher.matches(&LogicalPath::new("common/main.yml")));
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
fn sqlite_round_trip_validates_logical_hash() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("paradoxcode-rules-{nonce}.pdxrules"));
    let rules = RuleSet::from_model(RulesModel {
        game_id: "test-game".to_owned(),
        ..RulesModel::default()
    });
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
