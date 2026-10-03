//! Frozen checkout adaptation is isolated from the current native completion exporter.
use crate::{args::Args, process, report};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
#[path = "../historical_harness.rs"]
mod harness;
pub fn execute(args: &Args, selected: &Path, workspace: &Path) -> Result<String, String> {
    let output = report::output(
        workspace,
        Path::new(args.required("--output")?),
        "performance-results",
    )?;
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let baseline = report::json(Path::new(args.required("--baseline")?))?;
    if baseline["metadata"]["status"] != "passed"
        || !baseline["metadata"]["tool_errors"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return Err("historical baseline is incomplete".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".historical-")
        .tempdir_in(&output)
        .map_err(|e| e.to_string())?;
    let mut fixtures = String::new();
    let mut ids = BTreeSet::new();
    let input_probes = baseline["completions"]
        .as_array()
        .filter(|p| !p.is_empty())
        .ok_or("historical baseline has no probes")?;
    for (i, probe) in input_probes.iter().enumerate() {
        let id = probe["id"].as_str().ok_or("missing historical probe id")?;
        if !ids.insert(id) {
            return Err("duplicate historical probe id".into());
        }
        let name = probe["golden_file"].as_str().ok_or("missing golden file")?;
        let text = super::super::client::golden(workspace, name)?;
        if report::hash(text.as_bytes()) != probe["source_sha256"] {
            return Err(format!("historical golden source drifted: {id}"));
        }
        let offset = super::super::client::byte_offset(
            &text,
            probe["position"]["line"]
                .as_u64()
                .ok_or("missing probe line")? as usize,
            probe["position"]["character"]
                .as_u64()
                .ok_or("missing probe column")? as usize,
        )?;
        let document = probe["document_path"]
            .as_str()
            .ok_or("missing probe document")?;
        if document.contains(['\n', '\r', '\t'])
            || id.contains(['\n', '\r', '\t'])
            || Path::new(document)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("invalid historical probe path/id".into());
        }
        let path = temporary.path().join(format!("probe-{i}.txt"));
        fs::write(&path, text).map_err(|e| e.to_string())?;
        fixtures.push_str(&format!("{id}\t{document}\t{offset}\t{}\n", path.display()));
    }
    fs::write(temporary.path().join("fixtures.tsv"), fixtures).map_err(|e| e.to_string())?;
    fs::create_dir(temporary.path().join("src")).map_err(|e| e.to_string())?;
    let mut cargo="[package]\nname=\"pdc-completion-inventory\"\nversion=\"0.0.0\"\nedition=\"2024\"\n[dependencies]\nserde_json=\"1\"\n".to_owned();
    for name in ["engine", "ide", "game", "text", "rules"] {
        cargo.push_str(&format!(
            "{name} = {{ path = {} }}\n",
            json!(selected.join("crates").join(name).to_string_lossy())
        ));
    }
    fs::write(temporary.path().join("Cargo.toml"), cargo).map_err(|e| e.to_string())?;
    let initializer = if args.get("--mode") == Some("legacy") {
        "let mut host = AnalysisHost::with_profile(game::eu4::first_party_rules().unwrap(), game::eu4::profile());"
    } else {
        "let ir = game::eu4::first_party_ir().unwrap(); let mut host = AnalysisHost::with_ir(rules::RuleSet::from_ir_catalog(&ir), ir.game.profile.clone(), ir);"
    };
    let mut source = harness::SOURCE.replace("INITIALISE_HOST", initializer);
    if !fs::read_to_string(selected.join("crates/engine/src/lib.rs"))
        .map_err(|e| e.to_string())?
        .contains("ANALYZER_BUILD_ID")
    {
        let begin = source
            .find("    let cache = IndexCache::load(")
            .ok_or("missing cache adapter marker")?;
        let end = source
            .find("    eprintln!(\"rule_hash=")
            .ok_or("missing rule identity marker")?;
        source.replace_range(begin..end,"    host.install_index_cache(IndexCache::load(Path::new(&args[0])).expect(\"load cache\")).expect(\"install matching cache\");\n");
    }
    fs::write(temporary.path().join("src/main.rs"), source).map_err(|e| e.to_string())?;
    let log = fs::File::create(output.join("build.log")).map_err(|e| e.to_string())?;
    process::run(
        process::command("cargo")
            .args(["build", "--offline", "--release", "--manifest-path"])
            .arg(temporary.path().join("Cargo.toml"))
            .arg("--target-dir")
            .arg(selected.join("target"))
            .stdout(std::process::Stdio::from(
                log.try_clone().map_err(|e| e.to_string())?,
            ))
            .stderr(std::process::Stdio::from(log)),
    )?;
    let binary = selected.join("target/release").join(if cfg!(windows) {
        "pdc-completion-inventory.exe"
    } else {
        "pdc-completion-inventory"
    });
    let overlays = temporary.path().join("project");
    fs::create_dir(&overlays).map_err(|e| e.to_string())?;
    let log = fs::File::create(output.join("run.log")).map_err(|e| e.to_string())?;
    process::run(
        process::command(&binary)
            .arg(
                Path::new(args.required("--cache")?)
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
            )
            .arg(temporary.path().join("fixtures.tsv"))
            .arg(output.join("items.tsv"))
            .arg(&overlays)
            .stdout(std::process::Stdio::from(
                log.try_clone().map_err(|e| e.to_string())?,
            ))
            .stderr(std::process::Stdio::from(log)),
    )?;
    let hash = baseline["metadata"]["rules"]
        .get("rule_hash")
        .or_else(|| baseline["metadata"]["rules"].get("manifest_rule_hash"))
        .and_then(Value::as_str)
        .ok_or("missing historical rule identity")?;
    if !fs::read_to_string(output.join("run.log"))
        .map_err(|e| e.to_string())?
        .lines()
        .any(|line| line == format!("rule_hash={hash}"))
    {
        return Err("historical harness/baseline rule identity mismatch".into());
    }
    let mut probes = input_probes
        .iter()
        .map(|probe| {
            let mut probe = probe.clone();
            probe["full_labels"] = json!([]);
            probe["full_candidates"] = json!([]);
            (probe["id"].as_str().unwrap().to_owned(), probe)
        })
        .collect::<BTreeMap<_, _>>();
    let mut counts = BTreeMap::new();
    for line in fs::read_to_string(output.join("items.tsv"))
        .map_err(|e| e.to_string())?
        .lines()
    {
        let parts = line.split('\t').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err("invalid historical inventory row".into());
        }
        let probe = probes.get_mut(parts[0]).ok_or("unknown historical probe")?;
        if parts[1] == "#count" {
            if counts
                .insert(
                    parts[0].to_owned(),
                    parts[2].parse::<usize>().map_err(|e| e.to_string())?,
                )
                .is_some()
            {
                return Err("duplicate historical candidate total".into());
            }
        } else {
            let labels = probe["full_labels"].as_array_mut().unwrap();
            if parts[1].parse::<usize>().map_err(|e| e.to_string())? != labels.len() {
                return Err("incomplete historical inventory".into());
            }
            labels.push(json!(parts[2]));
        }
    }
    for line in fs::read_to_string(output.join("items.jsonl"))
        .map_err(|e| e.to_string())?
        .lines()
    {
        let mut item: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let id = item["probe"]
            .as_str()
            .ok_or("missing historical detail probe")?
            .to_owned();
        item.as_object_mut()
            .ok_or("invalid historical candidate detail")?
            .remove("probe");
        let probe = probes.get_mut(&id).ok_or("unknown detail probe")?;
        let details = probe["full_candidates"].as_array_mut().unwrap();
        if item["ordinal"].as_u64() != Some(details.len() as u64) {
            return Err("incomplete historical candidate details".into());
        }
        details.push(item);
    }
    for (id, probe) in &mut probes {
        let labels = probe["full_labels"].as_array().unwrap();
        if counts.get(id) != Some(&labels.len())
            || probe["full_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| &d["label"])
                .ne(labels.iter())
        {
            return Err("truncated or mismatched historical candidate details".into());
        }
        let expected = probe["labels"]
            .as_array()
            .ok_or("missing bounded historical labels")?
            .iter()
            .map(Value::to_string)
            .collect::<BTreeSet<_>>();
        if expected != labels.iter().take(512).map(Value::to_string).collect() {
            return Err(format!("historical IDE/LSP mismatch for {id}"));
        }
        probe["full_count"] = json!(labels.len());
        probe["harness_matches_bounded_lsp"] = json!(true);
    }
    report::write(
        &output.join("inventory.json"),
        &json!({"status":"exported-unbounded-ide-inventory","mode":args.get("--mode").unwrap_or("ir"),"repo":selected,"binary_sha256":report::file_hash(&binary)?,"rules":baseline["metadata"]["rules"],"probes":probes.into_values().collect::<Vec<_>>()}),
    )?;
    Ok(format!(
        "Exported {} complete historical IDE inventories",
        ids.len()
    ))
}
