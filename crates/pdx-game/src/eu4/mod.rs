//! EU4 profile data layered on the game-independent rules runtime.
//!
//! Everything here is EU4-specific: installation discovery facts, script folder
//! whitelist, first-party rules bootstrap, and the structured mission model.

pub mod mission;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{GameInstallDescriptor, PlatformExecutablePaths};
use pdx_rules::rulec::{SourceBundle, load_source_bundle};
use pdx_rules::{
    FileCategory, FileMatcher, FileResolutionPolicy, GameProfile, ParserKind,
    ProfileConditionalDefinitionRule, ProfileContainerValueDefinitionRule, ProfileDefinitionRule,
    ProfileMatchMode, ProfileMemberNameSuffixRule, ProfileReferenceRule, ProfileRootScopeRule,
    ProfileScopeCompatibility, ProfileTextMatcher, ProfileTokenDefinitionRule,
    ProfileValueDefinitionRule, RuleSet, RulesModel, SourceEncoding, SymbolDescriptor,
    SymbolResolutionPolicy,
};

const FIRST_PARTY_SOURCE: SourceBundle<'static> = SourceBundle {
    manifest: include_bytes!("../../../../rules/eu4/manifest.json"),
    catalog: include_bytes!("../../../../rules/eu4/catalog.json"),
    semantic_rules: include_bytes!("../../../../rules/eu4/semantic-rules.json"),
    enum_values: include_bytes!("../../../../rules/eu4/enum-values.json"),
    type_root_keys: include_bytes!("../../../../rules/eu4/type-root-keys.json"),
    type_root_scopes: include_bytes!("../../../../rules/eu4/type-root-scopes.json"),
    type_descriptors: include_bytes!("../../../../rules/eu4/type-descriptors.json"),
    localisation_bindings: include_bytes!("../../../../rules/eu4/localisation-bindings.json"),
};

static RULE_CACHE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

/// EU4 source directory whitelist.
///
/// Every script root is bounded to direct files only; supported nested script directories are
/// listed as separate roots. `map` is the one fixed-name exception: its known vanilla paths are
/// enumerated below so generated map assets cannot enter the script index. `localisation` remains
/// recursive because it is owned by the separate Localisation language.
pub const SCRIPT_FOLDERS: &[&str] = &[
    "common",
    "common/advisortypes",
    "common/ages",
    "common/ai_army",
    "common/ai_attitudes",
    "common/ai_personalities",
    "common/ancestor_personalities",
    "common/bookmarks",
    "common/buildings",
    "common/cb_types",
    "common/centers_of_trade",
    "common/church_aspects",
    "common/client_states",
    "common/colonial_regions",
    "common/countries",
    "common/country_colors",
    "common/country_tags",
    "common/cultures",
    "common/custom_country_colors",
    "common/custom_gui",
    "common/custom_ideas",
    "common/decrees",
    "common/defender_of_faith",
    "common/defines",
    "common/diplomatic_actions",
    "common/disasters",
    "common/dynasty_colors",
    "common/estate_agendas",
    "common/estate_crown_land",
    "common/estate_privileges",
    "common/estates",
    "common/estates_preload",
    "common/event_modifiers",
    "common/factions",
    "common/federation_advancements",
    "common/fervor",
    "common/fetishist_cults",
    "common/flagship_modifications",
    "common/golden_bulls",
    "common/government_mechanics",
    "common/governments",
    "common/government_names",
    "common/government_ranks",
    "common/government_reforms",
    "common/great_projects",
    "common/hegemons",
    "common/holy_orders",
    "common/ideas",
    "common/imperial_incidents",
    "common/imperial_reforms",
    "common/incidents",
    "common/institutions",
    "common/insults",
    "common/isolationism",
    "common/leader_personalities",
    "common/mercenary_companies",
    "common/natives",
    "common/naval_doctrines",
    "common/new_diplomatic_actions",
    "common/on_actions",
    "common/opinion_modifiers",
    "common/parliament_bribes",
    "common/parliament_issues",
    "common/peace_treaties",
    "common/personal_deities",
    "common/policies",
    "common/powerprojection",
    "common/prices",
    "common/professionalism",
    "common/province_names",
    "common/province_triggered_modifiers",
    "common/rebel_types",
    "common/region_colors",
    "common/religions",
    "common/religious_conversions",
    "common/religious_reforms",
    "common/revolt_triggers",
    "common/revolution",
    "common/ruler_personalities",
    "common/scripted_effects",
    "common/scripted_functions",
    "common/scripted_triggers",
    "common/state_edicts",
    "common/static_modifiers",
    "common/subject_type_upgrades",
    "common/subject_types",
    "common/technologies",
    "common/timed_modifiers",
    "common/tradecompany_investments",
    "common/tradegoods",
    "common/tradenodes",
    "common/trade_companies",
    "common/trading_policies",
    "common/triggered_modifiers",
    "common/units",
    "common/units_display",
    "common/wargoal_types",
    "customizable_localization",
    "decisions",
    "events",
    "hints",
    "history/advisors",
    "history/countries",
    "history/diplomacy",
    "history/provinces",
    "history/wars",
    "map",
    "music",
    "missions",
    "sound",
    "sound/amb",
    "sound/battle",
    "sound/battle/naval",
    "tutorial",
    "gfx",
    "gfx/combat_result",
    "gfx/sprite_packs",
    "gfx/sprite_packs_order",
    "interface",
    "interface/assets",
    "interface/government_mechanics",
    "interface/state_view",
    "localisation",
];

