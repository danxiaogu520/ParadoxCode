//! User-facing command-line entry points.

use std::fmt;
use std::path::PathBuf;
use std::time::Instant;

use engine::{
    AnalysisHost, IndexCache, IndexCacheError, SourceRoot, SourceRootId, SourceRootKind,
    WorkspaceChange, WorkspaceError, WorkspaceScanToken,
};
use game::{
    CandidateSource, DiscoveredInstallation, DiscoveryOptions, DiscoveryOutcome, DiscoveryToken,
    GameInstallDescriptor, UserConfigError, UserConfiguration, UserPaths, discover_installations,
    select_installation, validate_installation_for_source,
};

use pdc::stable_dependency_root_id;

const USAGE: &str = "usage (cargo tools <command> ... or: cargo run -p tools -- <command> ...):
  index [vanilla] --source <EU4 directory> --output <cache.pdcindex>
  index dependency --id <id> --source <directory> --output <cache.pdcindex>
  setup vanilla [--game eu4] [--root <directory>]... [--source <game directory>]
  check policy|release|all [--root <repository root>]
  gates [core|core-fast|perf|vscode|release|fuzz|all]... [--root <repository root>]
  release package --version <semver> --target <target> --binary <path> --output-dir <path> [--root <repository root>]
  release verify --version <semver> --directory <path> [--root <repository root>]";
const SUPPORTED_GAME_INSTALLATIONS: &[GameInstallDescriptor] = &[game::eu4::INSTALL_DESCRIPTOR];

/// Executes one repository tooling command and returns text intended for stdout.
pub fn execute(args: &[String]) -> Result<String, CliError> {
    match args {
        [index, vanilla, rest @ ..] if index == "index" && vanilla == "vanilla" => {
            index_vanilla(rest)
        }
        // `index --source ... --output ...` is the Vanilla-layer spelling; the kind is
        // implicit in the reserved root id 0.
        [index, rest @ ..] if index == "index" && !rest.is_empty() && rest[0] != "dependency" => {
            index_vanilla(rest)
        }
        [index] if index == "index" => Err(CliError::Usage(USAGE.to_owned())),
        [index, dependency, rest @ ..] if index == "index" && dependency == "dependency" => {
            index_dependency(rest)
        }
        [setup, vanilla, rest @ ..] if setup == "setup" && vanilla == "vanilla" => {
            let paths = UserPaths::platform()?;
            setup_vanilla(rest, &paths)
        }
        [check, sub, rest @ ..] if check == "check" => execute_check(sub, rest),
        [gates, rest @ ..] if gates == "gates" => execute_gates(rest),
        [release, sub, rest @ ..] if release == "release" => execute_release(sub, rest),
        _ => Err(CliError::Usage(USAGE.to_owned())),
    }
}

/// Parses `gates` arguments: zero or more group names plus an optional
/// `--root`, defaulting to the `all` group when no group is named.
fn execute_gates(args: &[String]) -> Result<String, CliError> {
    let mut groups = Vec::new();
    let mut root = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::Usage(format!("missing value for --root\n\n{USAGE}"))
                })?;
                if root.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --root".to_owned(),
                    ));
                }
                index += 2;
            }
            group => {
                groups.push(group.to_owned());
                index += 1;
            }
        }
    }
    let unknown = groups
        .iter()
        .find(|group| crate::gates::gate_actions(group).is_none());
    if let Some(group) = unknown {
        return Err(CliError::Usage(format!(
            "unknown gate group: {group}\n\n{USAGE}"
        )));
    }
    if groups.is_empty() {
        groups.push("all".to_owned());
    }
    let root = crate::gates::resolve_root(root.as_deref())?;
    crate::gates::run_gates(&groups, &root)
        .map(|executed| format!("tools gates passed ({executed} gates run)"))
}

