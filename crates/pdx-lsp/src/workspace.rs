use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use pdx_engine::{SourceRoot, SourceRootId, SourceRootKind, WorkspaceScanToken};
use serde::Deserialize;
use serde_json::Value;

use crate::protocol::RpcError;
use crate::{INVALID_PARAMS, PROJECT_CONFIG_MAX_BYTES, REQUEST_CANCELLED};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct WorkspaceInitializationOptions {
    project_config: Option<PathBuf>,
    mod_directory: Option<PathBuf>,
    dependencies: Option<Vec<DependencyConfiguration>>,
    vanilla_index_cache: Option<PathBuf>,
    game_directory: Option<PathBuf>,
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
struct ProjectConfiguration {
    #[serde(alias = "modDirectory")]
    mod_directory: Option<PathBuf>,
    dependencies: Option<Vec<DependencyConfiguration>>,
    #[serde(alias = "vanillaIndexCache")]
    vanilla_index_cache: Option<PathBuf>,
    /// Game installation root whose `interface/*.gfx` and `gfx/interface/missions`
    /// textures back the mission-tree preview. Optional: when absent, the
    /// server performs a one-time quick discovery at initialize.
    #[serde(alias = "gameDirectory")]
    game_directory: Option<PathBuf>,
    /// Extension-only `[server]` table (e.g. the language-server binary path
    /// used by the Zed / VS Code toolkits). Declared so a single
    /// `.pdx/project.toml` can serve both editors and pdx-ls, while
    /// `deny_unknown_fields` still rejects genuine typos.
    #[serde(default)]
    server: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedSourceRoots {
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) roots: Vec<SourceRoot>,
    pub(crate) index_cache: Option<PathBuf>,
    pub(crate) vanilla_explicit: bool,
    /// Configured game installation root used by the mission-tree texture
    /// loader, when the caller supplied one.
    pub(crate) game_directory: Option<PathBuf>,
    /// Dependencies configured with a persistent index cache. These roots are excluded from
    /// live scanning and are installed from their cache files instead.
    pub(crate) dependency_caches: Vec<DependencyIndexCache>,
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
    let mut project = if let Some(project_config) = inline.project_config.as_deref() {
        let path = resolve_path(project_config, base.as_deref(), "projectConfig")?;
        if !path.is_file() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("projectConfig is not a file: {}", path.display()),
            ));
        }
        load_project_config(&path, cancellation)?
    } else if let Some(workspace_root) = base.as_deref()
        && workspace_root.join(".pdx/project.toml").is_file()
    {
        // Universal configuration: a `.pdx/project.toml` next to the workspace
        // root is discovered automatically, so Zed and VS Code share the same
        // config with no per-editor setup. An explicitly configured
        // `projectConfig` always wins over this file.
        load_project_config(&workspace_root.join(".pdx/project.toml"), cancellation)?
    } else {
        ProjectConfiguration::default()
    };
    if inline.mod_directory.is_some() {
        project.mod_directory = inline.mod_directory;
    }
    if inline.dependencies.is_some() {
        project.dependencies = inline.dependencies;
    }
    if inline.vanilla_index_cache.is_some() {
        project.vanilla_index_cache = inline.vanilla_index_cache;
    }
    if inline.game_directory.is_some() {
        project.game_directory = inline.game_directory;
    }
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
    })
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

/// Loads and parses a `.pdx/project.toml` project configuration. Fails loudly
/// on unreadable, oversized, or ill-formed config — never silently ignored.
fn load_project_config(
    path: &Path,
    cancellation: &WorkspaceScanToken,
) -> Result<ProjectConfiguration, RpcError> {
    if cancellation.is_cancelled() {
        return Err(RpcError::new(REQUEST_CANCELLED, "request was cancelled"));
    }
    let file = fs::File::open(path).map_err(|error| {
        RpcError::new(
            INVALID_PARAMS,
            format!("cannot open projectConfig {}: {error}", path.display()),
        )
    })?;
    let mut text = String::new();
    file.take(PROJECT_CONFIG_MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| {
            RpcError::new(
                INVALID_PARAMS,
                format!("cannot read projectConfig {}: {error}", path.display()),
            )
        })?;
    if text.len() as u64 > PROJECT_CONFIG_MAX_BYTES {
        return Err(RpcError::new(INVALID_PARAMS, "projectConfig exceeds 1 MiB"));
    }
    toml::from_str::<ProjectConfiguration>(&text).map_err(|error| {
        RpcError::new(
            INVALID_PARAMS,
            format!("invalid projectConfig TOML: {error}"),
        )
    })
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
