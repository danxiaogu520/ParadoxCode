//! Workspace-context fingerprint for the diagnostics cache.
//!
//! A closed file's diagnostics depend on more than its own text: other
//! files' indexed contributions (definitions, dynamic definitions, flag
//! writes), the overlay set (open documents hide their disk entries), the
//! texture catalog, rules, the game profile, root priorities, and the
//! localisation language preference. The fingerprint below hashes all of
//! those into one value; a validation pass may reuse a file's cached
//! diagnostics whenever both its own content hash and this fingerprint are
//! unchanged.
//!
//! Soundness contract: every input family that can alter one file's
//! diagnostics by changing something *outside* that file must be hashed
//! here. Adding a new cross-file input to the analyzer means extending this
//! function (or the per-shard `contribution_fingerprint` it builds on).

use crate::snapshot::AnalysisSnapshot;
use index::{ParsedSource, shard_for_source};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use vfs::{DocumentId, DocumentSource, SourceFile};

/// Computes the workspace context fingerprint for one snapshot.
///
/// Overlay-backed files contribute the shard derived from the overlay's
/// resident trees through the same builder the scan path uses, so opening a
/// document without editing it leaves the fingerprint unchanged, and editing
/// a document only moves the fingerprint when its exported contribution
/// actually changed. Overlays without a backing source file fall back to a
/// content hash (any edit then invalidates every cached file).
pub fn workspace_context_fingerprint(snapshot: &AnalysisSnapshot) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    snapshot.rules().rule_hash().to_hex().hash(&mut hasher);
    // The host pins its profile Arc for the whole session, so the allocation
    // address identifies the profile object without ABA risk.
    format!("{:p}", Arc::as_ptr(&snapshot.game_profile_handle())).hash(&mut hasher);
    snapshot.texture_catalog_generation().hash(&mut hasher);
    for language in snapshot.preferred_localisation_languages() {
        language.hash(&mut hasher);
    }
    for root in snapshot.source_roots() {
        format!("{root:?}").hash(&mut hasher);
    }

    let mut backed_overlays = BTreeMap::new();
    let mut unbacked_overlays = Vec::new();
    for (id, document) in snapshot.documents() {
        if document.source() != DocumentSource::Overlay {
            continue;
        }
        match document
            .path()
            .and_then(|path| snapshot.source_file_id_for_path(path))
        {
            Some(file) => {
                backed_overlays.insert(file, id.clone());
            }
            None => unbacked_overlays.push(document.text_handle()),
        }
    }

    for (id, file) in snapshot.source_files() {
        id.hash(&mut hasher);
        if let Some(document) = backed_overlays.get(id) {
            if let Some(fingerprint) = overlay_contribution_fingerprint(snapshot, file, document) {
                fingerprint.hash(&mut hasher);
                continue;
            }
            // Overlay without resident trees: fall back to hashing its text so
            // any change still invalidates.
            if let Some(document) = snapshot.document(document) {
                hash_text(document.text_handle(), &mut hasher);
                continue;
            }
        }
        match snapshot.file_state(*id) {
            Some(state) => state.shard.contribution_fingerprint().hash(&mut hasher),
            None => u64::MAX.hash(&mut hasher),
        }
    }
    for text in unbacked_overlays {
        hash_text(text, &mut hasher);
    }
    hasher.finish()
}

fn overlay_contribution_fingerprint(
    snapshot: &AnalysisSnapshot,
    file: &SourceFile,
    document: &DocumentId,
) -> Option<u64> {
    let document = snapshot.document(document)?;
    let ParsedSource::Text(parsed) = document.parsed()?;
    let hir = document.hir_handle()?;
    Some(shard_for_source(file, parsed, &hir, snapshot.rules()).contribution_fingerprint())
}

fn hash_text(text: Arc<str>, hasher: &mut impl Hasher) {
    let bytes: &str = &text;
    bytes.hash(hasher);
}
