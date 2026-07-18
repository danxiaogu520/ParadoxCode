//! EU4-specific semantic lowering boundary.
//!
//! Phase 0 establishes the scope and file identities without implementing EU4 semantics.

use pdx_eu4::Eu4Rules;
use pdx_syntax::ParsedFile;

/// A conservative semantic scope value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scope {
    /// No scope is known yet; later analysis must avoid cascading errors.
    Unknown,
    /// The root scope of a file.
    Root,
}

/// A lowered file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HirFile {
    syntax: ParsedFile,
    scope: Scope,
}

impl HirFile {
    /// Returns the source syntax handle.
    #[must_use]
    pub const fn syntax(&self) -> &ParsedFile {
        &self.syntax
    }

    /// Returns the conservative file scope.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }
}

/// Lowers a parsed EU4 file into the Phase 0 HIR shell.
#[must_use]
pub fn lower(syntax: ParsedFile, _rules: &Eu4Rules) -> HirFile {
    HirFile { syntax, scope: Scope::Unknown }
}
