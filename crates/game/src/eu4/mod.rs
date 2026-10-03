//! EU4 profile data layered on the game-independent rules runtime.
//!
//! Everything here is EU4-specific: the installation descriptor compiled from `game.json`,
//! the embedded first-party IR bootstrap, and the structured mission model. Production data
//! lives under `rules/eu4`.

pub mod mission;

use std::sync::OnceLock;

use std::sync::Arc;

use rules::ir::RulesIr;
use rules::{GameProfile, RuleSet};

static FIRST_PARTY_PROFILE: OnceLock<GameProfile> = OnceLock::new();
static FIRST_PARTY_IR: OnceLock<Result<Arc<RulesIr>, String>> = OnceLock::new();

/// Stable identity stored by EU4 rule artifacts and selected by the server.
pub const GAME_ID: &str = "eu4";

include!(concat!(env!("OUT_DIR"), "/eu4.install.rs"));

/// Loads the checked first-party IR baked by `build.rs`.
///
/// Source validation and lowering run during the build. At runtime the arena
/// is decoded once, its lookup indexes are restored, and callers share an Arc.
///
/// # Errors
/// Returns [`rules::RulesError`] if the embedded compiled payload cannot be decoded.
pub fn first_party_ir() -> Result<Arc<RulesIr>, rules::RulesError> {
    match FIRST_PARTY_IR.get_or_init(|| source_ir().map_err(|error| error.to_string())) {
        Ok(ir) => Ok(Arc::clone(ir)),
        Err(error) => Err(rules::RulesError::Source(error.clone())),
    }
}

/// The file-scanning catalog for the active IR. No legacy semantic source is loaded.
pub fn runtime_rules() -> Result<RuleSet, rules::RulesError> {
    let ir = first_party_ir()?;
    Ok(RuleSet::from_ir_catalog(&ir))
}

fn source_ir() -> Result<Arc<RulesIr>, rules::RulesError> {
    let ir = RulesIr::from_baked(include_bytes!(concat!(env!("OUT_DIR"), "/eu4.ir.json")))
        .map_err(|error| rules::RulesError::Source(error.to_string()))?;
    if ir.game_id() != GAME_ID {
        return Err(rules::RulesError::GameMismatch {
            expected: GAME_ID.to_owned(),
            actual: ir.game_id().to_owned(),
        });
    }
    Ok(Arc::new(ir))
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

    /// Catalog for isolated source-scanning callers.
    #[must_use]
    pub fn bootstrap_rules(self) -> RuleSet {
        runtime_rules().expect("embedded IR catalog")
    }

    /// Returns the data-only EU4 semantic interpretation used by generic runtime crates.
    #[must_use]
    pub fn data(self) -> GameProfile {
        profile()
    }
}

/// Configuration declared by the compiled game package.
#[must_use]
pub fn profile() -> GameProfile {
    FIRST_PARTY_PROFILE
        .get_or_init(|| first_party_ir().expect("embedded IR").game.profile.clone())
        .clone()
}

