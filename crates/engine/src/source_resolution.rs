//! Shared file and overlay ownership for immutable workspace queries.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::AnalysisSnapshot;
use rules::FileResolutionPolicy;
use text::{AbsPath, LogicalPath};
use vfs::{DocumentId, DocumentSource, ResolvedCandidate, SourceFileId};

#[derive(Default)]
struct SourceResolution {
    candidates: BTreeMap<LogicalPath, Vec<ResolvedCandidate>>,
    files: BTreeSet<SourceFileId>,
    documents: BTreeSet<DocumentId>,
    suppressed: BTreeSet<DocumentId>,
    overlays: Vec<(DocumentId, Option<AbsPath>)>,
}

impl AnalysisSnapshot {
    fn source_resolution(&self) -> Arc<SourceResolution> {
        const KEY: &str = "effective-source-ownership";
        if let Some(cached) = self.query_cache().get(self.revision(), KEY) {
            return cached;
        }
        let overlays = self
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
            .map(|document| {
                (
                    document.id().clone(),
                    document.path().map(AbsPath::normalize),
                )
            })
            .collect::<Vec<_>>();
        const INPUTS_KEY: &str = "source-ownership-inputs";
        if let Some(cached) = self
            .query_cache()
            .get::<SourceResolution>(self.revision(), INPUTS_KEY)
            && cached.overlays == overlays
        {
            self.query_cache().insert(
                self.revision(),
                crate::CacheDomain::Documents,
                KEY.into(),
                Arc::clone(&cached),
            );
            return cached;
        }
        let mut result = SourceResolution {
            overlays,
            ..Default::default()
        };
        let mut owners: BTreeMap<AbsPath, DocumentId> = BTreeMap::new();
        for document in self
            .documents()
            .values()
            .filter(|document| document.source() == DocumentSource::Overlay)
        {
            let Some(path) = document.path() else {
                result.documents.insert(document.id().clone());
                continue;
            };
            let path = AbsPath::normalize(path);
            match owners.get(&path) {
                Some(owner) if !prefer_overlay_document(owner, document.id()) => {
                    result.suppressed.insert(document.id().clone());
                }
                previous => {
                    if let Some(previous) = previous {
                        result.suppressed.insert(previous.clone());
                    }
                    owners.insert(path, document.id().clone());
                }
            }
        }
        let priorities = self
            .source_roots()
            .iter()
            .map(|root| (root.id, vfs::root_priority(root)))
            .collect::<BTreeMap<_, _>>();
        let mut hidden_files = BTreeSet::new();
        for file in self.source_files().values() {
            if owners.contains_key(&file.physical_path) {
                hidden_files.insert(file.id);
            }
            result
                .candidates
                .entry(file.logical_path.clone())
                .or_default()
                .push(ResolvedCandidate {
                    logical_path: file.logical_path.clone(),
                    file_id: Some(file.id),
                    document_id: None,
                    priority: priorities.get(&file.root_id).copied().unwrap_or(0),
                    resolution: Some(file.resolution),
                    active: false,
                });
        }
        for (path, id) in owners {
            let Some(root) = self
                .source_roots()
                .iter()
                .filter(|root| path.starts_with(&root.path))
                .max_by_key(|root| root.path.as_os_str().len())
            else {
                // Standalone and untitled documents retain their existing editor semantics.
                result.documents.insert(id);
                continue;
            };
            let Some(logical) = path
                .strip_prefix(&root.path)
                .ok()
                .and_then(|path| LogicalPath::parse(&path.to_string_lossy()).ok())
            else {
                continue;
            };
            let policy = self
                .rules()
                .classify(&logical)
                .map_or(FileResolutionPolicy::ReplaceByRelativePath, |category| {
                    category.resolution
                });
            result
                .candidates
                .entry(logical.clone())
                .or_default()
                .push(ResolvedCandidate {
                    logical_path: logical,
                    file_id: None,
                    document_id: Some(id),
                    priority: vfs::root_priority(root),
                    resolution: Some(policy),
                    active: false,
                });
        }
        for candidates in result.candidates.values_mut() {
            candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority));
            let merge = candidates
                .iter()
                .find(|candidate| {
                    candidate
                        .file_id
                        .is_none_or(|id| !hidden_files.contains(&id))
                })
                .is_some_and(|candidate| candidate.resolution == Some(FileResolutionPolicy::Merge));
            let mut winner = false;
            for candidate in candidates.iter_mut() {
                if candidate
                    .file_id
                    .is_some_and(|id| hidden_files.contains(&id))
                {
                    continue;
                }
                candidate.active = merge || !winner;
                winner = true;
                if candidate.active {
                    if let Some(file) = candidate.file_id {
                        result.files.insert(file);
                    }
                    if let Some(document) = &candidate.document_id {
                        result.documents.insert(document.clone());
                    }
                }
            }
            candidates.sort_by_key(|candidate| {
                (
                    !candidate.active,
                    std::cmp::Reverse(candidate.priority),
                    candidate.document_id.is_none(),
                )
            });
        }
        let result = Arc::new(result);
        self.query_cache().insert(
            self.revision(),
            crate::CacheDomain::SourceOwnership,
            INPUTS_KEY.into(),
            Arc::clone(&result),
        );
        self.query_cache().insert(
            self.revision(),
            crate::CacheDomain::Documents,
            KEY.into(),
            Arc::clone(&result),
        );
        result
    }

    pub(crate) fn resolution_candidates(&self, path: &LogicalPath) -> Vec<ResolvedCandidate> {
        self.source_resolution()
            .candidates
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether this physical file/document currently contributes to its logical source path.
    /// An editor buffer replaces its own disk candidate and retains its configured root order.
    #[must_use]
    pub fn source_is_effective(
        &self,
        document: Option<&DocumentId>,
        file: Option<SourceFileId>,
    ) -> bool {
        let resolution = self.source_resolution();
        if let Some(document) = document {
            return resolution.documents.contains(document);
        }
        file.is_some_and(|file| resolution.files.contains(&file))
    }

    /// Original/decoded spelling twins suppressed by the shared physical-document owner.
    #[must_use]
    pub fn suppressed_overlay_documents(&self) -> BTreeSet<DocumentId> {
        self.source_resolution().suppressed.clone()
    }
}

