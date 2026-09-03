//! Parser-based accounting of scripted flag names across a game installation.
//!
//! Reads the semantic rule fragments under `--source`, derives the flag key
//! table from them (`dynamic_set` values mark write sites such as
//! `set_country_flag`, `dynamic` values mark read sites such as
//! `has_country_flag` and the `flag` child of `had_ruler_flag`), then parses
//! every script file under `--game` and aggregates, per flag kind:
//!
//! - `sets`:  names written by `set_*_flag` statements
//! - `reads`: names referenced by `has_*`/`had_*`/`clr_*` statements
//! - `engine_set`: reads that no scanned statement ever writes — the
//!   engine-defined seed list for the unknown-flag diagnostic
//!
//! Names containing `$` (macro parameters) are skipped: their runtime value
//! is not statically knowable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pdx_parser::{CstKind, CstNode, FileFormat, ParsedFile, parse};
use rayon::prelude::*;
use serde_json::Value as Json;

const FLAG_KINDS: [&str; 6] = [
    "country_flag",
    "global_flag",
    "province_flag",
    "ruler_flag",
    "heir_flag",
    "consort_flag",
];

#[derive(Default)]
struct FlagKeyTable {
    /// `set_country_flag`-style leaf keys writing a flag of the mapped kind.
    writes: BTreeMap<String, &'static str>,
    /// `has_country_flag`/`clr_country_flag`-style leaf keys reading a flag.
    reads_leaf: BTreeMap<String, &'static str>,
    /// `had_ruler_flag`-style node keys whose `flag` child names the flag.
    reads_block: BTreeMap<String, &'static str>,
}

fn flag_kind(value: &Json, field: &str) -> Option<&'static str> {
    let kind = value.get(field)?.as_str()?;
    FLAG_KINDS
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(kind))
        .copied()
}

fn exact_key(rule: &Json) -> Option<String> {
    let key = rule.get("key")?;
    let exact = key.get("exact")?.as_str()?;
    (!exact.is_empty()).then(|| exact.to_ascii_lowercase())
}

fn parent_path(rule: &Json) -> Option<&Vec<Json>> {
    rule.get("parent_path").and_then(Json::as_array)
}

fn shape_is(rule: &Json, shape: &str) -> bool {
    rule.get("shape").and_then(Json::as_str) == Some(shape)
}

/// Builds the writer/reader key table from the semantic context fragments.
///
/// Block readers are discovered structurally: a top-level `node`-shaped rule
/// whose key matches an owner named by a one-segment parent path of a `flag`
/// child row carrying the same dynamic kind.
fn load_flag_table(source: &Path) -> Result<FlagKeyTable, String> {
    let contexts = source.join("semantic/contexts");
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&contexts)
        .map_err(|error| format!("cannot read {}: {error}", contexts.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    files.sort();
    let mut table = FlagKeyTable::default();
    let mut node_candidates: BTreeMap<String, &'static str> = BTreeMap::new();
    for path in &files {
        let rows: Json = serde_json::from_str(
            &std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
        )
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let Some(rows) = rows.as_array() else {
            continue;
        };
        for rule in rows {
            let Some(value) = rule.get("value") else {
                continue;
            };
            let Some(key) = exact_key(rule) else {
                continue;
            };
            if let Some(kind) = flag_kind(value, "dynamic_set") {
                if shape_is(rule, "leaf") && parent_path(rule).is_none_or(Vec::is_empty) {
                    table.writes.insert(key, kind);
                }
                continue;
            }
            let Some(kind) = flag_kind(value, "dynamic") else {
                continue;
            };
            match parent_path(rule).map(|path| path.as_slice()) {
                None | Some([]) => {
                    if shape_is(rule, "leaf") {
                        table.reads_leaf.insert(key, kind);
                    }
                }
                // `had_ruler_flag = { flag = NAME days = N }`: the top row is
                // `any_scalar` node-shaped; only the `flag` child row marks
                // the owner as a block reader. The child rows carry duplicate
                // kinds (cwtools legacy: `had_ruler_flag` also declares heir
                // and consort children), so the kind is derived from the
                // owner key, which is unambiguous.
                Some([owner]) => {
                    if key == "flag"
                        && shape_is(rule, "leaf")
                        && let Some(owner) = owner.as_str()
                    {
                        let lowered_owner = owner.to_ascii_lowercase();
                        let kind_from_key = lowered_owner.strip_prefix("had_").and_then(|stem| {
                            FLAG_KINDS
                                .iter()
                                .find(|candidate| candidate.eq_ignore_ascii_case(stem))
                                .copied()
                        });
                        node_candidates.insert(lowered_owner, kind_from_key.unwrap_or(kind));
                    }
                }
                _ => {}
            }
        }
    }
    for (owner, kind) in node_candidates {
        table.reads_block.insert(owner, kind);
    }
    if table.writes.is_empty() || table.reads_leaf.is_empty() {
        return Err("flag key table is empty: --source does not look like rule data".to_owned());
    }
    Ok(table)
}

fn option<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
        .ok_or_else(|| format!("missing {name} <value>"))
}

