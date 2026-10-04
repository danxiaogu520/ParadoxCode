//! Deterministic loading of a whole rules source directory.
//!
//! `game.json` supplies package identity and non-language configuration. Every other regular
//! `.json` file is a source, discovered recursively and merged in relative-path order.
//! Directory names have no semantic meaning; there is no source manifest.

use crate::{ir::GameConfig, profile::GameProfile, source::RuleFile};
use serde::Deserialize;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Package configuration file at the source root.
pub const GAME: &str = "game.json";

/// One in-memory source file.
#[derive(Clone, Copy, Debug)]
pub struct BundleFile<'a> {
    /// Normalized relative source path.
    pub path: &'a str,
    /// File bytes.
    pub bytes: &'a [u8],
}

/// An in-memory source directory; source entries are sorted before parsing.
#[derive(Clone, Copy, Debug)]
pub struct Bundle<'a> {
    /// Root `game.json` bytes.
    pub game: &'a [u8],
    /// Files; unrelated non-JSON entries are ignored.
    pub files: &'a [BundleFile<'a>],
}

/// Package identity, separate from the language IR and runtime profile.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    /// Language version; unsupported versions are rejected before compilation.
    pub source_format_version: u32,
    /// Stable game identity.
    pub game_id: String,
    /// Optional target game release.
    #[serde(default)]
    pub target_game_version: Option<String>,
}

/// Parsed sources and configuration.
#[derive(Clone, Debug)]
pub struct Sources {
    /// Sources in normalized relative-path order.
    pub files: Vec<(String, RuleFile)>,
    /// Non-language configuration.
    pub game: GameConfig,
    /// Package identity from `game.json`.
    pub identity: PackageIdentity,
}

