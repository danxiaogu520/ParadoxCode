//! Loading a rules-v2 source bundle (`docs/rules-language.md` §1).
//!
//! A bundle is the `manifest.json` that lists every source file, the
//! `game.json` payload sitting beside it, and the source bytes. Both the
//! embedded first-party bundle (`crates/game`) and the filesystem loader
//! (tooling) go through here, so the manifest contract — every declared file
//! is present, nothing undeclared is, the game identity agrees — is enforced
//! in one place.
//!
//! The bundle does not compile anything: [`load_bundle`] hands back parsed
//! [`RuleFile`]s and the [`GameConfig`] for `crate::lower::lower`.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ir::GameConfig;
use crate::profile::GameProfile;
use crate::source::RuleFile;

/// The manifest file name inside a bundle directory.
pub const MANIFEST: &str = "manifest.json";

/// The non-language configuration file name inside a bundle directory.
pub const GAME: &str = "game.json";

/// One file of an in-memory bundle.
#[derive(Clone, Copy, Debug)]
pub struct BundleFile<'a> {
    /// The path as spelled in the manifest, relative to the bundle root.
    pub path: &'a str,
    /// The file's bytes.
    pub bytes: &'a [u8],
}

/// An in-memory rules-v2 bundle.
#[derive(Clone, Copy, Debug)]
pub struct Bundle<'a> {
    /// The `manifest.json` bytes.
    pub manifest: &'a [u8],
    /// The `game.json` bytes.
    pub game: &'a [u8],
    /// Every file the manifest declares.
    pub files: &'a [BundleFile<'a>],
}

/// The parsed `manifest.json`.
///
/// The manifest carries tooling identity, not language: `game_id` and
/// `target_game_version` are reported for diagnostics but do not participate
/// in compilation, and `game.json` is deliberately not one of `files`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The game identity this bundle was authored for.
    #[serde(default)]
    pub game_id: Option<String>,
    /// The game version this bundle targets.
    #[serde(default)]
    pub target_game_version: Option<String>,
    /// Every source file, relative to the bundle root, in load order.
    pub files: Vec<String>,
}

/// A loaded bundle: parsed sources plus the non-language configuration.
#[derive(Clone, Debug)]
pub struct Sources {
    /// `(source file name, parsed file)` in manifest order.
    pub files: Vec<(String, RuleFile)>,
    /// The non-language `game.json` payload.
    pub game: GameConfig,
    /// The manifest that named the sources.
    pub manifest: Manifest,
}

/// Why a bundle could not be loaded.
#[derive(Debug)]
pub enum BundleError {
    /// A file could not be read.
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// A file is not valid JSON of its declared shape.
    Json {
        /// The file that failed.
        path: PathBuf,
        /// The underlying failure.
        source: serde_json::Error,
    },
    /// The bundle violates the manifest contract.
    Validation(String),
}