fn setup_vanilla(args: &[String], paths: &UserPaths) -> Result<String, CliError> {
    let mut game = None::<String>;
    let mut source = None::<PathBuf>;
    let mut roots = Vec::<PathBuf>::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            flag @ ("--game" | "--root" | "--source") => {
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::Usage(format!("missing value for {flag}\n\n{USAGE}"))
                })?;
                match flag {
                    "--game" => {
                        if game.replace(value.clone()).is_some() {
                            return Err(CliError::Usage(
                                "option supplied more than once: --game".to_owned(),
                            ));
                        }
                    }
                    "--root" => roots.push(PathBuf::from(value)),
                    "--source" => {
                        if source.replace(PathBuf::from(value)).is_some() {
                            return Err(CliError::Usage(
                                "option supplied more than once: --source".to_owned(),
                            ));
                        }
                    }
                    _ => unreachable!("matched setup option"),
                }
                index += 2;
            }
            flag => {
                return Err(CliError::Usage(format!(
                    "unknown option: {flag}\n\n{USAGE}"
                )));
            }
        }
    }
    let games = if let Some(game) = game.as_deref() {
        let descriptor = SUPPORTED_GAME_INSTALLATIONS
            .iter()
            .find(|descriptor| descriptor.game_id == game)
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "unsupported game: {game}; supported games: {}",
                    SUPPORTED_GAME_INSTALLATIONS
                        .iter()
                        .map(|descriptor| descriptor.game_id)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        std::slice::from_ref(descriptor)
    } else {
        SUPPORTED_GAME_INSTALLATIONS
    };
    if source.is_some() && games.len() != 1 {
        return Err(CliError::Usage(
            "--source requires --game when more than one game is supported".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(games.len());
    for descriptor in games {
        output.push(setup_game(
            *descriptor,
            source.clone(),
            roots.clone(),
            paths,
        )?);
    }
    Ok(output.join("\n\n"))
}

fn setup_game(
    descriptor: GameInstallDescriptor,
    source: Option<PathBuf>,
    roots: Vec<PathBuf>,
    paths: &UserPaths,
) -> Result<String, CliError> {
    let mut configuration = UserConfiguration::load(&paths.config_file)?;
    let previous = configuration
        .games
        .get(descriptor.game_id)
        .cloned()
        .unwrap_or_default();
    let explicit_search = source.is_some() || !roots.is_empty();
    let candidates = if let Some(source) = source {
        let source = dunce::canonicalize(&source).map_err(|error| CliError::Path {
            field: "--source",
            path: source,
            error,
        })?;
        if !validate_installation_for_source(&source, &descriptor) {
            return Err(CliError::Discovery(format!(
                "{} is not a valid {} installation; expected an executable and common, events, missions, decisions, and localisation directories",
                source.display(),
                descriptor.display_name
            )));
        }
        vec![explicit_installation(source)]
    } else {
        match previous.vanilla_source.filter(|source| {
            !explicit_search && validate_installation_for_source(source, &descriptor)
        }) {
            Some(source) => vec![DiscoveredInstallation {
                game_build: previous.game_build,
                ..explicit_installation(source)
            }],
            None => {
                let report = discover_installations(
                    &descriptor,
                    &DiscoveryOptions {
                        roots,
                        include_platform_locations: true,
                    },
                    &DiscoveryToken::new(),
                );
                if report.cancelled {
                    return Err(CliError::Discovery(
                        "Vanilla discovery was cancelled".to_owned(),
                    ));
                }
                report.installations
            }
        }
    };

    let selection = match select_installation(&candidates) {
        Some(selection) => selection,
        None => {
            let game = configuration
                .games
                .entry(descriptor.game_id.to_owned())
                .or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::NotFound);
            configuration.save(&paths.config_file)?;
            return Err(CliError::Discovery(format!(
                "no valid {} installation was found; retry with --source <directory> or --root <directory>",
                descriptor.display_name
            )));
        }
    };
    let selected = selection.selected.clone();

    let cache_path = paths.vanilla_cache(descriptor.game_id);
    let summary = match build_cache(
        SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            selected.path.clone(),
        ),
        &cache_path,
        "Vanilla",
    ) {
        Ok(summary) => summary,
        Err(error) => {
            let game = configuration
                .games
                .entry(descriptor.game_id.to_owned())
                .or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::Failed);
            game.vanilla_source = Some(selected.path);
            configuration.save(&paths.config_file)?;
            return Err(error);
        }
    };
    let game = configuration
        .games
        .entry(descriptor.game_id.to_owned())
        .or_default();
    game.auto_discovery_attempted = true;
    game.discovery_outcome = Some(DiscoveryOutcome::Configured);
    game.vanilla_source = Some(selected.path.clone());
    game.vanilla_cache = Some(cache_path.clone());
    game.resolved_via = Some(selected.source.label().to_owned());
    game.game_build = selected.game_build;
    configuration.save(&paths.config_file)?;
    let mut output = format!(
        "{} configured\nsource: {} ({})\ncache: {}",
        descriptor.display_name,
        selected.path.display(),
        selected.source.label(),
        cache_path.display()
    );
    if !selection.alternatives.is_empty() {
        output.push_str(&format!(
            "\nalternatives: {} (rerun with --source <directory> to choose another)",
            selection
                .alternatives
                .iter()
                .map(|candidate| candidate.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    output.push_str(&format!("\n{summary}"));
    Ok(output)
}

/// Wraps an explicitly chosen or previously configured directory as a candidate.
fn explicit_installation(path: PathBuf) -> DiscoveredInstallation {
    DiscoveredInstallation {
        path,
        source: CandidateSource::Explicit,
        game_build: None,
        marker_modified: None,
    }
}

fn index_vanilla(args: &[String]) -> Result<String, CliError> {
    let mut source = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}\n\n{USAGE}")))?;
        let target = match flag.as_str() {
            "--source" => &mut source,
            "--output" => &mut output,
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown option: {flag}\n\n{USAGE}"
                )));
            }
        };
        if target.replace(PathBuf::from(value)).is_some() {
            return Err(CliError::Usage(format!(
                "option supplied more than once: {flag}"
            )));
        }
        index += 2;
    }
    let source = required_path(source, "--source")?;
    let output = required_path(output, "--output")?;
    let source = dunce::canonicalize(&source).map_err(|error| CliError::Path {
        field: "--source",
        path: source,
        error,
    })?;
    if !source.is_dir() {
        return Err(CliError::Usage(format!(
            "--source is not a directory: {}",
            source.display()
        )));
    }

    build_cache(
        SourceRoot::new(SourceRootId::new(0), SourceRootKind::Vanilla, source),
        &output,
        "Vanilla",
    )
}

