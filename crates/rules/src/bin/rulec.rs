//! `rulec` — the rule-source compiler front end (see
//! `docs/rules-language.md` §10.3).
//!
//! Subcommands:
//!
//! - `rulec check <source-dir>`: recursively parse the rule sources under the
//!   directory and run the compile-time semantic checks
//!   (§10.1), printing one diagnostic per line. Exits non-zero on errors.
//! - `rulec fmt <source-dir> [--check] [--expanded]`: format source files,
//!   optionally expanding defaults or checking without writing.
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
        "fmt" => run_format(&arguments.collect::<Vec<_>>()),
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
         \x20 fmt <source-dir> [--check] [--expanded]\n\
         \x20                        format sources or check their canonical form\n\
         \x20 schema [--output PATH]  write the rules-language JSON Schema\n\
         \x20                        (default {DEFAULT_SCHEMA_PATH})\n\
         \x20 help                   show this help"
    )
}

fn run_format(arguments: &[String]) -> ExitCode {
    let mut source_dir = None;
    let mut expanded = false;
    let mut check = false;
    for argument in arguments {
        match argument.as_str() {
            "--expanded" if !expanded => expanded = true,
            "--check" if !check => check = true,
            "--help" | "-h" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') && source_dir.is_none() => {
                source_dir = Some(PathBuf::from(other))
            }
            other => {
                eprintln!("rulec fmt: unexpected argument `{other}`\n\n{}", usage());
                return ExitCode::from(2);
            }
        }
    }
    let Some(source_dir) = source_dir else {
        eprintln!("rulec fmt: missing <source-dir>\n\n{}", usage());
        return ExitCode::from(2);
    };
    let prepare = || -> Result<Vec<(PathBuf, String)>, String> {
        let sources =
            rules::bundle::load_directory(&source_dir).map_err(|error| error.to_string())?;
        let mut changes = Vec::new();
        for (name, source) in sources.files {
            let path = source_dir.join(name);
            let formatted =
                rules::format::render(&source, expanded).map_err(|error| error.to_string())?;
            let current = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if current != formatted {
                changes.push((path, formatted));
            }
        }
        Ok(changes)
    };
    let changes = match prepare() {
        Ok(changes) => changes,
        Err(error) => {
            eprintln!("rulec fmt: {error}");
            return ExitCode::from(1);
        }
    };
    for (path, formatted) in &changes {
        if check {
            println!("needs formatting: {}", path.display());
        } else if let Err(error) = std::fs::write(path, formatted) {
            eprintln!("rulec fmt: {}: {error}", path.display());
            return ExitCode::from(1);
        }
    }
    println!(
        "rulec fmt: {} file(s) {}",
        changes.len(),
        if check {
            "need formatting"
        } else {
            "formatted"
        }
    );
    if check && !changes.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
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
    let (sources, package_identity) = match load_sources(&source_dir) {
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
    let identity = match &package_identity.target_game_version {
        Some(version) => format!(" ({} {version})", package_identity.game_id),
        None => format!(" ({})", package_identity.game_id),
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

/// Package identity from game.json, separate from the rule language.
type SourceIdentity = rules::bundle::PackageIdentity;

fn load_sources(source_dir: &Path) -> Result<(Vec<(String, RuleFile)>, SourceIdentity), String> {
    let loaded = rules::bundle::load_directory(source_dir).map_err(|error| error.to_string())?;
    Ok((loaded.files, loaded.identity))
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
