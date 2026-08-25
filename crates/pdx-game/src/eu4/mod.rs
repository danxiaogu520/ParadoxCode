//! EU4 profile data layered on the game-independent rules runtime.
//!
//! Everything here is EU4-specific: installation discovery facts, the embedded first-party rule
//! bootstrap, and the structured mission model. Semantic/profile data itself lives under
//! `rules/eu4` and is carried by the compiled `RuleSet`.

pub mod mission;

use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{GameInstallDescriptor, PlatformExecutablePaths};
use pdx_rules::rulec::{SourceBundle, SourceFile, load_source_bundle};
use pdx_rules::{
    FileCategory, FileMatcher, FileResolutionPolicy, GameProfile, ParserKind, RuleSet, RulesModel,
    SymbolDescriptor, SymbolResolutionPolicy,
};

const FIRST_PARTY_FILES: &[SourceFile<'static>] = &[
    SourceFile {
        path: "catalog/file-categories.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/file-categories.json"),
    },
    SourceFile {
        path: "catalog/records/aliases.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/aliases.json"),
    },
    SourceFile {
        path: "catalog/records/effects.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/effects.json"),
    },
    SourceFile {
        path: "catalog/records/enums.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/enums.json"),
    },
    SourceFile {
        path: "catalog/records/localisation.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/localisation.json"),
    },
    SourceFile {
        path: "catalog/records/modifiers.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/modifiers.json"),
    },
    SourceFile {
        path: "catalog/records/rule_nodes.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/rule_nodes.json"),
    },
    SourceFile {
        path: "catalog/records/scopes.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/scopes.json"),
    },
    SourceFile {
        path: "catalog/records/subtypes.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/subtypes.json"),
    },
    SourceFile {
        path: "catalog/records/triggers.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/triggers.json"),
    },
    SourceFile {
        path: "catalog/records/types.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/records/types.json"),
    },
    SourceFile {
        path: "catalog/symbol-descriptors.json",
        bytes: include_bytes!("../../../../rules/eu4/catalog/symbol-descriptors.json"),
    },
    SourceFile {
        path: "semantic/contexts/effect.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/contexts/effect.json"),
    },
    SourceFile {
        path: "semantic/contexts/modifier.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/contexts/modifier.json"),
    },
    SourceFile {
        path: "semantic/contexts/on-action.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/contexts/on-action.json"),
    },
    SourceFile {
        path: "semantic/contexts/special.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/contexts/special.json"),
    },
    SourceFile {
        path: "semantic/contexts/trigger.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/contexts/trigger.json"),
    },
    SourceFile {
        path: "semantic/definitions/common.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/common.json"),
    },
    SourceFile {
        path: "semantic/definitions/decisions.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/decisions.json"),
    },
    SourceFile {
        path: "semantic/definitions/events.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/events.json"),
    },
    SourceFile {
        path: "semantic/definitions/history.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/history.json"),
    },
    SourceFile {
        path: "semantic/definitions/interface.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/interface.json"),
    },
    SourceFile {
        path: "semantic/definitions/map.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/map.json"),
    },
    SourceFile {
        path: "semantic/definitions/missions.json",
        bytes: include_bytes!("../../../../rules/eu4/semantic/definitions/missions.json"),
    },
    SourceFile {
        path: "types/descriptors/common.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/common.json"),
    },
    SourceFile {
        path: "types/descriptors/events.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/events.json"),
    },
    SourceFile {
        path: "types/descriptors/history.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/history.json"),
    },
    SourceFile {
        path: "types/descriptors/interface.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/interface.json"),
    },
    SourceFile {
        path: "types/descriptors/map.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/map.json"),
    },
    SourceFile {
        path: "types/descriptors/other.json",
        bytes: include_bytes!("../../../../rules/eu4/types/descriptors/other.json"),
    },
    SourceFile {
        path: "types/root-keys.json",
        bytes: include_bytes!("../../../../rules/eu4/types/root-keys.json"),
    },
    SourceFile {
        path: "types/root-scopes.json",
        bytes: include_bytes!("../../../../rules/eu4/types/root-scopes.json"),
    },
    SourceFile {
        path: "values/enums/diplomacy.json",
        bytes: include_bytes!("../../../../rules/eu4/values/enums/diplomacy.json"),
    },
    SourceFile {
        path: "values/enums/map.json",
        bytes: include_bytes!("../../../../rules/eu4/values/enums/map.json"),
    },
    SourceFile {
        path: "values/enums/military.json",
        bytes: include_bytes!("../../../../rules/eu4/values/enums/military.json"),
    },
    SourceFile {
        path: "values/enums/other.json",
        bytes: include_bytes!("../../../../rules/eu4/values/enums/other.json"),
    },
    SourceFile {
        path: "values/enums/religion.json",
        bytes: include_bytes!("../../../../rules/eu4/values/enums/religion.json"),
    },
    SourceFile {
        path: "localisation/bindings/common.json",
        bytes: include_bytes!("../../../../rules/eu4/localisation/bindings/common.json"),
    },
    SourceFile {
        path: "localisation/bindings/events.json",
        bytes: include_bytes!("../../../../rules/eu4/localisation/bindings/events.json"),
    },
    SourceFile {
        path: "localisation/bindings/other.json",
        bytes: include_bytes!("../../../../rules/eu4/localisation/bindings/other.json"),
    },
    SourceFile {
        path: "profile/dynamic.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/dynamic.json"),
    },
    SourceFile {
        path: "profile/filesystem.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/filesystem.json"),
    },
    SourceFile {
        path: "profile/install.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/install.json"),
    },
    SourceFile {
        path: "profile/lexicon.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/lexicon.json"),
    },
    SourceFile {
        path: "profile/scopes.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/scopes.json"),
    },
    SourceFile {
        path: "profile/semantics.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/semantics.json"),
    },
    SourceFile {
        path: "profile/symbols.json",
        bytes: include_bytes!("../../../../rules/eu4/profile/symbols.json"),
    },
];

