//! Compatibility adapter for explicitly selected historical analyzer checkouts.
pub const SOURCE: &str = r###"
use engine::{AnalysisHost, DocumentId, IndexCache, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange};
use std::{fs, io::Write, path::Path};
use text::AbsPath;
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    INITIALISE_HOST
    let root = Path::new(&args[3]);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(SourceRootId::new(u32::MAX), SourceRootKind::Project, AbsPath::normalize(root))]));
    let cache = IndexCache::load(Path::new(&args[0])).expect("load cache");
    if cache.metadata().build_id == engine::ANALYZER_BUILD_ID {
        host.install_index_cache(cache).expect("install matching build cache");
    } else {
        // A standalone harness has its own compilation settings. Rebuild from the recorded
        // source instead of bypassing the production cache compatibility check.
        let vanilla = cache.source_root().clone();
        let expected_source = cache.metadata().source_fingerprint.clone();
        drop(cache);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![vanilla]));
        host.refresh_source_roots().expect("rebuild for the harness analyzer build");
        let rebuilt = IndexCache::from_snapshot(&host.snapshot()).expect("materialize rebuilt cache");
        assert_eq!(rebuilt.metadata().source_fingerprint, expected_source, "recorded corpus changed since the baseline");
        let selected = host.snapshot();
        host = AnalysisHost::with_ir(selected.rules().clone(), selected.game_profile().clone(), selected.ir_handle());
        drop(selected);
        host.apply_change(WorkspaceChange::SetSourceRoots(vec![
            SourceRoot::new(SourceRootId::new(u32::MAX), SourceRootKind::Project, AbsPath::normalize(root)),
        ]));
        host.install_index_cache(rebuilt).expect("install harness build cache");
        println!("cache_build_mismatch=reindexed-identical-recorded-source");
    }
    eprintln!("rule_hash={}", host.snapshot().rules().rule_hash().to_hex());
    let mut out = fs::File::create(&args[2]).unwrap();
    let mut details = fs::File::create(Path::new(&args[2]).with_extension("jsonl")).unwrap();
    for row in fs::read_to_string(&args[1]).unwrap().lines() {
        let parts = row.split('\t').collect::<Vec<_>>();
        let path = root.join(parts[1]);
        let id = DocumentId::new(format!("file://{}", path.display()));
        host.open_document(id.clone(), 1, fs::read_to_string(parts[3]).unwrap(), Some(AbsPath::normalize(&path))).unwrap();
        let items = ide::complete(&host.snapshot(), &id, parts[2].parse().unwrap()).items;
        writeln!(out, "{}\t#count\t{}", parts[0], items.len()).unwrap();
        for (ordinal, item) in items.iter().enumerate() {
            assert!(!item.label.contains(['\n', '\r', '\t']));
            writeln!(out, "{}\t{}\t{}", parts[0], ordinal, item.label).unwrap();
            writeln!(details, "{}", serde_json::json!({
                "probe": parts[0], "ordinal": ordinal, "label": item.label,
                "kind": format!("{:?}", item.kind), "detail": item.detail,
                "insert_text": item.insert_text,
                "replacement_range": [u32::from(item.replacement_range.start()), u32::from(item.replacement_range.end())],
            })).unwrap();
        }
        host.close_document(&id).unwrap();
    }
}
"###;
