//! Shared packaging CLI for full and lightweight tools builds.
use crate::cli::CliError;
use std::path::PathBuf;
const USAGE: &str = "tools release package --version VERSION --target TARGET --binary PATH --output-dir PATH | verify --version VERSION --directory PATH";

pub fn execute(sub: &str, args: &[String]) -> Result<String, CliError> {
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