const FIRST_PARTY_SOURCE: SourceBundle<'static> = SourceBundle {
    manifest: include_bytes!("../../../../rules/eu4/manifest.json"),
    files: FIRST_PARTY_FILES,
};

static RULE_CACHE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static FIRST_PARTY_PROFILE: OnceLock<GameProfile> = OnceLock::new();

/// Stable identity stored by EU4 rule artifacts and selected by the server.
pub const GAME_ID: &str = "eu4";

/// Data-only installation discovery facts for the supported EU4 profile.
pub const INSTALL_DESCRIPTOR: GameInstallDescriptor = GameInstallDescriptor {
    game_id: GAME_ID,
    display_name: "Europa Universalis IV",
    executable_paths: PlatformExecutablePaths {
        windows: &["eu4.exe"],
        linux: &["eu4"],
        macos: &["Europa Universalis IV.app/Contents/MacOS/eu4"],
    },
    validation_directories: &["common", "events", "missions", "decisions", "localisation"],
    installation_directory_names: &["Europa Universalis IV"],
};

/// Loads and validates the first-party EU4 rules from the embedded JSON source bundle.
///
/// This path is used by tests and callers that do not have a user cache location. The official
/// language-server entry point uses [`first_party_rules_cached`] so runtime queries still consume
/// a validated, read-only SQLite artifact.
pub fn first_party_rules() -> Result<RuleSet, pdx_rules::RulesError> {
    source_rules()
}