/// EU4 script files directly under the common directory.
pub const COMMON_ROOT_FILES: &[&str] = &[
    "achievements.txt",
    "alerts.txt",
    "graphicalculturetype.txt",
    "historial_lucky.txt",
    "technology.txt",
];

/// EU4 map script paths supported by the vanilla game and the reference mod.
///
/// Map data contains many generated assets (for example `map/random/tiles/*.txt`) that are not
/// script sources. Keep the language/index deliberately name-based so those assets are never
/// pulled in by a recursive glob.
pub const MAP_ROOT_FILES: &[&str] = &[
    "ambient_object.txt",
    "area.txt",
    "climate.txt",
    "continent.txt",
    "lakes/00_lakes.txt",
    "positions.txt",
    "provincegroup.txt",
    "random/RNWScenarios.txt",
    "random/RandomLakeNames.txt",
    "random/RandomLandNames.txt",
    "random/RandomSeaNames.txt",
    "region.txt",
    "seasons.txt",
    "superregion.txt",
    "terrain.txt",
    "trade_winds.txt",
];

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
    let matcher = |mode, pattern| ProfileTextMatcher::insensitive(mode, pattern);
    let definition =
        |path_mode, path, key_mode, key, kind: &str, name_field: Option<&str>, requires_value| {
            ProfileDefinitionRule {
                path: matcher(path_mode, path),
                key: matcher(key_mode, key),
                kind: kind.to_owned(),
                name_field: name_field.map(str::to_owned),
                requires_value,
            }
        };
    let reference = |mode, key, kind: &str| ProfileReferenceRule {
        key: matcher(mode, key),
        kind: kind.to_owned(),
        excluded_keys: Vec::new(),
        excluded_paths: Vec::new(),
    };
    // `name`/`desc`/`title`/`tooltip` are localisation keys in event-style content, but
    // literal text in history files (dynast names, rebel names) and resource identifiers
    // in interface files (mesh/sprite names).
    let text_paths = ["history/countries/", "history/provinces/", "interface/"]
        .into_iter()
        .map(|pattern| matcher(ProfileMatchMode::Contains, pattern))
        .collect::<Vec<_>>();
    let localisation_reference = |mode, key| ProfileReferenceRule {
        key: matcher(mode, key),
        kind: "localisation".to_owned(),
        excluded_keys: Vec::new(),
        excluded_paths: text_paths.clone(),
    };
    let mut definitions = vec![
        definition(
            ProfileMatchMode::Contains,
            "scripted_effect",
            ProfileMatchMode::Any,
            "",
            "scripted_effect",
            None,
            false,
        ),
        definition(
            ProfileMatchMode::Contains,
            "scripted_trigger",
            ProfileMatchMode::Any,
            "",
            "scripted_trigger",
            None,
            false,
        ),
        definition(
            ProfileMatchMode::Contains,
            "events/",
            ProfileMatchMode::Any,
            "",
            "event",
            Some("id"),
            false,
        ),
        definition(
            ProfileMatchMode::Any,
            "",
            ProfileMatchMode::Suffix,
            "_event",
            "event",
            Some("id"),
            false,
        ),
        definition(
            ProfileMatchMode::Any,
            "",
            ProfileMatchMode::Exact,
            "country_event",
            "event",
            Some("id"),
            true,
        ),
        definition(
            ProfileMatchMode::Any,
            "",
            ProfileMatchMode::Exact,
            "province_event",
            "event",
            Some("id"),
            true,
        ),
        definition(
            ProfileMatchMode::Prefix,
            "common/country_tags/",
            ProfileMatchMode::Any,
            "",
            "country_tag",
            None,
            true,
        ),
    ];
    for (directory, kind) in [
        ("common/cultures", "culture"),
        ("common/religions", "religion"),
        ("common/tradenodes", "trade_node"),
        ("common/colonial_regions", "colonial_region"),
        ("common/estates", "estate"),
        ("common/ideas", "idea_group"),
        ("common/governments", "government"),
        ("common/government_reforms", "government_reform"),
        ("common/subject_types", "subject_type"),
        ("common/technologies", "technology"),
        ("common/buildings", "building"),
        ("common/units", "unit_type"),
        ("common/mercenary_companies", "mercenary_company"),
        ("common/trade_companies", "trade_company"),
        ("common/advisortypes", "advisor_type"),
        ("common/leader_personalities", "leader_personality"),
        ("common/ruler_personalities", "ruler_personality"),
        ("common/event_modifiers", "event_modifier"),
        ("common/static_modifiers", "static_modifier"),
        ("common/timed_modifiers", "timed_modifier"),
        ("common/triggered_modifiers", "triggered_modifier"),
        ("common/subject_type_upgrades", "subject_type_upgrade"),
        ("common/peace_treaties", "peace_treaty"),
        ("common/casus_belli", "casus_belli"),
        ("common/cb_types", "casus_belli"),
        ("common/wargoal_types", "wargoal_type"),
        ("common/institutions", "institution"),
        ("common/great_projects", "great_project"),
        ("common/estate_privileges", "estate_privilege"),
        ("common/estate_agendas", "estate_agenda"),
        ("common/diplomatic_actions", "diplomatic_action"),
        ("common/new_diplomatic_actions", "diplomatic_action"),
        ("common/disasters", "disaster"),
        ("common/rebel_types", "rebel_type"),
        ("common/insults", "insult"),
        ("common/opinion_modifiers", "opinion_modifier"),
        ("common/tradegoods", "tradegood"),
    ] {
        definitions.push(definition(
            ProfileMatchMode::Directory,
            directory,
            ProfileMatchMode::Any,
            "",
            kind,
            None,
            false,
        ));
    }
    let value_definition =
        |key: &str, parent_key: Option<&str>, kind: &str| ProfileValueDefinitionRule {
            key: ProfileTextMatcher::insensitive(ProfileMatchMode::Exact, key.to_owned()),
            parent_key: parent_key.map(|key| {
                ProfileTextMatcher::insensitive(ProfileMatchMode::Exact, key.to_owned())
            }),
            kind: kind.to_owned(),
        };
    let mut value_definitions = vec![
        value_definition("set_country_flag", None, "country_flag"),
        value_definition("set_global_flag", None, "global_flag"),
        value_definition("set_province_flag", None, "province_flag"),
        value_definition("set_ruler_flag", None, "ruler_flag"),
        value_definition("set_heir_flag", None, "heir_flag"),
        value_definition("set_consort_flag", None, "consort_flag"),
        value_definition("save_event_target_as", None, "event_target"),
        value_definition("save_global_event_target_as", None, "global_event_target"),
        value_definition("exile_ruler_as", None, "exiled_ruler"),
        value_definition("exile_heir_as", None, "exiled_heir"),
        value_definition("exile_consort_as", None, "exiled_consort"),
        value_definition("exiled_as", Some("define_exiled_ruler"), "exiled_ruler"),
        value_definition("exiled_as", Some("define_exiled_heir"), "exiled_heir"),
        value_definition("exiled_as", Some("define_exiled_consort"), "exiled_consort"),
        value_definition("set_saved_name", None, "saved_name"),
    ];
    for parent in [
        "set_variable",
        "change_variable",
        "new_variable",
        "new_variables",
    ] {
        value_definitions.push(value_definition("which", Some(parent), "variable"));
    }
    let container_value_definition =
        |key: &str, name_field: &str, kind: &str| ProfileContainerValueDefinitionRule {
            key: ProfileTextMatcher::insensitive(ProfileMatchMode::Exact, key.to_owned()),
            name_field: name_field.to_owned(),
            kind: kind.to_owned(),
        };
    let container_value_definitions = vec![
        container_value_definition("exile_ruler_as", "name", "exiled_ruler"),
        container_value_definition("exile_heir_as", "name", "exiled_heir"),
        container_value_definition("exile_consort_as", "name", "exiled_consort"),
    ];
    let scan_roots = SCRIPT_FOLDERS
        .iter()
        .map(|folder| (*folder).to_owned())
        .collect::<Vec<_>>();
    let scan_root_max_depths = scan_roots
        .iter()
        .filter(|root| root.as_str() != "localisation")
        .map(|root| {
            let max_depth = if root == "map" { 1 } else { 0 };
            (root.clone(), max_depth)
        })
        .collect::<BTreeMap<_, _>>();
    let scan_root_files = BTreeMap::from([
        (
            "common".to_owned(),
            COMMON_ROOT_FILES
                .iter()
                .map(|file| (*file).to_owned())
                .collect(),
        ),
        (
            "map".to_owned(),
            MAP_ROOT_FILES
                .iter()
                .map(|file| (*file).to_owned())
                .collect(),
        ),
    ]);
    GameProfile {
        game_id: GAME_ID.to_owned(),
        source_encoding: SourceEncoding::Windows1252,
        scan_roots,
        scan_root_max_depths,
        scan_root_files,
        scan_extensions: ["txt", "gfx", "yml", "yaml"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        definitions,
        references: vec![
            reference(ProfileMatchMode::Exact, "event", "event"),
            reference(ProfileMatchMode::Exact, "events", "event"),
            reference(ProfileMatchMode::Exact, "event_id", "event"),
            reference(ProfileMatchMode::Exact, "trigger_event", "event"),
            reference(ProfileMatchMode::Suffix, "_event", "event"),
            reference(
                ProfileMatchMode::Contains,
                "scripted_effect",
                "scripted_effect",
            ),
            reference(ProfileMatchMode::Exact, "call_effect", "scripted_effect"),
            // Sound files use `default_effect`/`specific_effect` to name sound groups,
            // not scripted effects.
            ProfileReferenceRule {
                key: matcher(ProfileMatchMode::Suffix, "_effect"),
                kind: "scripted_effect".to_owned(),
                excluded_keys: ["default_effect", "specific_effect"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                excluded_paths: Vec::new(),
            },
            reference(
                ProfileMatchMode::Contains,
                "scripted_trigger",
                "scripted_trigger",
            ),
            reference(ProfileMatchMode::Exact, "call_trigger", "scripted_trigger"),
            reference(ProfileMatchMode::Suffix, "_trigger", "scripted_trigger"),
            reference(ProfileMatchMode::Exact, "localisation", "localisation"),
            reference(ProfileMatchMode::Exact, "localization", "localisation"),
            reference(ProfileMatchMode::Exact, "loc_key", "localisation"),
            localisation_reference(ProfileMatchMode::Exact, "name"),
            localisation_reference(ProfileMatchMode::Exact, "desc"),
            localisation_reference(ProfileMatchMode::Exact, "title"),
            localisation_reference(ProfileMatchMode::Exact, "tooltip"),
        ],
        value_definitions,
        container_value_definitions,
        container_definitions: Vec::new(),
        conditional_definitions: vec![ProfileConditionalDefinitionRule {
            path: matcher(ProfileMatchMode::Contains, "common/government_reforms/"),
            kind: "hardcoded_legacy_government".to_owned(),
            required_field: "legacy_government".to_owned(),
            required_value: "yes".to_owned(),
            absent_field: "legacy_equivalent".to_owned(),
        }],
        token_definitions: vec![
            ProfileTokenDefinitionRule {
                path: matcher(ProfileMatchMode::Prefix, "common/scripted_effects/"),
                delimiter: '$',
                inner_kind: "scripted_effect_param".to_owned(),
                wrapped_kind: "scripted_effect_param_dollar".to_owned(),
            },
            ProfileTokenDefinitionRule {
                path: matcher(ProfileMatchMode::Prefix, "common/scripted_triggers/"),
                delimiter: '$',
                inner_kind: "scripted_effect_param".to_owned(),
                wrapped_kind: "scripted_effect_param_dollar".to_owned(),
            },
        ],
        scope_names: [
            "any",
            "root",
            "from",
            "prev",
            "previous",
            "prev_prev",
            "this",
            "owner",
            "controller",
            "capital",
            "capital_scope",
            "location",
            "province",
            "country",
            "trade_node",
            "unit",
            "monarch",
            "heir",
            "consort",
            "mercenary_company",
            "rebel_faction",
            "religion",
            "culture",
            "advisor",
            "leader",
            "trade_company",
            "global",
            "none",
            "overlord",
            "emperor",
            "event_target",
            "global_event_target",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        scope_completions: [
            "ROOT",
            "THIS",
            "FROM",
            "PREV",
            "country",
            "province",
            "trade_node",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        root_scopes: vec![
            ProfileRootScopeRule {
                key: matcher(ProfileMatchMode::Exact, "country_event"),
                scope: "country".to_owned(),
            },
            ProfileRootScopeRule {
                key: matcher(ProfileMatchMode::Exact, "province_event"),
                scope: "province".to_owned(),
            },
        ],
        scope_compatibilities: vec![ProfileScopeCompatibility {
            actual: "trade_node".to_owned(),
            expected: "province".to_owned(),
        }],
        transparent_scope_wrappers: ["AND", "OR", "NOT"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        control_flow_keys: [
            "AND",
            "OR",
            "NOT",
            "if",
            "else",
            "limit",
            "trigger",
            "effect",
            "hidden_effect",
            "random_list",
            "modifier",
            "option",
            "immediate",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        semantic_context_inheritance: [
            ("type:advisor_type", vec!["modifier"]),
            ("type:ancestor_personalities", vec!["modifier"]),
            ("type:customideas", vec!["modifier"]),
            ("type:country_history", vec!["effect"]),
            ("type:cult", vec!["modifier"]),
            ("type:government_ranks", vec!["modifier"]),
            ("type:leader_personality", vec!["modifier"]),
            ("type:on_action", vec!["effect"]),
            ("type:personal_deity", vec!["modifier"]),
            ("type:policy", vec!["modifier"]),
            ("type:professionalism_modifier", vec!["modifier"]),
            ("type:province_history", vec!["effect"]),
            ("type:province_triggered_modifier", vec!["modifier"]),
            (
                "type:ruler_personality",
                vec!["modifier", "personality_modifier"],
            ),
            ("type:static_modifier", vec!["modifier"]),
            ("type:terrain", vec!["modifier"]),
            ("type:trading_policy", vec!["modifier"]),
            ("type:triggered_modifier", vec!["modifier"]),
            ("imperial_incident_option", vec!["trigger"]),
            ("imperial_incident_option_modifier", vec!["trigger"]),
            ("root:fervor", vec!["modifier"]),
            ("root:power_projection", vec!["modifier"]),
        ]
        .into_iter()
        .map(|(context, inherited)| {
            (
                context.to_owned(),
                inherited.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect(),
        quoted_script_definition_keys: [
            matcher(ProfileMatchMode::Exact, "effect"),
            matcher(ProfileMatchMode::Suffix, "_effect"),
        ]
        .into_iter()
        .collect(),
        dynamic_scope_prefixes: vec!["event_target".to_owned(), "global_event_target".to_owned()],
        dynamic_value_prefixes: ["variable", "modifier"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        open_world_value_kinds: [
            "country_flag",
            "global_flag",
            "province_flag",
            "ruler_flag",
            "heir_flag",
            "consort_flag",
            "saved_name",
            "named_unrest",
            "event_target",
            "global_event_target",
            "dynasty_name",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        member_kind_aliases: [
            ("country_tags", "country_tag"),
            ("country_tag", "country_tag"),
            ("trade_nodes", "trade_node"),
            ("tradenodes", "trade_node"),
            ("trade_node", "trade_node"),
            ("colonial_regions", "colonial_region"),
            ("colonial_region", "colonial_region"),
            ("government_reforms", "government_reform"),
            ("government_reform", "government_reform"),
            ("subject_types", "subject_type"),
            ("subject_type", "subject_type"),
            ("mercenary_companies", "mercenary_company"),
            ("mercenary_company", "mercenary_company"),
            ("trade_companies", "trade_company"),
            ("trade_company", "trade_company"),
            ("event_modifiers", "event_modifier"),
            ("ruler_personality", "ancestor_personalities"),
            ("event_modifier", "event_modifier"),
            ("static_modifiers", "static_modifier"),
            ("static_modifier", "static_modifier"),
            ("timed_modifiers", "timed_modifier"),
            ("timed_modifier", "timed_modifier"),
            ("triggered_modifiers", "triggered_modifier"),
            ("triggered_modifier", "triggered_modifier"),
            ("peace_treaties", "peace_treaty"),
            ("peace_treaty", "peace_treaty"),
            ("wargoal_types", "wargoal_type"),
            ("wargoal_type", "wargoal_type"),
            ("advisortypes", "advisor_type"),
            ("advisor_type", "advisor_type"),
            ("leader_personalities", "leader_personality"),
            ("leader_personality", "leader_personality"),
            ("ruler_personalities", "ruler_personality"),
            ("ruler_personality", "ruler_personality"),
            ("idea_groups", "idea_group"),
            ("idea_group", "idea_group"),
            ("buildings", "building"),
            ("building", "building"),
            ("technologies", "technology"),
            ("technology", "technology"),
            ("religions", "religion"),
            ("religion", "religion"),
            ("cultures", "culture"),
            ("culture", "culture"),
            ("scripted_effect_params", "scripted_effect_param"),
            (
                "scripted_effect_params_dollar",
                "scripted_effect_param_dollar",
            ),
            ("hardcoded_legacygovernments", "hardcoded_legacy_government"),
            (
                "hardcoded_legacy_only_governments",
                "hardcoded_legacy_government",
            ),
            ("modifiers", "static_modifier"),
            ("modifier", "static_modifier"),
            ("ruler_personality", "ancestor_personalities"),
        ]
        .into_iter()
        .map(|(alias, kind)| (alias.to_owned(), kind.to_owned()))
        .collect(),
        member_name_suffixes: vec![ProfileMemberNameSuffixRule {
            kinds: [
                "ruler_personality",
                "leader_personality",
                "ancestor_personalities",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            suffix: "_personality".to_owned(),
        }],
        fallback_keys: [
            "id",
            "name",
            "desc",
            "title",
            "picture",
            "type",
            "trigger",
            "immediate",
            "option",
            "options",
            "ai_chance",
            "effect",
            "hidden",
            "is_triggered_only",
            "country_event",
            "province_event",
            "event",
            "always",
            "limit",
            "else",
            "if",
            "custom_tooltip",
            "tooltip",
            "text",
            "scope",
            "from",
            "root",
            "prev",
            "owner",
            "controller",
            "capital",
            "location",
            "value",
            "factor",
            "modifier",
            "add",
            "remove",
            "set",
            "yes",
            "no",
            "true",
            "false",
            "random",
            "weight",
            "mean_time_to_happen",
            "days",
            "months",
            "years",
            "chance",
            "is_valid",
            "allow",
            "target",
            "file",
            "path",
            "color",
            "culture",
            "religion",
            "province",
            "country",
            "tag",
            "flag",
            "has_country_flag",
            "set_country_flag",
            "clr_country_flag",
            "has_global_flag",
            "set_global_flag",
            "clr_global_flag",
            "add_manpower",
            "add_prestige",
            "add_stability",
            "add_treasury",
            "change_variable",
            "check_variable",
            "set_variable",
            "save_event_target_as",
            "fire_event",
            "call_scripted_effect",
            "call_scripted_trigger",
            "scripted_effect",
            "scripted_trigger",
            "localisation",
            "localization",
            "loc_key",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        enum_extra_members: [
            (
                "scripted_effect_params".to_owned(),
                vec!["scaled_skill".to_owned()],
            ),
            (
                "country_tags".to_owned(),
                ["F", "T"]
                    .into_iter()
                    .flat_map(|prefix| (0..100).map(move |number| format!("{prefix}{number:02}")))
                    .collect(),
            ),
        ]
        .into_iter()
        .collect(),
    }
}

/// Returns a useful minimal catalog for an EU4 workspace.
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
                    extensions: vec!["yml".to_owned(), "yaml".to_owned()],
                    path_suffix: None,
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "script".to_owned(),
                parser: ParserKind::Script,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    extensions: vec![
                        "txt".to_owned(),
                        "gui".to_owned(),
                        "gfx".to_owned(),
                        "asset".to_owned(),
                        "sfx".to_owned(),
                    ],
                    path_suffix: None,
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "asset".to_owned(),
                parser: ParserKind::Asset,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
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
                    case_sensitive: false,
                },
            },
            FileCategory {
                id: "syntax-only".to_owned(),
                parser: ParserKind::SyntaxOnly,
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    extensions: vec!["json".to_owned(), "lua".to_owned()],
                    path_suffix: None,
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

    use super::{
        COMMON_ROOT_FILES, Eu4Profile, GAME_ID, MAP_ROOT_FILES, SCRIPT_FOLDERS, bootstrap_rules,
        first_party_rules, first_party_rules_cached, first_party_rules_ephemeral, profile,
    };
    use pdx_rules::SourceEncoding;

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
        assert_eq!(profile.scan_roots().len(), SCRIPT_FOLDERS.len());
        assert!(profile.scan_roots().iter().any(|root| root == "common"));
        assert_eq!(profile.scan_root_max_depth("common"), Some(0));
        assert!(profile.scan_root_files("common").is_some_and(|files| {
            files
                .iter()
                .map(String::as_str)
                .eq(COMMON_ROOT_FILES.iter().copied())
        }));
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
        assert!(profile.scan_root_files("map").is_some_and(|files| {
            files
                .iter()
                .map(String::as_str)
                .eq(MAP_ROOT_FILES.iter().copied())
        }));
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
        assert_eq!(rules.game_id(), GAME_ID);
        assert!(!rules.model().semantic.rules.is_empty());
        assert_eq!(
            rules.model().semantic.localisation_bindings.len(),
            190,
            "embedded source must carry the complete first-party type localisation map"
        );
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
