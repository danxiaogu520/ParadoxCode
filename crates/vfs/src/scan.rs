//! Source-root discovery, bounded reads, and source identity helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use encoding_rs::WINDOWS_1252;
use rules::{GameProfile, SourceEncoding};
use text::{AbsPath, LogicalPath};

use crate::model::{
    SourceFile, SourceFileId, SourceRoot, SourceRootId, WorkspaceError, WorkspaceScanFilters,
    WorkspaceScanIssue, WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanReport,
    WorkspaceScanToken,
};

pub fn record_scan_issue(
    report: &mut WorkspaceScanReport,
    limits: WorkspaceScanLimits,
    kind: WorkspaceScanIssueKind,
    path: PathBuf,
    detail: String,
) {
    report.skipped_entries = report.skipped_entries.saturating_add(1);
    if report.issues.len() < limits.max_reported_issues {
        report
            .issues
            .push(WorkspaceScanIssue { kind, path, detail });
    } else {
        report.omitted_issues = report.omitted_issues.saturating_add(1);
    }
}

pub fn collect_whitelisted_files(
    root: &std::path::Path,
    profile: &GameProfile,
    filters: &WorkspaceScanFilters,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, AbsPath)>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    let root_metadata = fs::metadata(root).map_err(WorkspaceError::Io)?;
    if !root_metadata.is_dir() {
        return Err(WorkspaceError::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!(
                "workspace source root is not a directory: {}",
                root.display()
            ),
        )));
    }

    let mut roots = profile
        .scan_roots()
        .iter()
        .map(|scan_root| {
            LogicalPath::parse(scan_root)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(PathBuf::from(scan_root)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    roots.sort();
    roots.dedup();
    let mut collapsed_roots = Vec::with_capacity(roots.len());
    for scan_root in roots {
        if collapsed_roots.iter().any(|parent: &LogicalPath| {
            parent.as_str() == scan_root.as_str()
                || scan_root
                    .as_str()
                    .strip_prefix(parent.as_str())
                    .is_some_and(|remainder| {
                        let Some(remainder) = remainder.strip_prefix('/') else {
                            return false;
                        };
                        let distance = remainder
                            .split('/')
                            .filter(|component| !component.is_empty())
                            .count();
                        match (
                            profile.scan_root_max_depth(parent.as_str()),
                            profile.scan_root_max_depth(scan_root.as_str()),
                        ) {
                            (None, _) => true,
                            (Some(parent_max_depth), Some(child_max_depth)) => {
                                parent_max_depth >= distance.saturating_add(child_max_depth)
                            }
                            (Some(_), None) => false,
                        }
                    })
        }) {
            continue;
        }
        collapsed_roots.push(scan_root);
    }

    let mut seen = BTreeSet::new();
    let mut scan = DiskScanContext {
        limits,
        profile,
        filters,
        report,
        output,
        seen: &mut seen,
        cancellation,
    };
    for scan_root in collapsed_roots {
        scan.cancellation.checkpoint()?;
        if scan.filters.ignores_directory(scan_root.as_str()) {
            continue;
        }
        let depth = scan_root
            .as_str()
            .split('/')
            .filter(|component| !component.is_empty())
            .count();
        let current = if scan_root.as_str().is_empty() {
            root.to_owned()
        } else {
            root.join(scan_root.as_str())
        };
        if depth > limits.max_depth {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::DepthLimitExceeded,
                current,
                format!(
                    "whitelisted directory depth exceeds the configured limit of {}",
                    limits.max_depth
                ),
            );
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryUnreadable,
                    current,
                    error.to_string(),
                );
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::SymlinkSkipped,
                current,
                "symbolic links are not followed during workspace discovery".to_owned(),
            );
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        collect_disk_files(
            root,
            &current,
            depth,
            depth,
            profile.scan_root_max_depth(scan_root.as_str()),
            &mut scan,
        )?;
    }
    Ok(())
}

struct DiskScanContext<'a> {
    limits: WorkspaceScanLimits,
    profile: &'a GameProfile,
    filters: &'a WorkspaceScanFilters,
    report: &'a mut WorkspaceScanReport,
    output: &'a mut Vec<(LogicalPath, AbsPath)>,
    seen: &'a mut BTreeSet<LogicalPath>,
    cancellation: &'a WorkspaceScanToken,
}

