use std::path::PathBuf;

use pdx_cwt::{ImportOptions, import_with_options};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), pdx_cwt::ImportError> {
    let mut arguments = std::env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(pdx_cwt::ImportError::Arguments("usage: pdx-cwt import --source <dir> --output <rules/eu4.pdxrules> [--manifest <path>] [--report <path>]".to_owned()));
    };
    if command != "import" {
        return Err(pdx_cwt::ImportError::Arguments(format!(
            "unknown command: {}",
            command.to_string_lossy()
        )));
    }
    let mut source = None;
    let mut output = None;
    let mut manifest = None;
    let mut report = None;
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or_else(|| {
            pdx_cwt::ImportError::Arguments(format!(
                "missing value for {}",
                argument.to_string_lossy()
            ))
        })?;
        match argument.to_string_lossy().as_ref() {
            "--source" => source = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--manifest" => manifest = Some(PathBuf::from(value)),
            "--report" => report = Some(PathBuf::from(value)),
            other => {
                return Err(pdx_cwt::ImportError::Arguments(format!("unknown option: {other}")));
            }
        }
    }
    let options = ImportOptions {
        source: source
            .ok_or_else(|| pdx_cwt::ImportError::Arguments("--source is required".to_owned()))?,
        output: output
            .ok_or_else(|| pdx_cwt::ImportError::Arguments("--output is required".to_owned()))?,
        manifest,
        report,
    };
    let report = import_with_options(&options)?;
    println!(
        "imported {} CWT files into {} (rule_hash={})",
        report.input_count,
        report.output.display(),
        report.rule_hash
    );
    Ok(())
}
