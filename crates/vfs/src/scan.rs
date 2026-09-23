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
    if !profile.scan_archive_roots.is_empty() {
        collect_archive_files(
            root,
            profile,
            filters,
            limits,
            report,
            output,
            &mut seen,
            cancellation,
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

/// Supplements discovery with read-only archive tiers (see
/// [`GameProfile::scan_archive_roots`]). Extracted archive directories are walked like any
/// other directory, and every `*.zip` inside a tier is opened so its matching entries join
/// discovery as virtual files addressed as `<zip path>!<entry path>`. Entries already
/// discovered by the main pass keep precedence, so a tier can only add names the extracted
/// game data does not carry.
#[allow(clippy::too_many_arguments)]
fn collect_archive_files(
    root: &std::path::Path,
    profile: &GameProfile,
    filters: &WorkspaceScanFilters,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, AbsPath)>,
    seen: &mut BTreeSet<LogicalPath>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    for archive_root in &profile.scan_archive_roots {
        cancellation.checkpoint()?;
        let current = root.join(archive_root);
        if !current.is_dir() {
            continue;
        }
        walk_archive_tier(
            root,
            &current,
            0,
            profile,
            filters,
            limits,
            report,
            output,
            seen,
            cancellation,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn walk_archive_tier(
    root: &std::path::Path,
    current: &std::path::Path,
    depth: usize,
    profile: &GameProfile,
    filters: &WorkspaceScanFilters,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, AbsPath)>,
    seen: &mut BTreeSet<LogicalPath>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    cancellation.checkpoint()?;
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::DirectoryUnreadable,
                current.to_owned(),
                error.to_string(),
            );
            return Ok(());
        }
    };
    let mut entries = entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        cancellation.checkpoint()?;
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::DirectoryEntryUnreadable,
                    path.clone(),
                    error.to_string(),
                );
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if depth >= limits.max_depth {
                record_scan_issue(
                    report,
                    limits,
                    WorkspaceScanIssueKind::DepthLimitExceeded,
                    path,
                    format!(
                        "archive tier nesting exceeds the configured limit of {}",
                        limits.max_depth
                    ),
                );
                continue;
            }
            walk_archive_tier(
                root,
                &path,
                depth + 1,
                profile,
                filters,
                limits,
                report,
                output,
                seen,
                cancellation,
            )?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            collect_zip_entries(
                &path,
                profile,
                filters,
                limits,
                report,
                output,
                seen,
                cancellation,
            )?;
            continue;
        }
        // Extracted tiers keep the game layout one pack directory below the tier, so
        // `builtin_dlc/<pack>/interface/foo.gfx` surfaces as `interface/foo.gfx`.
        let Some(relative) = path
            .strip_prefix(root)
            .ok()
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        else {
            continue;
        };
        if !archive_entry_has_allowed_extension(&relative, profile) {
            continue;
        }
        let Some(logical) = archive_flattened_logical_path(&relative) else {
            continue;
        };
        push_archive_entry(
            logical,
            AbsPath::normalize(&path),
            filters,
            limits,
            report,
            output,
            seen,
        );
    }
    Ok(())
}

/// Enumerates the matching entries of one zip archive and adds them as virtual files.
#[allow(clippy::too_many_arguments)]
fn collect_zip_entries(
    zip_path: &std::path::Path,
    profile: &GameProfile,
    filters: &WorkspaceScanFilters,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, AbsPath)>,
    seen: &mut BTreeSet<LogicalPath>,
    cancellation: &WorkspaceScanToken,
) -> Result<(), WorkspaceError> {
    let file = match fs::File::open(zip_path) {
        Ok(file) => file,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                zip_path.to_owned(),
                error.to_string(),
            );
            return Ok(());
        }
    };
    let archive = match zip::ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                zip_path.to_owned(),
                format!("zip archive could not be opened: {error}"),
            );
            return Ok(());
        }
    };
    // Zip payloads already use the game-data layout (`interface/foo.gfx`), so entry
    // names map to logical paths verbatim.
    let mut names: Vec<String> = archive.file_names().map(ToOwned::to_owned).collect();
    names.sort();
    for name in names {
        cancellation.checkpoint()?;
        if name.ends_with('/') || !archive_entry_has_allowed_extension(&name, profile) {
            continue;
        }
        let Ok(logical) = LogicalPath::parse(&name) else {
            continue;
        };
        let virtual_path = PathBuf::from(format!("{}!{}", zip_path.display(), name));
        push_archive_entry(
            logical,
            AbsPath::normalize(&virtual_path),
            filters,
            limits,
            report,
            output,
            seen,
        );
    }
    Ok(())
}