fn collect_disk_files(
    root: &std::path::Path,
    current: &std::path::Path,
    depth: usize,
    root_depth: usize,
    root_max_relative_depth: Option<usize>,
    scan: &mut DiskScanContext<'_>,
) -> Result<(), WorkspaceError> {
    scan.cancellation.checkpoint()?;
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if depth == 0 => return Err(WorkspaceError::Io(error)),
        Err(error) => {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::DirectoryUnreadable,
                current.to_owned(),
                error.to_string(),
            );
            return Ok(());
        }
    };
    let mut entries = entries
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    current.to_owned(),
                    error.to_string(),
                );
                None
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        scan.cancellation.checkpoint()?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    path,
                    error.to_string(),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            record_scan_issue(
                scan.report,
                scan.limits,
                WorkspaceScanIssueKind::SymlinkSkipped,
                path,
                "symbolic links are not followed during workspace discovery".to_owned(),
            );
            continue;
        }
        if file_type.is_dir() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
                .to_string_lossy()
                .replace('\\', "/");
            if scan.filters.ignores_directory(&relative) {
                continue;
            }
            let relative_depth = depth.saturating_sub(root_depth);
            if root_max_relative_depth.is_some_and(|max_depth| relative_depth >= max_depth) {
                continue;
            }
            if ignored_workspace_directory(&entry.file_name()) {
                continue;
            }
            if depth >= scan.limits.max_depth {
                record_scan_issue(
                    scan.report,
                    scan.limits,
                    WorkspaceScanIssueKind::DepthLimitExceeded,
                    path,
                    format!(
                        "directory nesting exceeds the configured limit of {}",
                        scan.limits.max_depth
                    ),
                );
                continue;
            }
            collect_disk_files(
                root,
                &path,
                depth + 1,
                root_depth,
                root_max_relative_depth,
                scan,
            )?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        if scan.filters.ignores_file(&relative) {
            continue;
        }
        if scan.report.discovered_files >= scan.limits.max_files {
            return Err(WorkspaceError::FileLimitExceeded {
                limit: scan.limits.max_files,
            });
        }
        scan.report.discovered_files = scan.report.discovered_files.saturating_add(1);
        if !scan.profile.allows_scan_file(&relative) {
            continue;
        }
        let logical = LogicalPath::parse(&relative)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?;
        if !scan.seen.insert(logical.clone()) {
            continue;
        }
        scan.output.push((logical, AbsPath::normalize(&path)));
    }
    Ok(())
}

fn ignored_workspace_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".hg" | ".svn" | "node_modules" | "target")
    )
}

pub fn read_source_file(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    source_encoding: SourceEncoding,
) -> Option<String> {
    read_source_file_cancellable(
        path,
        limits,
        report,
        &WorkspaceScanToken::new(),
        source_encoding,
    )
    .ok()
    .flatten()
}

pub fn read_source_file_cancellable(
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    cancellation: &WorkspaceScanToken,
    source_encoding: SourceEncoding,
) -> Result<Option<String>, WorkspaceError> {
    cancellation.checkpoint()?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                path.to_owned(),
                error.to_string(),
            );
            return Ok(None);
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::MetadataUnreadable,
                path.to_owned(),
                error.to_string(),
            );
            return Ok(None);
        }
    };
    if !metadata.is_file() {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileUnreadable,
            path.to_owned(),
            "source path is not a regular file".to_owned(),
        );
        return Ok(None);
    }
    if metadata.len() > limits.max_file_size {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileTooLarge,
            path.to_owned(),
            format!(
                "file size {} exceeds the configured limit of {} bytes",
                metadata.len(),
                limits.max_file_size
            ),
        );
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    if let Err(error) = file
        .take(limits.max_file_size.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileUnreadable,
            path.to_owned(),
            error.to_string(),
        );
        return Ok(None);
    }
    cancellation.checkpoint()?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > limits.max_file_size) {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileTooLarge,
            path.to_owned(),
            format!(
                "file grew beyond the configured limit of {} bytes",
                limits.max_file_size
            ),
        );
        return Ok(None);
    }
    let mut legacy = false;
    let mut encoding_recovered = false;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let detail = error.to_string();
            let bytes = error.into_bytes();
            if source_encoding == SourceEncoding::Windows1252 && looks_like_legacy_text(&bytes) {
                let (text, had_errors) = WINDOWS_1252.decode_without_bom_handling(&bytes);
                legacy = true;
                encoding_recovered = had_errors;
                text.into_owned()
            } else if looks_like_legacy_text(&bytes) {
                encoding_recovered = true;
                String::from_utf8_lossy(&bytes).into_owned()
            } else {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::InvalidUtf8,
                    path.to_owned(),
                    detail,
                );
                return Ok(None);
            }
        }
    };
    let (text, sanitized) = sanitize_recovered_text(text);
    if sanitized {
        encoding_recovered = true;
    }
    if encoding_recovered {
        record_scan_notice(
            report,
            limits,
            WorkspaceScanIssueKind::EncodingRecovered,
            path.to_owned(),
            "one or more encoded source spans were replaced with whitespace; surrounding syntax was retained"
                .to_owned(),
        );
    }
    if legacy {
        report.legacy_encoded_files = report.legacy_encoded_files.saturating_add(1);
    }
    Ok(Some(text))
}