fn collect_script_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Art directories never carry script statements.
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if matches!(name.as_str(), "gfx" | "sound" | "music" | "videos") {
                continue;
            }
            collect_script_files(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "txt" || extension == "gui")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Scalar text of a property's value child, or `None` when the value is a
/// block/header (the caller treats those as non-scalar).
fn scalar_text<'a>(property: CstNode<'a>, parsed: &'a ParsedFile) -> Option<&'a str> {
    let value = property
        .children()
        .find(|child| child.kind() == CstKind::Value)?;
    let scalar = value
        .children()
        .find(|child| matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString))?;
    let text = parsed.text(scalar.range())?.trim();
    let text = text.strip_prefix('"').unwrap_or(text);
    let text = text.strip_suffix('"').unwrap_or(text);
    (!text.is_empty()).then_some(text)
}

fn property_key<'a>(property: CstNode<'a>, parsed: &'a ParsedFile) -> Option<&'a str> {
    let key = property
        .children()
        .find(|child| child.kind() == CstKind::Key)?;
    parsed
        .text(key.range())
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

fn walk_properties(
    node: CstNode<'_>,
    parsed: &ParsedFile,
    table: &FlagKeyTable,
    audit: &mut Audit,
) {
    for child in node.children() {
        match child.kind() {
            CstKind::Property => {
                if let Some(key) = property_key(child, parsed) {
                    let lowered = key.to_ascii_lowercase();
                    if let Some(kind) = table
                        .writes
                        .get(&lowered)
                        .or_else(|| table.reads_leaf.get(&lowered))
                        && let Some(name) = scalar_text(child, parsed)
                    {
                        if table.writes.contains_key(&lowered) {
                            audit.record_set(kind, name);
                        } else if !name.contains('$') {
                            audit.record_read(kind, name);
                        }
                    }
                    if let Some(kind) = table.reads_block.get(&lowered) {
                        for value in child.children().filter(|n| n.kind() == CstKind::Value) {
                            for block in value.children().filter(|n| n.kind() == CstKind::Block) {
                                for inner in
                                    block.children().filter(|n| n.kind() == CstKind::Property)
                                {
                                    if property_key(inner, parsed)
                                        .is_some_and(|key| key.eq_ignore_ascii_case("flag"))
                                        && let Some(name) = scalar_text(inner, parsed)
                                        && !name.contains('$')
                                    {
                                        audit.record_read(kind, name);
                                    }
                                }
                            }
                        }
                    }
                }
                walk_properties(child, parsed, table, audit);
            }
            CstKind::Block | CstKind::Value | CstKind::ParameterBlock => {
                walk_properties(child, parsed, table, audit);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct KindAudit {
    sets: BTreeMap<String, String>,
    /// Literal segments around `$param$` spans of parameterized writes:
    /// `built_dev_$building$` becomes the pattern `["built_dev_", ""]`.
    /// Such reads are mod-reachable, not engine-set.
    set_patterns: Vec<(String, String)>,
    reads: BTreeMap<String, String>,
}

#[derive(Default)]
struct Audit {
    kinds: BTreeMap<&'static str, KindAudit>,
    files_scanned: usize,
    files_failed: usize,
}

impl Audit {
    fn merge(&mut self, other: Audit) {
        self.files_scanned += other.files_scanned;
        self.files_failed += other.files_failed;
        for (kind, audit) in other.kinds {
            let target = self.kinds.entry(kind).or_default();
            for (folded, name) in audit.sets {
                target.sets.entry(folded).or_insert(name);
            }
            for pattern in audit.set_patterns {
                if !target.set_patterns.contains(&pattern) {
                    target.set_patterns.push(pattern);
                }
            }
            for (folded, name) in audit.reads {
                target.reads.entry(folded).or_insert(name);
            }
        }
    }

    fn record_set(&mut self, kind: &'static str, name: &str) {
        if name.contains('$') {
            // `set_X_flag = prefix$param$suffix`: every expansion of the
            // parameter is a legal write, so the read side must accept the
            // pattern rather than the literal.
            let lowered = name.to_ascii_lowercase();
            let mut segments = lowered.split('$');
            let prefix = segments.next().unwrap_or_default().to_owned();
            let suffix = segments.next_back().unwrap_or_default().to_owned();
            self.kinds
                .entry(kind)
                .or_default()
                .set_patterns
                .push((prefix, suffix));
            return;
        }
        let folded = name.to_ascii_lowercase();
        self.kinds
            .entry(kind)
            .or_default()
            .sets
            .entry(folded)
            .or_insert_with(|| name.to_owned());
    }

    fn record_read(&mut self, kind: &'static str, name: &str) {
        let folded = name.to_ascii_lowercase();
        self.kinds
            .entry(kind)
            .or_default()
            .reads
            .entry(folded)
            .or_insert_with(|| name.to_owned());
    }
}

fn scan_file(path: &Path, table: &FlagKeyTable) -> Audit {
    let mut audit = Audit::default();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => {
            audit.files_failed += 1;
            return audit;
        }
    };
    let source = String::from_utf8_lossy(&bytes);
    let parsed = parse(FileFormat::Script, &source);
    if !parsed.errors().is_empty() {
        // EU4 ships a few non-script .txt files; parsing them is still mostly
        // loss-aware, so scan anyway but keep count of imperfect parses.
        audit.files_failed += 1;
    }
    audit.files_scanned += 1;
    walk_properties(parsed.root(), &parsed, table, &mut audit);
    audit
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = || -> Result<(), String> {
        let source = PathBuf::from(option(&args, "--source")?);
        let game = PathBuf::from(option(&args, "--game")?);
        let output = args
            .iter()
            .position(|arg| arg == "--output")
            .and_then(|index| args.get(index + 1))
            .map(PathBuf::from);
        let table = load_flag_table(&source)?;
        eprintln!(
            "flag keys: {} writers, {} leaf readers, {} block readers",
            table.writes.len(),
            table.reads_leaf.len(),
            table.reads_block.len()
        );
        let mut files = Vec::new();
        collect_script_files(&game, &mut files)
            .map_err(|error| format!("cannot walk {}: {error}", game.display()))?;
        eprintln!("scanning {} script files", files.len());
        let audit = files.par_iter().map(|path| scan_file(path, &table)).reduce(
            Audit::default,
            |mut left, right| {
                left.merge(right);
                left
            },
        );
        let mut report = BTreeMap::new();
        for (kind, audit) in &audit.kinds {
            let matches_pattern = |name: &str| {
                audit.set_patterns.iter().any(|(prefix, suffix)| {
                    name.len() >= prefix.len() + suffix.len()
                        && name.starts_with(prefix.as_str())
                        && name.ends_with(suffix.as_str())
                })
            };
            let engine_set: Vec<&String> = audit
                .reads
                .keys()
                .filter(|folded| {
                    !audit.sets.contains_key(*folded) && !matches_pattern(folded.as_str())
                })
                .map(|folded| audit.reads.get(folded).expect("key of reads"))
                .collect();
            let entry = serde_json::json!({
                "sets": audit.sets.values().collect::<Vec<_>>(),
                "set_patterns": audit.set_patterns.iter()
                    .map(|(prefix, suffix)| format!("{prefix}*{suffix}"))
                    .collect::<Vec<_>>(),
                "reads": audit.reads.values().collect::<Vec<_>>(),
                "engine_set": engine_set,
            });
            report.insert(kind.to_owned(), entry);
        }
        let summary = serde_json::json!({
            "files_scanned": audit.files_scanned,
            "files_with_errors": audit.files_failed,
            "kinds": report,
        });
        let text = serde_json::to_string_pretty(&summary)
            .map_err(|error| format!("cannot serialize report: {error}"))?;
        match output {
            Some(path) => std::fs::write(&path, text)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?,
            None => println!("{text}"),
        }
        eprintln!(
            "files scanned: {} ({} with parse errors)",
            audit.files_scanned, audit.files_failed
        );
        Ok(())
    };
    if let Err(error) = run() {
        eprintln!("pdx-flag-audit: {error}");
        std::process::exit(1);
    }
}
