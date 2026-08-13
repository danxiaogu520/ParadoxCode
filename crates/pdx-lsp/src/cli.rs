//! User-facing command-line entry points.

use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use pdx_engine::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaCacheError, VanillaIndexCache,
    WorkspaceChange, WorkspaceError,
};
use pdx_game::{
    DiscoveryDepth, DiscoveryOptions, DiscoveryOutcome, DiscoveryToken, GameInstallDescriptor,
    UserConfigError, UserConfiguration, UserPaths, discover_installations, validate_installation,
    validate_installation_for_source,
};

use crate::workspace::stable_dependency_root_id;

const USAGE: &str = "usage:\n  pdx --version\n  pdx index vanilla --source <EU4 directory> --output <cache.pdxindex>\n  pdx index dependency --id <id> --source <directory> --output <cache.pdxindex>\n  pdx setup vanilla [--game eu4] [--deep] [--root <directory>]... [--source <game directory>]\n  pdx check policy|zed|release|grammar-fuzz|all [--root <repository root>]\n  pdx release package --version <semver> --target <target> --binary <path> --output-dir <path> [--root <repository root>]\n  pdx release verify --version <semver> --directory <path> [--root <repository root>]\n  pdx dev prepare-manifest [--root <repository root>]";
const SUPPORTED_GAME_INSTALLATIONS: &[GameInstallDescriptor] = &[pdx_game::eu4::INSTALL_DESCRIPTOR];

