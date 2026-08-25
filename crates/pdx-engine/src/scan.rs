//! Source-root discovery, bounded reads, and source identity helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use encoding_rs::WINDOWS_1252;
use pdx_rules::{GameProfile, SourceEncoding};
use pdx_text::LogicalPath;

use crate::model::{
    SourceFile, SourceFileId, SourceRoot, SourceRootId, WorkspaceError, WorkspaceScanIssue,
    WorkspaceScanIssueKind, WorkspaceScanLimits, WorkspaceScanReport, WorkspaceScanToken,
};

pub(crate) fn record_scan_issue(
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

pub(crate) fn collect_whitelisted_files(
    root: &std::path::Path,
    profile: &GameProfile,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, PathBuf)>,
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
        report,
        output,
        seen: &mut seen,
        cancellation,
    };
    for scan_root in collapsed_roots {
        scan.cancellation.checkpoint()?;
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
    report: &'a mut WorkspaceScanReport,
    output: &'a mut Vec<(LogicalPath, PathBuf)>,
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
        if scan.report.discovered_files >= scan.limits.max_files {
            return Err(WorkspaceError::FileLimitExceeded {
                limit: scan.limits.max_files,
            });
        }
        scan.report.discovered_files = scan.report.discovered_files.saturating_add(1);
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !scan.profile.allows_scan_file(&relative) {
            continue;
        }
        let logical = LogicalPath::parse(&relative)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?;
        if !scan.seen.insert(logical.clone()) {
            continue;
        }
        scan.output.push((logical, path));
    }
    Ok(())
}

fn ignored_workspace_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".hg" | ".svn" | "node_modules" | "target")
    )
}

pub(crate) fn read_source_file(
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

pub(crate) fn read_source_file_cancellable(
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
/// line, and other malformed bare tokens are blanked up to a structural delimiter. Braces and
/// line endings are retained, so an enclosing definition and its siblings remain parseable.
fn sanitize_recovered_text(text: String) -> (String, bool) {
    let mut chars = text.chars().collect::<Vec<_>>();
    let mut bad = vec![false; chars.len()];
    let mut has_bad = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if character == '\u{fffd}'
            || (character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
        {
            bad[index] = true;
            has_bad = true;
        }
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
        let (start, end) = if let Some(comment_start) = comment_start {
            (comment_start, line_end)
        } else {
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
                .position(|character| {
                    character.is_whitespace() || matches!(character, '{' | '}' | '#')
                })
                .map_or(line_end, |offset| index.saturating_add(offset));
            (start, end)
        };
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

pub(crate) fn stable_file_id(root: SourceRootId, logical: &LogicalPath) -> u64 {
    let mut value = 0xcbf29ce484222325_u64 ^ u64::from(root.get());
    for byte in logical.as_str().bytes() {
        value = (value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    value
}

/// Priority of one source root during overlay resolution.
///
/// Priorities come exclusively from the globally unique `order` assigned by the workspace
/// configuration (Vanilla 0, dependencies 1..n, Current Mod n+1). The root kind is a layer
/// identity and never participates in priority arithmetic.
pub(crate) fn root_priority(root: &SourceRoot) -> u64 {
    u64::from(root.order)
}

pub(crate) fn source_priorities(
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
