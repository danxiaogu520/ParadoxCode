//! EU4 profile data layered on the game-independent rules runtime.

use pdx_rules::{
    CsvDialect, FileCategory, FileMatcher, FileResolutionPolicy, GameProfile, ParserKind,
    ProfileDefinitionRule, ProfileMatchMode, ProfileReferenceRule, ProfileTextMatcher, RuleSet,
    RulesModel, SymbolDescriptor, SymbolResolutionPolicy,
};

/// Stable identity stored by EU4 rule artifacts and selected by the server.
pub const GAME_ID: &str = "eu4";

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
    GameProfile {
        game_id: GAME_ID.to_owned(),
        definitions: vec![
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
        ],
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
                id: "pdx-script".to_owned(),
                parser: ParserKind::PdxScript,
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
                id: "csv".to_owned(),
                parser: ParserKind::Csv(CsvDialect::Semicolon),
                resolution: FileResolutionPolicy::ReplaceByRelativePath,
                matcher: FileMatcher {
                    path_prefix: None,
                    extensions: vec!["csv".to_owned()],
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
        cwt: pdx_rules::CwtSemanticModel::default(),
    }
}

/// Returns the minimal EU4 bootstrap rule set.
#[must_use]
pub fn bootstrap_rules() -> RuleSet {
    Eu4Profile.bootstrap_rules()
}

#[cfg(test)]
mod tests {
    use super::{Eu4Profile, GAME_ID, bootstrap_rules, profile};

    #[test]
    fn profile_identity_and_bootstrap_catalog_are_stable() {
        assert_eq!(Eu4Profile.game_id(), GAME_ID);
        let rules = bootstrap_rules();
        assert!(rules.model().file_categories.iter().any(|category| category.id == "pdx-script"));
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
    }
}
