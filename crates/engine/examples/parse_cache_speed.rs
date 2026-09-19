//! Local experiment: transient reparse vs persistent parse cache.
//!
//! Runs over a directory of script files, timing (a) parse + lower,
//! (b) parse-cache load + lower, (c) cache load alone. Prints totals so the
//! "serve validation reparses from the parse cache" question can be answered
//! with numbers. Not wired into any gate; run with
//! `cargo run -p engine --release --example parse_cache_speed -- <dir>`.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: parse_cache_speed <script-dir>");
    let cache_root = std::env::temp_dir().join("pdc-parse-speed-cache");
    let _ = std::fs::remove_dir_all(&cache_root);
    let parse_cache = vfs::ParseCache::new(&cache_root);

    let rules = game::eu4::first_party_rules().expect("embedded rules");
    let profile = game::eu4::profile();

    let mut files: Vec<(PathBuf, String)> = Vec::new();
    collect_scripts(std::path::Path::new(&dir), &mut files);
    files.sort();
    println!("files: {}", files.len());

    let logical = text::LogicalPath::parse("probe.txt").expect("logical path");
    // Pass 0: populate the parse cache the way a scan would.
    let started = Instant::now();
    for (path, source) in &files {
        let (parsed, _) = index::parse_source(
            &rules::ParserKind::Script,
            source,
            Some(&logical),
            &rules,
            &profile,
        );
        let Some(engine::ParsedSource::Text(parsed)) = parsed else {
            continue;
        };
        let _ = parse_cache.store(
            &vfs::SourceFile {
                id: vfs::SourceFileId::new(0),
                root_id: vfs::SourceRootId::new(0),
                physical_path: text::AbsPath::normalize(path),
                logical_path: text::LogicalPath::parse("probe.txt").expect("logical path"),
                category_id: None,
                resolution: rules::FileResolutionPolicy::Merge,
            },
            parsed.format(),
            source,
            &parsed,
        );
    }
    println!("populate store: {:.0} ms", started.elapsed().as_millis());

    let started = Instant::now();
    let mut parse_lower_bytes = 0usize;
    for (_path, source) in &files {
        let (parsed, _) = index::parse_source(
            &rules::ParserKind::Script,
            source,
            Some(&logical),
            &rules,
            &profile,
        );
        let Some(engine::ParsedSource::Text(parsed)) = parsed else {
            continue;
        };
        parse_lower_bytes += source.len();
        let hir = hir::lower_with_profile((*parsed).clone(), &logical, &rules, &profile);
        std::hint::black_box((&parsed, &hir));
    }
    let parse_lower = started.elapsed();

    let mut cache_hits = 0usize;
    let mut cache_lower = 0usize;
    let mut load_only = 0usize;
    for (path, source) in &files {
        let file = vfs::SourceFile {
            id: vfs::SourceFileId::new(0),
            root_id: vfs::SourceRootId::new(0),
            physical_path: text::AbsPath::normalize(path),
            logical_path: text::LogicalPath::parse("probe.txt").expect("logical path"),
            category_id: None,
            resolution: rules::FileResolutionPolicy::Merge,
        };
        let (parsed, _) = index::parse_source(
            &rules::ParserKind::Script,
            source,
            Some(&logical),
            &rules,
            &profile,
        );
        let Some(engine::ParsedSource::Text(parsed)) = parsed else {
            continue;
        };
        let format = parsed.format();
        drop(parsed);
        let t = Instant::now();
        let cached = parse_cache.load(&file, format, source);
        load_only += t.elapsed().as_micros() as usize;
        let Some(cached) = cached else { continue };
        cache_hits += 1;
        let t = Instant::now();
        let hir = hir::lower_with_profile(cached, &logical, &rules, &profile);
        cache_lower += t.elapsed().as_micros() as usize;
        std::hint::black_box(&hir);
    }
    let _ = parse_lower_bytes;
    let _ = cache_hits;

    println!("parse+lower:  {:.0} ms total", parse_lower.as_millis());
    println!(
        "cache:        {} hits, load {:.0} ms + lower {:.0} ms",
        cache_hits,
        load_only as f64 / 1000.0,
        cache_lower as f64 / 1000.0
    );
    let _ = std::fs::remove_dir_all(&cache_root);
}

fn collect_scripts(dir: &std::path::Path, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_scripts(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "txt")
            && let Ok(source) = std::fs::read_to_string(&path)
        {
            out.push((path, source));
        }
    }
}