/// Compiles the embedded first-party JSON source into a user-local SQLite artifact when needed,
/// then loads that artifact as the immutable runtime rule set.
///
/// The cache is keyed by the artifact metadata and is never treated as an authority. A missing,
/// stale, corrupt, or mismatched cache is replaced only after a complete source validation and
/// SQLite round-trip succeeds. No external source path can replace the embedded JSON bundle.
pub fn first_party_rules_cached(cache_path: &Path) -> Result<RuleSet, pdx_rules::RulesError> {
    let rules = source_rules()?;
    if let Ok(cached) = RuleSet::load(cache_path)
        && cached == rules
    {
        return Ok(cached);
    }

    let parent = cache_path.parent().ok_or_else(|| {
        pdx_rules::RulesError::Source(format!(
            "rules cache path has no parent: {}",
            cache_path.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = temporary_rule_path(
        parent,
        cache_path.file_name().and_then(|name| name.to_str()),
    )?;
    let result = (|| {
        let loaded = compile_and_load(&rules, &temporary)?;
        if cache_path.exists() {
            fs::remove_file(cache_path)?;
        }
        fs::rename(&temporary, cache_path)?;
        Ok(loaded)
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Compiles the first-party source to a process-local temporary SQLite artifact and removes the
/// file after loading. This is used only when a platform cache directory cannot be resolved.
pub fn first_party_rules_ephemeral() -> Result<RuleSet, pdx_rules::RulesError> {
    let rules = source_rules()?;
    let temporary = temporary_rule_path(&std::env::temp_dir(), Some("pdx-ls-rules.pdxrules"))?;
    let result = compile_and_load(&rules, &temporary);
    let _ = fs::remove_file(&temporary);
    result
}

fn temporary_rule_path(
    directory: &Path,
    preferred_name: Option<&str>,
) -> Result<std::path::PathBuf, pdx_rules::RulesError> {
    let sequence = RULE_CACHE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = preferred_name.unwrap_or("rules.pdxrules");
    let temporary = directory.join(format!(".{name}.{}-{sequence}.tmp", std::process::id()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    Ok(temporary)
}

fn compile_and_load(rules: &RuleSet, path: &Path) -> Result<RuleSet, pdx_rules::RulesError> {
    rules.write_sqlite(path)?;
    let loaded = RuleSet::load(path)?;
    if loaded != *rules {
        return Err(pdx_rules::RulesError::Source(
            "generated rules artifact did not round-trip to the embedded source".to_owned(),
        ));
    }
    Ok(loaded)
}

fn source_rules() -> Result<RuleSet, pdx_rules::RulesError> {
    let (_, model) = load_source_bundle(FIRST_PARTY_SOURCE)
        .map_err(|error| pdx_rules::RulesError::Source(error.to_string()))?;
    let rules = RuleSet::from_model(model);
    rules.ensure_game(GAME_ID)?;
    Ok(rules)
}

/// The built-in EU4 profile.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Eu4Profile;

impl Eu4Profile {
    /// Returns this profile's stable artifact identity.
    #[must_use]
    pub const fn game_id(self) -> &'static str {
        GAME_ID
    }

    /// Builds the minimal rules used by isolated tests and bootstrap operation.
    #[must_use]
    pub fn bootstrap_rules(self) -> RuleSet {
        RuleSet::from_model(bootstrap_model())
    }

    /// Returns the data-only EU4 semantic interpretation used by generic runtime crates.
    #[must_use]
    pub fn data(self) -> GameProfile {
        profile()
    }
}

/// Returns EU4's built-in semantic profile data.
#[must_use]
pub fn profile() -> GameProfile {
    FIRST_PARTY_PROFILE
        .get_or_init(|| {
            first_party_rules()
                .expect("embedded EU4 profile source")
                .profile()
                .clone()
        })
        .clone()
}

/// Returns a useful minimal catalog for isolated bootstrap callers.
///
/// Official runtime composition uses [`first_party_rules`], whose profile is part of the rule
/// hash. This intentionally small fallback keeps crate-graph and synthetic tests independent of
/// the full catalog.
#[must_use]
pub fn bootstrap_model() -> RulesModel {
    RulesModel {
        game_id: GAME_ID.to_owned(),
        file_categories: vec![
            FileCategory {
                id: "localisation".to_owned(),
                parser: ParserKind::Localisation,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: Some("localisation".to_owned()),
                    path_exact: None,
                    extensions: vec!["yml".to_owned(), "yaml".to_owned()],
                    path_suffix: None,
                    path_exclude_prefixes: Vec::new(),
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "script".to_owned(),
                parser: ParserKind::Script,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    path_exact: None,
                    extensions: vec![
                        "txt".to_owned(),
                        "gui".to_owned(),
                        "gfx".to_owned(),
                        "asset".to_owned(),
                        "sfx".to_owned(),
                    ],
                    path_suffix: None,
                    path_exclude_prefixes: Vec::new(),
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "asset".to_owned(),
                parser: ParserKind::Asset,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    path_exact: None,
                    extensions: vec![
                        "png".to_owned(),
                        "dds".to_owned(),
                        "tga".to_owned(),
                        "jpg".to_owned(),
                        "jpeg".to_owned(),
                        "ogg".to_owned(),
                        "wav".to_owned(),
                        "fnt".to_owned(),
                    ],
                    path_suffix: None,
                    path_exclude_prefixes: Vec::new(),
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "syntax-only".to_owned(),
                parser: ParserKind::SyntaxOnly,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    path_exact: None,
                    extensions: vec!["json".to_owned(), "lua".to_owned()],
                    path_suffix: None,
                    path_exclude_prefixes: Vec::new(),
                    case_sensitive: false,
                },
            },
        ],
        symbol_descriptors: vec![
            SymbolDescriptor {
                kind_id: "event".to_owned(),
                resolution: SymbolResolutionPolicy::ReplaceBySymbol,
                case_sensitive: false,
            },
            SymbolDescriptor {
                kind_id: "scripted_effect".to_owned(),
                resolution: SymbolResolutionPolicy::ReplaceBySymbol,
                case_sensitive: false,
            },
            SymbolDescriptor {
                kind_id: "scripted_trigger".to_owned(),
                resolution: SymbolResolutionPolicy::ReplaceBySymbol,
                case_sensitive: false,
            },
            SymbolDescriptor {
                kind_id: "localisation".to_owned(),
                resolution: SymbolResolutionPolicy::ReplaceBySymbol,
                case_sensitive: false,
            },
        ],
        records: Vec::new(),
        semantic: pdx_rules::SemanticModel::default(),
        profile: GameProfile::default(),
    }
}

/// Returns the minimal EU4 bootstrap rule set.
#[must_use]
pub fn bootstrap_rules() -> RuleSet {
    Eu4Profile.bootstrap_rules()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        Eu4Profile, GAME_ID, bootstrap_rules, first_party_rules, first_party_rules_cached,
        first_party_rules_ephemeral, profile,
    };
    use pdx_rules::{RuleSet, SourceEncoding};
    use pdx_text::LogicalPath;

    #[test]
    fn profile_identity_and_bootstrap_catalog_are_stable() {
        assert_eq!(Eu4Profile.game_id(), GAME_ID);
        let rules = bootstrap_rules();
        assert!(
            rules
                .model()
                .file_categories
                .iter()
                .any(|category| category.id == "script")
        );
        assert!(
            rules
                .model()
                .symbol_descriptors
                .iter()
                .any(|symbol| symbol.kind_id == "event")
        );
        let profile = profile();
        assert_eq!(profile.game_id, GAME_ID);
        assert_eq!(profile.source_encoding, SourceEncoding::Windows1252);
        assert_eq!(profile.scan_roots().len(), 123);
        assert!(profile.scan_roots().iter().any(|root| root == "common"));
        assert_eq!(profile.scan_root_max_depth("common"), Some(0));
        assert_eq!(
            profile.scan_root_files("common").map(|files| files.len()),
            Some(5)
        );
        assert!(
            profile
                .scan_roots()
                .iter()
                .any(|root| root == "common/ai_army")
        );
        assert_eq!(profile.scan_root_max_depth("common/ai_army"), Some(0));
        assert_eq!(profile.scan_root_max_depth("events"), Some(0));
        assert_eq!(
            profile.scan_root_max_depth("interface/government_mechanics"),
            Some(0)
        );
        assert_eq!(profile.scan_root_max_depth("map"), Some(1));
        assert_eq!(
            profile.scan_root_files("map").map(|files| files.len()),
            Some(16)
        );
        for root in profile.scan_roots() {
            if root == "localisation" || root == "map" {
                continue;
            }
            assert_eq!(
                profile.scan_root_max_depth(root),
                Some(0),
                "script root must not recurse: {root}"
            );
        }
        assert_eq!(profile.scan_root_max_depth("localisation"), None);
        assert!(
            profile
                .scan_roots()
                .iter()
                .any(|root| root == "common/estate_crown_land")
        );
        assert!(
            !profile
                .scan_roots()
                .iter()
                .any(|root| root == "common/native_advancement")
        );
        assert!(profile.allows_scan_file("common/technology.txt"));
        assert!(!profile.allows_scan_file("common/example.txt"));
        assert!(!profile.allows_scan_file("common/unlisted/example.txt"));
        assert!(profile.allows_scan_file("common/ai_army/example.txt"));
        assert!(!profile.allows_scan_file("common/ai_army/nested/example.txt"));
        assert!(profile.allows_scan_file("map/area.txt"));
        assert!(profile.allows_scan_file("map/lakes/00_lakes.txt"));
        assert!(profile.allows_scan_file("map/random/RandomLandNames.txt"));
        assert!(!profile.allows_scan_file("map/unknown.txt"));
        assert!(!profile.allows_scan_file("map/random/tiles/tile0.txt"));
        assert!(!profile.allows_scan_file("map/lakes/unknown.txt"));
        assert_eq!(profile.scan_extensions(), ["txt", "gfx", "yml", "yaml"]);
        assert!(
            profile
                .scan_roots()
                .iter()
                .any(|root| root == "localisation")
        );
        assert_eq!(
            profile
                .definition("common/events/example.txt", "country_event")
                .map(|rule| rule.kind.as_str()),
            Some("event")
        );
        assert_eq!(profile.reference_kind("title"), Some("localisation"));
        assert_eq!(
            profile
                .definition("common/cultures/example.txt", "germanic")
                .map(|rule| rule.kind.as_str()),
            Some("culture")
        );
        assert_eq!(
            profile.value_definition_kind("which", Some("set_variable")),
            Some("variable")
        );
        assert!(profile.token_definitions.iter().any(|rule| {
            rule.inner_kind == "scripted_effect_param"
                && rule.path.matches("common/scripted_effects/example.txt")
        }));
        assert_eq!(profile.root_scope("country_event"), Some("country"));
        assert!(profile.is_scope("capital_scope"));
        assert!(profile.scopes_compatible("trade_node", "province"));
        assert_eq!(
            profile.member_kind_alias("COUNTRY_TAGS"),
            Some("country_tag")
        );
        assert!(
            profile
                .fallback_keys
                .iter()
                .any(|key| key == "add_treasury")
        );
        assert!(profile.enum_extra_member("scripted_effect_params", "scaled_skill"));
        assert!(profile.is_control_flow_key("limit"));
        assert!(!profile.is_control_flow_key("add_core"));
    }

    #[test]
    fn embedded_first_party_source_matches_the_eu4_profile() {
        let rules = first_party_rules().expect("embedded EU4 source");
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (_, source_model) = pdx_rules::rulec::load_source(&repository_root.join("rules/eu4"))
            .expect("filesystem EU4 source");
        let source_rules = RuleSet::from_model(source_model);
        assert_eq!(rules.game_id(), GAME_ID);
        assert_eq!(rules, source_rules);
        assert_eq!(rules.profile(), &profile());
        assert!(!rules.model().semantic.rules.is_empty());
        assert_eq!(
            rules.model().semantic.localisation_bindings.len(),
            187,
            "embedded source must carry the complete first-party type localisation map"
        );
    }

    #[test]
    fn first_party_file_categories_are_closed_over_common_and_generated_map_paths() {
        let rules = first_party_rules().expect("embedded EU4 source");
        let classify = |path: &str| {
            rules
                .classify(&LogicalPath::parse(path).expect("logical path"))
                .map(|category| category.id.as_str())
        };

        assert_eq!(
            classify("common/achievements.txt"),
            Some("eu4-path-common-achievements")
        );
        assert_eq!(
            classify("common/technology.txt"),
            Some("eu4-path-common-technology")
        );
        assert_eq!(
            classify("common/alerts.txt"),
            Some("eu4-path-common-alerts")
        );
        assert_eq!(
            classify("common/graphicalculturetype.txt"),
            Some("eu4-path-common-graphicalculturetype")
        );
        assert_eq!(
            classify("common/historial_lucky.txt"),
            Some("eu4-path-common-historial_lucky")
        );
        assert_eq!(
            classify("common/ai_attitudes/00_ai_attitudes.txt"),
            Some("eu4-path-common-ai_attitudes")
        );
        assert_eq!(
            classify("common/defines/difficulty_easy.lua"),
            Some("eu4-path-common-defines-lua")
        );
        assert_eq!(
            classify("common/defines/00_mod_defines.txt"),
            Some("eu4-path-common-defines")
        );
        assert_eq!(classify("common/unknown.txt"), None);
        assert_eq!(classify("common/native_advancements/00_native.txt"), None);
        assert_eq!(classify("common/defines.lua"), None);
        assert_eq!(classify("map/unknown.txt"), Some("eu4-path-map"));
        assert_eq!(classify("map/random/tiles/tile0.txt"), None);
        assert_eq!(classify("dlc_metadata/dlc_info/00_dlc_info.txt"), None);
        assert_eq!(classify("gfx/entities/african_units.asset"), None);
    }

    #[test]
    fn first_party_rules_cache_compiles_and_rebuilds_sqlite() {
        let directory = tempfile::tempdir().expect("temporary cache directory");
        let cache = directory.path().join("rules/eu4/rules.pdxrules");
        let first = first_party_rules_cached(&cache).expect("compile first-party source");
        assert!(cache.is_file());
        assert_eq!(pdx_rules::RuleSet::load(&cache).expect("load cache"), first);

        let stale = directory.path().join("rules/eu4/stale.pdxrules");
        bootstrap_rules()
            .write_sqlite(&stale)
            .expect("write stale cache fixture");
        let rebuilt_stale = first_party_rules_cached(&stale).expect("rebuild stale cache");
        assert_eq!(rebuilt_stale, first);
        assert_eq!(
            pdx_rules::RuleSet::load(&stale).expect("load rebuilt stale cache"),
            first
        );

        fs::write(&cache, b"corrupt rules").expect("corrupt cache fixture");
        let rebuilt = first_party_rules_cached(&cache).expect("rebuild corrupt cache");
        assert_eq!(rebuilt, first);
        assert_eq!(
            pdx_rules::RuleSet::load(&cache).expect("load rebuilt cache"),
            first
        );

        assert_eq!(
            first_party_rules_ephemeral().expect("compile ephemeral rules"),
            first
        );
    }
}
