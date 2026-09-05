//! Editor-neutral quick-fix queries.
//!
//! Diagnostics remain the single source of semantic truth.  This query only projects the safe
//! edits attached to those diagnostics into a bounded range-filtered result for protocol or CLI
//! adapters.

use pdx_engine::{AnalysisSnapshot, DocumentId};
use pdx_text::TextRange;

use crate::diagnostics::diagnostics_with_cancellation;
use crate::types::{CancellationToken, Cancelled, Diagnostic, QuickFix};

/// Maximum number of edits returned for one editor request.
pub const MAX_QUICK_FIXES: usize = 64;

/// One safe edit paired with the diagnostic that motivated it, so protocol
/// adapters can associate code actions with their published diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeFix {
    /// The edit to apply.
    pub fix: QuickFix,
    /// The diagnostic the edit resolves, in editor-neutral form.
    pub diagnostic: Diagnostic,
}

/// Returns safe fixes for one open document, optionally restricted to an editor range.
pub fn quick_fixes_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    range: Option<TextRange>,
    cancellation: &CancellationToken,
) -> Result<Vec<CodeFix>, Cancelled> {
    cancellation.checkpoint()?;
    let diagnostics = diagnostics_with_cancellation(snapshot, document, cancellation)?;
    let mut fixes = Vec::new();
    for diagnostic in diagnostics {
        for fix in diagnostic.fixes.clone() {
            cancellation.checkpoint()?;
            if range.is_none_or(|requested| ranges_intersect(fix.range, requested)) {
                fixes.push(CodeFix {
                    fix,
                    diagnostic: diagnostic.clone(),
                });
                if fixes.len() == MAX_QUICK_FIXES {
                    return Ok(fixes);
                }
            }
        }
    }
    Ok(fixes)
}

fn ranges_intersect(left: TextRange, right: TextRange) -> bool {
    if left.is_empty() || right.is_empty() {
        let position = if left.is_empty() {
            left.start()
        } else {
            right.start()
        };
        return position >= left.start()
            && position <= left.end()
            && position >= right.start()
            && position <= right.end();
    }
    left.start() < right.end() && right.start() < left.end()
}

#[cfg(test)]
mod tests {
    use super::ranges_intersect;
    use pdx_text::TextRange;

    fn range(start: u32, end: u32) -> TextRange {
        TextRange::new(start, end).expect("valid range")
    }

    #[test]
    fn range_filter_handles_carets_and_half_open_ranges() {
        assert!(ranges_intersect(range(3, 7), range(5, 5)));
        assert!(ranges_intersect(range(3, 7), range(7, 7)));
        assert!(!ranges_intersect(range(3, 7), range(8, 8)));
        assert!(ranges_intersect(range(3, 7), range(6, 9)));
        assert!(!ranges_intersect(range(3, 7), range(7, 9)));
    }
}
