//! Installed loose/archive assets and exact per-diagnostic evidence.
use super::{compare::read_metadata, identity_json, rules_identity};
use crate::{args::Args, report};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

type Evidence = BTreeMap<String, Vec<Value>>;
#[derive(Default)]
pub struct Inventory {
    pub files: Evidence,
    pub sprites: Evidence,
    pub archive_errors: Vec<Value>,
}
fn normalized(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}
fn sprites(text: &str, origin: &str, inventory: &mut Inventory) {
    let text = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let pattern =
        Regex::new(r#"\bspriteType\s*=\s*\{[^{}]*?\bname\s*=\s*(?:"([^"\r\n]+)"|([^\s{}#]+))"#)
            .expect("sprite pattern");
    for matched in pattern.captures_iter(&text) {
        let name = matched
            .get(1)
            .or_else(|| matched.get(2))
            .expect("name")
            .as_str();
        let line = text[..matched.get(0).unwrap().start()]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1;
        inventory
            .sprites
            .entry(name.to_lowercase())
            .or_default()
            .push(json!({"source":origin,"line":line,"name":name,"declaration":"spriteType"}));
    }
}
pub fn inventory(root: &Path) -> Result<Inventory, String> {
    if !root.is_dir() {
        return Err("installation is not a directory".into());
    }
    let mut result = Inventory::default();
    for path in report::files(root)? {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        result
            .files
            .entry(relative.to_lowercase())
            .or_default()
            .push(json!({"kind":"loose","path":relative}));
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "gfx" {
            sprites(
                &report::text(&fs::read(&path).map_err(|e| e.to_string())?).0,
                &relative,
                &mut result,
            );
        }
        if ext == "zip" {
            let archive_result = (|| -> Result<(), String> {
                let mut archive =
                    zip::ZipArchive::new(fs::File::open(&path).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                for i in 0..archive.len() {
                    let mut member = archive.by_index(i).map_err(|e| e.to_string())?;
                    if member.is_dir() {
                        continue;
                    }
                    let name = normalized(member.name());
                    result
                        .files
                        .entry(name.to_lowercase())
                        .or_default()
                        .push(json!({"kind":"archive","path":name,"archive":relative}));
                    if name.to_lowercase().ends_with(".gfx") {
                        if member.size() > 16 * 1024 * 1024 {
                            return Err("GFX archive member exceeds source limit".into());
                        }
                        let mut bytes = Vec::new();
                        member.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                        sprites(
                            &report::text(&bytes).0,
                            &format!("{relative}!{name}"),
                            &mut result,
                        );
                    }
                }
                Ok(())
            })();
            if let Err(error) = archive_result {
                result
                    .archive_errors
                    .push(json!({"archive":relative,"error":error}));
            }
        }
    }
    Ok(result)
}
fn set(entry: &mut Value, kind: &str, explanation: &str, evidence: Vec<Value>) {
    entry["classification"] = json!(kind);
    entry["explanation"] = json!(explanation);
    entry["evidence"] = json!(evidence);
}
fn group(message: &str) -> String {
    let mut head = message.lines().next().unwrap_or("").to_owned();
    static PATTERNS: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    for regex in PATTERNS.get_or_init(|| {
        [
            r"^texture file `[^`]*`",
            r"^invalid value `[^`]*`",
            r"^argument `[^`]*`",
            r"renders as `[^`]*`",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("group pattern"))
        .collect()
    }) {
        head = regex
            .replace_all(&head, |captures: &regex::Captures<'_>| {
                let prefix = captures[0].split('`').next().unwrap_or("").trim_end();
                format!("{prefix} VALUE")
            })
            .into_owned();
    }
    head
}
fn frozen(
    args: &Args,
    input: &Value,
    rules: &Value,
) -> Result<(BTreeMap<String, Value>, Value), String> {
    let prior = args
        .path("--prior-review")
        .ok_or("missing --prior-review")?;
    let summary = report::json(&prior.join("summary.json"))?;
    let errors = report::json(&prior.join("errors.json"))?;
    let prior_report = Path::new(summary["report"].as_str().ok_or("missing frozen report")?);
    if report::file_hash(prior_report)? != summary["report_sha256"].as_str().unwrap_or("") {
        return Err("prior resource report differs from its frozen identity".into());
    }
    let before = read_metadata(Path::new(args.required("--prior-cache")?))?;
    let after = read_metadata(Path::new(args.required("--cache")?))?;
    let previous_rules = summary
        .get("rules")
        .or_else(|| summary.get("manifest"))
        .ok_or("missing frozen rule identity")?;
    if before["rule_hash"] != previous_rules["rule_hash"]
        || after["rule_hash"] != rules["rule_hash"]
    {
        return Err("resource review cache and report rule identities differ".into());
    }
    let previous = report::json(prior_report)?;
    for (report, cache) in [(input, &after), (&previous, &before)] {
        let source = Path::new(
            report["inputs"]["source"]
                .as_str()
                .ok_or("missing source")?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;
        let cached = Path::new(
            cache["source_root"]
                .as_str()
                .ok_or("missing cache source")?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;
        if source != cached {
            return Err("resource review report and cache source roots differ".into());
        }
    }
    for key in ["source_fingerprint", "source_root"] {
        if before[key].as_str().is_none_or(str::is_empty) || before[key] != after[key] {
            return Err(format!("resource review requires identical {key}"));
        }
    }
    let entries = errors
        .as_array()
        .ok_or("invalid frozen error inventory")?
        .iter()
        .filter(|e| {
            matches!(
                e["classification"].as_str(),
                Some(
                    "corpus-missing-resource"
                        | "missing-resource-review"
                        | "resource-resolution-review"
                        | "sprite-declaration-review"
                )
            )
        })
        .map(|e| {
            Ok((
                e["id"]
                    .as_str()
                    .ok_or("missing frozen error ID")?
                    .to_owned(),
                e.clone(),
            ))
        })
        .collect::<Result<_, String>>()?;
    Ok((entries, summary))
}
pub fn review(
    report_path: &Path,
    input: &Value,
    args: &Args,
) -> Result<(Vec<Value>, Value), String> {
    report::ensure_complete(input)?;
    let rules = rules_identity(input)?;
    if args.path("--installation").is_some() == args.path("--prior-review").is_some() {
        return Err("select exactly one of --installation and --prior-review".into());
    }
    let source = Path::new(
        input["inputs"]["source"]
            .as_str()
            .ok_or("missing report source")?,
    );
    let (inventory, inherited, installation, resource_review) = if let Some(root) =
        args.path("--installation")
    {
        (
            inventory(&root)?,
            BTreeMap::new(),
            json!(root.canonicalize().map_err(|e| e.to_string())?),
            json!({"mode":"live-installation","live_installation_checked":true}),
        )
    } else {
        let (entries, previous) = frozen(args, input, &rules)?;
        let prior = args.path("--prior-review").unwrap();
        (
            Inventory {
                archive_errors: previous["archive_errors"]
                    .as_array()
                    .ok_or("invalid archive errors")?
                    .clone(),
                ..Default::default()
            },
            entries,
            previous["installation"].clone(),
            json!({"mode":"historical-exact-diagnostic-identities","source_fingerprint":read_metadata(Path::new(args.required("--cache")?))?["source_fingerprint"],"source_review":prior.canonicalize().map_err(|e|e.to_string())?,"summary_sha256":report::file_hash(&prior.join("summary.json"))?,"errors_sha256":report::file_hash(&prior.join("errors.json"))?,"live_installation_checked":false}),
        )
    };
    let decisions = args
        .path("--decisions")
        .map(|p| report::json(&p))
        .transpose()?
        .unwrap_or(json!({}));
    let decisions = decisions.as_object().ok_or("decisions must be an object")?;
    let mut used = BTreeSet::new();
    let mut occurrences = BTreeMap::<String, usize>::new();
    let mut errors = Vec::new();
    let texture =
        Regex::new(r"^texture file `([^`]*)` not found in any mod, game, or DLC pack root$")
            .unwrap();
    let picture = Regex::new(r"^invalid value `([^`]*)` for `picture`").unwrap();
    for file in input["files"]
        .as_array()
        .ok_or("missing full report files")?
    {
        let diagnostics = file["diagnostics"]
            .as_array()
            .ok_or("invalid diagnostics")?;
        if !diagnostics.iter().any(|d| d["severity"] == 1) {
            continue;
        }
        let physical = Path::new(
            file["physical_path"]
                .as_str()
                .ok_or("missing physical path")?,
        );
        let bytes = fs::read(physical).map_err(|e| format!("{}: {e}", physical.display()))?;
        let text = if file["encoding"] == "utf-8" {
            String::from_utf8(bytes).map_err(|e| e.to_string())?
        } else {
            report::text(&bytes).0
        };
        let lines = text.lines().collect::<Vec<_>>();
        for diagnostic in diagnostics.iter().filter(|d| d["severity"] == 1) {
            let identity = identity_json(&json!([
                file["path"],
                diagnostic["code"],
                diagnostic["range"],
                diagnostic["message"]
            ]));
            let occurrence = occurrences.entry(identity.clone()).or_default();
            *occurrence += 1;
            let id = report::hash(format!("{identity}:{occurrence}").as_bytes());
            let message = diagnostic["message"]
                .as_str()
                .ok_or("missing diagnostic message")?;
            let line = diagnostic["range"]["start"]["line"]
                .as_u64()
                .ok_or("invalid diagnostic range")? as usize;
            let context = (line.saturating_sub(3)..lines.len().min(line.saturating_add(4)))
                .map(|i| json!({"line":i+1,"text":lines[i]}))
                .collect::<Vec<_>>();
            let mut entry = diagnostic.clone();
            entry["id"] = json!(id);
            entry["path"] = file["path"].clone();
            entry["group"] = json!(group(message));
            entry["context"] = json!(context);
            entry["classification"] = json!("unresolved");
            entry["explanation"] = Value::Null;
            entry["evidence"] = json!([]);
            if let Some(matched) = texture.captures(message) {
                let path = normalized(&matched[1]);
                let mut resolved = path.clone();
                let mut found = inventory
                    .files
                    .get(&path.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                if found.is_empty()
                    && matches!(
                        Path::new(&path)
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(str::to_ascii_lowercase)
                            .as_deref(),
                        Some("dds" | "tga")
                    )
                {
                    let suffix = if path.to_lowercase().ends_with(".tga") {
                        "dds"
                    } else {
                        "tga"
                    };
                    resolved = Path::new(&path)
                        .with_extension(suffix)
                        .to_string_lossy()
                        .into_owned();
                    found = inventory
                        .files
                        .get(&resolved.to_lowercase())
                        .cloned()
                        .unwrap_or_default();
                    for evidence in &mut found {
                        evidence["extension_fallback"] = json!(true);
                        evidence["resolver"] =
                            json!("crates/engine/src/texture.rs::extension_fallback");
                    }
                }
                if found.is_empty() {
                    set(
                        &mut entry,
                        "missing-resource-review",
                        if args.flag("--prior-review") {
                            "No resource evidence exists in the frozen review; the installation was not rechecked."
                        } else {
                            "No matching installed loose file or archive member was found; this alone does not prove a Vanilla bug."
                        },
                        found,
                    );
                } else if !source.join(resolved).is_file() {
                    set(
                        &mut entry,
                        "corpus-missing-resource",
                        "The text corpus omits an asset present in the installed game or a DLC archive.",
                        found,
                    );
                } else {
                    set(
                        &mut entry,
                        "resource-resolution-review",
                        "The installation contains this asset; inspect path case and corpus/VFS resolution.",
                        found,
                    );
                }
            } else if let Some(matched) = picture.captures(message) {
                let name = matched[1].to_lowercase();
                let mut found = inventory.sprites.get(&name).cloned().unwrap_or_default();
                found.extend(
                    inventory
                        .sprites
                        .get(&format!("gfx_{name}"))
                        .cloned()
                        .unwrap_or_default(),
                );
                if !found.is_empty() {
                    let missing = found.iter().all(|e| {
                        e["source"]
                            .as_str()
                            .and_then(|s| s.split_once('!'))
                            .is_some_and(|(archive, _)| !source.join(archive).is_file())
                    });
                    set(
                        &mut entry,
                        if missing {
                            "corpus-missing-resource"
                        } else {
                            "sprite-declaration-review"
                        },
                        if missing {
                            "The event picture has a direct spriteType declaration inside an installed DLC archive; the text corpus excludes that archive."
                        } else {
                            "The installation has a matching GFX name; inspect the declaration and DLC/source inventory before accepting it."
                        },
                        found,
                    );
                }
            }
            if let Some(previous) = inherited.get(&id) {
                for key in ["classification", "explanation", "evidence"] {
                    entry[key] = previous[key].clone();
                }
                entry["resource_evidence_origin"] = json!(args.required("--prior-review")?);
            }
            if let Some(decision) = decisions.get(&id) {
                if !matches!(
                    decision["classification"].as_str(),
                    Some(
                        "vanilla-bug"
                            | "rule-bug"
                            | "analyzer-bug"
                            | "valid-diagnostic"
                            | "corpus-missing-resource"
                    )
                ) {
                    return Err(format!("invalid decision classification: {id}"));
                }
                if decision["explanation"].as_str().is_none_or(str::is_empty)
                    || !decision["evidence"]
                        .as_array()
                        .is_some_and(|e| !e.is_empty())
                {
                    return Err(format!("decision requires explanation and evidence: {id}"));
                }
                for key in ["classification", "explanation", "evidence"] {
                    entry[key] = decision[key].clone();
                }
                used.insert(id.clone());
            }
            errors.push(entry);
        }
    }
    if input["summary"]["errors"].as_u64() != Some(errors.len() as u64) {
        return Err("inventory does not match the full report error total".into());
    }
    if decisions.keys().collect::<BTreeSet<_>>() != used.iter().collect::<BTreeSet<_>>() {
        return Err("decisions contain stale or unmatched diagnostic identities".into());
    }
    let mut counts = BTreeMap::<String, usize>::new();
    let mut codes = BTreeMap::<String, usize>::new();
    let mut groups = BTreeMap::<(String, String, String), Vec<Value>>::new();
    for entry in &errors {
        let kind = entry["classification"].as_str().unwrap().to_owned();
        let code = entry["code"].as_str().unwrap_or("unknown").to_owned();
        *counts.entry(kind.clone()).or_default() += 1;
        *codes.entry(code.clone()).or_default() += 1;
        groups
            .entry((code, entry["group"].as_str().unwrap().to_owned(), kind))
            .or_default()
            .push(entry["id"].clone());
    }
    let pending: usize = counts
        .iter()
        .filter(|(kind, _)| {
            !matches!(
                kind.as_str(),
                "vanilla-bug" | "valid-diagnostic" | "corpus-missing-resource"
            )
        })
        .map(|(_, n)| n)
        .sum();
    let mut groups = groups.into_iter().map(|((code,message,classification),ids)|json!({"code":code,"message":message,"classification":classification,"count":ids.len(),"error_ids":ids})).collect::<Vec<_>>();
    groups.sort_by_key(|g| std::cmp::Reverse(g["count"].as_u64().unwrap()));
    let summary = json!({"status":if pending == 0 && inventory.archive_errors.is_empty() {"explained"} else {"semantic-review-required"},"criterion":"Every error explained by evidence; legacy equality is not required.","report":report_path.canonicalize().map_err(|e|e.to_string())?,"report_sha256":report::file_hash(report_path)?,"rules":rules,"installation":installation,"resource_review":resource_review,"files_diagnosed":input["files"].as_array().unwrap().len(),"total_errors":errors.len(),"by_code":codes,"classifications":counts,"pending_errors":pending,"archive_errors":inventory.archive_errors,"groups":groups});
    Ok((errors, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn fixture() -> (tempfile::TempDir, Value, Args) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let game = root.join("game");
        let source = root.join("source");
        fs::create_dir_all(game.join("gfx")).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("input.txt"),
            "texturefile = gfx/card.tga\npicture = DLC_CARD\n",
        )
        .unwrap();
        fs::write(game.join("gfx/card.dds"), []).unwrap();
        let mut zip = zip::ZipWriter::new(fs::File::create(game.join("dlc.zip")).unwrap());
        zip.start_file(
            "interface/card.gfx",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"spriteTypes = { spriteType = { name = \"DLC_CARD\" } }")
            .unwrap();
        zip.finish().unwrap();
        let range = json!({"start":{"line":0,"character":14},"end":{"line":0,"character":26}});
        let texture = json!({"severity":1,"code":"UnknownTexturePath","range":range,"message":"texture file `gfx/card.tga` not found in any mod, game, or DLC pack root"});
        let picture = json!({"severity":1,"code":"InvalidValue","range":range,"message":"invalid value `dlc_card` for `picture`\nexpected a sprite name"});
        let input = json!({"status":"passed","tool_errors":[],"summary":{"errors":3},"inputs":{"source":source,"server_identity":{"version":"0.4.2"},"rules":{"rule_hash":"frozen","source_format_version":13}},"files":[{"path":"input.txt","physical_path":source.join("input.txt"),"encoding":"utf-8","diagnostics":[texture,texture,picture]}]});
        report::write(&root.join("report.json"), &input).unwrap();
        let args = Args::parse(
            &["--installation".into(), game.display().to_string()],
            &["--installation"],
            &[],
        )
        .unwrap();
        (directory, input, args)
    }
    #[test]
    fn preserves_python_ids_duplicate_coverage_and_dlc_evidence() {
        let (directory, input, args) = fixture();
        let (errors, summary) =
            review(&directory.path().join("report.json"), &input, &args).unwrap();
        assert_eq!(
            errors
                .iter()
                .map(|e| e["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "5f16af683ca389480401377e1e47db7965e603ff52a9fd94c022524a75cfacce",
                "04d22e557dd72bd6e78bbe802556e6a026d954e03e7dfa9445afbb59223e0142",
                "2c603e36898f9e7855181d59419d03a8654175add09a1baea22539005310e47b"
            ]
        );
        assert_eq!(
            summary["classifications"],
            json!({"corpus-missing-resource":3})
        );
        assert_eq!(errors[0]["evidence"][0]["extension_fallback"], true);
        assert_eq!(errors[2]["evidence"][0]["declaration"], "spriteType");
        assert_eq!(summary["rules"]["rule_hash"], "frozen");
        assert_eq!(summary["rules"]["lsp_version"], "0.4.2");
        assert!(summary.get("manifest").is_none());
        fs::copy(
            directory.path().join("game/dlc.zip"),
            directory.path().join("source/dlc.zip"),
        )
        .unwrap();
        let (_, summary) = review(&directory.path().join("report.json"), &input, &args).unwrap();
        assert_eq!(summary["pending_errors"], 1);
    }
    #[test]
    fn refuses_incomplete_totals_identities_and_stale_decisions() {
        let (directory, mut input, mut args) = fixture();
        let path = directory.path().join("report.json");
        input["summary"]["errors"] = json!(4);
        assert!(
            review(&path, &input, &args)
                .unwrap_err()
                .contains("error total")
        );
        input["summary"]["errors"] = json!(3);
        input["inputs"]["rules"]["manifest_rule_hash"] = json!("different");
        assert!(
            review(&path, &input, &args)
                .unwrap_err()
                .contains("identities differ")
        );
        input["inputs"]["rules"]
            .as_object_mut()
            .unwrap()
            .remove("manifest_rule_hash");
        input["inputs"]["rules"]["rule_hash"] = json!("");
        assert!(
            review(&path, &input, &args)
                .unwrap_err()
                .contains("missing embedded rule identity")
        );
        input["inputs"]["rules"]["rule_hash"] = json!("frozen");
        let decisions = directory.path().join("decisions.json");
        report::write(&decisions, &json!({"not-an-error":{}})).unwrap();
        args.values
            .insert("--decisions".into(), vec![decisions.display().to_string()]);
        assert!(
            review(&path, &input, &args)
                .unwrap_err()
                .contains("stale or unmatched")
        );
        input["status"] = json!("incomplete");
        assert!(
            review(&path, &input, &args)
                .unwrap_err()
                .contains("incomplete")
        );
    }
    #[test]
    fn frozen_reuse_requires_identical_corpus_and_untampered_report() {
        let (directory, mut input, args) = fixture();
        let root = directory.path();
        let path = root.join("report.json");
        let (errors, mut summary) = review(&path, &input, &args).unwrap();
        let prior = root.join("prior");
        report::write(&prior.join("errors.json"), &json!(errors)).unwrap();
        report::write(&prior.join("summary.json"), &summary).unwrap();
        let source = input["inputs"]["source"].as_str().unwrap();
        for (name, hash) in [("old.db", "frozen"), ("new.db", "new")] {
            let db = rusqlite::Connection::open(root.join(name)).unwrap();
            db.execute_batch("CREATE TABLE metadata(key TEXT, value TEXT)")
                .unwrap();
            for (key, value) in [
                ("rule_hash", hash),
                ("source_root", source),
                ("source_fingerprint", "same-corpus"),
            ] {
                db.execute("INSERT INTO metadata VALUES (?1,?2)", [key, value])
                    .unwrap();
            }
        }
        input["inputs"]["rules"]["rule_hash"] = json!("new");
        let new_path = root.join("new-report.json");
        report::write(&new_path, &input).unwrap();
        fs::remove_dir_all(root.join("game")).unwrap();
        let args = Args::parse(
            &[
                "--prior-review".into(),
                prior.display().to_string(),
                "--prior-cache".into(),
                root.join("old.db").display().to_string(),
                "--cache".into(),
                root.join("new.db").display().to_string(),
            ],
            &["--prior-review", "--prior-cache", "--cache"],
            &[],
        )
        .unwrap();
        let (current_errors, current_summary) = review(&new_path, &input, &args).unwrap();
        assert_eq!(current_summary["pending_errors"], 0);
        assert_eq!(
            current_summary["resource_review"]["live_installation_checked"],
            false
        );
        // Historical resource reviews embedded identity under this old field name.
        let rules = summary.as_object_mut().unwrap().remove("rules").unwrap();
        summary["manifest"] = rules;
        report::write(&prior.join("summary.json"), &summary).unwrap();
        let (historical_errors, _) = review(&new_path, &input, &args).unwrap();
        assert_eq!(historical_errors, current_errors);
        let db = rusqlite::Connection::open(root.join("new.db")).unwrap();
        db.execute(
            "UPDATE metadata SET value='changed' WHERE key='source_fingerprint'",
            [],
        )
        .unwrap();
        assert!(
            review(&new_path, &input, &args)
                .unwrap_err()
                .contains("identical source_fingerprint")
        );
        db.execute(
            "UPDATE metadata SET value='same-corpus' WHERE key='source_fingerprint'",
            [],
        )
        .unwrap();
        fs::write(path, "{}").unwrap();
        assert!(
            review(&new_path, &input, &args)
                .unwrap_err()
                .contains("frozen identity")
        );
    }
}
