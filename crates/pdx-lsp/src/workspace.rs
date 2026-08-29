use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pdx_analysis::{DiagnosticCode, Severity};
use pdx_engine::{
    SourceRoot, SourceRootId, SourceRootKind, WorkspaceScanFilters, WorkspaceScanLimits,
    WorkspaceScanToken,
};
use serde::Deserialize;
use serde_json::Value;

use crate::protocol::RpcError;
use crate::{INVALID_PARAMS, REQUEST_CANCELLED};

/// A zero interval disables the optional quiet background re-scan.  This mirrors the reference
/// server's opt-in behavior so existing clients never acquire an unexpected periodic disk walk.
pub(crate) const DEFAULT_BACKGROUND_REINDEX_INTERVAL_MINUTES: u64 = 0;
pub(crate) const DEFAULT_BACKGROUND_REINDEX_IDLE_SECONDS: u64 = 15;
pub(crate) const DEFAULT_WORKSPACE_WIDE_DIAGNOSTICS: bool = true;
const MAX_BACKGROUND_REINDEX_INTERVAL_MINUTES: u64 = 7 * 24 * 60;
const MAX_BACKGROUND_REINDEX_IDLE_SECONDS: u64 = 24 * 60 * 60;
pub(crate) const MAX_IGNORED_DIAGNOSTIC_CODES: usize = 256;
const MAX_DIAGNOSTIC_SEVERITY_OVERRIDES: usize = 256;
const MAX_PREFERRED_LOCALISATION_LANGUAGES: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum VanillaMode {
    /// Use explicit editor/user caches and the normal one-time automatic discovery path.
    #[default]
    Auto,
    /// Use an available cache, but never discover or build a new Vanilla cache automatically.
    CacheOnly,
    /// Do not install a Vanilla source root for this workspace.
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PerformanceProfile {
    /// Balanced parsing concurrency for ordinary workspaces.
    #[default]
    Balanced,
    /// Fewer parsing workers and tighter resource bounds for laptops or very large trees.
    Conservative,
    /// Use the engine's maximum bounded parsing concurrency.
    Fast,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceInitializationOptions {
    /// Compatibility sentinel for clients that still send the removed shared project file.
    /// The value is never read; initialization fails with an actionable migration message.
    project_config: Option<Value>,
    mod_directory: Option<PathBuf>,
    dependencies: Option<Vec<DependencyConfiguration>>,
    vanilla_index_cache: Option<PathBuf>,
    game_directory: Option<PathBuf>,
    /// Optional quiet workspace re-scan cadence. Zero disables it.
    background_reindex_interval_minutes: Option<u64>,
    /// Minimum editor-idle window before a quiet re-scan is allowed to start.
    background_reindex_idle_seconds: Option<u64>,
    /// Optional glob patterns for files excluded before workspace discovery.
    #[serde(alias = "ignoreFiles")]
    ignore_file_patterns: Option<Vec<String>>,
    /// Optional glob patterns for directories pruned before workspace discovery.
    #[serde(alias = "ignoreDirs")]
    ignore_directories: Option<Vec<String>>,
    /// Optional diagnostic categories hidden from published LSP diagnostics.
    #[serde(alias = "ignoreDiagnosticCodes")]
    ignored_error_codes: Option<Vec<String>>,
    /// Whether workspace scans publish diagnostics for closed Current Mod files.
    workspace_wide_diagnostics: Option<bool>,
    /// Vanilla symbol source policy. Defaults to automatic discovery/build.
    vanilla_mode: Option<String>,
    /// Preferred localisation language order used by hover and mission titles.
    preferred_localisation_languages: Option<Vec<String>>,
    /// Source-root layers eligible to contribute completion members.
    completion_source_layers: Option<Vec<String>>,
    /// Coarse bounded parsing-concurrency profile.
    performance_profile: Option<String>,
    /// Per-category diagnostic severity overrides (`error`, `warning`, `info`, `hint`, `off`).
    diagnostic_severity_overrides: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct DependencyConfiguration {
    id: String,
    path: PathBuf,
    /// Optional persistent index cache for this dependency. When configured, the dependency is
    /// not scanned live; the cache is loaded (or built once in the background) instead.
    #[serde(default)]
    index: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedSourceRoots {
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) roots: Vec<SourceRoot>,
    pub(crate) index_cache: Option<PathBuf>,
    pub(crate) vanilla_explicit: bool,
    /// Configured game installation root used by the mission-tree texture
    /// loader and editor-guided Vanilla setup, when the caller supplied one.
    pub(crate) game_directory: Option<PathBuf>,
    /// Dependencies configured with a persistent index cache. These roots are excluded from
    /// live scanning and are installed from their cache files instead.
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
    /// Cadence for the optional quiet background source-root re-scan.
    pub(crate) background_reindex_interval_minutes: u64,
    /// User-idle window required before a quiet background re-scan.
    pub(crate) background_reindex_idle_seconds: u64,
    /// Bounded file and directory globs applied to live source-root scans.
    pub(crate) scan_filters: WorkspaceScanFilters,
    /// Canonical wire-facing diagnostic categories hidden from LSP output.
    pub(crate) ignored_diagnostic_codes: Vec<String>,
    /// Whether workspace refreshes publish diagnostics for closed Current Mod files.
    pub(crate) workspace_wide_diagnostics: bool,
    /// Vanilla symbol source policy selected for this workspace.
    pub(crate) vanilla_mode: VanillaMode,
    /// Preferred localisation language order used by analysis queries.
    pub(crate) preferred_localisation_languages: Vec<String>,
    /// Source-root layers eligible to contribute completion members.
    pub(crate) completion_source_layers: Vec<SourceRootKind>,
    /// Coarse bounded parsing-concurrency profile.
    pub(crate) performance_profile: PerformanceProfile,
    /// Resource limits derived from the selected performance profile.
    pub(crate) scan_limits: WorkspaceScanLimits,
    /// Per-category diagnostic severity overrides.
    pub(crate) diagnostic_severity_overrides: BTreeMap<String, Option<Severity>>,
}

/// A dependency configured with a persistent index cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DependencyIndexCache {
    /// The configured dependency root with its caller-assigned identity and order.
    pub(crate) root: SourceRoot,
    /// Where the cache file is stored (or will be built).
    pub(crate) index_path: PathBuf,
}
pub(crate) fn resolve_source_roots(
    client_root: Option<&Path>,
    initialization_options: Option<Value>,
    cancellation: &WorkspaceScanToken,
) -> Result<ResolvedSourceRoots, RpcError> {
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    let inline = initialization_options.map_or_else(
        || Ok(WorkspaceInitializationOptions::default()),
        |value| {
            serde_json::from_value::<WorkspaceInitializationOptions>(value).map_err(|error| {
                RpcError::new(
                    INVALID_PARAMS,
                    format!("invalid initializationOptions: {error}"),
                )
            })
        },
    )?;
    let base = client_root.map(Path::to_path_buf);
    if inline.project_config.is_some() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "projectConfig is no longer supported; configure pdx-ls separately in VS Code or Zed",
        ));
    }
    if let Some(workspace_root) = base.as_deref()
        && workspace_root.join(".pdx").join("project.toml").is_file()
    {
        return Err(RpcError::new(
            INVALID_PARAMS,
            "the shared .pdx/project.toml configuration is no longer supported; remove it and configure VS Code or Zed separately",
        ));
    }
    let project = inline;
    let background_reindex_interval_minutes = project
        .background_reindex_interval_minutes
        .unwrap_or(DEFAULT_BACKGROUND_REINDEX_INTERVAL_MINUTES);
    if background_reindex_interval_minutes > MAX_BACKGROUND_REINDEX_INTERVAL_MINUTES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!(
                "backgroundReindexIntervalMinutes must be at most {MAX_BACKGROUND_REINDEX_INTERVAL_MINUTES}"
            ),
        ));
    }
    let background_reindex_idle_seconds = project
        .background_reindex_idle_seconds
        .unwrap_or(DEFAULT_BACKGROUND_REINDEX_IDLE_SECONDS);
    if background_reindex_idle_seconds > MAX_BACKGROUND_REINDEX_IDLE_SECONDS {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!(
                "backgroundReindexIdleSeconds must be at most {MAX_BACKGROUND_REINDEX_IDLE_SECONDS}"
            ),
        ));
    }
    let scan_filters = WorkspaceScanFilters::new(
        project.ignore_file_patterns.unwrap_or_default(),
        project.ignore_directories.unwrap_or_default(),
    )
    .map_err(|error| {
        RpcError::new(
            INVALID_PARAMS,
            format!("invalid workspace ignore filters: {error}"),
        )
    })?;
    let ignored_diagnostic_codes =
        normalize_ignored_diagnostic_codes(project.ignored_error_codes.unwrap_or_default())?;
    let workspace_wide_diagnostics = project
        .workspace_wide_diagnostics
        .unwrap_or(DEFAULT_WORKSPACE_WIDE_DIAGNOSTICS);
    let vanilla_mode = normalize_vanilla_mode(project.vanilla_mode.as_deref())?;
    let preferred_localisation_languages = normalize_preferred_localisation_languages(
        project.preferred_localisation_languages.unwrap_or_default(),
    )?;
    let completion_source_layers =
        normalize_completion_source_layers(project.completion_source_layers.unwrap_or_default())?;
    let performance_profile =
        normalize_performance_profile(project.performance_profile.as_deref())?;
    let scan_limits = scan_limits_for_performance_profile(performance_profile);
    let diagnostic_severity_overrides = normalize_diagnostic_severity_overrides(
        project.diagnostic_severity_overrides.unwrap_or_default(),
    )?;
    let game_directory = project
        .game_directory
        .as_deref()
        .map(|path| resolve_directory(path, base.as_deref(), "gameDirectory"))
        .transpose()?;
    let vanilla_index_cache = project
        .vanilla_index_cache
        .as_deref()
        .map(|path| resolve_configured_path(path, base.as_deref(), "vanillaIndexCache"))
        .transpose()?;
    let vanilla_explicit = vanilla_index_cache.is_some();

    let current_mod = match project.mod_directory.as_deref() {
        Some(path) => Some(resolve_directory(path, base.as_deref(), "modDirectory")?),
        None => client_root
            .filter(|path| path.is_dir())
            .map(fs::canonicalize)
            .transpose()
            .map_err(|error| {
                RpcError::new(
                    INVALID_PARAMS,
                    format!("cannot resolve workspace root: {error}"),
                )
            })?,
    };
    let mut configured = Vec::<(String, PathBuf, Option<PathBuf>)>::new();
    let mut root_ids = BTreeMap::<u32, String>::new();
    for dependency in project.dependencies.unwrap_or_default() {
        if dependency.id.trim().is_empty() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "dependency id must not be empty",
            ));
        }
        if dependency.id != dependency.id.trim() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "dependency id must not have surrounding whitespace: {}",
                    dependency.id
                ),
            ));
        }
        if configured
            .iter()
            .any(|(id, _, _)| id.eq_ignore_ascii_case(&dependency.id))
        {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("duplicate dependency id: {}", dependency.id),
            ));
        }
        // An indexed dependency may keep its source directory offline because the cache holds
        // all indexed data; the directory is validated again when a rebuild is needed. Live
        // dependencies must exist because they are scanned every session.
        let path = match dependency.index.as_ref() {
            Some(_) => {
                let path =
                    resolve_configured_path(&dependency.path, base.as_deref(), "dependency path")?;
                if path.is_dir() {
                    fs::canonicalize(&path).map_err(|error| {
                        RpcError::new(
                            INVALID_PARAMS,
                            format!("cannot resolve dependency path: {error}"),
                        )
                    })?
                } else {
                    path
                }
            }
            None => resolve_directory(&dependency.path, base.as_deref(), "dependency path")?,
        };
        let index = match dependency.index {
            None => None,
            Some(index) => Some(resolve_configured_path(
                &index,
                base.as_deref(),
                "dependency index",
            )?),
        };
        let root_id = stable_dependency_root_id(&dependency.id);
        if let Some(previous) = root_ids.insert(root_id, dependency.id.clone()) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "dependency root id collision between {previous} and {}",
                    dependency.id
                ),
            ));
        }
        configured.push((dependency.id, path, index));
    }

    let mut paths = configured
        .iter()
        .map(|(_, path, _)| path)
        .collect::<Vec<_>>();
    if let Some(current_mod) = current_mod.as_ref() {
        paths.push(current_mod);
    }
    for (index, left) in paths.iter().enumerate() {
        for right in paths.iter().skip(index + 1) {
            if left.starts_with(right) || right.starts_with(left) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!(
                        "source roots must not overlap: {} and {}",
                        left.display(),
                        right.display()
                    ),
                ));
            }
        }
    }

    let mut roots = Vec::with_capacity(configured.len().saturating_add(1));
    let mut dependency_caches = Vec::new();
    let dependency_count = configured.len();
    for (order, (id, path, index)) in configured.into_iter().enumerate() {
        // Orders are globally unique across all layers: 0 belongs to the Vanilla layer, live
        // dependencies and cached dependencies share the 1..=n range in configuration order,
        // and the Current Mod takes n+1.
        let order = u32::try_from(order)
            .map_err(|_| {
                RpcError::new(
                    INVALID_PARAMS,
                    "too many dependency roots to assign stable order",
                )
            })?
            .saturating_add(1);
        let mut root = SourceRoot::new(
            SourceRootId::new(stable_dependency_root_id(&id)),
            SourceRootKind::Dependency,
            path,
        );
        root.order = order;
        match index {
            Some(index_path) => dependency_caches.push(DependencyIndexCache { root, index_path }),
            None => roots.push(root),
        }
    }
    if let Some(path) = current_mod.clone() {
        let mut current_root = SourceRoot::new(
            SourceRootId::new(u32::MAX),
            SourceRootKind::CurrentMod,
            path,
        );
        current_root.order = u32::try_from(dependency_count)
            .map_err(|_| {
                RpcError::new(
                    INVALID_PARAMS,
                    "too many dependency roots to assign stable order",
                )
            })?
            .saturating_add(1);
        roots.push(current_root);
    }
    Ok(ResolvedSourceRoots {
        workspace_root: current_mod.or(base),
        roots,
        index_cache: vanilla_index_cache,
        vanilla_explicit,
        game_directory,
        dependency_caches,
        background_reindex_interval_minutes,
        background_reindex_idle_seconds,
        scan_filters,
        ignored_diagnostic_codes,
        workspace_wide_diagnostics,
        vanilla_mode,
        preferred_localisation_languages,
        completion_source_layers,
        performance_profile,
        scan_limits,
        diagnostic_severity_overrides,
    })
}

