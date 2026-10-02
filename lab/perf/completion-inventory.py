#!/usr/bin/env python3
"""Export fixed-probe IDE completions before the LSP's result cap.

Builds a temporary harness against the selected checkout without modifying its
source. Corpus-derived inventories must remain in performance-results.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


RUST = r'''
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
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--mode', choices=('legacy', 'ir'), required=True)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    repo = args.repo.resolve()
    output = args.output.resolve()
    workspace = Path(__file__).resolve().parents[2]
    output.relative_to(workspace / 'performance-results')
    output.mkdir(parents=True, exist_ok=True)
    baseline = json.loads(args.baseline.read_text())
    assert baseline['metadata']['status'] == 'passed'
    with tempfile.TemporaryDirectory(prefix='pdc-completion-inventory-') as temporary:
        root = Path(temporary)
        fixtures = []
        for probe in baseline['completions']:
            lines = (workspace / 'crates/ide/src/tests/golden' / probe['golden_file']).read_text().splitlines()
            begin = lines.index('// input:') + 1
            source = []
            for line in lines[begin:]:
                if not line.startswith('//   '):
                    break
                source.append(line[5:])
            source = '\n'.join(source)
            if probe.get('source_sha256') and hashlib.sha256(source.encode()).hexdigest() != probe['source_sha256']:
                raise ValueError(f"golden input differs from the frozen LSP probe: {probe['id']}")
            source += '\n'
            source_lines = source.splitlines(keepends=True)
            line = probe['position']['line']
            character = probe['position']['character']
            prefix = source_lines[line].encode('utf-16-le')[:character * 2].decode('utf-16-le')
            offset = len((''.join(source_lines[:line]) + prefix).encode())
            path = root / (probe['id'] + '.txt')
            path.write_text(source)
            fixtures.append('\t'.join((probe['id'], probe['document_path'], str(offset), str(path))))
        manifest = root / 'fixtures.tsv'
        manifest.write_text('\n'.join(fixtures) + '\n')
        dependencies = '\n'.join(f'{name} = {{ path = {json.dumps(str(repo / "crates" / name))} }}' for name in ('engine', 'ide', 'game', 'text', 'rules'))
        (root / 'Cargo.toml').write_text('[package]\nname="pdc-completion-inventory"\nversion="0.0.0"\nedition="2024"\n[dependencies]\n' + dependencies + '\nserde_json="1"\n')
        (root / 'src').mkdir()
        initializer = ('let ir = game::eu4::first_party_ir().unwrap(); let mut host = AnalysisHost::with_ir(rules::RuleSet::from_ir_catalog(&ir), ir.game.profile.clone(), ir);'
                       if args.mode == 'ir' else 'let mut host = AnalysisHost::with_profile(game::eu4::first_party_rules().unwrap(), game::eu4::profile());')
        harness = RUST.replace('INITIALISE_HOST', initializer)
        if 'ANALYZER_BUILD_ID' not in (repo / 'crates/engine/src/lib.rs').read_text():
            # Historical comparison checkouts predate build stamps and retain their own API.
            begin = harness.index('    let cache = IndexCache::load(')
            end = harness.index('    eprintln!("rule_hash=', begin)
            harness = harness[:begin] + '    host.install_index_cache(IndexCache::load(Path::new(&args[0])).expect("load cache")).expect("install matching cache");\n' + harness[end:]
        (root / 'src/main.rs').write_text(harness)
        cargo = ['cargo', 'build', '--offline', '--release', '--manifest-path', str(root / 'Cargo.toml'), '--target-dir', str(repo / 'target')]
        with (output / 'build.log').open('w') as log:
            subprocess.run(cargo, stdout=log, stderr=subprocess.STDOUT, check=True)
        binary = repo / 'target/release/pdc-completion-inventory'
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        overlays = root / 'project'
        overlays.mkdir()
        with (output / 'run.log').open('w') as log:
            subprocess.run([str(binary), str(args.cache.resolve()), str(manifest), str(output / 'items.tsv'), str(overlays)], stdout=log, stderr=subprocess.STDOUT, check=True)
        identity = 'rule_hash=' + baseline['metadata']['rules']['manifest_rule_hash']
        if identity not in (output / 'run.log').read_text().splitlines():
            raise ValueError('harness/baseline rule identity mismatch')
    probes = {item['id']: {**item, 'full_labels': [], 'full_candidates': []} for item in baseline['completions']}
    counts = {}
    for row in (output / 'items.tsv').read_text().splitlines():
        probe, ordinal, label = row.split('\t')
        if ordinal == '#count':
            if probe in counts:
                raise ValueError(f'duplicate probe: {probe}')
            counts[probe] = int(label)
        else:
            if int(ordinal) != len(probes[probe]['full_labels']):
                raise ValueError(f'incomplete inventory: {probe}')
            probes[probe]['full_labels'].append(label)
    for row in (output / 'items.jsonl').read_text().splitlines():
        item = json.loads(row)
        probe = probes[item.pop('probe')]
        if item['ordinal'] != len(probe['full_candidates']):
            raise ValueError(f"incomplete candidate detail inventory: {probe['id']}")
        probe['full_candidates'].append(item)
    for probe in probes.values():
        if counts.get(probe['id']) != len(probe['full_labels']):
            raise ValueError(f'truncated inventory: {probe["id"]}')
        if [item['label'] for item in probe['full_candidates']] != probe['full_labels']:
            raise ValueError(f"candidate details differ from labels: {probe['id']}")
        expected = set(probe['labels'])
        actual = set(probe['full_labels'][:512])
        if expected != actual:
            raise ValueError(f"harness/LSP mismatch for {probe['id']}: added {sorted(actual - expected)[:8]}, removed {sorted(expected - actual)[:8]}")
        probe['full_count'] = len(probe['full_labels'])
        probe['harness_matches_bounded_lsp'] = True
    result = {'status': 'exported-unbounded-ide-inventory', 'mode': args.mode, 'repo': str(repo), 'binary_sha256': digest,
              'rules': baseline['metadata']['rules'], 'probes': list(probes.values())}
    (output / 'inventory.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print('Exported', len(probes), 'complete IDE inventories; each first 512 matches the recorded LSP response.')


if __name__ == '__main__':
    main()
