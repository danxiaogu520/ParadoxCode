//! EU4 profile data layered on the game-independent rules runtime.

use crate::{GameInstallDescriptor, PlatformExecutablePaths};
use pdx_rules::{
    FileCategory, FileMatcher, FileResolutionPolicy, GameProfile, ParserKind,
    ProfileConditionalDefinitionRule, ProfileDefinitionRule,
    ProfileMatchMode, ProfileReferenceRule, ProfileRootScopeRule, ProfileScopeCompatibility,
    ProfileTextMatcher, ProfileTokenDefinitionRule, ProfileValueDefinitionRule, RuleSet,
    RulesModel, SymbolDescriptor, SymbolResolutionPolicy,
};

const FIRST_PARTY_RULES: &[u8] = include_bytes!("../../../rules/eu4.pdxrules");

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

/// Loads the immutable first-party EU4 rules embedded in the official binary.
///
/// No path, environment variable, initialization option, or project setting can replace these
/// rules. A failure indicates a broken build artifact rather than user configuration.
pub fn first_party_rules() -> Result<RuleSet, pdx_rules::RulesError> {
    let rules = RuleSet::load_embedded(FIRST_PARTY_RULES)?;
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
        value_definition("set_saved_name", None, "saved_name"),
    ];
    for parent in ["set_variable", "change_variable", "new_variable", "new_variables"] {
        value_definitions.push(value_definition("which", Some(parent), "variable"));
    }
    GameProfile {
        game_id: GAME_ID.to_owned(),
        definitions,
        references: vec![
            reference(ProfileMatchMode::Exact, "event", "event"),
            reference(ProfileMatchMode::Exact, "events", "event"),
            reference(ProfileMatchMode::Exact, "event_id", "event"),
            reference(ProfileMatchMode::Exact, "trigger_event", "event"),
            reference(ProfileMatchMode::Suffix, "_event", "event"),
            reference(ProfileMatchMode::Contains, "scripted_effect", "scripted_effect"),
            reference(ProfileMatchMode::Exact, "call_effect", "scripted_effect"),
            reference(ProfileMatchMode::Suffix, "_effect", "scripted_effect"),
            reference(ProfileMatchMode::Contains, "scripted_trigger", "scripted_trigger"),
            reference(ProfileMatchMode::Exact, "call_trigger", "scripted_trigger"),
            reference(ProfileMatchMode::Suffix, "_trigger", "scripted_trigger"),
            reference(ProfileMatchMode::Exact, "localisation", "localisation"),
            reference(ProfileMatchMode::Exact, "localization", "localisation"),
            reference(ProfileMatchMode::Exact, "loc_key", "localisation"),
            reference(ProfileMatchMode::Exact, "name", "localisation"),
            reference(ProfileMatchMode::Exact, "desc", "localisation"),
            reference(ProfileMatchMode::Exact, "title", "localisation"),
            reference(ProfileMatchMode::Exact, "tooltip", "localisation"),
        ],
        value_definitions,
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
            "event_target",
            "global_event_target",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        scope_completions: ["root", "this", "from", "prev", "country", "province", "trade_node"]
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
        transparent_scope_wrappers: ["AND", "OR", "NOT"].into_iter().map(str::to_owned).collect(),
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
            ("scripted_effect_params_dollar", "scripted_effect_param_dollar"),
            ("hardcoded_legacygovernments", "hardcoded_legacy_government"),
            ("hardcoded_legacy_only_governments", "hardcoded_legacy_government"),
            ("modifiers", "static_modifier"),
            ("modifier", "static_modifier"),
        ]
        .into_iter()
        .map(|(alias, kind)| (alias.to_owned(), kind.to_owned()))
        .collect(),
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
        enum_extra_members: [(
            "scripted_effect_params".to_owned(),
            vec!["scaled_skill".to_owned()],
        )]
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
                    path_prefix: None,
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
    use super::{Eu4Profile, GAME_ID, bootstrap_rules, first_party_rules, profile};

    #[test]
    fn profile_identity_and_bootstrap_catalog_are_stable() {
        assert_eq!(Eu4Profile.game_id(), GAME_ID);
        let rules = bootstrap_rules();
        assert!(rules.model().file_categories.iter().any(|category| category.id == "script"));
        assert!(rules.model().symbol_descriptors.iter().any(|symbol| symbol.kind_id == "event"));
        let profile = profile();
        assert_eq!(profile.game_id, GAME_ID);
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
        assert_eq!(profile.value_definition_kind("which", Some("set_variable")), Some("variable"));
        assert!(profile.token_definitions.iter().any(|rule| {
            rule.inner_kind == "scripted_effect_param"
                && rule.path.matches("common/scripted_effects/example.txt")
        }));
        assert_eq!(profile.root_scope("country_event"), Some("country"));
        assert!(profile.is_scope("capital_scope"));
        assert!(profile.scopes_compatible("trade_node", "province"));
        assert_eq!(profile.member_kind_alias("COUNTRY_TAGS"), Some("country_tag"));
        assert!(profile.fallback_keys.iter().any(|key| key == "add_treasury"));
        assert!(profile.enum_extra_member("scripted_effect_params", "scaled_skill"));
    }

    #[test]
    fn embedded_first_party_rules_match_the_eu4_profile() {
        let rules = first_party_rules().expect("embedded EU4 rules");
        assert_eq!(rules.game_id(), GAME_ID);
        assert!(!rules.model().semantic.rules.is_empty());
    }
}