pub(crate) fn normalize_vanilla_mode(value: Option<&str>) -> Result<VanillaMode, RpcError> {
    match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(VanillaMode::Auto),
        "cacheonly" | "cache_only" | "cache-only" => Ok(VanillaMode::CacheOnly),
        "disabled" | "off" => Ok(VanillaMode::Disabled),
        value => Err(RpcError::new(
            INVALID_PARAMS,
            format!("vanillaMode must be auto, cacheOnly, or disabled (got {value})"),
        )),
    }
}

pub(crate) fn normalize_performance_profile(
    value: Option<&str>,
) -> Result<PerformanceProfile, RpcError> {
    match value
        .unwrap_or("balanced")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "balanced" => Ok(PerformanceProfile::Balanced),
        "conservative" => Ok(PerformanceProfile::Conservative),
        "fast" => Ok(PerformanceProfile::Fast),
        value => Err(RpcError::new(
            INVALID_PARAMS,
            format!("performanceProfile must be conservative, balanced, or fast (got {value})"),
        )),
    }
}

pub(crate) fn scan_limits_for_performance_profile(
    profile: PerformanceProfile,
) -> WorkspaceScanLimits {
    WorkspaceScanLimits {
        max_workers: match profile {
            PerformanceProfile::Conservative => 2,
            PerformanceProfile::Balanced => 8,
            PerformanceProfile::Fast => 12,
        },
        ..WorkspaceScanLimits::default()
    }
}