fn index_dependency(args: &[String]) -> Result<String, CliError> {
    let mut id = None::<String>;
    let mut source = None::<PathBuf>;
    let mut output = None::<PathBuf>;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}\n\n{USAGE}")))?;
        let duplicate = match flag.as_str() {
            "--id" => id.replace(value.clone()).is_some(),
            "--source" => source.replace(PathBuf::from(value)).is_some(),
            "--output" => output.replace(PathBuf::from(value)).is_some(),
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown option: {flag}\n\n{USAGE}"
                )));
            }
        };
        if duplicate {
            return Err(CliError::Usage(format!(
                "option supplied more than once: {flag}"
            )));
        }
        index += 2;
    }
    let id = required_value(id, "--id")?;
    if id.trim().is_empty() || id != id.trim() {
        return Err(CliError::Usage(format!(
            "dependency id must not be empty or have surrounding whitespace: {id}\n\n{USAGE}"
        )));
    }
    let source = required_path(source, "--source")?;
    let output = required_path(output, "--output")?;
    let source = dunce::canonicalize(&source).map_err(|error| CliError::Path {
        field: "--source",
        path: source,
        error,
    })?;
    if !source.is_dir() {
        return Err(CliError::Usage(format!(
            "--source is not a directory: {}",
            source.display()
        )));
    }
    build_cache(
        SourceRoot::new(
            SourceRootId::new(stable_dependency_root_id(&id)),
            SourceRootKind::Dependency,
            source,
        ),
        &output,
        &format!("Dependency {id}"),
    )
}

