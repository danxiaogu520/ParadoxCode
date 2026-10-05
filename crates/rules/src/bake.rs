//! Deterministic compilation for build-time embedding. Invalid sources never produce a payload.

use std::path::Path;

/// Checked arena and its deterministic serialized payload.
pub struct BakedRules {
    pub ir: crate::ir::RulesIr,
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
    let mut value = serde_json::to_value(&ir).map_err(|error| error.to_string())?;
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    Ok(BakedRules { ir, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_bake_is_reproducible_and_round_trips() {
        // Corpus acceptance remains covered by lower::tests and first_party_ir.
        // Reproducible serialization needs representative fields, not two full
        // first-party corpus compilations for each test run.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::write(
            root.join("game.json"),
            r#"{"game_id":"test","source_format_version":14}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("rules.json"),
            r#"{
                "types": {"event": {}},
                "files": {"events": {"path": "events", "ext": "txt", "root": "on_actions_file"}},
                "schemas": {"on_actions_file": {"fields": {
                    "id": {"value": "def<event>", "card": "1"},
                    "enabled": {"value": "bool", "card": "0..1"}
                }}}
            }"#,
        )
        .unwrap();
        let first = compile(root).expect("bake");
        let second = compile(root).expect("rebake");
        assert_eq!(first.ir.fingerprint(), second.ir.fingerprint());
        assert_eq!(first.bytes, second.bytes);
        let ir = crate::ir::RulesIr::from_baked(&first.bytes).expect("decode");
        assert_eq!(ir.fingerprint(), first.ir.fingerprint());
        assert!(ir.schema_by_name("on_actions_file").is_some());
        assert!(ir.type_by_name("event").is_some());
        assert!(
            ir.file_rule(&text::LogicalPath::parse("events/example.txt").unwrap())
                .is_some()
        );
    }
}
