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
    let name = if cfg!(windows) {
        "paradoxcode.exe"
    } else {
        "paradoxcode"
    };
    let path = root.join("target/debug").join(name);
    cargo(
        root,
        &["build", "--locked", "-p", "pdc", "--bin", "paradoxcode"],
    )?;
    Ok(path)
}
