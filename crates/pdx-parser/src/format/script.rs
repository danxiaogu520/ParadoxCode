use super::common::{contains_line_break, fits_line, indent};
use crate::{
    CstKind, CstNode, ParsedFile, encode_quoted_script_text,
    parse_quoted_script as parse_quoted_payload,
};
use pdx_text::TextRange;
const MAX_QUOTED_SCRIPT_DEPTH: usize = 64;
pub(super) fn format_script(file: &ParsedFile) -> String {
    PdxFormatter::new(file, 0).document()
}
enum ValueLayout {
    Inline {
        text: String,
        width_sensitive: bool,
    },
    Expanded {
        opener: String,
        body: Vec<String>,
        closer: String,
    },
}

impl ValueLayout {
    fn inline(text: impl Into<String>) -> Self {
        Self::Inline {
            text: text.into(),
            width_sensitive: false,
        }
    }

    fn width_sensitive(text: impl Into<String>) -> Self {
        Self::Inline {
            text: text.into(),
            width_sensitive: true,
        }
    }
}

struct PdxFormatter<'file> {
    file: &'file ParsedFile,
    quoted_script_depth: usize,
}

impl<'file> PdxFormatter<'file> {
    fn new(file: &'file ParsedFile, quoted_script_depth: usize) -> Self {
        Self {
            file,
            quoted_script_depth,
        }
    }

    fn document(&self) -> String {
        let lines = self.document_lines();
        if lines.is_empty() {
            return String::new();
        }
        let mut output = lines.join("\n");
        output.push('\n');
        output
    }

    fn document_lines(&self) -> Vec<String> {
        let children = self.file.root().children();
        let has_bom = children
            .first()
            .is_some_and(|node| node.kind() == CstKind::Bom);
        let children = if has_bom { &children[1..] } else { children };
        let mut lines = self.sequence(children, 0);
        if has_bom {
            if let Some(first) = lines.first_mut() {
                first.insert(0, '\u{feff}');
            } else {
                lines.push("\u{feff}".to_owned());
            }
        }
        lines
    }

    fn sequence(&self, children: &[CstNode], depth: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut index = 0;
        while index < children.len() {
            let node = &children[index];
            if node.kind() == CstKind::Bom {
                index += 1;
                continue;
            }
            if node.kind() == CstKind::Comment {
                lines.push(format!("{}{}", indent(depth), self.text(node)));
                index += 1;
                continue;
            }

            let mut item = self.item(node, depth);
            if let Some(comment) = children.get(index + 1)
                && comment.kind() == CstKind::Comment
                && self.same_source_line(node, comment)
            {
                if let Some(last) = item.last_mut() {
                    last.push(' ');
                    last.push_str(self.text(comment));
                }
                index += 1;
            }
            lines.extend(item);
            index += 1;
        }
        lines
    }

    fn item(&self, node: &CstNode, depth: usize) -> Vec<String> {
        match node.kind() {
            CstKind::Property => self.property(node, depth),
            CstKind::HeaderBlock => self.header_item(node, depth),
            CstKind::ParameterBlock => self.parameter_item(node, depth),
            CstKind::BareValue | CstKind::QuotedString => {
                match self.value(node, depth, false, false) {
                    ValueLayout::Inline { text, .. } => vec![format!("{}{}", indent(depth), text)],
                    ValueLayout::Expanded {
                        opener,
                        body,
                        closer,
                    } => {
                        let mut lines = vec![format!("{}{}", indent(depth), opener)];
                        lines.extend(body);
                        lines.push(closer);
                        lines
                    }
                }
            }
            CstKind::Comment => vec![format!("{}{}", indent(depth), self.text(node))],
            CstKind::Value => node
                .children()
                .first()
                .map_or_else(Vec::new, |child| self.item(child, depth)),
            _ => vec![format!("{}{}", indent(depth), self.text(node).trim())],
        }
    }