impl fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Validation(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for BundleError {}

/// Parses an in-memory bundle.
///
/// # Errors
///
/// Returns [`BundleError`] when a declared file is missing from `files`, when
/// `files` carries an undeclared file or a duplicate, when a file does not
/// parse as its declared shape, or when the game identity of `game.json`
/// disagrees with the manifest's.
pub fn load_bundle(bundle: Bundle<'_>) -> Result<Sources, BundleError> {
    let manifest_path = Path::new("<bundle>").join(MANIFEST);
    let manifest = parse_manifest(&manifest_path, bundle.manifest)?;
    let game_path = Path::new("<bundle>").join(GAME);
    let profile = parse_profile(&game_path, bundle.game)?;
    validate_manifest(&manifest)?;
    validate_game(&manifest, &profile)?;

    let mut provided = Vec::with_capacity(bundle.files.len());
    for file in bundle.files {
        provided.push(file.path);
    }
    validate_file_set(&manifest, &provided)?;

    let mut files = Vec::with_capacity(manifest.files.len());
    for name in &manifest.files {
        let file = bundle
            .files
            .iter()
            .find(|file| file.path == name)
            .expect("the file set was validated");
        files.push((name.clone(), parse_rule_file(Path::new(name), file.bytes)?));
    }
    Ok(Sources {
        files,
        game: GameConfig { profile },
        manifest,
    })
}

/// Loads a bundle from a directory: `<directory>/manifest.json`,
/// `<directory>/game.json`, and every file the manifest lists.
///
/// # Errors
///
/// Returns [`BundleError`] under the same conditions as [`load_bundle`], plus
/// a filesystem failure for any file that cannot be read.
pub fn load_directory(directory: &Path) -> Result<Sources, BundleError> {
    let manifest_path = directory.join(MANIFEST);
    let manifest_bytes = read(&manifest_path)?;
    let manifest = parse_manifest(&manifest_path, &manifest_bytes)?;
    let game_path = directory.join(GAME);
    let game_bytes = read(&game_path)?;
    let profile = parse_profile(&game_path, &game_bytes)?;
    validate_manifest(&manifest)?;
    validate_game(&manifest, &profile)?;

    let mut files = Vec::with_capacity(manifest.files.len());
    for name in &manifest.files {
        let path = directory.join(name);
        let bytes = read(&path)?;
        files.push((name.clone(), parse_rule_file(&path, &bytes)?));
    }
    Ok(Sources {
        files,
        game: GameConfig { profile },
        manifest,
    })
}

fn read(path: &Path) -> Result<Vec<u8>, BundleError> {
    std::fs::read(path).map_err(|source| BundleError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_manifest(path: &Path, bytes: &[u8]) -> Result<Manifest, BundleError> {
    serde_json::from_slice(bytes).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_profile(path: &Path, bytes: &[u8]) -> Result<GameProfile, BundleError> {
    serde_json::from_slice(bytes).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_rule_file(path: &Path, bytes: &[u8]) -> Result<RuleFile, BundleError> {
    serde_json::from_slice(bytes).map_err(|source| BundleError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_manifest(manifest: &Manifest) -> Result<(), BundleError> {
    if manifest.files.is_empty() {
        return Err(BundleError::Validation(
            "the manifest lists no source files".to_owned(),
        ));
    }
    for (index, name) in manifest.files.iter().enumerate() {
        if manifest.files[..index].contains(name) {
            return Err(BundleError::Validation(format!(
                "the manifest lists `{name}` more than once"
            )));
        }
        if name == MANIFEST || name == GAME {
            return Err(BundleError::Validation(format!(
                "the manifest must not list `{name}`; it is bundle configuration, not a source"
            )));
        }
    }
    Ok(())
}

fn validate_game(manifest: &Manifest, profile: &GameProfile) -> Result<(), BundleError> {
    if let Some(game_id) = manifest.game_id.as_deref()
        && !game_id.is_empty()
        && profile.game_id != game_id
    {
        return Err(BundleError::Validation(format!(
            "game.json declares game_id `{}`, but the manifest declares `{game_id}`",
            profile.game_id
        )));
    }
    Ok(())
}

fn validate_file_set(manifest: &Manifest, provided: &[&str]) -> Result<(), BundleError> {
    for name in &manifest.files {
        if !provided.contains(&name.as_str()) {
            return Err(BundleError::Validation(format!(
                "the manifest lists `{name}`, which the bundle does not carry"
            )));
        }
    }
    for path in provided {
        if !manifest.files.iter().any(|name| name == path) {
            return Err(BundleError::Validation(format!(
                "the bundle carries `{path}`, which the manifest does not list"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_JSON: &str = r#"{
      "game_id": "eu4",
      "target_game_version": "1.37.5",
      "files": ["core/scopes.json"]
    }"#;

    const GAME_JSON: &str = r#"{ "game_id": "eu4" }"#;

    const SCOPES_JSON: &str = r#"{
      "scopes": { "types": ["country"], "registers": { "root": {} } }
    }"#;

    fn bundle<'a>(files: &'a [BundleFile<'a>]) -> Bundle<'a> {
        Bundle {
            manifest: MANIFEST_JSON.as_bytes(),
            game: GAME_JSON.as_bytes(),
            files,
        }
    }

    #[test]
    fn a_complete_bundle_loads() {
        let files = [BundleFile {
            path: "core/scopes.json",
            bytes: SCOPES_JSON.as_bytes(),
        }];
        let sources = load_bundle(bundle(&files)).expect("loads");
        assert_eq!(sources.files.len(), 1);
        assert_eq!(sources.files[0].0, "core/scopes.json");
        assert_eq!(sources.game.game_id(), "eu4");
        assert_eq!(
            sources.manifest.target_game_version.as_deref(),
            Some("1.37.5")
        );
    }

    #[test]
    fn undeclared_and_missing_files_are_rejected() {
        let files = [BundleFile {
            path: "core/other.json",
            bytes: SCOPES_JSON.as_bytes(),
        }];
        let failure = load_bundle(bundle(&files)).expect_err("rejects");
        assert!(failure.to_string().contains("does not carry"), "{failure}");

        let files = [
            BundleFile {
                path: "core/scopes.json",
                bytes: SCOPES_JSON.as_bytes(),
            },
            BundleFile {
                path: "core/extra.json",
                bytes: SCOPES_JSON.as_bytes(),
            },
        ];
        let failure = load_bundle(bundle(&files)).expect_err("rejects");
        assert!(failure.to_string().contains("does not list"), "{failure}");
    }

    #[test]
    fn the_manifest_must_not_list_its_own_configuration() {
        let manifest = br#"{ "files": ["manifest.json"] }"#;
        let failure = load_bundle(Bundle {
            manifest,
            game: GAME_JSON.as_bytes(),
            files: &[],
        })
        .expect_err("rejects");
        assert!(
            failure.to_string().contains("bundle configuration"),
            "{failure}"
        );
    }

    #[test]
    fn a_disagreeing_game_identity_is_rejected() {
        let manifest = br#"{ "game_id": "ck3", "files": ["core/scopes.json"] }"#;
        let files = [BundleFile {
            path: "core/scopes.json",
            bytes: SCOPES_JSON.as_bytes(),
        }];
        let failure = load_bundle(Bundle {
            manifest,
            game: GAME_JSON.as_bytes(),
            files: &files,
        })
        .expect_err("rejects");
        assert!(
            failure.to_string().contains("but the manifest declares"),
            "{failure}"
        );
    }
}
