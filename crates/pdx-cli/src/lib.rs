//! User-facing command-line entry points.

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use pdx_game::{
    DiscoveryDepth, DiscoveryOptions, DiscoveryOutcome, DiscoveryToken, GameInstallDescriptor,
    UserConfigError, UserConfiguration, UserPaths, discover_installations, validate_installation,
};
use pdx_workspace::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaCacheError, VanillaIndexCache,
    WorkspaceChange, WorkspaceError,
};

const USAGE: &str = "usage:\n  pdx --version\n  pdx index vanilla --source <EU4 directory> --output <cache.pdxindex>\n  pdx setup vanilla [--game eu4] [--deep] [--root <directory>]... [--source <game directory>]";
const SUPPORTED_GAME_INSTALLATIONS: &[GameInstallDescriptor] = &[pdx_game_eu4::INSTALL_DESCRIPTOR];

/// Executes one `pdx` command and returns text intended for stdout.
pub fn execute_pdx(args: &[String]) -> Result<String, CliError> {
    match args {
        [argument] if argument == "--version" || argument == "-V" => Ok("pdx 0.1.0".to_owned()),
        [index, vanilla, rest @ ..] if index == "index" && vanilla == "vanilla" => {
            index_vanilla(rest)
        }
        [setup, vanilla, rest @ ..] if setup == "setup" && vanilla == "vanilla" => {
            let paths = UserPaths::platform()?;
            setup_vanilla(rest, &paths)
        }
        _ => Err(CliError::Usage(USAGE.to_owned())),
    }
}

fn setup_vanilla(args: &[String], paths: &UserPaths) -> Result<String, CliError> {
    let mut game = None::<String>;
    let mut source = None::<PathBuf>;
    let mut roots = Vec::<PathBuf>::new();
    let mut deep = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--deep" => {
                if deep {
                    return Err(CliError::Usage(
                        "option supplied more than once: --deep".to_owned(),
                    ));
                }
                deep = true;
                index += 1;
            }
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
                return Err(CliError::Usage(format!("unknown option: {flag}\n\n{USAGE}")));
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
        output.push(setup_game(*descriptor, source.clone(), roots.clone(), deep, paths)?);
    }
    Ok(output.join("\n\n"))
}

fn setup_game(
    descriptor: GameInstallDescriptor,
    source: Option<PathBuf>,
    roots: Vec<PathBuf>,
    deep: bool,
    paths: &UserPaths,
) -> Result<String, CliError> {
    let mut configuration = UserConfiguration::load(&paths.config_file)?;
    let previous = configuration.games.get(descriptor.game_id).cloned().unwrap_or_default();
    let explicit_search = source.is_some() || deep || !roots.is_empty();
    let candidates = if let Some(source) = source {
        let source = std::fs::canonicalize(&source).map_err(|error| CliError::Path {
            field: "--source",
            path: source,
            error,
        })?;
        if !validate_installation(&source, &descriptor) {
            return Err(CliError::Discovery(format!(
                "{} is not a valid {} installation; expected an executable and common, events, missions, decisions, and localisation directories",
                source.display(),
                descriptor.display_name
            )));
        }
        vec![source]
    } else {
        match previous
            .vanilla_source
            .filter(|source| !explicit_search && validate_installation(source, &descriptor))
        {
            Some(source) => vec![source],
            None => {
                let report = discover_installations(
                    &descriptor,
                    &DiscoveryOptions {
                        depth: if deep { DiscoveryDepth::Deep } else { DiscoveryDepth::Quick },
                        roots,
                        include_platform_locations: true,
                    },
                    &DiscoveryToken::new(),
                );
                if report.cancelled {
                    return Err(CliError::Discovery("Vanilla discovery was cancelled".to_owned()));
                }
                report.installations
            }
        }
    };

    let selected = match candidates.as_slice() {
        [] => {
            let game = configuration.games.entry(descriptor.game_id.to_owned()).or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::NotFound);
            configuration.save(&paths.config_file)?;
            return Err(CliError::Discovery(format!(
                "no valid {} installation was found; retry with --deep, --root, or --source",
                descriptor.display_name
            )));
        }
        [only] => only.clone(),
        many => select_candidate(descriptor.display_name, many)?,
    };

    let cache_path = paths.vanilla_cache(descriptor.game_id);
    let summary = match build_eu4_cache(&selected, &cache_path) {
        Ok(summary) => summary,
        Err(error) => {
            let game = configuration.games.entry(descriptor.game_id.to_owned()).or_default();
            game.auto_discovery_attempted = true;
            game.discovery_outcome = Some(DiscoveryOutcome::Failed);
            game.vanilla_source = Some(selected);
            configuration.save(&paths.config_file)?;
            return Err(error);
        }
    };
    let game = configuration.games.entry(descriptor.game_id.to_owned()).or_default();
    game.auto_discovery_attempted = true;
    game.discovery_outcome = Some(DiscoveryOutcome::Configured);
    game.vanilla_source = Some(selected.clone());
    game.vanilla_cache = Some(cache_path.clone());
    configuration.save(&paths.config_file)?;
    Ok(format!(
        "{} configured\nsource: {}\ncache: {}\n{summary}",
        descriptor.display_name,
        selected.display(),
        cache_path.display()
    ))
}