    fn property(&self, node: &CstNode, depth: usize) -> Vec<String> {
        let Some((key, operator, value)) = property_parts(node) else {
            return vec![format!("{}{}", indent(depth), self.text(node).trim())];
        };
        let prefix = format!(
            "{}{} {} ",
            indent(depth),
            self.text(key),
            self.text(operator).trim()
        );
        let mut layout = self.value(value, depth, false, false);
        if let ValueLayout::Inline {
            text,
            width_sensitive: true,
        } = &layout
            && !fits_line(&format!("{prefix}{text}"))
        {
            layout = self.value(value, depth, false, true);
        }
        compose(prefix, layout)
    }

    fn header_item(&self, node: &CstNode, depth: usize) -> Vec<String> {
        let Some((header, block)) = header_parts(node) else {
            return vec![format!("{}{}", indent(depth), self.text(node).trim())];
        };
        let prefix = format!("{}{} ", indent(depth), self.text(header));
        let mut layout = self.block(block, depth, false, false);
        if let ValueLayout::Inline {
            text,
            width_sensitive: true,
        } = &layout
            && !fits_line(&format!("{prefix}{text}"))
        {
            layout = self.block(block, depth, false, true);
        }
        compose(prefix, layout)
    }

    fn parameter_item(&self, node: &CstNode, depth: usize) -> Vec<String> {
        match self.parameter(node, depth, false) {
            ValueLayout::Inline { text, .. } => vec![format!("{}{}", indent(depth), text)],
            ValueLayout::Expanded {
                opener,
                body,
                closer,
            } => {
                let mut lines = vec![format!("{}{}", indent(depth), opener)];
                lines.extend(body);
                lines.push(closer);
                lines
            }
        }
    }

    fn value(
        &self,
        node: &CstNode,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let node = if node.kind() == CstKind::Value {
            node.children().first().unwrap_or(node)
        } else {
            node
        };
        match node.kind() {
            CstKind::BareValue => ValueLayout::inline(self.text(node)),
            CstKind::QuotedString => self.quoted(node, depth, compact, force_expand),
            CstKind::Block => self.block(node, depth, compact, force_expand),
            CstKind::HeaderBlock => self.header_value(node, depth, compact, force_expand),
            CstKind::ParameterBlock => self.parameter(node, depth, compact),
            _ => ValueLayout::inline(self.text(node).trim()),
        }
    }

    fn header_value(
        &self,
        node: &CstNode,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let Some((header, block)) = header_parts(node) else {
            return ValueLayout::inline(self.text(node).trim());
        };
        let header = self.text(header);
        match self.block(block, depth, compact, force_expand) {
            ValueLayout::Inline {
                text,
                width_sensitive,
            } => ValueLayout::Inline {
                text: format!("{header} {text}"),
                width_sensitive,
            },
            ValueLayout::Expanded {
                opener,
                body,
                closer,
            } => ValueLayout::Expanded {
                opener: format!("{header} {opener}"),
                body,
                closer,
            },
        }
    }

    fn block(
        &self,
        node: &CstNode,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let children = node.children();
        if compact {
            let items = children
                .iter()
                .filter(|child| child.kind() != CstKind::Bom)
                .map(|child| self.compact_item(child, depth))
                .collect::<Option<Vec<_>>>();
            if let Some(items) = items {
                return if items.is_empty() {
                    ValueLayout::inline("{ }")
                } else {
                    ValueLayout::inline(format!("{{ {} }}", items.join(" ")))
                };
            }
        }

        let has_comment = children
            .iter()
            .any(|child| child.kind() == CstKind::Comment);
        if !force_expand && !has_comment {
            if children.is_empty() {
                return ValueLayout::inline("{ }");
            }
            if children.iter().all(is_scalar_node)
                && let Some(items) = children
                    .iter()
                    .map(|child| self.inline_item(child, depth))
                    .collect::<Option<Vec<_>>>()
            {
                return ValueLayout::inline(format!("{{ {} }}", items.join(" ")));
            }
            if children.len() == 1
                && children[0].kind() == CstKind::Property
                && let Some(property) = self.inline_property(&children[0], depth)
            {
                return ValueLayout::width_sensitive(format!("{{ {property} }}"));
            }
        }

        let header_comment = children
            .first()
            .filter(|node| node.kind() == CstKind::Comment);
        let body_children = if header_comment.is_some() {
            &children[1..]
        } else {
            children
        };
        let opener = header_comment.map_or_else(
            || "{".to_owned(),
            |comment| format!("{{ {}", self.text(comment)),
        );
        ValueLayout::Expanded {
            opener,
            body: self.sequence(body_children, depth.saturating_add(1)),
            closer: format!("{}}}", indent(depth)),
        }
    }

