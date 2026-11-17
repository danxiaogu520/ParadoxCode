//! Immutable overlay namespaces shared by semantic lookups in one fact view.
use hir::HirFile;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

#[derive(Default)]
pub struct OverlaySymbols {
    members: FxHashMap<String, FxHashSet<String>>,
    subtypes: FxHashMap<(String, String), FxHashSet<String>>,
    templates: FxHashMap<(String, String), Arc<rules::template::Template>>,
    patterns: FxHashMap<String, crate::FlagWriteIndex>,
}

impl OverlaySymbols {
    pub fn new(overlays: &[Arc<HirFile>]) -> Self {
        let mut result = Self::default();
        let mut counts = FxHashMap::<(String, String), usize>::default();
        for hir in overlays {
            for definition in hir.definitions() {
                let kind = definition.kind.to_ascii_lowercase();
                let name = definition.name.to_ascii_lowercase();
                result
                    .members
                    .entry(kind.clone())
                    .or_default()
                    .insert(name.clone());
                *counts.entry((kind.clone(), name)).or_default() += 1;
                if definition.name.contains('$') {
                    result
                        .patterns
                        .entry(kind)
                        .or_default()
                        .record(&definition.name);
                }
            }
            for attributes in hir.definition_attributes() {
                result
                    .subtypes
                    .entry((
                        attributes.kind.to_ascii_lowercase(),
                        attributes.name.to_ascii_lowercase(),
                    ))
                    .or_default()
                    .extend(
                        attributes
                            .subtypes
                            .iter()
                            .map(|name| name.to_ascii_lowercase()),
                    );
            }
        }
        for hir in overlays {
            for definition in hir.definitions() {
                let key = (
                    definition.kind.to_ascii_lowercase(),
                    definition.name.to_ascii_lowercase(),
                );
                if counts.get(&key) == Some(&1)
                    && let Some(template) =
                        hir.dynamic_template(&definition.kind, &definition.name, definition.range)
                {
                    result.templates.insert(key, Arc::new(template.clone()));
                }
            }
        }
        result
    }

    pub fn member(&self, kind: &str, name: &str) -> bool {
        self.members
            .get(&kind.to_ascii_lowercase())
            .is_some_and(|names| names.contains(&name.to_ascii_lowercase()))
    }
    pub fn subtype(&self, kind: &str, name: &str, subtype: &str) -> bool {
        self.subtypes
            .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
            .is_some_and(|names| names.contains(&subtype.to_ascii_lowercase()))
    }
    /// Outer None permits an index lookup. Some(None) denotes an overriding
    /// unavailable or ambiguous overlay, which must suppress that fallback.
    pub fn template(
        &self,
        kind: &str,
        name: &str,
    ) -> Option<Option<Arc<rules::template::Template>>> {
        self.member(kind, name).then(|| {
            self.templates
                .get(&(kind.to_ascii_lowercase(), name.to_ascii_lowercase()))
                .cloned()
        })
    }
    pub fn pattern_member(&self, kind: &str, name: &str) -> bool {
        self.patterns
            .get(&kind.to_ascii_lowercase())
            .is_some_and(|patterns| {
                patterns.membership(name) != crate::FlagWriteMembership::Unknown
            })
    }
}