pub(crate) fn normalize_preferred_localisation_languages(
    values: Vec<String>,
) -> Result<Vec<String>, RpcError> {
    if values.len() > MAX_PREFERRED_LOCALISATION_LANGUAGES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!(
                "preferredLocalisationLanguages accepts at most {MAX_PREFERRED_LOCALISATION_LANGUAGES} entries"
            ),
        ));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty()
            || !value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("invalid localisation language: {value}"),
            ));
        }
        if !normalized.iter().any(|existing| existing == &value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

pub(crate) fn normalize_completion_source_layers(
    values: Vec<String>,
) -> Result<Vec<SourceRootKind>, RpcError> {
    if values.is_empty() {
        return Ok(vec![
            SourceRootKind::CurrentMod,
            SourceRootKind::Dependency,
            SourceRootKind::Vanilla,
        ]);
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let layer = match value.trim().to_ascii_lowercase().as_str() {
            "currentmod" | "current_mod" | "current-mod" => SourceRootKind::CurrentMod,
            "dependency" | "dependencies" => SourceRootKind::Dependency,
            "vanilla" => SourceRootKind::Vanilla,
            value => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!(
                        "completionSourceLayers entries must be currentMod, dependencies, or vanilla (got {value})"
                    ),
                ));
            }
        };
        if !normalized.contains(&layer) {
            normalized.push(layer);
        }
    }
    Ok(normalized)
}

