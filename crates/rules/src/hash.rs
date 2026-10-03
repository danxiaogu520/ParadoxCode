use std::fmt;
/// A stable digest of canonical rule content.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct RuleHash([u8; 32]);

impl RuleHash {
    /// Returns the all-zero placeholder hash used by the empty Phase 0 database.
    #[must_use]
    pub const fn empty() -> Self {
        Self([0; 32])
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Creates a hash from raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the lower-case hexadecimal form stored in manifests and diagnostics.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl fmt::Debug for RuleHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("RuleHash").field(&self.0).finish()
    }
}
