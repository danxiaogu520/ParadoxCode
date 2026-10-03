use super::{
    FileCategory, FileMatcher, FileResolutionPolicy, GameProfile, ParserKind, ProfileMatchMode,
    ProfileTextMatcher, RuleSet,
};
use text::LogicalPath;

#[test]
fn empty_catalog_has_a_deterministic_identity() {
    let rules = RuleSet::empty();
    assert_eq!(rules.rule_hash(), RuleSet::empty().rule_hash());
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
    let rules = RuleSet::from_catalog(
        "test".to_owned(),
        vec![
            category("script", None, None),
            category("common-root", Some("common"), None),
            category("common-technology", None, Some("common/technology.txt")),
        ],
        GameProfile::empty("test"),
    );
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
fn profile_scripted_localisation_directory_matching_uses_path_segments() {
    let mut profile = GameProfile::empty("test");
    profile.scripted_localisation_directories = vec![
        "scripted_localisation".to_owned(),
        "scripted_localization".to_owned(),
        "scripted_loc".to_owned(),
    ];

    assert!(profile.is_scripted_localisation_path("common/scripted_localisation/defs.txt"));
    assert!(profile.is_scripted_localisation_path("dlc/foo/common/scripted_loc/defs.txt"));
    assert!(!profile.is_scripted_localisation_path("common/ideas/scripted_loc.txt"));
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
fn logical_path_for_uri_keeps_the_most_specific_trailing_directory() {
    let category = |id: &str, path_prefix: Option<&str>| FileCategory {
        id: id.to_owned(),
        parser: ParserKind::Script,
        resolution: FileResolutionPolicy::Merge,
        matcher: FileMatcher {
            path_prefix: path_prefix.map(str::to_owned),
            path_exact: None,
            extensions: vec!["txt".to_owned()],
            path_suffix: None,
            path_exclude_prefixes: Vec::new(),
            case_sensitive: false,
        },
    };
    let rules = RuleSet::from_catalog(
        "test".to_owned(),
        vec![
            category("script", None),
            category("common-root", Some("common")),
            category("scripted-effects", Some("common/scripted_effects")),
            category("events", Some("events")),
        ],
        GameProfile::empty("test"),
    );

    let derived = |uri: &str| {
        rules
            .logical_path_for_uri(uri)
            .map(|path| path.as_str().to_owned())
    };

    // Arbitrary URI prefixes are dropped; the deepest recognized directory wins.
    assert_eq!(
        derived("file:///tmp/common/scripted_effects/00_a.txt").as_deref(),
        Some("common/scripted_effects/00_a.txt")
    );
    // 目录感知候选胜出: URI 垃圾前缀被剥掉。
    assert_eq!(
        derived("file:///c%/Users/mod/events/hello.txt").as_deref(),
        Some("events/hello.txt")
    );
    // Scheme-less identifiers behave the same.
    assert_eq!(
        derived("common/scripted_effects/00_a.txt").as_deref(),
        Some("common/scripted_effects/00_a.txt")
    );
    // Unrecognizable buffers yield nothing so callers keep the bare file name.
    assert_eq!(derived("untitled:Untitled-1"), None);
    assert_eq!(derived("file:///tmp/no-extension"), None);
}