/// Strips the tier directory plus one content-pack segment from an extracted tier path:
/// `builtin_dlc/<pack>/interface/foo.gfx` -> `interface/foo.gfx`.
fn archive_flattened_logical_path(relative: &str) -> Option<LogicalPath> {
    let mut segments = relative.split('/');
    segments.next()?;
    segments.next()?;
    let flattened = segments.collect::<Vec<_>>().join("/");
    LogicalPath::parse(&flattened).ok()
}

fn archive_entry_has_allowed_extension(name: &str, profile: &GameProfile) -> bool {
    let Some((_, extension)) = name
        .rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.'))
    else {
        return false;
    };
    profile
        .scan_archive_extensions
        .iter()
        .any(|allowed| extension.eq_ignore_ascii_case(allowed.strip_prefix('.').unwrap_or(allowed)))
}

fn push_archive_entry(
    logical: LogicalPath,
    physical: AbsPath,
    filters: &WorkspaceScanFilters,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    output: &mut Vec<(LogicalPath, AbsPath)>,
    seen: &mut BTreeSet<LogicalPath>,
) {
    if filters.ignores_file(logical.as_str()) {
        return;
    }
    // Tiers are supplementary: once the discovery budget is spent, stop adding instead
    // of failing the whole scan.
    if report.discovered_files >= limits.max_files {
        return;
    }
    if !seen.insert(logical.clone()) {
        return;
    }
    report.discovered_files = report.discovered_files.saturating_add(1);
    output.push((logical, physical));
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
    if let Some((zip_path, entry)) = split_archive_path(path) {
        return read_archive_entry(
            &zip_path,
            &entry,
            limits,
            report,
            cancellation,
            source_encoding,
        );
    }
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
    Ok(decode_source_bytes(
        &bytes,
        source_encoding,
        path,
        limits,
        report,
    ))
}

