//! `rules-migrate` — one-shot legacy → rules-v2 rules source conversion.
//!
//! Usage: `rules-migrate [--source DIR] [--out DIR] [--report PATH]`.
//! See `crates/tools/src/migrate.rs` and `docs/rules-redesign.md` §6.

use std::path::PathBuf;
use std::process::ExitCode;

use tools::migrate::{self, Options};

fn main() -> ExitCode {
    let mut options = Options {
        source: PathBuf::from("rules/eu4"),
        out: PathBuf::from("rules/eu4-v2"),
        report: PathBuf::from("docs/rules-migrate-report.md"),
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--source" => match arguments.next() {
                Some(path) => options.source = PathBuf::from(path),
                None => return fail("--source needs a directory"),
            },
            "--out" => match arguments.next() {
                Some(path) => options.out = PathBuf::from(path),
                None => return fail("--out needs a directory"),
            },
            "--report" => match arguments.next() {
                Some(path) => options.report = PathBuf::from(path),
                None => return fail("--report needs a path"),
            },
            "--help" | "-h" => {
                println!(
                    "Usage: rules-migrate [--source DIR] [--out DIR] [--report PATH]\n\
                     \n\
                     Converts the legacy rules corpus into a rules-v2 source tree."
                );
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unexpected argument `{other}`")),
        }
    }
    match migrate::run(&options) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rules-migrate: {error}");
            ExitCode::from(1)
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("rules-migrate: {message}");
    ExitCode::from(2)
}
