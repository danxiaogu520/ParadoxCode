//! Deterministic local reports and canonical output boundaries.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub fn json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}
pub fn write(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    crate::release::atomic_write(path, bytes.as_bytes()).map_err(|e| e.to_string())
}
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub fn file_hash(path: &Path) -> Result<String, String> {
    Ok(hash(
        &fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
    ))
}
fn canonical_output(path: &Path) -> Result<PathBuf, String> {
    let path = normalized_absolute(path)?;
    let mut parent = path.as_path();
    let mut tail = Vec::new();
    while !parent.exists() {
        tail.push(parent.file_name().ok_or("invalid output path")?.to_owned());
        parent = parent.parent().ok_or("invalid output parent")?;
    }
    let mut result = parent.canonicalize().map_err(|e| e.to_string())?;
    for component in tail.iter().rev() {
        result.push(component);
    }
    Ok(result)
}
fn normalized_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for component in std::path::absolute(path)
        .map_err(|e| e.to_string())?
        .components()
    {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    return Err("output traversal above filesystem root".into());
                }
            }
            Component::CurDir => {}
            component => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}
pub fn output(root: &Path, path: &Path, zone: &str) -> Result<PathBuf, String> {
    let base = root.join("target").join(zone);
    let resolved = canonical_output(path)?;
    if !normalized_absolute(path)?.starts_with(normalized_absolute(&base)?)
        || !resolved.starts_with(canonical_output(&base)?)
        || !resolved.starts_with(canonical_output(&root.join("target"))?)
    {
        return Err(format!(
            "generated output must stay under {}",
            base.display()
        ));
    }
    Ok(resolved)
}
pub fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(path: &Path, depth: usize, result: &mut Vec<PathBuf>) -> Result<(), String> {
        if depth > 64 || result.len() > 100_000 {
            return Err("source discovery limit exceeded".into());
        }
        let metadata =
            fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if metadata.is_file() {
            result.push(path.to_owned());
        } else if metadata.is_dir() {
            let mut entries = fs::read_dir(path)
                .map_err(|e| e.to_string())?
                .map(|e| e.map(|e| e.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            entries.sort();
            for entry in entries {
                walk(&entry, depth + 1, result)?;
            }
        }
        Ok(())
    }
    let mut result = Vec::new();
    walk(root, 0, &mut result)?;
    Ok(result)
}
pub fn text(bytes: &[u8]) -> (String, &'static str) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        (text.trim_start_matches('\u{feff}').to_owned(), "utf-8")
    } else {
        (
            encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
            "windows-1252",
        )
    }
}
pub fn ensure_complete(report: &Value) -> Result<(), String> {
    if report["status"] != "passed" || !report["tool_errors"].as_array().is_some_and(Vec::is_empty)
    {
        return Err("audit incomplete or contains tool errors".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_rejects_traversal_and_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(
            output(
                root.path(),
                &root.path().join("target/performance-results/a.json"),
                "performance-results"
            )
            .is_ok()
        );
        assert!(
            output(
                root.path(),
                &other.path().join("a.json"),
                "performance-results"
            )
            .is_err()
        );
        assert!(output(root.path(), &root.path().join("target/../a.json"), "").is_err());
        #[cfg(unix)]
        {
            fs::create_dir(root.path().join("target")).unwrap();
            std::os::unix::fs::symlink(
                other.path(),
                root.path().join("target/performance-results"),
            )
            .unwrap();
            assert!(output(root.path(), &other.path().join("a.json"), "").is_err());
        }
    }
}