/// Decodes raw source bytes into scan text, recording the same recovery notices for
/// archive entries as for disk files.
fn decode_source_bytes(
    bytes: &[u8],
    source_encoding: SourceEncoding,
    path: &std::path::Path,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
) -> Option<String> {
    let mut legacy = false;
    let mut encoding_recovered = false;
    let text = match String::from_utf8(bytes.to_vec()) {
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
                return None;
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
    Some(text)
}

/// Splits a virtual archive path (`<zip path>!<entry path>`) back into its parts.
///
/// The separator is matched from the right so a `!` inside a directory name of the
/// archive location cannot be mistaken for the split point; archive entry names come
/// from the game data and never contain it.
pub fn split_archive_path(path: &std::path::Path) -> Option<(PathBuf, String)> {
    let text = path.to_str()?;
    let (zip_path, entry) = text.rsplit_once('!')?;
    if zip_path.is_empty() || entry.is_empty() {
        return None;
    }
    Some((PathBuf::from(zip_path), entry.to_owned()))
}

/// Reads the raw bytes of one zip entry addressed through a virtual scan path.
///
/// The read is bounded by `max_bytes`; a missing archive or entry, an unreadable
/// entry, or one that exceeds the bound all yield `None` so callers can treat an
/// archive member like any other unresolvable file reference.
#[must_use]
pub fn read_archive_entry_bytes(
    zip_path: &std::path::Path,
    entry: &str,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry_file = archive.by_name(entry).ok()?;
    let mut bytes = Vec::new();
    Read::take(&mut entry_file, max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if u64::try_from(bytes.len()).map_or(true, |size| size > max_bytes) {
        return None;
    }
    Some(bytes)
}

/// Reads one entry out of a zip archive addressed through a virtual scan path.
fn read_archive_entry(
    zip_path: &std::path::Path,
    entry: &str,
    limits: WorkspaceScanLimits,
    report: &mut WorkspaceScanReport,
    cancellation: &WorkspaceScanToken,
    source_encoding: SourceEncoding,
) -> Result<Option<String>, WorkspaceError> {
    let file = match fs::File::open(zip_path) {
        Ok(file) => file,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                zip_path.to_owned(),
                error.to_string(),
            );
            return Ok(None);
        }
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                zip_path.to_owned(),
                format!("zip archive could not be opened: {error}"),
            );
            return Ok(None);
        }
    };
    let mut entry_file = match archive.by_name(entry) {
        Ok(entry_file) => entry_file,
        Err(error) => {
            record_scan_issue(
                report,
                limits,
                WorkspaceScanIssueKind::FileUnreadable,
                PathBuf::from(format!("{}!{}", zip_path.display(), entry)),
                format!("zip entry could not be read: {error}"),
            );
            return Ok(None);
        }
    };
    let mut bytes = Vec::new();
    if let Err(error) =
        Read::take(&mut entry_file, limits.max_file_size.saturating_add(1)).read_to_end(&mut bytes)
    {
        record_scan_issue(
            report,
            limits,
            WorkspaceScanIssueKind::FileUnreadable,
            PathBuf::from(format!("{}!{}", zip_path.display(), entry)),
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
            PathBuf::from(format!("{}!{}", zip_path.display(), entry)),
            format!(
                "zip entry size {} exceeds the configured limit of {} bytes",
                bytes.len(),
                limits.max_file_size
            ),
        );
        return Ok(None);
    }
    let virtual_path = PathBuf::from(format!("{}!{}", zip_path.display(), entry));
    Ok(decode_source_bytes(
        &bytes,
        source_encoding,
        &virtual_path,
        limits,
        report,
    ))
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
                    "too many {kind} patterns (maximum {})",
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

    fn profile_with_archives(
        scan_roots: &[&str],
        archive_roots: &[&str],
        extensions: &[&str],
    ) -> GameProfile {
        let mut profile = profile_with_roots(scan_roots, &["gfx"]);
        profile.scan_archive_roots = archive_roots
            .iter()
            .map(|root| (*root).to_owned())
            .collect();
        profile.scan_archive_extensions = extensions.iter().map(|e| (*e).to_owned()).collect();
        profile
    }

    fn write_zip(path: &std::path::Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            zip.start_file::<_, ()>(*name, zip::write::SimpleFileOptions::default())
                .expect("start entry");
            std::io::Write::write_all(&mut zip, contents.as_bytes()).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }

    fn scan_paths(root: &std::path::Path, profile: &GameProfile) -> Vec<(String, String)> {
        let mut output = Vec::new();
        let mut report = WorkspaceScanReport::default();
        collect_whitelisted_files(
            root,
            profile,
            &WorkspaceScanFilters::default(),
            WorkspaceScanLimits::default(),
            &mut report,
            &mut output,
            &WorkspaceScanToken::new(),
        )
        .expect("scan fixture root");
        output
            .into_iter()
            .map(|(logical, physical)| {
                (logical.as_str().to_owned(), physical.display().to_string())
            })
            .collect()
    }

    #[test]
    fn archive_tiers_surface_zip_entries_and_extracted_files() {
        let root = test_directory("archives");
        fs::create_dir_all(root.join("interface")).expect("interface directory");
        fs::write(root.join("interface/base.gfx"), "spriteTypes = { }").expect("base sprite file");
        fs::create_dir_all(root.join("builtin_dlc/dlc100/interface")).expect("builtin pack");
        fs::write(
            root.join("builtin_dlc/dlc100/interface/extracted.gfx"),
            "spriteTypes = { }",
        )
        .expect("extracted sprite file");
        fs::create_dir_all(root.join("dlc/dlc200")).expect("dlc directory");
        write_zip(
            &root.join("dlc/dlc200/dlc200.zip"),
            &[
                ("interface/from_zip.gfx", "spriteTypes = { }"),
                ("events/hidden.txt", "country_event = { }"),
            ],
        );

        let profile = profile_with_archives(&["interface"], &["builtin_dlc", "dlc"], &["gfx"]);
        let paths = scan_paths(&root, &profile);
        let logicals = paths
            .iter()
            .map(|(logical, _)| logical.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            logicals,
            vec![
                "interface/base.gfx",
                "interface/extracted.gfx",
                "interface/from_zip.gfx"
            ],
            "archive tiers add their gfx payloads under the game-data layout"
        );
        let zip_entry = &paths[2].1;
        assert!(
            zip_entry.ends_with("interface/from_zip.gfx") && zip_entry.contains('!'),
            "zip entries are addressed through the virtual separator path: {zip_entry}"
        );

        let mut report = WorkspaceScanReport::default();
        let text = read_source_file(
            std::path::Path::new(zip_entry),
            WorkspaceScanLimits::default(),
            &mut report,
            SourceEncoding::Utf8,
        )
        .expect("read zip entry");
        assert_eq!(text, "spriteTypes = { }");
        assert!(
            report.issues.is_empty(),
            "reading a valid zip entry reports no issues: {:?}",
            report.issues
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn archive_tiers_defer_to_discovered_disk_files_and_extension_filters() {
        let root = test_directory("archive-dedup");
        fs::create_dir_all(root.join("interface")).expect("interface directory");
        fs::write(root.join("interface/shared.gfx"), "spriteTypes = { }")
            .expect("disk sprite file");
        fs::create_dir_all(root.join("dlc/pack")).expect("dlc directory");
        write_zip(
            &root.join("dlc/pack/pack.zip"),
            &[
                ("interface/shared.gfx", "spriteTypes = { overridden }"),
                ("interface/extra.gfx", "spriteTypes = { }"),
                ("localisation/hidden.yml", "l_english:"),
            ],
        );

        let profile = profile_with_archives(&["interface"], &["dlc"], &["gfx"]);
        let paths = scan_paths(&root, &profile);
        let shared = paths
            .iter()
            .find(|(logical, _)| logical.as_str() == "interface/shared.gfx")
            .expect("shared entry");
        assert!(
            !shared.1.contains('!'),
            "the discovered disk file keeps precedence over the zip duplicate: {}",
            shared.1
        );
        assert_eq!(
            paths
                .iter()
                .filter(|(logical, _)| logical.as_str() == "interface/extra.gfx")
                .count(),
            1,
            "names the disk data lacks still surface from the archive"
        );
        assert!(
            !paths
                .iter()
                .any(|(logical, _)| logical.as_str().ends_with(".yml")),
            "extensions outside the archive whitelist never surface"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn archive_tiers_stay_off_without_profile_declaration() {
        let root = test_directory("archive-off");
        fs::create_dir_all(root.join("dlc/pack")).expect("dlc directory");
        write_zip(
            &root.join("dlc/pack/pack.zip"),
            &[("interface/from_zip.gfx", "spriteTypes = { }")],
        );
        let profile = profile_with_roots(&["interface"], &["gfx"]);
        let paths = scan_paths(&root, &profile);
        assert!(
            paths.is_empty(),
            "no archive tier is walked unless the profile declares one: {paths:?}"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
