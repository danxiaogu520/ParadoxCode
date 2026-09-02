use crate::{CstKind, CstNode, ParsedFile};
use pdx_text::TextRange;
pub(super) fn format_localisation(file: &ParsedFile) -> String {
    let mut lines = Vec::new();
    let mut bom = false;
    for node in file.root().children() {
        match node.kind() {
            CstKind::Bom => bom = true,
            CstKind::Comment => lines.push(file.text(node.range()).unwrap_or("").to_owned()),
            CstKind::LanguageHeader => {
                let key = node
                    .children()
                    .next()
                    .and_then(|child| file.text(child.range()));
                if let Some(key) = key {
                    lines.push(format!("{key}:"));
                }
            }
            CstKind::LocalisationEntry => {
                lines.push(format_localisation_entry(file, node));
            }
            _ => {}
        }
    }
    if bom {
        if let Some(first) = lines.first_mut() {
            first.insert(0, '\u{feff}');
        } else {
            lines.push("\u{feff}".to_owned());
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn format_localisation_entry(file: &ParsedFile, node: CstNode<'_>) -> String {
    let key = node
        .children()
        .find(|child| child.kind() == CstKind::LocalisationKey)
        .and_then(|child| file.text(child.range()))
        .unwrap_or("");
    let version = node
        .children()
        .find(|child| child.kind() == CstKind::Version)
        .and_then(|child| file.text(child.range()));
    let value_node = node.children().find(|child| {
        matches!(
            child.kind(),
            CstKind::LocalisationString | CstKind::UnquotedValue
        )
    });
    let value = value_node
        .and_then(|child| file.text(child.range()))
        .unwrap_or("");
    let has_colon = value_node.is_some_and(|value_node| {
        let range = TextRange::new(
            node.children()
                .find(|child| child.kind() == CstKind::LocalisationKey)
                .map_or(node.range().start(), |child| child.range().end()),
            value_node.range().start(),
        )
        .unwrap_or_else(|| TextRange::empty(value_node.range().start()));
        file.text(range).is_some_and(|text| text.contains(':'))
    });
    let mut line = if has_colon {
        format!("{key}:{} {value}", version.unwrap_or(""))
    } else {
        format!("{key} {value}")
    };
    if let Some(comment) = node
        .children()
        .find(|child| child.kind() == CstKind::Comment)
    {
        line.push(' ');
        line.push_str(file.text(comment.range()).unwrap_or(""));
    }
    line
}
