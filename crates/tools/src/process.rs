//! Child processes use argument vectors, inherited diagnostics and explicit working directories.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[cfg(windows)]
    {
        if matches!(program.as_ref().to_str(), Some("npm" | "npx")) {
            let mut result = Command::new("cmd");
            result.args(["/d", "/c"]).arg(program);
            return result;
        }
    }
    Command::new(program)
}
pub fn run(command: &mut Command) -> Result<(), String> {
    let description = format!("{command:?}");
    let status = command
        .status()
        .map_err(|e| format!("{description}: {e}"))?;
    if !status.success() {
        return Err(format!("{description} failed: {status}"));
    }
    Ok(())
}
pub fn capture(command: &mut Command) -> Result<Output, String> {
    let output = command.output().map_err(|e| format!("{command:?}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command:?} failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}
pub fn cargo(root: &Path, arguments: &[&str]) -> Result<(), String> {
    run(command("cargo").current_dir(root).args(arguments))
}
pub fn server(root: &Path, explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        let path = Path::new(path)
            .canonicalize()
            .map_err(|e| format!("--server {path}: {e}"))?;
        if !path.is_file() {
            return Err(format!("server is not a file: {}", path.display()));
        }
        return Ok(path);
    }
    build_server(root, false)
}

/// Use the executable Cargo actually produced, including for a shared target directory.
pub fn build_server(root: &Path, release: bool) -> Result<PathBuf, String> {
    let mut build = command("cargo");
    build.current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "pdc",
        "--bin",
        "paradoxcode",
        "--message-format=json-render-diagnostics",
    ]);
    if release {
        build.arg("--release");
    }
    build.stderr(std::process::Stdio::inherit());
    let output = capture(&mut build)?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|message| {
            (message["reason"] == "compiler-artifact" && message["target"]["name"] == "paradoxcode")
                .then(|| message["executable"].as_str().map(PathBuf::from))
                .flatten()
        })
        .ok_or_else(|| "Cargo did not report a paradoxcode executable".into())
}
