//! User-facing command-line entry points.

use std::fmt;
use std::path::PathBuf;

use pdx_rules::RuleSet;
use pdx_workspace::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaCacheError, VanillaIndexCache,
    WorkspaceChange, WorkspaceError,
};

const USAGE: &str = "usage:\n  pdx --version\n  pdx index vanilla --rules <eu4.pdxrules> --source <EU4 directory> --output <cache.pdxindex>";

/// Executes one `pdx` command and returns text intended for stdout.
pub fn execute_pdx(args: &[String]) -> Result<String, CliError> {
    match args {
        [argument] if argument == "--version" || argument == "-V" => Ok("pdx 0.1.0".to_owned()),
        [index, vanilla, rest @ ..] if index == "index" && vanilla == "vanilla" => {
            index_vanilla(rest)
        }
        _ => Err(CliError::Usage(USAGE.to_owned())),
    }
}

fn index_vanilla(args: &[String]) -> Result<String, CliError> {
    let mut rules = None;
    let mut source = None;
    let mut output = None;
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError::Usage(format!("missing value for {flag}\n\n{USAGE}")))?;
        let target = match flag.as_str() {
            "--rules" => &mut rules,
            "--source" => &mut source,
            "--output" => &mut output,
            _ => return Err(CliError::Usage(format!("unknown option: {flag}\n\n{USAGE}"))),
        };
        if target.replace(PathBuf::from(value)).is_some() {
            return Err(CliError::Usage(format!("option supplied more than once: {flag}")));
        }
        index += 2;
    }
    let rules_path = required_path(rules, "--rules")?;
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

    let rules = RuleSet::load(&rules_path)?;
    rules.ensure_game(pdx_game_eu4::GAME_ID)?;
    let mut host = AnalysisHost::with_profile(rules, pdx_game_eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        source,
    )]));
    let report = host.refresh_source_roots()?;
    let cache = VanillaIndexCache::from_snapshot(&host.snapshot())?;
    cache.save(&output)?;
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
}

impl CliError {
    /// Returns `2` for usage mistakes and `1` for runtime failures.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Path { .. } | Self::Rules(_) | Self::Workspace(_) | Self::Cache(_) => 1,
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
            Self::Usage(_) => None,
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use pdx_workspace::VanillaIndexCache;

    use super::{CliError, execute_pdx};

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
        let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4.pdxrules");
        let args = vec![
            "index".to_owned(),
            "vanilla".to_owned(),
            "--rules".to_owned(),
            rules.display().to_string(),
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
}
