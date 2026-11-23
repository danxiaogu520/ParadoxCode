//! Real-workspace replay keeps dependency order and gives refresh workers private index copies.
use crate::{args::Args, report};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) struct Dependencies {
    pub options: Vec<Value>,
    pub inventory: Vec<Value>,
    _copies: Option<tempfile::TempDir>,
}

impl Dependencies {
    pub fn new(args: &Args, relative_to: &Path) -> Result<Self, String> {
        let bindings = |flag: &str| -> Result<Vec<(String, PathBuf)>, String> {
            args.values
                .get(flag)
                .into_iter()
                .flatten()
                .map(|s| {
                    let (id, path) = s
                        .split_once('=')
                        .filter(|(id, path)| !id.trim().is_empty() && !path.is_empty())
                        .ok_or_else(|| format!("{flag} needs ID=PATH"))?;
                    Ok((id.to_owned(), PathBuf::from(path)))
                })
                .collect()
        };
        let mut indexes = BTreeMap::new();
        for (id, path) in bindings("--dependency-index")? {
            if indexes.insert(id.to_ascii_lowercase(), path).is_some() {
                return Err(format!("duplicate dependency index for {id}"));
            }
        }
        let copies = if indexes.is_empty() {
            None
        } else {
            Some(
                tempfile::Builder::new()
                    .prefix("pdc-perf-indexes-")
                    .tempdir()
                    .map_err(|e| e.to_string())?,
            )
        };
        let mut options = Vec::new();
        let mut inventory = Vec::new();
        for (id, path) in bindings("--dependency")? {
            let path = relative_to.join(path);
            let mut setup = json!({"id":id,"path":path});
            let mut evidence = setup.clone();
            if let Some(source) = indexes.remove(&id.to_ascii_lowercase()) {
                let source = source
                    .canonicalize()
                    .map_err(|e| format!("dependency index for {id}: {e}"))?;
                let copy = copies
                    .as_ref()
                    .unwrap()
                    .path()
                    .join(format!("{}.pdcindex", options.len()));
                std::fs::copy(&source, &copy).map_err(|e| format!("copy index for {id}: {e}"))?;
                evidence["index"] = json!(source);
                evidence["index_sha256"] = json!(report::file_hash(&copy)?);
                setup["index"] = json!(copy);
            }
            options.push(setup);
            inventory.push(evidence);
        }
        if let Some(id) = indexes.keys().next() {
            return Err(format!(
                "--dependency-index {id} needs a matching --dependency"
            ));
        }
        Ok(Self {
            options,
            inventory,
            _copies: copies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_replays_keep_order_and_refresh_only_private_copies() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.pdcindex");
        std::fs::write(&original, b"frozen").unwrap();
        let args = Args::parse(
            &[
                "--dependency".into(),
                "a=first".into(),
                "--dependency".into(),
                "b=second".into(),
                "--dependency-index".into(),
                format!("B={}", original.display()),
            ],
            &["--dependency", "--dependency-index"],
            &[],
        )
        .unwrap();
        let deps = Dependencies::new(&args, temp.path()).unwrap();
        assert_eq!(deps.options[0]["id"], "a");
        assert!(deps.options[0].get("index").is_none());
        assert_eq!(deps.options[1]["id"], "b");
        std::fs::write(deps.options[1]["index"].as_str().unwrap(), b"refreshed").unwrap();
        assert_eq!(std::fs::read(original).unwrap(), b"frozen");
        assert_eq!(deps.inventory[1]["index_sha256"], report::hash(b"frozen"));
    }
}