    fn inline_item(&self, node: &CstNode, depth: usize) -> Option<String> {
        match node.kind() {
            CstKind::BareValue => Some(self.text(node).to_owned()),
            CstKind::QuotedString => match self.quoted(node, depth, false, false) {
                ValueLayout::Inline { text, .. } if !contains_line_break(&text) => Some(text),
                _ => None,
            },
            CstKind::Property => self.inline_property(node, depth),
            CstKind::HeaderBlock => {
                let (header, block) = header_parts(node)?;
                match self.block(block, depth, false, false) {
                    ValueLayout::Inline { text, .. } if !contains_line_break(&text) => {
                        Some(format!("{} {text}", self.text(header)))
                    }
                    _ => None,
                }
            }
            CstKind::Block => match self.block(node, depth, false, false) {
                ValueLayout::Inline { text, .. } if !contains_line_break(&text) => Some(text),
                _ => None,
            },
            CstKind::ParameterBlock => match self.parameter(node, depth, false) {
                ValueLayout::Inline { text, .. } => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    fn inline_property(&self, node: &CstNode, depth: usize) -> Option<String> {
        let (key, operator, value) = property_parts(node)?;
        let value = match self.value(value, depth, false, false) {
            ValueLayout::Inline { text, .. } if !contains_line_break(&text) => text,
            _ => return None,
        };
        let property = format!("{} {} {value}", self.text(key), self.text(operator).trim());
        fits_line(&property).then_some(property)
    }

    fn compact_item(&self, node: &CstNode, depth: usize) -> Option<String> {
        match node.kind() {
            CstKind::Comment | CstKind::Bom => None,
            CstKind::BareValue => Some(self.text(node).to_owned()),
            CstKind::QuotedString => match self.quoted(node, depth, true, false) {
                ValueLayout::Inline { text, .. } => Some(text),
                _ => None,
            },
            CstKind::Property => {
                let (key, operator, value) = property_parts(node)?;
                let value = match self.value(value, depth, true, false) {
                    ValueLayout::Inline { text, .. } => text,
                    _ => return None,
                };
                Some(format!(
                    "{} {} {value}",
                    self.text(key),
                    self.text(operator).trim()
                ))
            }
            CstKind::HeaderBlock => {
                let (header, block) = header_parts(node)?;
                let value = match self.block(block, depth, true, false) {
                    ValueLayout::Inline { text, .. } => text,
                    _ => return None,
                };
                Some(format!("{} {value}", self.text(header)))
            }
            CstKind::Block => match self.block(node, depth, true, false) {
                ValueLayout::Inline { text, .. } => Some(text),
                _ => None,
            },
            CstKind::ParameterBlock => match self.parameter(node, depth, true) {
                ValueLayout::Inline { text, .. } => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    fn parameter(&self, node: &CstNode, depth: usize, compact: bool) -> ValueLayout {
        let Some(condition) = node
            .children()
            .first()
            .filter(|node| node.kind() == CstKind::ParameterCondition)
        else {
            return ValueLayout::inline(self.text(node).trim());
        };
        let body = &node.children()[1..];
        let condition = self.text(condition);
        let has_comment = node_has_comment(node);
        if compact || !has_comment {
            let items = body
                .iter()
                .map(|child| self.compact_item(child, depth))
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default();
            return ValueLayout::inline(format!("[[{condition}]{}]", items.join(" ")));
        }
        ValueLayout::Expanded {
            opener: format!("[[{condition}]"),
            body: self.sequence(body, depth.saturating_add(1)),
            closer: format!("{}]", indent(depth)),
        }
    }

    fn quoted(
        &self,
        node: &CstNode,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let source = self.text(node);
        let Some(script) = quoted_script(source, self.quoted_script_depth) else {
            return ValueLayout::inline(source);
        };
        let inner = PdxFormatter::new(&script.parsed, self.quoted_script_depth + 1);
        let lines = inner.document_lines();
        if lines.is_empty() {
            return ValueLayout::inline(source);
        }
        if compact || (!force_expand && lines.len() == 1 && !contains_line_break(&lines[0])) {
            return ValueLayout::width_sensitive(format!(
                "\"{}\"",
                encode_quoted_script_text(&lines[0])
            ));
        }
        ValueLayout::Expanded {
            opener: "\"".to_owned(),
            body: lines
                .into_iter()
                .map(|line| {
                    format!(
                        "{}{}",
                        indent(depth.saturating_add(1)),
                        encode_quoted_script_text(&line)
                    )
                })
                .collect(),
            closer: format!("{}\"", indent(depth)),
        }
    }

    fn text(&self, node: &CstNode) -> &str {
        self.file.text(node.range()).unwrap_or("")
    }

    fn same_source_line(&self, left: &CstNode, right: &CstNode) -> bool {
        let Some(gap) = self.file.text(
            TextRange::new(left.range().end(), right.range().start())
                .unwrap_or_else(|| TextRange::empty(left.range().end())),
        ) else {
            return false;
        };
        !contains_line_break(gap)
    }
}

fn compose(prefix: String, layout: ValueLayout) -> Vec<String> {
    match layout {
        ValueLayout::Inline { text, .. } => vec![format!("{prefix}{text}")],
        ValueLayout::Expanded {
            opener,
            body,
            closer,
        } => {
            let mut lines = vec![format!("{prefix}{opener}")];
            lines.extend(body);
            lines.push(closer);
            lines
        }
    }
}

fn property_parts(node: &CstNode) -> Option<(&CstNode, &CstNode, &CstNode)> {
    let children = node.children();
    let key = children.iter().find(|child| child.kind() == CstKind::Key)?;
    let operator = children
        .iter()
        .find(|child| child.kind() == CstKind::Operator)?;
    let value = children
        .iter()
        .find(|child| child.kind() == CstKind::Value)?;
    Some((key, operator, value))
}

fn header_parts(node: &CstNode) -> Option<(&CstNode, &CstNode)> {
    let children = node.children();
    let header = children.first()?;
    let block = children
        .iter()
        .find(|child| child.kind() == CstKind::Block)?;
    Some((header, block))
}

fn is_scalar_node(node: &CstNode) -> bool {
    matches!(node.kind(), CstKind::BareValue | CstKind::QuotedString)
}

fn node_has_comment(node: &CstNode) -> bool {
    node.kind() == CstKind::Comment || node.children().iter().any(node_has_comment)
}

pub(super) struct QuotedScript {
    pub(super) parsed: ParsedFile,
}

pub(super) fn quoted_script(source: &str, depth: usize) -> Option<QuotedScript> {
    if depth >= MAX_QUOTED_SCRIPT_DEPTH || !contains_line_break(source) {
        return None;
    }
    let parsed = parse_quoted_payload(source)?.parsed().clone();
    if !parsed.errors().is_empty() || !has_semantic_item(parsed.root()) {
        return None;
    }
    Some(QuotedScript { parsed })
}

fn has_semantic_item(root: &CstNode) -> bool {
    root.children().iter().any(|node| {
        matches!(
            node.kind(),
            CstKind::Property | CstKind::HeaderBlock | CstKind::ParameterBlock
        )
    })
}
