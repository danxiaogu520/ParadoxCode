//! `rulec` — the rules-v2 source compiler front end (see
//! `docs/rules-language.md` §10.3).
//!
//! Subcommands:
//!
//! - `rulec check <source-dir>`: parse the rules sources listed by the
//!   directory's `manifest.json` and run the compile-time semantic checks
//!   (§10.1), printing one diagnostic per line. Exits non-zero on errors.
//! - `rulec schema [--output PATH]`: write the JSON Schema of the source
//!   language (§10.2) for editor completion and validation.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rules::compile;
use rules::source::{self, RuleFile};

const DEFAULT_SCHEMA_PATH: &str = "rules/rules-language.schema.json";

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(command) = arguments.next() else {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    };
    match command.as_str() {
        "check" => run_check(&arguments.collect::<Vec<_>>()),
        "schema" => run_schema(&arguments.collect::<Vec<_>>()),
        "--help" | "-h" | "help" => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("rulec: unknown command `{other}`\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

fn usage() -> String {
    format!(
        "Usage: rulec <command> [options]\n\
         \n\
         Commands:\n\
         \x20 check <source-dir>      parse and semantically check a rules source directory\n\
         \x20 schema [--output PATH]  write the rules-language JSON Schema\n\
         \x20                        (default {DEFAULT_SCHEMA_PATH})\n\
         \x20 help                   show this help"
    )
}

fn run_check(arguments: &[String]) -> ExitCode {
    let mut source_dir: Option<PathBuf> = None;
    for argument in arguments {
        match argument.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') && source_dir.is_none() => {
                source_dir = Some(PathBuf::from(other));
            }
            other => {
                eprintln!("rulec check: unexpected argument `{other}`\n\n{}", usage());
                return ExitCode::from(2);
            }
        }
    }
    let Some(source_dir) = source_dir else {
        eprintln!("rulec check: missing <source-dir>\n\n{}", usage());
        return ExitCode::from(2);
    };
    let (sources, manifest_identity) = match load_sources(&source_dir) {
        Ok(loaded) => loaded,
        Err(failure) => {
            eprintln!("rulec check: {failure}");
            return ExitCode::from(1);
        }
    };
    let diagnostics = compile::check(&sources);
    let mut errors = 0usize;
    let mut warnings = 0usize;
    for diagnostic in &diagnostics {
        match diagnostic.severity {
            source::Severity::Error => errors += 1,
            source::Severity::Warning | source::Severity::Info => warnings += 1,
        }
        let column = diagnostic
            .column
            .map_or_else(String::new, |column| format!(":{column}"));
        println!(
            "{}{}{}: {} {}: {}",
            diagnostic.file,
            diagnostic.pointer,
            column,
            diagnostic.severity,
            diagnostic.code,
            diagnostic.message
        );
    }
    let identity = match (
        &manifest_identity.game_id,
        &manifest_identity.target_game_version,
    ) {
        (Some(game), Some(version)) => format!(" ({game} {version})"),
        (Some(game), None) => format!(" ({game})"),
        (None, Some(version)) => format!(" ({version})"),
        (None, None) => String::new(),
    };
    println!(
        "rulec check{identity}: {} file(s), {} error(s), {} warning(s)",
        sources.len(),
        errors,
        warnings
    );
    if errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// The `manifest.json` of a rules source directory: the file list plus
/// identity metadata. Tooling configuration, not language.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceManifest {
    #[serde(default)]
    game_id: Option<String>,
    #[serde(default)]
    target_game_version: Option<String>,
    files: Vec<String>,
}

fn load_sources(source_dir: &Path) -> Result<(Vec<(String, RuleFile)>, SourceManifest), String> {
    let manifest_path = source_dir.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest: SourceManifest = serde_json::from_str(&manifest_text)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let mut sources = Vec::with_capacity(manifest.files.len());
    for name in &manifest.files {
        let path = source_dir.join(name);
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let file: RuleFile =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        sources.push((name.clone(), file));
    }
    Ok((sources, manifest))
}

fn run_schema(arguments: &[String]) -> ExitCode {
    let mut output = PathBuf::from(DEFAULT_SCHEMA_PATH);
    let mut iterator = arguments.iter();
    while let Some(argument) = iterator.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            "--output" => match iterator.next() {
                Some(path) => output = PathBuf::from(path),
                None => {
                    eprintln!("rulec schema: `--output` needs a path");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("rulec schema: unexpected argument `{other}`\n\n{}", usage());
                return ExitCode::from(2);
            }
        }
    }
    let rendered = source::json_schema_pretty();
    if let Err(failure) = std::fs::write(&output, &rendered) {
        eprintln!("rulec schema: {}: {failure}", output.display());
        return ExitCode::from(1);
    }
    println!("rulec schema: wrote {}", output.display());
    ExitCode::SUCCESS
}