fn record_scan_notice(
    report: &mut WorkspaceScanReport,
    limits: WorkspaceScanLimits,
    kind: WorkspaceScanIssueKind,
    path: PathBuf,
    detail: String,
) {
    if report.issues.len() < limits.max_reported_issues {
        report
            .issues
            .push(WorkspaceScanIssue { kind, path, detail });
    } else {
        report.omitted_issues = report.omitted_issues.saturating_add(1);
    }
}

/// Replaces malformed game-encoded spans without discarding the containing source file.
///
/// EU4 stores some localised text in a game-specific byte encoding. After decoding with
/// replacement characters, the surrounding script/localisation structure is still useful to
/// the index. Quoted values are blanked as one token, comments are blanked to the end of their
/// line, and other malformed bare tokens are blanked up to a structural delimiter. Structural
/// braces are retained for bare tokens, while braces inside comments are blanked with the rest of
/// the comment so commented-out script cannot become active syntax.
fn sanitize_recovered_text(text: String) -> (String, bool) {
    let mut chars = text.chars().collect::<Vec<_>>();
    let mut bad = vec![false; chars.len()];
    let mut has_bad = false;
    let mut index = 0;
    while index < chars.len() {
        // EU4dll escape triples (marker U+0010..=U+0013 plus two payload characters)
        // are intentional transcoded content, not damage: consume them whole so the
        // preview layer can decode their values later. The escape set guarantees the
        // payload never contains structural characters (quotes, newline, '#'), so the
        // surrounding span analysis stays intact. An orphan marker at end of input
        // remains flagged as damage.
        if matches!(chars[index], '\u{0010}'..='\u{0013}') {
            if chars.len() - index >= 3 {
                index += 3;
                continue;
            }
            bad[index] = true;
            has_bad = true;
            index += 1;
            continue;
        }
        let character = chars[index];
        if character == '\u{fffd}'
            || (character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
        {
            bad[index] = true;
            has_bad = true;
        }
        index += 1;
    }
    if !has_bad {
        return (text, false);
    }

    let quote_spans = quoted_spans(&chars);
    let mut masked = vec![false; chars.len()];
    for (index, is_bad) in bad.iter().copied().enumerate() {
        if !is_bad || masked[index] {
            continue;
        }
        if let Some((start, end)) = quote_spans
            .iter()
            .copied()
            .find(|(start, end)| *start <= index && index < *end)
        {
            for slot in start.saturating_add(1)..end {
                if !matches!(chars[slot], '\r' | '\n') {
                    chars[slot] = ' ';
                }
                masked[slot] = true;
            }
            continue;
        }

        let line_start = index
            .checked_sub(1)
            .and_then(|slot| {
                chars[..=slot]
                    .iter()
                    .rposition(|character| *character == '\n')
            })
            .map_or(0, |slot| slot.saturating_add(1));
        let line_end = chars[index..]
            .iter()
            .position(|character| *character == '\n')
            .map_or(chars.len(), |offset| index.saturating_add(offset));
        let comment_start = chars[line_start..index]
            .iter()
            .position(|character| *character == '#')
            .map(|offset| line_start.saturating_add(offset));
        if let Some(comment_start) = comment_start {
            for slot in comment_start..line_end {
                if !matches!(chars[slot], '\r' | '\n') {
                    chars[slot] = ' ';
                }
                masked[slot] = true;
            }
            continue;
        }

        let start = chars[line_start..=index]
            .iter()
            .rposition(|character| {
                character.is_whitespace() || matches!(character, '=' | '{' | '}' | ':')
            })
            .map_or(line_start, |offset| {
                line_start.saturating_add(offset).saturating_add(1)
            });
        let end = chars[index..line_end]
            .iter()
            .position(|character| character.is_whitespace() || matches!(character, '{' | '}' | '#'))
            .map_or(line_end, |offset| index.saturating_add(offset));
        for slot in start..end {
            if !matches!(chars[slot], '\r' | '\n' | '{' | '}') {
                chars[slot] = ' ';
            }
            masked[slot] = true;
        }
    }
    (chars.into_iter().collect(), true)
}

fn quoted_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut opening = None;
    let mut escaped = false;
    let mut comment = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if let Some(start) = opening {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                spans.push((start, index));
                opening = None;
            }
        } else if comment {
            if character == '\n' {
                comment = false;
            }
        } else if character == '#' {
            comment = true;
        } else if character == '"' {
            opening = Some(index);
        }
    }
    if let Some(start) = opening {
        spans.push((start, chars.len()));
    }
    spans
}