/// Executes one `pdx` command and returns text intended for stdout.
pub fn execute_pdx(args: &[String]) -> Result<String, CliError> {
    match args {
        [argument] if argument == "--version" || argument == "-V" => Ok("pdx 0.1.0".to_owned()),
        [index, vanilla, rest @ ..] if index == "index" && vanilla == "vanilla" => {
            index_vanilla(rest)
        }
        [index, dependency, rest @ ..] if index == "index" && dependency == "dependency" => {
            index_dependency(rest)
        }
        [setup, vanilla, rest @ ..] if setup == "setup" && vanilla == "vanilla" => {
            let paths = UserPaths::platform()?;
            setup_vanilla(rest, &paths)
        }
        [check, sub, rest @ ..] if check == "check" => execute_check(sub, rest),
        [release, sub, rest @ ..] if release == "release" => execute_release(sub, rest),
        [dev, prepare_manifest, rest @ ..]
            if dev == "dev" && prepare_manifest == "prepare-manifest" =>
        {
            dev_prepare_manifest(rest)
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
            deep,
            paths,
        )?);
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
    let previous = configuration
        .games
        .get(descriptor.game_id)
        .cloned()
        .unwrap_or_default();
    let explicit_search = source.is_some() || deep || !roots.is_empty();
    let candidates = if let Some(source) = source {
        let source = std::fs::canonicalize(&source).map_err(|error| CliError::Path {
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
                        depth: if deep {
                            DiscoveryDepth::Deep
                        } else {
                            DiscoveryDepth::Quick
                        },
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

    let selected = match candidates.as_slice() {
        [] => {
            let game = configuration
                .games
                .entry(descriptor.game_id.to_owned())
                .or_default();
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
    let summary = match build_cache(
        SourceRoot::new(
            SourceRootId::new(0),
            SourceRootKind::Vanilla,
            selected.clone(),
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
            game.vanilla_source = Some(selected);
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
    io::stdin()
        .read_line(&mut answer)
        .map_err(CliError::Interactive)?;
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
    let source = std::fs::canonicalize(&source).map_err(|error| CliError::Path {
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
    let source = std::fs::canonicalize(&source).map_err(|error| CliError::Path {
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
    let rules = pdx_game::eu4::first_party_rules()?;
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![root]));
    let scan_started = Instant::now();
    let report = host.refresh_source_roots()?;
    let scan_elapsed = scan_started.elapsed();
    let cache_started = Instant::now();
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot())?;
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
            | Self::UserConfig(_)
            | Self::Interactive(_)
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
            Self::UserConfig(error) => write!(formatter, "{error}"),
            Self::Interactive(error) => write!(formatter, "interactive selection failed: {error}"),
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
            Self::Interactive(error) => Some(error),
            Self::Usage(_) | Self::Discovery(_) | Self::CheckFailed => None,
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
        "zed" => crate::check::check_zed_extension(&root),
        "release" => crate::check::check_release_artifact(&root),
        "grammar-fuzz" => crate::check::check_grammar_fuzz(&root),
        "all" => {
            let mut all = Vec::new();
            all.extend(crate::check::check_project_policy(&root));
            all.extend(crate::check::check_zed_extension(&root));
            all.extend(crate::check::check_release_artifact(&root));
            all
        }
        _ => return Err(CliError::Usage(format!("unknown check: {sub}\n\n{USAGE}"))),
    };
    let all_pass = crate::check::report(&results);
    if all_pass {
        Ok(format!("pdx check {sub} passed"))
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

fn dev_prepare_manifest(args: &[String]) -> Result<String, CliError> {
    let root = parse_root_flag(args)?;
    let manifest_path = root.join("editors/zed/extension.toml");
    let mut text = fs::read_to_string(&manifest_path).map_err(|error| CliError::Path {
        field: "manifest",
        path: manifest_path.clone(),
        error,
    })?;

    let repo = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "remote", "get-url", "origin"])
        .output()
        .map_err(|error| CliError::Usage(format!("cannot get git remote: {error}")))?;
    if !repo.status.success() {
        return Err(CliError::Usage(
            "the Zed development manifest needs an origin remote so Zed can fetch grammar sources"
                .to_owned(),
        ));
    }
    let repository = String::from_utf8_lossy(&repo.stdout).trim().to_owned();

    let rev = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .map_err(|error| CliError::Usage(format!("cannot get git revision: {error}")))?;
    if !rev.status.success() {
        return Err(CliError::Usage(
            "the Zed development manifest needs a Git checkout so its grammar revision can be pinned"
                .to_owned(),
        ));
    }
    let revision = String::from_utf8_lossy(&rev.stdout).trim().to_owned();

    {
        let (grammar_id, grammar_dir_name) = ("eu4", "eu4");
        let table = format!("[grammars.{grammar_id}]");
        if let Some(start) = text.find(&table) {
            let next = text[start + table.len()..]
                .find("\n[")
                .map(|offset| start + table.len() + offset);
            let end = next.unwrap_or(text.len());
            let block = &text[start..end];
            let mut new_block = block
                .replacen(
                    &extract_toml_value(block, "repository"),
                    &format!(r#""{repository}""#),
                    1,
                )
                .replacen(
                    &extract_toml_value(block, "rev"),
                    &format!(r#""{revision}""#),
                    1,
                )
                .replacen(
                    &extract_toml_value(block, "path"),
                    &format!(r#""grammars/tree-sitter-{grammar_dir_name}""#),
                    1,
                );
            if !new_block.ends_with('\n') {
                new_block.push('\n');
            }
            text = format!("{}{new_block}{}", &text[..start], &text[end..]);
        }
    }
    fs::write(&manifest_path, &text).map_err(|error| CliError::Path {
        field: "manifest",
        path: manifest_path.clone(),
        error,
    })?;
    Ok(format!(
        "Updated {} for this checkout.",
        manifest_path.display()
    ))
}

fn extract_toml_value(block: &str, key: &str) -> String {
    for line in block.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key} = ")) {
            return rest.to_owned();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pdx_engine::{SourceRootId, SourceRootKind, VanillaIndexCache};
    use pdx_game::{DiscoveryOutcome, UserConfiguration, UserPaths};

    use super::{CliError, execute_pdx, setup_vanilla};

    #[test]
    fn version_and_invalid_usage_have_stable_results() {
        assert_eq!(
            execute_pdx(&["--version".to_owned()]).expect("version"),
            "pdx 0.1.0"
        );
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
    fn index_dependency_builds_a_cache_with_the_stable_root_identity() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-cli-dependency-cache-{nonce}"));
        let source = root.join("dependency");
        fs::create_dir_all(source.join("common/events")).expect("fixture directory");
        fs::write(
            source.join("common/events/definitions.txt"),
            "country_event = { id = dep.1 }\n",
        )
        .expect("fixture source");
        let output = root.join("cache/dependency.pdxindex");
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

        let summary = execute_pdx(&args).expect("build dependency cache");
        assert!(summary.contains("Dependency dep-a cache written to"));
        assert!(summary.contains("indexed files: 1"));
        let cache = VanillaIndexCache::load(&output).expect("load dependency cache");
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
        for directory in pdx_game::eu4::INSTALL_DESCRIPTOR.validation_directories {
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
                fs::canonicalize(&source)
                    .expect("canonical source")
                    .as_path()
            )
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
        for directory in pdx_game::eu4::INSTALL_DESCRIPTOR.validation_directories {
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
                fs::canonicalize(source)
                    .expect("canonical source")
                    .as_path()
            )
        );
        assert!(game.vanilla_cache.is_none());
    }
}
