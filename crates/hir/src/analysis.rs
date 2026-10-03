//! Editor-independent coverage and evidence states for bounded semantic queries.
use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};

/// A resource or representation boundary that prevented a requested query from completing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AnalysisLimit {
    /// The active call stack reached its configured bound.
    CallDepth,
    /// The query exhausted its semantic-node budget.
    Nodes,
    /// A name-only recursive guard cannot prove termination for the actual binding state.
    RecursiveState,
    /// A declared definition has no recoverable semantic representation.
    UnavailableTemplate,
    /// A secondary text parse exceeded its byte budget.
    TextBytes,
    /// A secondary parse exceeded its node budget.
    ParseNodes,
    /// A quoted/anonymous consumption chain exceeded its bound.
    ConsumptionDepth,
    /// Results or source instances were truncated during projection.
    Output,
    /// A consumer depends on a separately reported unfinished semantic query.
    DependentQuery,
    /// Binding-dependent syntax needs a broader text parse before its structure is known.
    StructuralRecovery,
}

impl AnalysisLimit {
    /// Stable, editor-independent explanation for a limit.
    pub const fn message(self) -> &'static str {
        match self {
            Self::CallDepth => "Template analysis reached its call-depth limit",
            Self::Nodes => "Template analysis exhausted its semantic-node budget",
            Self::RecursiveState => "Template analysis retained an unresolved recursive state",
            Self::UnavailableTemplate => "Template source could not be fully represented",
            Self::TextBytes => "Template text exceeded the analysis byte budget",
            Self::ParseNodes => "Template parsing exceeded the analysis node budget",
            Self::ConsumptionDepth => "Template consumption exceeded the analysis depth limit",
            Self::Output => "Template result projection exceeded its output budget",
            Self::DependentQuery => "A related Template query did not complete",
            Self::StructuralRecovery => "Template syntax requires binding-dependent recovery",
        }
    }
}

/// Input knowledge that is still unresolved even when traversal completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ResidualReason {
    /// A key expression depends on an unknown binding.
    DynamicKey,
    /// An expression contains another unbound parameter.
    Binding,
    /// Several rule interpretations remain feasible for the same container.
    Interpretation,
    /// A scope register or transition remains unknown.
    Scope,
    /// Text interpretation is outside the adopted project support profile.
    TextInterpretation,
}

/// Coverage belongs to one query goal; it is independent of boolean validity.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnalysisCoverage {
    /// Traversal/projection frontiers that remain unfinished.
    pub limits: BTreeSet<AnalysisLimit>,
    /// Unresolved input relationships.
    pub residuals: BTreeSet<ResidualReason>,
}

impl AnalysisCoverage {
    /// Returns whether the requested traversal completed; unknown input may remain.
    pub fn is_complete(&self) -> bool {
        self.limits.is_empty()
    }
    /// Returns whether the result has both complete coverage and no unresolved input.
    pub fn is_known(&self) -> bool {
        self.is_complete() && self.residuals.is_empty()
    }
    /// Combines coverage for conjunctive goals without erasing proven evidence.
    pub fn merge(&mut self, other: &Self) {
        self.limits.extend(other.limits.iter().copied());
        self.residuals.extend(other.residuals.iter().copied());
    }
    /// Stable explanation of unfinished traversal.
    pub fn limit_description(&self) -> String {
        self.limits
            .iter()
            .map(|limit| limit.message())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// A reusable value plus the exact coverage under which it was derived.
#[derive(Clone, Debug)]
pub struct Analysis<T> {
    /// Proven or partial result value.
    pub value: T,
    /// Query-local coverage and residual input relationships.
    pub coverage: AnalysisCoverage,
}

impl<T> Deref for Analysis<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> DerefMut for Analysis<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}
impl<T: IntoIterator> IntoIterator for Analysis<T> {
    type Item = T::Item;
    type IntoIter = T::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        self.value.into_iter()
    }
}
impl<'a, T> IntoIterator for &'a Analysis<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&self.value).into_iter()
    }
}

/// A conclusion cannot be inferred from the absence of diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Validation {
    /// Every requested requirement has a proof.
    Valid,
    /// At least one requirement has an independent rejection witness.
    Invalid,
    /// Input knowledge or coverage is insufficient.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_completion_and_input_knowledge_are_independent() {
        let mut coverage = AnalysisCoverage::default();
        assert!(coverage.is_known());
        coverage.residuals.insert(ResidualReason::Binding);
        assert!(coverage.is_complete());
        assert!(!coverage.is_known());
        coverage.limits.insert(AnalysisLimit::Nodes);
        assert!(!coverage.is_complete());
    }
}