/// Chooses the owner of raw/decoded documents over the same physical file.
#[must_use]
pub fn prefer_overlay_document(current: &DocumentId, candidate: &DocumentId) -> bool {
    let decoded = |id: &DocumentId| {
        id.as_str()
            .split_once(':')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("pdcloc"))
    };
    match (decoded(current), decoded(candidate)) {
        (false, true) => true,
        (true, false) => false,
        _ => candidate > current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_reuses_content_edits_and_refreshes_when_a_decoded_owner_opens() {
        let ir = game::eu4::first_party_ir().unwrap();
        let mut host = crate::AnalysisHost::with_ir(
            rules::RuleSet::from_ir_catalog(&ir),
            ir.game.profile.clone(),
            ir,
        );
        let path = AbsPath::normalize(&std::env::temp_dir().join("owned-source/events/search.txt"));
        let raw = DocumentId::new(format!("file://{}", path.display()));
        host.open_document(
            raw.clone(),
            1,
            "country_event = { id = search.1 }".into(),
            Some(path.clone()),
        )
        .unwrap();
        let first_snapshot = host.snapshot();
        let first = first_snapshot.source_resolution();
        host.apply_document_changes(
            &raw,
            2,
            &[vfs::TextChange::full("country_event = { id = search.2 }")],
        )
        .unwrap();
        let edited = host.snapshot().source_resolution();
        assert!(Arc::ptr_eq(&first, &edited));
        let decoded = DocumentId::new(format!("pdcloc://{}", path.display()));
        host.open_document(
            decoded.clone(),
            1,
            "country_event = { id = search.3 }".into(),
            Some(path),
        )
        .unwrap();
        let current = host.snapshot().source_resolution();
        assert!(!Arc::ptr_eq(&first, &current));
        assert!(current.suppressed.contains(&raw));
        assert!(
            !first.suppressed.contains(&raw),
            "older snapshots keep their owner"
        );
        assert!(first_snapshot.source_resolution().suppressed.is_empty());
    }
}