/// Why a source directory could not be loaded.
#[derive(Debug)]
pub enum BundleError {
    /// A filesystem operation failed.
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file is not valid JSON of its declared shape.
    Json {
        /// Affected path.
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
    /// Package or path validation failed.
    Validation(String),
}
impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Validation(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for BundleError {}

/// Parses and sorts an in-memory source directory.
///
/// # Errors
/// Rejects invalid configuration, unsupported versions, malformed sources, duplicate paths,
/// configuration entries in the source list, and non-normalized or escaping paths.
pub fn load_bundle(bundle: Bundle<'_>) -> Result<Sources, BundleError> {
    let (identity, profile) = parse_configuration(Path::new(GAME), bundle.game)?;
    let mut entries = Vec::new();
    for file in bundle.files {
        let path = Path::new(file.path);
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
            || file.path.contains('\\')
            || file
                .path
                .split('/')
                .any(|part| part.is_empty() || part == ".")
        {
            return Err(BundleError::Validation(format!(
                "invalid relative source path `{}`",
                file.path
            )));
        }
        if file.path == GAME {
            return Err(BundleError::Validation(
                "game.json is package configuration, not a source".to_owned(),
            ));
        }
        if path.extension().is_some_and(|ext| ext == "json") {
            entries.push(*file);
        }
    }
    entries.sort_by_key(|file| file.path);
    if let Some(pair) = entries.windows(2).find(|pair| pair[0].path == pair[1].path) {
        return Err(BundleError::Validation(format!(
            "the bundle carries `{}` more than once",
            pair[0].path
        )));
    }
    if entries.is_empty() {
        return Err(BundleError::Validation(
            "the directory contains no rule source files".to_owned(),
        ));
    }
    let files = entries
        .into_iter()
        .map(|file| {
            parse_rule_file(Path::new(file.path), file.bytes)
                .map(|source| (file.path.to_owned(), source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Sources {
        files,
        game: GameConfig { profile },
        identity,
    })
}

/// Recursively loads regular JSON files in normalized relative-path order.
///
/// # Errors
/// Returns configuration, parse, or filesystem errors. Symbolic links are rejected to prevent
/// cycles, aliases, and sources escaping the package root.
pub fn load_directory(directory: &Path) -> Result<Sources, BundleError> {
    let game = read(&directory.join(GAME))?;
    let mut paths = Vec::new();
    discover(directory, directory, &mut paths)?;
    paths.sort();
    let owned = paths
        .into_iter()
        .map(|name| read(&directory.join(&name)).map(|bytes| (name, bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    let files = owned
        .iter()
        .map(|(path, bytes)| BundleFile { path, bytes })
        .collect::<Vec<_>>();
    load_bundle(Bundle {
        game: &game,
        files: &files,
    })
}
fn discover(root: &Path, directory: &Path, paths: &mut Vec<String>) -> Result<(), BundleError> {
    let io = |path: &Path, source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    };
    for entry in std::fs::read_dir(directory).map_err(|e| io(directory, e))? {
        let entry = entry.map_err(|e| io(directory, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| io(&path, e))?;
        if kind.is_symlink() {
            return Err(BundleError::Validation(format!(
                "symbolic links are not rule sources: {}",
                path.display()
            )));
        }
        if kind.is_dir() {
            discover(root, &path, paths)?;
        } else if kind.is_file()
            && path != root.join(GAME)
            && path.extension().is_some_and(|ext| ext == "json")
        {
            let relative = path.strip_prefix(root).expect("discovered below the root");
            let name = relative.to_str().ok_or_else(|| {
                BundleError::Validation(format!("non-UTF-8 source path: {}", path.display()))
            })?;
            paths.push(name.replace(std::path::MAIN_SEPARATOR, "/"));
        }
    }
    Ok(())
}
fn read(path: &Path) -> Result<Vec<u8>, BundleError> {
    std::fs::read(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })
}
fn parse_configuration(
    path: &Path,
    bytes: &[u8],
) -> Result<(PackageIdentity, GameProfile), BundleError> {
    let json_error = |source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    };
    let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(json_error)?;
    let mut metadata = serde_json::Map::new();
    if let Some(object) = value.as_object_mut() {
        for name in ["source_format_version", "target_game_version"] {
            if let Some(value) = object.remove(name) {
                metadata.insert(name.to_owned(), value);
            }
        }
        if let Some(game_id) = object.get("game_id") {
            metadata.insert("game_id".to_owned(), game_id.clone());
        }
    }
    let identity: PackageIdentity = serde_json::from_value(metadata.into()).map_err(json_error)?;
    if identity.source_format_version != 14 {
        return Err(BundleError::Validation(format!(
            "unsupported source_format_version {}; expected 14",
            identity.source_format_version
        )));
    }
    let profile: GameProfile = serde_json::from_value(value).map_err(json_error)?;
    if let Some(spec) = &profile.mission_view {
        if spec.symbol_kind.is_empty() {
            return Err(BundleError::Validation(
                "mission_view.symbol_kind must not be empty".to_owned(),
            ));
        }
        for (section, fields, order) in [
            (
                "tree_fields",
                serde_json::to_value(&spec.tree_fields).map_err(json_error)?,
                &spec.tree_field_order,
            ),
            (
                "node_fields",
                serde_json::to_value(&spec.node_fields).map_err(json_error)?,
                &spec.node_field_order,
            ),
        ] {
            let fields = fields.as_object().expect("field spec is an object");
            let mut spellings = std::collections::BTreeSet::new();
            for value in fields.values() {
                let value = value.as_str().expect("field spelling");
                if value.is_empty() || !spellings.insert(value) {
                    return Err(BundleError::Validation(format!(
                        "mission_view.{section} has an empty or duplicate field spelling"
                    )));
                }
            }
            let roles = order
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            if roles.len() != order.len() || roles != fields.keys().map(String::as_str).collect() {
                return Err(BundleError::Validation(format!(
                    "mission_view.{section} order must name every role exactly once"
                )));
            }
        }
    }
    Ok((identity, profile))
}
fn parse_rule_file(path: &Path, bytes: &[u8]) -> Result<RuleFile, BundleError> {
    serde_json::from_slice(bytes).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const GAME_JSON: &[u8] =
        br#"{"source_format_version":14,"game_id":"test","target_game_version":"1.0"}"#;
    #[test]
    fn input_order_and_unrelated_files_do_not_affect_the_ir() {
        let a = BundleFile {
            path: "any/a.json",
            bytes: br#"{"schemas":{"body":{}}}"#,
        };
        let z = BundleFile {
            path: "z.json",
            bytes: br#"{"files":{"script":{"path":"","root":"body"}}}"#,
        };
        let junk = BundleFile {
            path: "notes.txt",
            bytes: b"not JSON",
        };
        let first = load_bundle(Bundle {
            game: GAME_JSON,
            files: &[z, a, junk],
        })
        .unwrap();
        let second = load_bundle(Bundle {
            game: GAME_JSON,
            files: &[a, z],
        })
        .unwrap();
        assert_eq!(
            first
                .files
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["any/a.json", "z.json"]
        );
        assert_eq!(first.identity.target_game_version.as_deref(), Some("1.0"));
        assert_eq!(
            crate::lower::lower(&first.files, first.game)
                .unwrap()
                .fingerprint(),
            crate::lower::lower(&second.files, second.game)
                .unwrap()
                .fingerprint()
        );
    }
    #[test]
    fn discovers_new_files_and_rejects_duplicate_definitions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(GAME), GAME_JSON).unwrap();
        std::fs::create_dir(root.path().join("arbitrary")).unwrap();
        std::fs::write(
            root.path().join("arbitrary/a.json"),
            br#"{"schemas":{"body":{}}}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("notes.md"), "not JSON").unwrap();
        let before = load_directory(root.path()).unwrap();
        assert_eq!(before.files.len(), 1);
        std::fs::write(root.path().join("b.json"), br#"{"schemas":{"body":{}}}"#).unwrap();
        let after = load_directory(root.path()).unwrap();
        assert_eq!(after.files.len(), 2);
        let failure = crate::lower::lower(&after.files, after.game).unwrap_err();
        assert!(
            failure
                .diagnostics()
                .iter()
                .any(|d| d.message.contains("body"))
        );
        std::fs::remove_file(root.path().join("b.json")).unwrap();
        let again = load_directory(root.path()).unwrap();
        assert_eq!(
            crate::lower::lower(&before.files, before.game)
                .unwrap()
                .fingerprint(),
            crate::lower::lower(&again.files, again.game)
                .unwrap()
                .fingerprint()
        );
    }
    #[test]
    fn invalid_packages_and_duplicate_or_escaping_paths_are_rejected() {
        let valid = BundleFile {
            path: "a.json",
            bytes: b"{}",
        };
        for game in [
            br#"{"game_id":"test"}"#.as_slice(),
            br#"{"game_id":"test","source_format_version":12}"#,
            br#"{"game_id":"test","source_format_version":14,"typo":true}"#,
        ] {
            assert!(
                load_bundle(Bundle {
                    game,
                    files: &[valid]
                })
                .is_err()
            );
        }
        assert!(
            load_bundle(Bundle {
                game: GAME_JSON,
                files: &[valid, valid]
            })
            .is_err()
        );
        for path in [
            "../escape.json",
            "/absolute.json",
            "a//b.json",
            "game.json",
            "a\\b.json",
            "a/./b.json",
        ] {
            assert!(
                load_bundle(Bundle {
                    game: GAME_JSON,
                    files: &[BundleFile { path, bytes: b"{}" }]
                })
                .is_err(),
                "{path}"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn directory_links_are_rejected_without_recursing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(GAME), GAME_JSON).unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("cycle")).unwrap();
        assert!(
            load_directory(root.path())
                .unwrap_err()
                .to_string()
                .contains("symbolic links")
        );
    }
}