fn build_cache(
    root: SourceRoot,
    output: &std::path::Path,
    label: &str,
) -> Result<String, CliError> {
    let started = Instant::now();
    let rules = game::eu4::first_party_rules()?;
    let profile = rules.profile().clone();
    // An existing validated cache is refreshed in place: only files whose content fingerprint
    // changed are reindexed. Any load or refresh failure (missing file, stale rules, corrupt
    // data) falls back to a full scan and rebuild.
    if let Ok(existing) =
        IndexCache::load_cancellable_for_install(output, &WorkspaceScanToken::new())
        && let Ok(cache) = existing.refresh(&rules, &profile)
    {
        let save_started = Instant::now();
        cache.save(output)?;
        let save_elapsed = save_started.elapsed();
        return Ok(format!(
            "{label} cache refreshed from {}\nindexed files: {}\nscan time: 0 ms\ncache save: {} ms\ntotal time: {} ms\nsource fingerprint: {}\nrules hash: {}",
            output.display(),
            cache.metadata().indexed_files,
            save_elapsed.as_millis(),
            started.elapsed().as_millis(),
            cache.metadata().source_fingerprint,
            cache.metadata().rule_hash
        ));
    }
    let mut host = AnalysisHost::with_profile(rules, profile);
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![root]));
    let scan_started = Instant::now();
    let report = host.refresh_source_roots()?;
    let scan_elapsed = scan_started.elapsed();
    let cache_started = Instant::now();
    let cache = IndexCache::from_snapshot(&host.snapshot())?;
    let cache_elapsed = cache_started.elapsed();
    let save_started = Instant::now();
    cache.save(output)?;
    let save_elapsed = save_started.elapsed();
    Ok(format!(
        "{label} cache written to {}\nindexed files: {}\nlegacy encoded files: {}\nskipped entries: {}\nscan time: {} ms\ncache materialization: {} ms\ncache save: {} ms\ntotal time: {} ms\nsource fingerprint: {}\nrules hash: {}",
        output.display(),
        cache.metadata().indexed_files,
        report.legacy_encoded_files,
        report.skipped_entries,
        scan_elapsed.as_millis(),
        cache_elapsed.as_millis(),
        save_elapsed.as_millis(),
        started.elapsed().as_millis(),
        cache.metadata().source_fingerprint,
        cache.metadata().rule_hash
    ))
}

fn required_value(value: Option<String>, flag: &'static str) -> Result<String, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("missing required option: {flag}\n\n{USAGE}")))
}

fn required_path(value: Option<PathBuf>, flag: &'static str) -> Result<PathBuf, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("missing required option: {flag}\n\n{USAGE}")))
}

/// User-facing CLI failures with stable process exit categories.
#[derive(Debug)]
pub enum CliError {
    /// Command-line shape or option validation failed.
    Usage(String),
    /// A configured filesystem path could not be resolved.
    Path {
        field: &'static str,
        path: PathBuf,
        error: std::io::Error,
    },
    /// The rules artifact failed validation.
    Rules(rules::RulesError),
    /// The one-shot Vanilla source scan failed.
    Workspace(WorkspaceError),
    /// The persistent cache could not be built or written.
    Cache(IndexCacheError),
    /// Installation discovery or candidate selection did not produce a usable source.
    Discovery(String),
    /// A spawned quality-gate command could not be executed.
    Exec(String),
    /// User-local discovery configuration failed.
    UserConfig(UserConfigError),
    /// Quality-gate checks found failures.
    CheckFailed,
}

impl CliError {
    /// Returns `2` for usage mistakes and `1` for runtime failures.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Path { .. }
            | Self::Rules(_)
            | Self::Workspace(_)
            | Self::Cache(_)
            | Self::Discovery(_)
            | Self::Exec(_)
            | Self::UserConfig(_)
            | Self::CheckFailed => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Path { field, path, error } => {
                write!(
                    formatter,
                    "cannot resolve {field} {}: {error}",
                    path.display()
                )
            }
            Self::Rules(error) => write!(formatter, "rules artifact error: {error}"),
            Self::Workspace(error) => write!(formatter, "Vanilla indexing error: {error}"),
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Discovery(message) => formatter.write_str(message),
            Self::Exec(message) => write!(formatter, "gate execution failed: {message}"),
            Self::UserConfig(error) => write!(formatter, "{error}"),
            Self::CheckFailed => formatter.write_str("one or more checks failed"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path { error, .. } => Some(error),
            Self::Rules(error) => Some(error),
            Self::Workspace(error) => Some(error),
            Self::Cache(error) => Some(error),
            Self::UserConfig(error) => Some(error),
            Self::Usage(_) | Self::Discovery(_) | Self::Exec(_) | Self::CheckFailed => None,
        }
    }
}

