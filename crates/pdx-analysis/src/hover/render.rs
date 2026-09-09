//! Structured hover content and the Markdown atoms shared by every hover producer.

use crate::types::Hover;
use pdx_text::TextRange;

/// Structured hover content before Markdown rendering.
///
/// Every hover producer builds a model instead of formatting Markdown ad hoc: the title is
/// the `###` heading and each section is an already-rendered block. Rendering joins them with
/// blank lines. Facts callers need to branch on (such as whether a localisation preview
/// attached) ride along as fields so callers never string-match rendered output.
pub(crate) struct HoverModel {
    pub(crate) title: String,
    pub(crate) sections: Vec<String>,
    pub(crate) has_localisation_preview: bool,
}

impl HoverModel {
    pub(crate) fn new(title: String) -> Self {
        Self {
            title,
            sections: Vec::new(),
            has_localisation_preview: false,
        }
    }

    pub(crate) fn push_section(&mut self, section: String) {
        self.sections.push(section);
    }

    pub(crate) fn extend_sections(&mut self, sections: impl IntoIterator<Item = String>) {
        self.sections.extend(sections);
    }

    /// Renders the title and sections joined by blank lines.
    pub(crate) fn render(&self) -> String {
        std::iter::once(self.title.as_str())
            .chain(self.sections.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(crate) fn into_hover_with_range(self, range: TextRange) -> Hover {
        Hover {
            contents: self.render(),
            range: Some(range),
        }
    }
}

/// Renders a symbol spelling as an inline code span without breaking out of the backtick fence.
pub(crate) fn code_span(value: &str) -> String {
    format!("`{}`", value.replace('`', "'"))
}
