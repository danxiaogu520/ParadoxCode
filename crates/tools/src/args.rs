//! Shared strict argument parsing for developer commands.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Args {
    pub values: BTreeMap<String, Vec<String>>,
    pub positional: Vec<String>,
    pub forwarded: Vec<String>,
}
impl Args {
    pub fn parse(args: &[String], values: &[&str], flags: &[&str]) -> Result<Self, String> {
        let mut result = Self::default();
        let mut i = 0;
        while i < args.len() {
            let key = &args[i];
            if key == "--" {
                result.forwarded.extend_from_slice(&args[i + 1..]);
                break;
            }
            if values.contains(&key.as_str()) {
                i += 1;
                let value = args
                    .get(i)
                    .filter(|s| !s.starts_with("--"))
                    .ok_or_else(|| format!("missing value for {key}"))?;
                result
                    .values
                    .entry(key.clone())
                    .or_default()
                    .push(value.clone());
            } else if flags.contains(&key.as_str()) || key == "--help" || key == "-h" {
                if result.values.insert(key.clone(), Vec::new()).is_some() {
                    return Err(format!("duplicate option: {key}"));
                }
            } else if key.starts_with('-') {
                return Err(format!("unknown option: {key}"));
            } else {
                result.positional.push(key.clone());
            }
            i += 1;
        }
        for (key, list) in &result.values {
            if list.len() > 1 && key != "--dependency" {
                return Err(format!("duplicate option: {key}"));
            }
        }
        Ok(result)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key)?.first().map(String::as_str)
    }
    pub fn environment(&mut self, bindings: &[(&str, &str)]) {
        for (key, name) in bindings {
            if !self.values.contains_key(*key)
                && let Ok(value) = std::env::var(name)
                && !value.trim().is_empty()
            {
                self.values.insert((*key).into(), vec![value]);
            }
        }
    }
    pub fn required(&self, key: &str) -> Result<&str, String> {
        self.get(key)
            .ok_or_else(|| format!("missing required option: {key}"))
    }
    pub fn flag(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    pub fn help(&self) -> bool {
        self.flag("--help") || self.flag("-h")
    }
    pub fn number(&self, key: &str, default: usize) -> Result<usize, String> {
        match self.get(key) {
            None => Ok(default),
            Some(value) => value
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{key} must be a positive integer")),
        }
    }
    pub fn path(&self, key: &str) -> Option<PathBuf> {
        self.get(key).map(PathBuf::from)
    }
    pub fn root(&self) -> Result<PathBuf, String> {
        let path = self
            .path("--repo")
            .or_else(|| self.path("--root"))
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        path.canonicalize()
            .map_err(|e| format!("repository {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_typos_missing_values_and_duplicates() {
        for args in [
            vec!["--typo"],
            vec!["--server"],
            vec!["--server", "a", "--server", "b"],
        ] {
            assert!(
                Args::parse(
                    &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    &["--server"],
                    &[]
                )
                .is_err()
            );
        }
    }
}
