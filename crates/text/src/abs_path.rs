//! Canonical absolute filesystem paths with one uniform spelling.
//!
//! Every `AbsPath` is produced by a constructor, never by wrapping a raw
//! `PathBuf`, so two `AbsPath`s for the same file always compare equal: the
//! Windows extended-length prefix is dropped, separators follow the platform
//! convention, and paths that reach the filesystem carry the on-disk case.
//! Keying maps by `AbsPath` therefore turns "every ingress must canonicalize"
//! from a convention into a type invariant.

use std::fmt;
use std::path::{Path, PathBuf};

use dunce::simplified;
use serde::{Deserialize, Serialize};

use crate::LogicalPath;

/// An absolute filesystem path with a normalized spelling.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AbsPath(PathBuf);

impl AbsPath {
    /// Canonicalizes an absolute path.
    ///
    /// If the path does not exist yet (an editor can open an unsaved buffer
    /// under its future path), the deepest existing ancestor is canonicalized
    /// and the missing components re-appended, so the spelling still matches
    /// what source discovery produces once the file appears.
    pub fn canonicalize(path: &Path) -> Result<Self, std::io::Error> {
        if let Ok(canonical) = dunce::canonicalize(path) {
            return Ok(Self(canonical));
        }
        let mut ancestor = path;
        let mut missing = Vec::new();
        while let Some(name) = ancestor.file_name() {
            missing.push(name.to_owned());
            let Some(parent) = ancestor.parent() else {
                break;
            };
            ancestor = parent;
            if let Ok(mut canonical) = dunce::canonicalize(ancestor) {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(Self(canonical));
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cannot canonicalize {}", path.display()),
        ))
    }

    /// Wraps a path whose spelling is already canonical, without touching the
    /// filesystem.
    ///
    /// Used on hot paths whose inputs inherit canonicality by construction:
    /// directory walks under a canonicalized root with symlinked entries
    /// skipped, and document paths that arrive from the protocol boundary
    /// already canonicalized. The spelling — not absoluteness — is the
    /// invariant here, so editor-attached relative path hints survive the same
    /// graceful fallback they always had.
    #[must_use]
    pub fn normalize(path: &Path) -> Self {
        Self(simplified(path).to_path_buf())
    }

    /// Extends this canonical root with a relative logical path.
    ///
    /// Logical paths are normalized (`.`/`..` folded, `/` separators) and
    /// discovery never follows symlinks, so the join preserves canonicality.
    #[must_use]
    pub fn join_logical(&self, relative: &LogicalPath) -> Self {
        Self(self.0.join(relative.as_str()))
    }

    /// Returns the underlying path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consumes the wrapper and returns the underlying path buffer.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for AbsPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for AbsPath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.to_string_lossy())
    }
}