/// Catalog for isolated source-scanning callers.
#[must_use]
pub fn bootstrap_rules() -> RuleSet {
    runtime_rules().expect("embedded IR catalog")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Eu4Profile, GAME_ID, bootstrap_rules, first_party_ir, profile, runtime_rules};
    use rules::SourceEncoding;
    use text::LogicalPath;

    #[test]
    fn installation_descriptor_matches_the_embedded_game_configuration() {
        let ir = first_party_ir().unwrap();
        let install = ir
            .game
            .profile
            .install
            .as_ref()
            .expect("install configuration");
        let descriptor = super::INSTALL_DESCRIPTOR;
        fn strings(values: &[String]) -> Vec<&str> {
            values.iter().map(String::as_str).collect()
        }
        assert_eq!(descriptor.game_id, ir.game_id());
        assert_eq!(descriptor.display_name, install.display_name);
        assert_eq!(
            descriptor.executable_paths.windows,
            strings(&install.executable_paths.windows)
        );
        assert_eq!(
            descriptor.executable_paths.linux,
            strings(&install.executable_paths.linux)
        );
        assert_eq!(
            descriptor.executable_paths.macos,
            strings(&install.executable_paths.macos)
        );
        assert_eq!(
            descriptor.validation_directories,
            strings(&install.validation_directories)
        );
        assert_eq!(
            descriptor.installation_directory_names,
            strings(&install.installation_directory_names)
        );
        assert_eq!(descriptor.steam_app_id, install.steam_app_id);
    }

    #[test]
    fn trade_node_execution_uses_only_province_and_keeps_its_symbol_namespace() {
        use rules::ir::{Matcher, RootRule, ScopeRef};

        let ir = first_party_ir().expect("embedded IR");
        let trade_node = ir.strings().lookup_folded("trade_node").unwrap();
        let province = ir.strings().lookup_folded("province").unwrap();
        assert!(ir.scopes.types.contains(&province));
        assert!(!ir.scopes.types.contains(&trade_node));
        assert!(ir.scopes.compat.is_empty());
        assert!(ir.types.iter().any(|ty| ty.name == trade_node));
        assert!(ir.matchers.iter().all(|matcher| {
            !matches!(matcher, Matcher::Scope(Some(scope)) if *scope == trade_node)
        }));
        for link in &ir.scopes.links {
            assert!(!link.from.contains(&ScopeRef::Type(trade_node)));
            assert_ne!(link.to, ScopeRef::Type(trade_node));
        }
        let effects = ir
            .fields
            .iter()
            .filter_map(|field| field.scope.as_ref())
            .chain(ir.files.iter().filter_map(|file| match &file.root {
                RootRule::Instance { scope, .. } => scope.as_ref(),
                _ => None,
            }));
        for effect in effects {
            assert!(!effect.scopes_in.contains(&trade_node));
            assert_ne!(effect.push, Some(trade_node));
            assert!(effect.set.iter().all(|(_, scope)| *scope != trade_node));
        }
        for profile in [profile(), ir.game.profile.clone()] {
            assert!(!profile.is_scope("trade_node"));
            assert!(
                !profile
                    .scope_completions
                    .iter()
                    .any(|scope| scope == "trade_node")
            );
            assert!(!profile.scopes_compatible("trade_node", "province"));
        }
    }

    #[test]
    fn profile_identity_and_bootstrap_catalog_are_stable() {
        assert_eq!(Eu4Profile.game_id(), GAME_ID);
        let rules = bootstrap_rules();
        assert!(
            rules
                .file_categories()
                .iter()
                .any(|category| category.id == "script")
        );
        let profile = profile();
        assert_eq!(profile.game_id, GAME_ID);
        assert_eq!(profile.source_encoding, SourceEncoding::Windows1252);
        assert_eq!(profile.scan_roots().len(), 126);
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
        assert_eq!(
            profile.scripted_localisation_directories,
            [
                "scripted_localisation".to_owned(),
                "scripted_localization".to_owned(),
                "scripted_loc".to_owned(),
            ]
        );
        assert!(profile.is_scripted_localisation_path("dlc/foo/common/scripted_loc/defs.txt"));
        assert!(!profile.is_scripted_localisation_path("common/ideas/scripted_loc.txt"));
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
        assert_eq!(profile, first_party_ir().unwrap().game.profile);
        assert_eq!(rules.rule_hash(), first_party_ir().unwrap().rule_hash());
    }

    #[test]
    fn embedded_source_matches_the_filesystem_bundle() {
        let ir = first_party_ir().expect("embedded EU4 rule source");
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sources = rules::bundle::load_directory(&repository_root.join("rules/eu4"))
            .expect("filesystem EU4 rule source");
        let from_disk = rules::lower::lower(&sources.files, sources.game)
            .expect("the filesystem bundle lowers");
        assert_eq!(ir.game_id(), GAME_ID);
        assert_eq!(ir.fingerprint(), from_disk.fingerprint());
        assert_eq!(
            ir.schemas.len(),
            from_disk.schemas.len(),
            "the embedded bundle compiles to the same schema arena"
        );
        assert_eq!(ir.fields.len(), from_disk.fields.len());
        assert_eq!(ir.matchers.len(), from_disk.matchers.len());
        assert_eq!(ir.files.len(), from_disk.files.len());
        let embedded: std::collections::BTreeSet<&str> =
            ir.strings().iter().map(|(_, text)| text).collect();
        let on_disk: std::collections::BTreeSet<&str> =
            from_disk.strings().iter().map(|(_, text)| text).collect();
        assert_eq!(
            embedded, on_disk,
            "the embedded bundle interns what the filesystem one does"
        );
    }

    #[test]
    fn first_party_sprite_references_stay_declared() {
        use rules::ir::{Matcher, TraitArgument};
        let ir = first_party_ir().unwrap();
        for name in ["building", "game_age", "golden_bull"] {
            let ty = ir.type_info(ir.type_by_name(name).unwrap());
            assert!(ty.trait_impls.iter().flat_map(|i| &i.arguments).any(|(_, argument)| {
                matches!(argument, TraitArgument::Binding(binding) if binding.required && binding.sprite.is_some_and(|template| ir.strings().resolve(template)=="GFX_$"))
            }), "{name}");
        }
        for field in ["icon", "alert_icon_gfx"] {
            assert!(ir.fields.iter().any(|f| {
                matches!(ir.matcher(f.key), Matcher::Literal(key) if ir.strings().resolve(*key)==field)
                    && matches!(f.value, rules::ir::FieldValue::Scalar(id) if matches!(ir.matcher(id),Matcher::Ref(rules::ir::RefTarget::Type{type_id,..}) if ir.strings().resolve(ir.type_info(*type_id).name)=="sprite"))
            }), "{field}");
        }
    }

    #[test]
    fn first_party_mission_bindings_stay_declared() {
        use rules::ir::TraitArgument;
        let ir = first_party_ir().unwrap();
        assert_eq!(
            ir.localisation_template_key("mission", "name", "probe"),
            Some("probe_title".to_owned())
        );
        let mission = ir.type_info(ir.type_by_name("mission").unwrap());
        for role in ["name", "desc"] {
            assert!(mission.trait_impls.iter().flat_map(|i| &i.arguments).any(|(name,arg)| {
                ir.strings().resolve(*name)==role && matches!(arg,TraitArgument::Binding(binding) if binding.required && binding.loc.is_some())
            }), "{role}");
        }
    }

    #[test]
    fn first_party_file_categories_are_closed_over_common_and_generated_map_paths() {
        let rules = runtime_rules().expect("embedded EU4 source");
        let classify = |path: &str| {
            rules
                .classify(&LogicalPath::parse(path).expect("logical path"))
                .map(|category| category.id.as_str())
        };

        assert_eq!(
            classify("common/achievements.txt"),
            Some("eu4_path_common_achievements")
        );
        assert_eq!(
            classify("common/technology.txt"),
            Some("eu4_path_common_technology")
        );
        assert_eq!(
            classify("common/alerts.txt"),
            Some("eu4_path_common_alerts")
        );
        assert_eq!(
            classify("common/graphicalculturetype.txt"),
            Some("eu4_path_common_graphicalculturetype")
        );
        assert_eq!(
            classify("common/historial_lucky.txt"),
            Some("eu4_path_common_historial_lucky")
        );
        assert_eq!(
            classify("common/ai_attitudes/00_ai_attitudes.txt"),
            Some("eu4_path_common_ai_attitudes")
        );
        assert_eq!(
            classify("common/defines/difficulty_easy.lua"),
            Some("eu4_path_common_defines_lua")
        );
        assert_eq!(
            classify("common/defines/00_mod_defines.txt"),
            Some("eu4_path_common_defines")
        );
        assert_eq!(classify("common/unknown.txt"), None);
        assert_eq!(classify("common/native_advancements/00_native.txt"), None);
        assert_eq!(classify("common/defines.lua"), None);
        assert_eq!(classify("map/unknown.txt"), Some("eu4_path_map"));
        assert_eq!(classify("map/random/tiles/tile0.txt"), None);
        assert_eq!(classify("dlc_metadata/dlc_info/00_dlc_info.txt"), None);
        assert_eq!(classify("gfx/entities/african_units.asset"), None);
    }
}