pub(crate) fn normalize_diagnostic_severity_overrides(
    values: BTreeMap<String, String>,
) -> Result<BTreeMap<String, Option<Severity>>, RpcError> {
    if values.len() > MAX_DIAGNOSTIC_SEVERITY_OVERRIDES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!(
                "diagnosticSeverityOverrides accepts at most {MAX_DIAGNOSTIC_SEVERITY_OVERRIDES} entries"
            ),
        ));
    }
    let mut normalized = BTreeMap::new();
    for (raw_code, raw_severity) in values {
        let Some(code) = DiagnosticCode::parse_name(raw_code.trim()) else {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown diagnostic code in diagnosticSeverityOverrides: {raw_code}"),
            ));
        };
        let severity = match raw_severity.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Severity::Error),
            "warning" | "warn" => Some(Severity::Warning),
            "info" | "information" => Some(Severity::Information),
            "hint" => Some(Severity::Hint),
            "off" | "none" => None,
            value => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    format!(
                        "diagnosticSeverityOverrides values must be error, warning, info, hint, or off (got {value})"
                    ),
                ));
            }
        };
        normalized.insert(code.as_str().to_owned(), severity);
    }
    Ok(normalized)
}

/// Validates and canonicalizes user-selected diagnostic categories. Unknown values are rejected so
/// a typo cannot make a setting appear to work while silently suppressing nothing.
pub(crate) fn normalize_ignored_diagnostic_codes(
    values: Vec<String>,
) -> Result<Vec<String>, RpcError> {
    if values.len() > MAX_IGNORED_DIAGNOSTIC_CODES {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("ignoredErrorCodes accepts at most {MAX_IGNORED_DIAGNOSTIC_CODES} entries"),
        ));
    }
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        let Some(code) = DiagnosticCode::parse_name(trimmed) else {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown diagnostic code in ignoredErrorCodes: {value}"),
            ));
        };
        let canonical = code.as_str().to_owned();
        if !normalized.iter().any(|existing| existing == &canonical) {
            normalized.push(canonical);
        }
    }
    Ok(normalized)
}