fn select_candidate(display_name: &str, candidates: &[PathBuf]) -> Result<PathBuf, CliError> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(CliError::Discovery(format!(
            "multiple valid {display_name} installations were found:\n{}\nrerun with --source <directory>",
            candidates
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }
    eprintln!("Multiple valid {display_name} installations were found:");
    for (index, candidate) in candidates.iter().enumerate() {
        eprintln!("  {}) {}", index + 1, candidate.display());
    }
    eprint!("Select an installation [1-{}]: ", candidates.len());
    io::stderr().flush().map_err(CliError::Interactive)?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).map_err(CliError::Interactive)?;
    let selected = answer
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|selected| (1..=candidates.len()).contains(selected))
        .ok_or_else(|| CliError::Discovery("invalid installation selection".to_owned()))?;
    Ok(candidates[selected - 1].clone())
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
            _ => return Err(CliError::Usage(format!("unknown option: {flag}\n\n{USAGE}"))),
        };
        if target.replace(PathBuf::from(value)).is_some() {
            return Err(CliError::Usage(format!("option supplied more than once: {flag}")));
        }
        index += 2;
    }
    let source = required_path(source, "--source")?;
    let output = required_path(output, "--output")?;
    let source = std::fs::canonicalize(&source).map_err(|error| CliError::Path {
        field: "--source",
        path: source,
        error,
    })?;
    if !source.is_dir() {
        return Err(CliError::Usage(format!("--source is not a directory: {}", source.display())));
    }

    build_eu4_cache(&source, &output)
}

fn build_eu4_cache(source: &std::path::Path, output: &std::path::Path) -> Result<String, CliError> {
    let rules = pdx_game_eu4::first_party_rules()?;
    let mut host = AnalysisHost::with_profile(rules, pdx_game_eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        source.to_owned(),
    )]));
    let report = host.refresh_source_roots()?;
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot())?;
    cache.save(output)?;
    Ok(format!(
        "Vanilla cache written to {}\nindexed files: {}\nskipped entries: {}\nsource fingerprint: {}\nrules hash: {}",
        output.display(),
        cache.metadata().indexed_files,
        report.skipped_entries,
        cache.metadata().source_fingerprint,
        cache.metadata().rule_hash
    ))
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
    Path { field: &'static str, path: PathBuf, error: std::io::Error },
    /// The rules artifact failed validation.
    Rules(pdx_rules::RulesError),
    /// The one-shot Vanilla source scan failed.
    Workspace(WorkspaceError),
    /// The persistent cache could not be built or written.
    Cache(VanillaCacheError),
    /// Installation discovery or candidate selection did not produce a usable source.
    Discovery(String),
    /// User-local discovery configuration failed.
    UserConfig(UserConfigError),
    /// Interactive candidate selection failed.
    Interactive(std::io::Error),
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
            | Self::UserConfig(_)
            | Self::Interactive(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Path { field, path, error } => {
                write!(formatter, "cannot resolve {field} {}: {error}", path.display())
            }
            Self::Rules(error) => write!(formatter, "rules artifact error: {error}"),
            Self::Workspace(error) => write!(formatter, "Vanilla indexing error: {error}"),
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Discovery(message) => formatter.write_str(message),
            Self::UserConfig(error) => write!(formatter, "{error}"),
            Self::Interactive(error) => write!(formatter, "interactive selection failed: {error}"),
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
            Self::Interactive(error) => Some(error),
            Self::Usage(_) | Self::Discovery(_) => None,
        }
    }
}