fn looks_like_legacy_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0)
        && bytes
            .iter()
            .any(|byte| matches!(*byte, b'=' | b'{' | b'}' | b'#' | b'\n' | b':'))
}

pub fn stable_file_id(root: SourceRootId, logical: &LogicalPath) -> u64 {
    let mut value = 0xcbf29ce484222325_u64 ^ u64::from(root.get());
    for byte in logical.as_str().bytes() {
        value = (value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    value
}

/// Priority of one source root during overlay resolution.
///
/// Priorities come exclusively from the globally unique `order` assigned by the workspace
/// configuration (Vanilla 0, dependencies 1..n, Project n+1). The root kind is a layer
/// identity and never participates in priority arithmetic.
pub fn root_priority(root: &SourceRoot) -> u64 {
    u64::from(root.order)
}

pub fn source_priorities(
    roots: &[SourceRoot],
    files: &BTreeMap<SourceFileId, SourceFile>,
) -> BTreeMap<SourceFileId, u64> {
    files
        .values()
        .filter_map(|file| {
            roots
                .iter()
                .find(|root| root.id == file.root_id)
                .map(|root| (file.id, root_priority(root)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceScanFilterError;
    use rules::GameProfile;

    fn test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("vfs-scan-{label}-{nonce}"));
        fs::create_dir_all(&directory).expect("test directory");
        directory
    }

    fn profile_with_roots(scan_roots: &[&str], extensions: &[&str]) -> GameProfile {
        let mut profile = GameProfile::empty("scan-test");
        profile.scan_roots = scan_roots.iter().map(|root| (*root).to_owned()).collect();
        profile.scan_extensions = extensions
            .iter()
            .map(|extension| (*extension).to_owned())
            .collect();
        profile
    }

    fn scan_root_directory(
        root: &std::path::Path,
        profile: &GameProfile,
        filters: &WorkspaceScanFilters,
    ) -> Vec<String> {
        let mut output = Vec::new();
        let mut report = WorkspaceScanReport::default();
        collect_whitelisted_files(
            root,
            profile,
            filters,
            WorkspaceScanLimits::default(),
            &mut report,
            &mut output,
            &WorkspaceScanToken::new(),
        )
        .expect("scan fixture root");
        output
            .into_iter()
            .map(|(logical, _)| logical.as_str().to_owned())
            .collect()
    }

    #[test]
    fn scan_filter_validation_reports_each_rejection_with_its_limit() {
        let too_many = (0..=WorkspaceScanFilters::MAX_PATTERNS)
            .map(|index| format!("generated-{index}"))
            .collect::<Vec<_>>();
        for (kind, patterns) in [("file", too_many.clone()), ("directory", too_many)] {
            let error = WorkspaceScanFilters::new(
                if kind == "file" {
                    patterns.clone()
                } else {
                    Vec::new()
                },
                if kind == "file" {
                    Vec::new()
                } else {
                    patterns.clone()
                },
            )
            .expect_err("pattern count above the bound must fail");
            assert_eq!(
                error,
                WorkspaceScanFilterError::TooMany {
                    kind,
                    limit: WorkspaceScanFilters::MAX_PATTERNS,
                }
            );
            assert_eq!(
                error.to_string(),
                format!(
                    "too many {kind} ignore patterns (maximum {})",
                    WorkspaceScanFilters::MAX_PATTERNS
                )
            );
        }

        let too_long = vec!["x".repeat(WorkspaceScanFilters::MAX_PATTERN_LENGTH + 1)];
        let error = WorkspaceScanFilters::new(too_long, Vec::new())
            .expect_err("pattern length above the bound must fail");
        assert_eq!(
            error,
            WorkspaceScanFilterError::TooLong {
                kind: "file",
                limit: WorkspaceScanFilters::MAX_PATTERN_LENGTH,
            }
        );

        let error = WorkspaceScanFilters::new(Vec::new(), vec!["bad\0pattern".to_owned()])
            .expect_err("NUL inside a pattern must fail");
        assert_eq!(error, WorkspaceScanFilterError::Nul { kind: "directory" });

        // The exact bounds stay accepted.
        WorkspaceScanFilters::new(
            vec!["x".repeat(WorkspaceScanFilters::MAX_PATTERN_LENGTH)],
            Vec::new(),
        )
        .expect("pattern at the bounds is accepted");
    }

    #[test]
    fn scan_filter_patterns_are_normalized_and_deduplicated() {
        let filters = WorkspaceScanFilters::new(
            vec![
                "./events\\legacy.txt".to_owned(),
                "/events/legacy.txt".to_owned(),
                "events/legacy.txt".to_owned(),
                String::new(),
                "/".to_owned(),
            ],
            vec!["events\\generated\\".to_owned()],
        )
        .expect("valid patterns");
        assert_eq!(
            filters.ignore_file_patterns(),
            ["events/legacy.txt".to_owned()]
        );
        assert_eq!(
            filters.ignore_directory_patterns(),
            ["events/generated/".to_owned()]
        );
    }

    #[test]
    fn scan_filters_match_basenames_wildcards_and_ignored_parents() {
        let filters = WorkspaceScanFilters::new(
            vec!["generated.txt".to_owned(), "skip_?.tmp".to_owned()],
            vec!["vendor".to_owned()],
        )
        .expect("valid patterns");
        for (path, ignored) in [
            ("generated.txt", true),
            ("deep/nested/generated.txt", true),
            ("deep/nested/keep.txt", false),
            ("skip_a.tmp", true),
            ("deep/skip_1.tmp", true),
            ("skip_ab.tmp", false),
            ("vendor/anything.txt", true),
            ("deep/vendor/anything.txt", true),
        ] {
            assert_eq!(filters.ignores_file(path), ignored, "file path `{path}`");
        }
        // A separator-free directory pattern matches the directory's own basename;
        // deeper segments are only reachable through the parent walk in
        // `ignores_file`, which is how `deep/vendor/anything.txt` above is ignored.
        assert!(filters.ignores_directory("app/vendor"));
        assert!(!filters.ignores_directory("app/vendorkeep"));
        assert!(!filters.ignores_directory("app/vendor/cache"));
    }

    #[test]
    fn collect_whitelisted_files_selects_profile_roots_and_reports_depth_issues() {
        let root = test_directory("whitelist");
        fs::create_dir_all(root.join("events/deep/deeper")).expect("events tree");
        fs::create_dir_all(root.join("common/nested")).expect("common tree");
        fs::create_dir_all(root.join("outside")).expect("unlisted tree");
        fs::write(root.join("events/alpha.txt"), "key = 1\n").expect("alpha");
        fs::write(root.join("events/beta.md"), "# not indexed").expect("beta");
        fs::write(root.join("events/deep/deeper/delta.txt"), "key = 4\n").expect("delta");
        fs::write(root.join("common/nested/gamma.txt"), "key = 3\n").expect("gamma");
        fs::write(root.join("outside/omega.txt"), "key = 5\n").expect("omega");

        let mut profile = profile_with_roots(&["events", "common"], &["txt"]);
        profile.scan_root_max_depths.insert("common".to_owned(), 0);

        let mut output = Vec::new();
        let mut report = WorkspaceScanReport::default();
        let limits = WorkspaceScanLimits {
            max_depth: 2,
            ..Default::default()
        };
        collect_whitelisted_files(
            &root,
            &profile,
            &WorkspaceScanFilters::default(),
            limits,
            &mut report,
            &mut output,
            &WorkspaceScanToken::new(),
        )
        .expect("scan fixture root");

        let collected = output
            .into_iter()
            .map(|(logical, _)| logical.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(collected, ["events/alpha.txt".to_owned()]);
        // The outside root is never walked and beta.md fails the extension
        // whitelist; a subtree nested past the global depth limit is reported,
        // and a scan root with an explicit depth of zero prunes silently.
        assert_eq!(report.discovered_files, 2);
        assert_eq!(report.skipped_entries, 1);
        assert_eq!(
            report
                .issues
                .iter()
                .map(|issue| (
                    issue.kind,
                    issue.path.file_name().and_then(|name| name.to_str())
                ))
                .collect::<Vec<_>>(),
            [(WorkspaceScanIssueKind::DepthLimitExceeded, Some("deeper"))]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn collect_whitelisted_files_prunes_ignored_names_and_respects_the_file_budget() {
        let root = test_directory("prune");
        fs::create_dir_all(root.join(".git")).expect("git directory");
        fs::create_dir_all(root.join("vendor/pkg")).expect("vendor tree");
        fs::create_dir_all(root.join("events")).expect("events directory");
        fs::write(root.join(".git/tracked.txt"), "key = 1\n").expect("git file");
        fs::write(root.join("vendor/pkg/lib.txt"), "key = 2\n").expect("vendor file");
        fs::write(root.join("generated.txt"), "key = 3\n").expect("generated file");
        fs::write(root.join("events/alpha.txt"), "key = 4\n").expect("alpha");
        fs::write(root.join("events/beta.txt"), "key = 5\n").expect("beta");

        let profile = profile_with_roots(&[""], &["txt"]);
        let filters =
            WorkspaceScanFilters::new(vec!["generated.txt".to_owned()], vec!["vendor".to_owned()])
                .expect("valid patterns");

        let mut output = Vec::new();
        let mut report = WorkspaceScanReport::default();
        let limits = WorkspaceScanLimits {
            max_files: 1,
            ..Default::default()
        };
        let error = collect_whitelisted_files(
            &root,
            &profile,
            &filters,
            limits,
            &mut report,
            &mut output,
            &WorkspaceScanToken::new(),
        )
        .expect_err("second discovered file must exceed the budget");
        assert!(matches!(
            error,
            WorkspaceError::FileLimitExceeded { limit: 1 }
        ));
        assert_eq!(report.discovered_files, 1);

        // Without the artificial budget the same tree collects only the event
        // files: `.git`, the ignored directory, and the ignored file are pruned.
        let collected = scan_root_directory(&root, &profile, &filters);
        assert_eq!(
            collected,
            ["events/alpha.txt".to_owned(), "events/beta.txt".to_owned()]
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn collect_whitelisted_files_skips_symbolic_links_with_an_issue() {
        let root = test_directory("symlink");
        fs::create_dir_all(root.join("events")).expect("events directory");
        let outside = root.join("outside.txt");
        fs::write(&outside, "key = 1\n").expect("outside target");
        fs::write(root.join("events/alpha.txt"), "key = 2\n").expect("alpha");
        std::os::unix::fs::symlink(&outside, root.join("events/link.txt")).expect("symlink");

        let profile = profile_with_roots(&["events"], &["txt"]);
        let mut output = Vec::new();
        let mut report = WorkspaceScanReport::default();
        collect_whitelisted_files(
            &root,
            &profile,
            &WorkspaceScanFilters::default(),
            WorkspaceScanLimits::default(),
            &mut report,
            &mut output,
            &WorkspaceScanToken::new(),
        )
        .expect("scan fixture root");

        let collected = output
            .into_iter()
            .map(|(logical, _)| logical.as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(collected, ["events/alpha.txt".to_owned()]);
        assert!(report.issues.iter().any(|issue| {
            issue.kind == WorkspaceScanIssueKind::SymlinkSkipped
                && issue.path.file_name().and_then(|name| name.to_str()) == Some("link.txt")
        }));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn collect_whitelisted_files_rejects_missing_roots_and_cancellation() {
        let root = test_directory("rejections");
        fs::write(root.join("file.txt"), "key = 1\n").expect("regular file");
        let profile = profile_with_roots(&["events"], &["txt"]);

        let error = collect_whitelisted_files(
            &root.join("file.txt"),
            &profile,
            &WorkspaceScanFilters::default(),
            WorkspaceScanLimits::default(),
            &mut WorkspaceScanReport::default(),
            &mut Vec::new(),
            &WorkspaceScanToken::new(),
        )
        .expect_err("a regular file is not a source root");
        assert!(
            matches!(error, WorkspaceError::Io(error) if error.kind() == std::io::ErrorKind::NotADirectory)
        );

        let cancelled = WorkspaceScanToken::new();
        cancelled.cancel();
        let error = collect_whitelisted_files(
            &root,
            &profile,
            &WorkspaceScanFilters::default(),
            WorkspaceScanLimits::default(),
            &mut WorkspaceScanReport::default(),
            &mut Vec::new(),
            &cancelled,
        )
        .expect_err("a cancelled token stops the scan");
        assert_eq!(error.to_string(), "workspace scan was cancelled");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