fn resolve_configured_path(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let base = base.ok_or_else(|| {
        RpcError::new(
            INVALID_PARAMS,
            format!("relative {field} requires a workspace root"),
        )
    })?;
    Ok(base.join(path))
}

fn resolve_directory(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    let path = resolve_path(path, base, field)?;
    if !path.is_dir() {
        return Err(RpcError::new(
            INVALID_PARAMS,
            format!("{field} is not a directory: {}", path.display()),
        ));
    }
    Ok(path)
}

fn resolve_path(
    path: &Path,
    base: Option<&Path>,
    field: &'static str,
) -> Result<PathBuf, RpcError> {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        let base = base.ok_or_else(|| {
            RpcError::new(
                INVALID_PARAMS,
                format!("relative {field} requires a workspace root"),
            )
        })?;
        base.join(path)
    };
    fs::canonicalize(&candidate).map_err(|error| {
        RpcError::new(
            INVALID_PARAMS,
            format!("cannot resolve {field} {}: {error}", candidate.display()),
        )
    })
}

pub(crate) fn stable_dependency_root_id(id: &str) -> u32 {
    let mut value = 0x811c9dc5_u32;
    for byte in id.bytes().map(|byte| byte.to_ascii_lowercase()) {
        value = (value ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    if matches!(value, 0 | u32::MAX) {
        value ^ 0x8000_0000
    } else {
        value
    }
}
