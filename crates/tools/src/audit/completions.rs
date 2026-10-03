//! Unbounded IDE completion inventories without a generated Cargo harness.
use crate::{args::Args, report};
use engine::{
    AnalysisHost, DocumentId, IndexCache, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use text::AbsPath;
mod historical;

pub fn execute(arguments: &[String]) -> Result<String, String> {
    let args = Args::parse(
        arguments,
        &[
            "--root",
            "--repo",
            "--baseline",
            "--cache",
            "--output",
            "--mode",
        ],
        &[],
    )?;
    if args.help() {
        return Ok(super::HELP.into());
    }
    let root = args.root()?;
    let compiled_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let mode = args.get("--mode").unwrap_or("ir");
    if !["ir", "legacy"].contains(&mode) {
        return Err("completion mode must be ir or legacy".into());
    }
    if mode == "legacy" || root != compiled_root {
        return historical::execute(&args, &root, &compiled_root);
    }
    let output = report::output(
        &root,
        Path::new(args.required("--output")?),
        "performance-results",
    )?;
    let baseline = report::json(Path::new(args.required("--baseline")?))?;
    if baseline["metadata"]["status"] != "passed"
        || !baseline["metadata"]["tool_errors"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return Err("baseline is incomplete or contains tool errors".into());
    }
    let ir = game::eu4::first_party_ir().map_err(|e| e.to_string())?;
    let hash = ir.fingerprint();
    let expected = baseline["metadata"]["rules"]
        .get("rule_hash")
        .or_else(|| baseline["metadata"]["rules"].get("manifest_rule_hash"))
        .and_then(Value::as_str)
        .ok_or("missing baseline rule hash")?;
    if hash != expected {
        return Err("selected tool and baseline rule identity mismatch; run tools built from the selected checkout".into());
    }
    let mut host = AnalysisHost::with_ir(
        rules::RuleSet::from_ir_catalog(&ir),
        ir.game.profile.clone(),
        ir,
    );
    let cache_path = Path::new(args.required("--cache")?);
    let cache = match IndexCache::load(cache_path) {
        Ok(cache) => cache,
        Err(error) => {
            if !matches!(
                error,
                engine::IndexCacheError::LspVersionMismatch { .. }
                    | engine::IndexCacheError::InvalidMetadata("lsp_version")
            ) {
                return Err(error.to_string());
            }
            let source = IndexCache::recorded_source_root(cache_path).map_err(|e| e.to_string())?;
            let metadata = super::compare::read_metadata(cache_path)?;
            let expected_source = metadata["source_fingerprint"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("missing recorded corpus identity")?;
            host.apply_change(WorkspaceChange::SetSourceRoots(vec![source]));
            host.refresh_source_roots().map_err(|e| e.to_string())?;
            let rebuilt = IndexCache::from_snapshot(&host.snapshot()).map_err(|e| e.to_string())?;
            if rebuilt.metadata().source_fingerprint != expected_source {
                return Err("recorded corpus changed since the baseline".into());
            }
            let snapshot = host.snapshot();
            host = AnalysisHost::with_ir(
                snapshot.rules().clone(),
                snapshot.game_profile().clone(),
                snapshot.ir_handle(),
            );
            rebuilt
        }
    };
    std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let overlay = tempfile::Builder::new()
        .prefix(".inventory-")
        .tempdir_in(&output)
        .map_err(|e| e.to_string())?;
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(u32::MAX),
        SourceRootKind::Project,
        AbsPath::normalize(overlay.path()),
    )]));
    host.install_index_cache(cache).map_err(|e| e.to_string())?;
    let mut probes = Vec::new();
    let mut ids = BTreeSet::new();
    for probe in baseline["completions"]
        .as_array()
        .filter(|p| !p.is_empty())
        .ok_or("missing completion probes")?
    {
        let id = probe["id"].as_str().ok_or("missing probe id")?;
        if !ids.insert(id) {
            return Err("duplicate probe id".into());
        }
        let text = super::client::golden(
            &root,
            probe["golden_file"].as_str().ok_or("missing golden file")?,
        )?;
        if report::hash(text.as_bytes()) != probe["source_sha256"] {
            return Err(format!("golden source drifted: {id}"));
        }
        let offset = super::client::byte_offset(
            &text,
            probe["position"]["line"]
                .as_u64()
                .ok_or("invalid probe line")? as usize,
            probe["position"]["character"]
                .as_u64()
                .ok_or("invalid probe column")? as usize,
        )?;
        let logical = probe["document_path"]
            .as_str()
            .ok_or("missing probe document")?;
        if Path::new(logical).is_absolute()
            || Path::new(logical)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("invalid probe document path".into());
        }
        let path = overlay.path().join(logical);
        let document = DocumentId::new(crate::lsp::uri(&path)?);
        host.open_document(document.clone(), 1, text, Some(AbsPath::normalize(&path)))
            .map_err(|e| e.to_string())?;
        let items = ide::complete(
            &host.snapshot(),
            &document,
            offset
                .try_into()
                .map_err(|_| "completion offset too large")?,
        )
        .items;
        let labels = items.iter().map(|i| i.label.clone()).collect::<Vec<_>>();
        let expected = probe["labels"]
            .as_array()
            .ok_or("missing bounded labels")?
            .iter()
            .map(|v| v.as_str().ok_or("invalid bounded label"))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let actual = labels
            .iter()
            .take(512)
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if expected != actual {
            return Err(format!(
                "IDE/LSP mismatch at {id}: added {:?}, removed {:?}",
                actual.difference(&expected).take(8).collect::<Vec<_>>(),
                expected.difference(&actual).take(8).collect::<Vec<_>>()
            ));
        }
        let details=items.iter().enumerate().map(|(ordinal,item)|json!({"ordinal":ordinal,"label":item.label,"kind":format!("{:?}",item.kind),"detail":item.detail,"insert_text":item.insert_text,"replacement_range":[item.replacement_range.start(),item.replacement_range.end()]})).collect::<Vec<_>>();
        let mut result = probe.clone();
        result["full_count"] = json!(labels.len());
        result["full_labels"] = json!(labels);
        result["full_candidates"] = json!(details);
        result["harness_matches_bounded_lsp"] = json!(true);
        probes.push(result);
        host.close_document(&document).map_err(|e| e.to_string())?;
    }
    report::write(
        &output.join("inventory.json"),
        &json!({"status":"exported-unbounded-ide-inventory","mode":"ir","repo":root,"rules":baseline["metadata"]["rules"],"tool_version":env!("CARGO_PKG_VERSION"),"probes":probes}),
    )?;
    Ok(format!(
        "Exported {} complete IDE inventories; each first 512 matches the recorded LSP response",
        ids.len()
    ))
}
