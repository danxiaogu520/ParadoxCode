//! Deterministic compilation for build-time embedding. Invalid sources never produce a payload.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Identity and arena sizes recorded beside the first-party source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub source_format_version: u32,
    pub game_id: String,
    pub target_game_version: Option<String>,
    pub rule_hash: String,
    pub schema_count: usize,
    pub field_count: usize,
    pub matcher_count: usize,
    pub file_category_count: usize,
}

/// Checked arena and reproducible identity produced together.
pub struct BakedRules {
    pub manifest: ArtifactManifest,
    pub bytes: Vec<u8>,
}

/// Checks and lowers a source bundle, then serializes its arena with sorted JSON object keys.
///
/// # Errors
/// Returns a source, semantic-check, or serialization error. No output is written on failure.
pub fn compile(directory: &Path) -> Result<BakedRules, String> {
    let sources = crate::bundle::load_directory(directory).map_err(|error| error.to_string())?;
    let ir = crate::lower::lower(&sources.files, sources.game).map_err(|error| {
        let diagnostics = error
            .diagnostics()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        format!("{error}\n{diagnostics}")
    })?;
    if let Some(spec) = &ir.game.profile.mission_view
        && ir.type_by_name(&spec.symbol_kind).is_none()
    {
        return Err(format!(
            "mission_view.symbol_kind `{}` is not a declared type",
            spec.symbol_kind
        ));
    }
    let manifest = ArtifactManifest {
        source_format_version: sources.identity.source_format_version,
        game_id: ir.game_id().to_owned(),
        target_game_version: sources.identity.target_game_version,
        rule_hash: ir.fingerprint(),
        schema_count: ir.schemas.len(),
        field_count: ir.fields.len(),
        matcher_count: ir.matchers.len(),
        file_category_count: ir.files.len(),
    };
    let mut value = serde_json::to_value(&ir).map_err(|error| error.to_string())?;
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    Ok(BakedRules { manifest, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_party_bake_is_reproducible_and_round_trips() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4");
        let first = compile(&root).expect("bake");
        let second = compile(&root).expect("rebake");
        assert_eq!(first.manifest, second.manifest);
        assert_eq!(first.bytes, second.bytes);
        let ir = crate::ir::RulesIr::from_baked(&first.bytes).expect("decode");
        assert_eq!(ir.fingerprint(), first.manifest.rule_hash);
        assert!(ir.schema_by_name("on_actions_file").is_some());
        assert!(ir.type_by_name("event").is_some());
        assert!(
            ir.file_rule(&text::LogicalPath::parse("events/example.txt").unwrap())
                .is_some()
        );
    }
}