impl From<pdx_rules::RulesError> for CliError {
    fn from(error: pdx_rules::RulesError) -> Self {
        Self::Rules(error)
    }
}

impl From<WorkspaceError> for CliError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<VanillaCacheError> for CliError {
    fn from(error: VanillaCacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<UserConfigError> for CliError {
    fn from(error: UserConfigError) -> Self {
        Self::UserConfig(error)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pdx_game::{DiscoveryOutcome, UserConfiguration, UserPaths};
    use pdx_workspace::VanillaIndexCache;

    use super::{CliError, execute_pdx, setup_vanilla};

    #[test]
    fn version_and_invalid_usage_have_stable_results() {
        assert_eq!(execute_pdx(&["--version".to_owned()]).expect("version"), "pdx 0.1.0");
        let error = execute_pdx(&[]).expect_err("missing command");
        assert!(matches!(error, CliError::Usage(_)));
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn index_vanilla_builds_and_refreshes_a_persistent_cache() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-cli-vanilla-cache-{nonce}"));
        let source = root.join("vanilla");
        fs::create_dir_all(source.join("common/events")).expect("fixture directory");
        fs::write(
            source.join("common/events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("fixture source");
        let output = root.join("cache/vanilla.pdxindex");
        let args = vec![
            "index".to_owned(),
            "vanilla".to_owned(),
            "--source".to_owned(),
            source.display().to_string(),
            "--output".to_owned(),
            output.display().to_string(),
        ];

        let first = execute_pdx(&args).expect("build cache");
        assert!(first.contains("indexed files: 1"));
        let first_cache = VanillaIndexCache::load(&output).expect("load first cache");
        assert_eq!(first_cache.metadata().indexed_files, 1);
        let second = execute_pdx(&args).expect("explicit refresh");
        assert!(second.contains("indexed files: 1"));
        let refreshed = VanillaIndexCache::load(&output).expect("load refreshed cache");
        assert_eq!(
            refreshed.metadata().source_fingerprint,
            first_cache.metadata().source_fingerprint
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn setup_vanilla_validates_indexes_and_persists_user_configuration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("Europa Universalis IV");
        for directory in pdx_game_eu4::INSTALL_DESCRIPTOR.validation_directories {
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
        fs::create_dir_all(source.join("common/events")).expect("indexed directory");
        fs::write(
            source.join("common/events/definitions.txt"),
            "country_event = { id = vanilla.1 }\n",
        )
        .expect("fixture source");
        let paths = UserPaths {
            config_file: temporary.path().join("config/config.toml"),
            cache_root: temporary.path().join("cache"),
        };
        let output = setup_vanilla(&["--source".to_owned(), source.display().to_string()], &paths)
            .expect("setup succeeds");
        assert!(output.contains("Europa Universalis IV configured"));
        let configuration =
            UserConfiguration::load(&paths.config_file).expect("load user configuration");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Configured));
        assert_eq!(
            game.vanilla_source.as_deref(),
            Some(fs::canonicalize(&source).expect("canonical source").as_path())
        );
        let cache_path = game.vanilla_cache.as_ref().expect("cache path");
        let cache = VanillaIndexCache::load(cache_path).expect("load generated cache");
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
        let error = setup_vanilla(&["--source".to_owned(), source.display().to_string()], &paths)
            .expect_err("incomplete installation rejected");
        assert!(matches!(error, CliError::Discovery(_)));
        assert!(!paths.config_file.exists());
    }

    #[test]
    fn setup_vanilla_retains_a_discovered_source_when_indexing_fails() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("Europa Universalis IV");
        for directory in pdx_game_eu4::INSTALL_DESCRIPTOR.validation_directories {
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

        let error = setup_vanilla(&["--source".to_owned(), source.display().to_string()], &paths)
            .expect_err("unrelated cache blocks indexing");
        assert!(matches!(error, CliError::Cache(_)));
        let configuration =
            UserConfiguration::load(&paths.config_file).expect("load failed setup state");
        let game = configuration.games.get("eu4").expect("EU4 configuration");
        assert!(game.auto_discovery_attempted);
        assert_eq!(game.discovery_outcome, Some(DiscoveryOutcome::Failed));
        assert_eq!(
            game.vanilla_source.as_deref(),
            Some(fs::canonicalize(source).expect("canonical source").as_path())
        );
        assert!(game.vanilla_cache.is_none());
    }
}