impl From<rules::RulesError> for CliError {
    fn from(error: rules::RulesError) -> Self {
        Self::Rules(error)
    }
}

impl From<WorkspaceError> for CliError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<IndexCacheError> for CliError {
    fn from(error: IndexCacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<UserConfigError> for CliError {
    fn from(error: UserConfigError) -> Self {
        Self::UserConfig(error)
    }
}

fn parse_root_flag(args: &[String]) -> Result<PathBuf, CliError> {
    let mut root = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--root" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| CliError::Usage(format!("missing value for --root\n\n{USAGE}")))?;
            if root.replace(PathBuf::from(value)).is_some() {
                return Err(CliError::Usage(
                    "option supplied more than once: --root".to_owned(),
                ));
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    root.ok_or_else(|| CliError::Usage(format!("missing required option: --root\n\n{USAGE}")))
}

fn execute_check(sub: &str, args: &[String]) -> Result<String, CliError> {
    let root = parse_root_flag(args)?;
    if !root.join("Cargo.toml").is_file() {
        return Err(CliError::Usage(format!(
            "--root {} does not contain Cargo.toml",
            root.display()
        )));
    }
    let results = match sub {
        "policy" => crate::check::check_project_policy(&root),
        "release" => crate::check::check_release_artifact(&root),
        "all" => {
            let mut all = Vec::new();
            all.extend(crate::check::check_project_policy(&root));
            all.extend(crate::check::check_editor_syntax_parity(&root));
            all.extend(crate::check::check_release_artifact(&root));
            all
        }
        _ => return Err(CliError::Usage(format!("unknown check: {sub}\n\n{USAGE}"))),
    };
    let all_pass = crate::check::report(&results);
    if all_pass {
        Ok(format!("tools check {sub} passed"))
    } else {
        Err(CliError::CheckFailed)
    }
}

fn execute_release(sub: &str, args: &[String]) -> Result<String, CliError> {
    let mut root = None;
    let mut version = None;
    let mut target = None;
    let mut binary = None;
    let mut output_dir = None;
    let mut directory = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        let value = args
            .get(index + 1)
            .cloned()
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}\n\n{USAGE}")))?;
        match flag.as_str() {
            "--root" => {
                if root.replace(PathBuf::from(&value)).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --root".to_owned(),
                    ));
                }
            }
            "--version" => {
                if version.replace(value).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --version".to_owned(),
                    ));
                }
            }
            "--target" => {
                if target.replace(value).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --target".to_owned(),
                    ));
                }
            }
            "--binary" => {
                if binary.replace(PathBuf::from(&value)).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --binary".to_owned(),
                    ));
                }
            }
            "--output-dir" => {
                if output_dir.replace(PathBuf::from(&value)).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --output-dir".to_owned(),
                    ));
                }
            }
            "--directory" => {
                if directory.replace(PathBuf::from(&value)).is_some() {
                    return Err(CliError::Usage(
                        "option supplied more than once: --directory".to_owned(),
                    ));
                }
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown option: {flag}\n\n{USAGE}"
                )));
            }
        }
        index += 2;
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    match sub {
        "package" => {
            let version =
                version.ok_or_else(|| CliError::Usage(format!("missing --version\n\n{USAGE}")))?;
            let target =
                target.ok_or_else(|| CliError::Usage(format!("missing --target\n\n{USAGE}")))?;
            let binary =
                binary.ok_or_else(|| CliError::Usage(format!("missing --binary\n\n{USAGE}")))?;
            let output_dir = output_dir
                .ok_or_else(|| CliError::Usage(format!("missing --output-dir\n\n{USAGE}")))?;

            let (limits, artifacts) = crate::release::load_contract(&root).map_err(|error| {
                CliError::Usage(format!("cannot load release contract: {error}"))
            })?;
            let _validated = crate::release::validate_release_version(&version)
                .map_err(|error| CliError::Usage(error.to_string()))?;
            let artifact = artifacts
                .iter()
                .find(|a| a.target == target)
                .ok_or_else(|| CliError::Usage(format!("unsupported target: {target}")))?;
            let (archive_path, _sidecar) =
                crate::release::package_target(&version, artifact, &binary, &output_dir, &limits)
                    .map_err(|error| CliError::Usage(error.to_string()))?;
            Ok(archive_path.display().to_string())
        }
        "verify" => {
            let version =
                version.ok_or_else(|| CliError::Usage(format!("missing --version\n\n{USAGE}")))?;
            let directory = directory
                .ok_or_else(|| CliError::Usage(format!("missing --directory\n\n{USAGE}")))?;
            let (limits, artifacts) = crate::release::load_contract(&root).map_err(|error| {
                CliError::Usage(format!("cannot load release contract: {error}"))
            })?;
            let _validated = crate::release::validate_release_version(&version)
                .map_err(|error| CliError::Usage(error.to_string()))?;
            crate::release::verify_release_directory(&version, &directory, &artifacts, &limits)
                .map_err(|error| CliError::Usage(error.to_string()))?;
            Ok("Complete server release matrix verified.".to_owned())
        }
        _ => Err(CliError::Usage(format!(
            "unknown release subcommand: {sub}\n\n{USAGE}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use engine::{IndexCache, SourceRootId, SourceRootKind};
    use game::{DiscoveryOutcome, UserConfiguration, UserPaths};

    use super::{CliError, execute, setup_vanilla};

    #[test]
    fn invalid_usage_has_stable_results() {
        let error = execute(&[]).expect_err("missing command");
        assert!(matches!(error, CliError::Usage(_)));
        assert_eq!(error.exit_code(), 2);
        let error = execute(&["--version".to_owned()]).expect_err("unknown command");
        assert!(matches!(error, CliError::Usage(_)));
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn index_vanilla_builds_and_refreshes_a_persistent_cache() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdc-cli-vanilla-cache-{nonce}"));
        let source = root.join("vanilla");
        fs::create_dir_all(source.join("events")).expect("fixture directory");
        fs::write(
            source.join("events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("fixture source");
        let output = root.join("cache/vanilla.pdcindex");
        let args = vec![
            "index".to_owned(),
            "vanilla".to_owned(),
            "--source".to_owned(),
            source.display().to_string(),
            "--output".to_owned(),
            output.display().to_string(),
        ];

        let first = execute(&args).expect("build cache");
        assert!(first.contains("indexed files: 1"));
        let first_cache = IndexCache::load(&output).expect("load first cache");
        assert_eq!(first_cache.metadata().indexed_files, 1);
        let second = execute(&args).expect("explicit refresh");
        assert!(second.contains("indexed files: 1"));
        let refreshed = IndexCache::load(&output).expect("load refreshed cache");
        assert_eq!(
            refreshed.metadata().source_fingerprint,
            first_cache.metadata().source_fingerprint
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn index_dependency_builds_a_cache_with_the_stable_root_identity() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdc-cli-dependency-cache-{nonce}"));
        let source = root.join("dependency");
        fs::create_dir_all(source.join("events")).expect("fixture directory");
        fs::write(
            source.join("events/definitions.txt"),
            "country_event = { id = dep.1 }\n",
        )
        .expect("fixture source");
        let output = root.join("cache/dependency.pdcindex");
        let args = vec![
            "index".to_owned(),
            "dependency".to_owned(),
            "--id".to_owned(),
            "dep-a".to_owned(),
            "--source".to_owned(),
            source.display().to_string(),
            "--output".to_owned(),
            output.display().to_string(),
        ];

        let summary = execute(&args).expect("build dependency cache");
        assert!(summary.contains("Dependency dep-a cache written to"));
        assert!(summary.contains("indexed files: 1"));
        let cache = IndexCache::load(&output).expect("load dependency cache");
        assert_eq!(cache.source_root().kind, SourceRootKind::Dependency);
        assert_eq!(
            cache.source_root().id,
            SourceRootId::new(super::stable_dependency_root_id("dep-a"))
        );
        assert_eq!(cache.metadata().indexed_files, 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn setup_vanilla_validates_indexes_and_persists_user_configuration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("Europa Universalis IV");
        for directory in game::eu4::INSTALL_DESCRIPTOR.validation_directories {
            fs::create_dir_all(source.join(directory)).expect("validation directory");
        }
        #[cfg(target_os = "windows")]
        let executable = source.join("eu4.exe");
        #[cfg(target_os = "linux")]
        let executable = source.join("eu4");
        #[cfg(target_os = "macos")]
        let executable = source.join("Europa Universalis IV.app/Contents/MacOS/eu4");
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture executable").expect("executable marker");
        fs::create_dir_all(source.join("events")).expect("indexed directory");
        fs::write(
            source.join("events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("fixture source");
        let paths = UserPaths {
            config_file: temporary.path().join("config/config.toml"),
            cache_root: temporary.path().join("cache"),
        };
        let output = setup_vanilla(
            &["--source".to_owned(), source.display().to_string()],
            &paths,
        )
        .expect("setup succeeds");
        assert!(output.contains("Europa Universalis IV configured"));
        let configuration =
            UserConfiguration::load(&paths.config_file).expect("load user configuration");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Configured));
        assert_eq!(
            game.vanilla_source.as_deref(),
            Some(
                dunce::canonicalize(&source)
                    .expect("canonical source")
                    .as_path()
            )
        );
        assert_eq!(game.resolved_via.as_deref(), Some("explicit"));
        let cache_path = game.vanilla_cache.as_ref().expect("cache path");
        let cache = IndexCache::load(cache_path).expect("load generated cache");
        assert_eq!(cache.metadata().game_id, "eu4");
        assert_eq!(cache.metadata().indexed_files, 1);
    }

    #[test]
    fn setup_vanilla_rejects_incomplete_installations() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("incomplete");
        fs::create_dir_all(&source).expect("source directory");
        #[cfg(target_os = "windows")]
        let executable = source.join("eu4.exe");
        #[cfg(target_os = "linux")]
        let executable = source.join("eu4");
        #[cfg(target_os = "macos")]
        let executable = source.join("Europa Universalis IV.app/Contents/MacOS/eu4");
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture executable").expect("executable marker");
        let paths = UserPaths {
            config_file: temporary.path().join("config/config.toml"),
            cache_root: temporary.path().join("cache"),
        };
        let error = setup_vanilla(
            &["--source".to_owned(), source.display().to_string()],
            &paths,
        )
        .expect_err("incomplete installation rejected");
        assert!(matches!(error, CliError::Discovery(_)));
        assert!(!paths.config_file.exists());
    }

    #[test]
    fn setup_vanilla_retains_a_discovered_source_when_indexing_fails() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("Europa Universalis IV");
        for directory in game::eu4::INSTALL_DESCRIPTOR.validation_directories {
            fs::create_dir_all(source.join(directory)).expect("validation directory");
        }
        #[cfg(target_os = "windows")]
        let executable = source.join("eu4.exe");
        #[cfg(target_os = "linux")]
        let executable = source.join("eu4");
        #[cfg(target_os = "macos")]
        let executable = source.join("Europa Universalis IV.app/Contents/MacOS/eu4");
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture executable").expect("executable marker");
        let paths = UserPaths {
            config_file: temporary.path().join("config/config.toml"),
            cache_root: temporary.path().join("cache"),
        };
        let cache_path = paths.vanilla_cache("eu4");
        fs::create_dir_all(cache_path.parent().expect("cache parent")).expect("cache directory");
        fs::write(&cache_path, b"not a ParadoxCode cache").expect("unrelated cache file");

        let error = setup_vanilla(
            &["--source".to_owned(), source.display().to_string()],
            &paths,
        )
        .expect_err("unrelated cache blocks indexing");
        assert!(matches!(error, CliError::Cache(_)));
        let configuration =
            UserConfiguration::load(&paths.config_file).expect("load failed setup state");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Failed));
        assert_eq!(
            game.vanilla_source.as_deref(),
            Some(
                dunce::canonicalize(source)
                    .expect("canonical source")
                    .as_path()
            )
        );
        assert!(game.vanilla_cache.is_none());
    }
}
