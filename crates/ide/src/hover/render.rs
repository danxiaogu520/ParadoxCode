//! Structured hover content and the Markdown atoms shared by every hover producer.

use crate::types::{Hover, HoverLink, Location, PreviewTarget};
use std::hash::{DefaultHasher, Hash, Hasher};
use text::TextRange;

/// Structured hover content before Markdown rendering.
///
/// Every hover producer builds a model instead of formatting Markdown ad hoc: the title is
/// the `###` heading and each section is an already-rendered block. Rendering joins them with
/// blank lines. Facts callers need to branch on (such as whether a localisation preview
/// attached) ride along as fields so callers never string-match rendered output.
pub(crate) struct HoverModel {
    pub(crate) preview: Option<PreviewTarget>,
    pub(crate) title: String,
    pub(crate) sections: Vec<String>,
    pub(crate) has_localisation_preview: bool,
    pub(crate) coverage: hir::analysis::AnalysisCoverage,
    definition: Option<String>,
    links: Vec<HoverLink>,
}

impl HoverModel {
    pub(crate) fn new(title: String) -> Self {
        Self {
            preview: None,
            title,
            sections: Vec::new(),
            has_localisation_preview: false,
            coverage: Default::default(),
            definition: None,
            links: Vec::new(),
        }
    }

    pub(crate) fn push_section(&mut self, section: String) {
        self.sections.push(section);
    }

    pub(crate) fn push_model(&mut self, other: Self) {
        self.sections.push(other.render());
        self.links.extend(other.links);
        self.coverage.merge(&other.coverage);
    }

    pub(crate) fn source_link(&mut self, label: &str, location: Location) -> String {
        let label = label
            .replace(['\n', '\r'], " ")
            .replace('\\', "\\\\")
            .replace('[', "\\[")
            .replace(']', "\\]")
            .replace('|', "\\|");
        let mut hash = DefaultHasher::new();
        location.hash(&mut hash);
        let destination = format!("pdc-hover:{:x}", hash.finish());
        let markdown = format!("[{label}]({destination})");
        self.links.push(HoverLink {
            label,
            destination,
            location,
        });
        markdown
    }

    pub(crate) fn set_definition(&mut self, label: &str, location: Location) {
        let link = self.source_link(label, location);
        self.definition = Some(format!("Defined in {link}"));
    }

    pub(crate) fn push_localisation(
        &mut self,
        rows: &[crate::localisation::LocalisationPreviewRow],
    ) {
        if rows.is_empty() {
            return;
        }
        self.has_localisation_preview |= rows.iter().any(|row| row.location.is_some());
        let section = crate::localisation::localisation_preview_section(rows, |label, location| {
            self.source_link(label, location)
        });
        self.push_section(section);
    }

    /// Renders the title and sections joined by blank lines.
    pub(crate) fn render(&self) -> String {
        std::iter::once(self.title.as_str())
            .chain(self.sections.iter().map(String::as_str))
            .chain(self.definition.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(crate) fn into_hover_with_range(self, range: TextRange) -> Hover {
        Hover {
            contents: self.render(),
            preview: self.preview,
            range: Some(range),
            links: self.links,
            coverage: self.coverage,
        }
    }
}

/// Renders a symbol spelling as an inline code span without breaking out of the backtick fence.
pub(crate) fn code_span(value: &str) -> String {
    format!("`{}`", value.replace('`', "'"))
}
