//! Memory-attribution probe for a full Current-Mod scan.
//!
//! Loads the embedded EU4 rules, scans one workspace root, and prints the
//! retained size of every per-file component (source text, CST nodes and
//! tokens, HIR collections, index shards, cached positions and previews) so
//! memory work targets the real hot spot. Not part of any test gate.
//!
//! Usage: `cargo run --release -p pdx-engine --example mem_probe -- <mod root>`

use std::mem::size_of;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use pdx_engine::{
    AnalysisHost, ParsedSource, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};

fn walk_cst(node: &pdx_parser::CstNode, stats: &mut (usize, usize)) {
    stats.0 += 1;
    stats.1 += node.children().len();
    for child in node.children() {
        walk_cst(child, stats);
    }
}

fn mib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0)
}

fn phase_rss(label: &str) {
    // External sampler watches stdout; hold the process still briefly so the
    // sample lands after the phase completes.
    println!("PHASE:{label}");
    std::thread::sleep(std::time::Duration::from_secs(4));
}

fn main() {
    let root_arg = std::env::args()
        .nth(1)
        .expect("usage: mem_probe <mod root>");
    let root = Path::new(&root_arg)
        .canonicalize()
        .expect("canonicalize root");
    let started = Instant::now();

    let rules = pdx_game::eu4::first_party_rules().expect("rules");
    let profile = pdx_game::eu4::profile();
    let mut host = AnalysisHost::with_profile(rules, profile);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::CurrentMod,
        root,
    )]));
    host.refresh_source_roots().expect("scan");
    let scan_seconds = started.elapsed().as_secs_f64();

    let snapshot = host.snapshot();
    let mut source_bytes = 0usize;
    let mut cst_nodes = 0usize;
    let mut cst_child_slots = 0usize;
    let mut token_count = 0usize;
    let mut hir_properties = 0usize;
    let mut hir_path_strings = 0usize;
    let mut hir_path_string_bytes = 0usize;
    let mut hir_key_bytes = 0usize;
    let mut hir_scalar_bytes = 0usize;
    let mut hir_definitions = 0usize;
    let mut hir_references = 0usize;
    let mut hir_definition_bytes = 0usize;
    let mut hir_reference_bytes = 0usize;
    let mut shard_definitions = 0usize;
    let mut shard_references = 0usize;
    let cached_positions = 0usize;
    let cached_previews = 0usize;
    let mut files = 0usize;
    let mut files_with_frontend = 0usize;

    for file in snapshot.source_files().values() {
        let Some(state) = snapshot.file_state(file.id) else {
            continue;
        };
        files += 1;
        source_bytes += state.source().len();
        if let Some(ParsedSource::Text(parsed)) = state.parsed() {
            files_with_frontend += 1;
            let mut stats = (0usize, 0usize);
            walk_cst(parsed.root(), &mut stats);
            cst_nodes += stats.0;
            cst_child_slots += stats.1;
            token_count += parsed.tokens().len();
        }
        if let Some(hir) = state.hir() {
            for property in hir.properties() {
                hir_properties += 1;
                hir_path_strings += property.path.len();
                hir_path_string_bytes += property
                    .path
                    .iter()
                    .map(|segment| segment.len() + 1 + size_of::<String>())
                    .sum::<usize>();
                hir_key_bytes += property.key.len() + 1 + size_of::<String>();
                if let Some(scalar) = &property.scalar {
                    hir_scalar_bytes += scalar.value.len() + 1 + size_of::<String>();
                }
            }
            for definition in hir.definitions() {
                hir_definitions += 1;
                hir_definition_bytes +=
                    definition.kind.len() + definition.name.len() + 2 + 2 * size_of::<String>();
            }
            for reference in hir.references() {
                hir_references += 1;
                hir_reference_bytes +=
                    reference.kind.len() + reference.name.len() + 2 + 2 * size_of::<String>();
            }
        }
        shard_definitions += state.shard().definitions.len();
        shard_references += state.shard().references.len();
    }
    let position_ranges = snapshot.index().position_ranges().len();

    let node_header = size_of::<pdx_parser::CstNode>();
    // Every non-leaf node owns one heap Vec: buffer (cap*8B, rounded by the
    // allocator to at least 16B) plus allocator overhead (~16B).
    let cst_vec_bytes = cst_child_slots * 8 + (cst_nodes - token_count.min(cst_nodes)) * 16;
    let hir_property_header =
        hir_properties * (size_of::<HirPropertyHeaderProxy>() + size_of::<Vec<String>>());

    println!("files: {files} (frontend retained: {files_with_frontend})");
    println!("scan wall: {scan_seconds:.1}s");
    println!("--- retained bytes (approximate) ---");
    println!("source text: {:.0} MiB", mib(source_bytes as f64));
    println!(
        "cst: {} nodes x {node_header}B = {:.0} MiB headers + ~{:.0} MiB vec buffers; tokens {} x 8B = {:.0} MiB",
        cst_nodes,
        mib((cst_nodes * node_header) as f64),
        mib(cst_vec_bytes as f64),
        token_count,
        mib((token_count * 8) as f64),
    );
    println!(
        "hir properties: {} headers ~{:.0} MiB; keys {:.0} MiB; scalars {:.0} MiB",
        hir_properties,
        mib(hir_property_header as f64),
        mib(hir_key_bytes as f64),
        mib(hir_scalar_bytes as f64),
    );
    println!(
        "hir paths: {} strings = {:.0} MiB (strings+headers)",
        hir_path_strings,
        mib(hir_path_string_bytes as f64)
    );
    println!(
        "hir definitions: {} = {:.0} MiB; references: {} = {:.0} MiB",
        hir_definitions,
        mib(hir_definition_bytes as f64),
        hir_references,
        mib(hir_reference_bytes as f64),
    );
    println!(
        "shard definitions: {}, references: {}",
        shard_definitions, shard_references
    );
    println!(
        "index position ranges: {} x ~64B = {:.0} MiB",
        position_ranges,
        mib((position_ranges * 64) as f64)
    );
    println!("cached positions: {cached_positions}, previews: {cached_previews}");

    // Phase timing over the same corpus: read, parse, lower. The remainder of
    // the scan cost is shard building, line indexes, and position extraction.
    let mut read_ns = 0u128;
    let mut parse_ns = 0u128;
    let mut lower_ns = 0u128;
    let mut phase_files = 0usize;
    for file in snapshot.source_files().values() {
        snapshot.file_state(file.id).expect("state");
        let logical = &file.logical_path;
        let Some(category) = snapshot.rules().classify(logical) else {
            continue;
        };
        let format = match category.parser {
            pdx_rules::ParserKind::Script => pdx_parser::FileFormat::Script,
            pdx_rules::ParserKind::Localisation => pdx_parser::FileFormat::Localisation,
            _ => continue,
        };
        let started = std::time::Instant::now();
        let Ok(bytes) = std::fs::read(&file.physical_path) else {
            continue;
        };
        read_ns += started.elapsed().as_nanos();
        let source: Arc<str> = Arc::from(String::from_utf8_lossy(&bytes).as_ref());
        let started = std::time::Instant::now();
        let parsed = Arc::new(pdx_parser::parse(format, &source));
        parse_ns += started.elapsed().as_nanos();
        let started = std::time::Instant::now();
        let hir = pdx_engine::hir::lower_with_profile(
            (*parsed).clone(),
            logical,
            snapshot.rules(),
            snapshot.game_profile(),
        );
        lower_ns += started.elapsed().as_nanos();
        phase_files += 1;
        drop(hir);
    }
    println!("--- phase timing over {phase_files} files ---");
    println!("read:  {:.1}s", read_ns as f64 / 1e9);
    println!("parse: {:.1}s", parse_ns as f64 / 1e9);
    println!("lower: {:.1}s", lower_ns as f64 / 1e9);

    // Post-scan phases: eviction and vanilla install, sampled externally.
    drop(snapshot);
    phase_rss("scan-retained");

    // Evict all frontends and report the post-eviction resident set.
    let evicted = host.evict_source_frontends(&|_| false);
    println!("evicted frontends: {evicted}");
    phase_rss("evicted");

    // Install the vanilla index cache like the LSP does and report the delta.
    let cache_path = std::env::var("LOCALAPPDATA")
        .map(|root| std::path::PathBuf::from(root).join("ParadoxCode/cache/eu4/vanilla.pdxindex"))
        .expect("LOCALAPPDATA");
    match pdx_engine::IndexCache::load(&cache_path) {
        Ok(cache) => {
            host.install_index_cache(cache).expect("install cache");
            phase_rss("vanilla-installed");
        }
        Err(error) => println!("cache load failed: {error}"),
    }
    phase_rss("end");
}

/// Placeholder sized like the non-String payload of one HIR property.
#[repr(C)]
struct HirPropertyHeaderProxy {
    a: u64,
    b: u64,
    c: u64,
    d: u32,
    e: u32,
    f: u64,
    g: u64,
}
