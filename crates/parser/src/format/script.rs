use super::ListLayout;
use super::common::{contains_line_break, fits_line, indent};
use super::paths::{is_asset_path_spelling, normalize_asset_path_separators};
use crate::{
    CstKind, CstNode, ParsedFile, encode_quoted_script_text,
    parse_quoted_script as parse_quoted_payload,
};
use std::borrow::Cow;
use std::collections::BTreeMap;
use text::TextRange;
const MAX_QUOTED_SCRIPT_DEPTH: usize = 64;
pub(super) fn format_script(
    file: &ParsedFile,
    layouts: &BTreeMap<TextRange, ListLayout>,
) -> String {
    PdcFormatter::new(file, 0, Some(layouts)).document()
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

struct PdcFormatter<'file> {
    file: &'file ParsedFile,
    quoted_script_depth: usize,
    list_layouts: Option<&'file BTreeMap<TextRange, ListLayout>>,
}

impl<'file> PdcFormatter<'file> {
    fn new(
        file: &'file ParsedFile,
        quoted_script_depth: usize,
        list_layouts: Option<&'file BTreeMap<TextRange, ListLayout>>,
    ) -> Self {
        Self {
            file,
            quoted_script_depth,
            list_layouts,
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
        let children: Vec<_> = self.file.root().children().collect();
        let has_bom = children
            .first()
            .is_some_and(|node| node.kind() == CstKind::Bom);
        let children = if has_bom {
            &children[1..]
        } else {
            &children[..]
        };
        let mut lines = self.sequence(children, 0, None);
        if has_bom {
            if let Some(first) = lines.first_mut() {
                first.insert(0, '\u{feff}');
            } else {
                lines.push("\u{feff}".to_owned());
            }
        }
        lines
    }

    fn sequence(&self, children: &[CstNode<'_>], depth: usize, pack: Option<usize>) -> Vec<String> {
        let mut lines = Vec::new();
        let mut index = 0;
        while index < children.len() {
            let node = children[index];
            if node.kind() == CstKind::Bom {
                index += 1;
                continue;
            }
            if node.kind() == CstKind::Comment {
                lines.push(format!("{}{}", indent(depth), self.text(node)));
                index += 1;
                continue;
            }

            let packed =
                pack.and_then(|stride| self.pack_scalars(&children[index..], depth, stride));
            let (mut item, consumed) = packed.unwrap_or_else(|| (self.item(node, depth), 1));
            index += consumed;
            if let Some(comment) = children.get(index)
                && comment.kind() == CstKind::Comment
                && self.same_source_line(children[index - 1], *comment)
            {
                if let Some(last) = item.last_mut() {
                    last.push(' ');
                    last.push_str(self.text(*comment));
                }
                index += 1;
            }
            lines.extend(item);
        }
        lines
    }

    fn pack_scalars(
        &self,
        children: &[CstNode<'_>],
        depth: usize,
        stride: usize,
    ) -> Option<(Vec<String>, usize)> {
        let mut items = Vec::new();
        for (index, child) in children.iter().copied().enumerate() {
            if !is_scalar_node(child)
                || (index > 0 && !self.same_source_line(children[index - 1], child))
            {
                break;
            }
            let Some(item) = self.inline_item(child, depth) else {
                break;
            };
            items.push(item);
        }
        if items.is_empty() {
            return None;
        }

        let indentation = indent(depth);
        let mut lines = Vec::new();
        let mut chunks = items.chunks(stride).map(|chunk| chunk.join(" "));
        let mut line = format!("{indentation}{}", chunks.next()?);
        for chunk in chunks {
            let next = format!("{line} {chunk}");
            if fits_line(&next) {
                line = next;
            } else {
                lines.push(line);
                line = format!("{indentation}{chunk}");
            }
        }
        lines.push(line);
        Some((lines, items.len()))
    }

    fn item(&self, node: CstNode<'_>, depth: usize) -> Vec<String> {
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
                .next()
                .map_or_else(Vec::new, |child| self.item(child, depth)),
            _ => vec![format!("{}{}", indent(depth), self.text(node).trim())],
        }
    }

    fn property(&self, node: CstNode<'_>, depth: usize) -> Vec<String> {
        let Some((key, operator, value)) = property_parts(node) else {
            return vec![format!("{}{}", indent(depth), self.text(node).trim())];
        };
        let prefix = format!(
            "{}{} {} ",
            indent(depth),
            canonical_keyword(self.text(key)),
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

    fn header_item(&self, node: CstNode<'_>, depth: usize) -> Vec<String> {
        let Some((header, block)) = header_parts(node) else {
            return vec![format!("{}{}", indent(depth), self.text(node).trim())];
        };
        let prefix = format!("{}{} ", indent(depth), canonical_keyword(self.text(header)));
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

    fn parameter_item(&self, node: CstNode<'_>, depth: usize) -> Vec<String> {
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
        node: CstNode<'_>,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let node = if node.kind() == CstKind::Value {
            node.children().next().unwrap_or(node)
        } else {
            node
        };
        match node.kind() {
            CstKind::BareValue => ValueLayout::inline(canonical_scalar(self.text(node))),
            CstKind::QuotedString => self.quoted(node, depth, compact, force_expand),
            CstKind::Block => self.block(node, depth, compact, force_expand),
            CstKind::HeaderBlock => self.header_value(node, depth, compact, force_expand),
            CstKind::ParameterBlock => self.parameter(node, depth, compact),
            _ => ValueLayout::inline(canonical_keyword(self.text(node).trim())),
        }
    }

    fn header_value(
        &self,
        node: CstNode<'_>,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let Some((header, block)) = header_parts(node) else {
            return ValueLayout::inline(self.text(node).trim());
        };
        let header = canonical_keyword(self.text(header));
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
        node: CstNode<'_>,
        depth: usize,
        compact: bool,
        mut force_expand: bool,
    ) -> ValueLayout {
        let children: Vec<_> = node.children().collect();
        let list_layout = self
            .list_layouts
            .and_then(|layouts| layouts.get(&node.range()).copied())
            .unwrap_or_default();
        let has_scalar_items = children.iter().any(|child| is_scalar_node(*child));
        let multiline_list = has_scalar_items
            && match list_layout {
                ListLayout::Inline => false,
                ListLayout::OnePerLine => children
                    .iter()
                    .filter(|child| is_scalar_node(**child))
                    .nth(1)
                    .is_some(),
                _ => contains_line_break(self.text(node)),
            };
        if compact && !multiline_list {
            let items = children
                .iter()
                .filter(|child| child.kind() != CstKind::Bom)
                .map(|child| self.compact_item(*child, depth))
                .collect::<Option<Vec<_>>>();
            if let Some(items) = items {
                if items.is_empty() {
                    return ValueLayout::inline("{ }");
                }
                let text = format!("{{ {} }}", items.join(" "));
                if !has_scalar_items || list_layout == ListLayout::Inline || fits_line(&text) {
                    return ValueLayout::inline(text);
                }
                force_expand = true;
            }
        }

        let has_comment = children
            .iter()
            .any(|child| child.kind() == CstKind::Comment);
        if !force_expand && !has_comment {
            if children.is_empty() {
                return ValueLayout::inline("{ }");
            }
            if !multiline_list
                && children.iter().all(|child| is_scalar_node(*child))
                && let Some(items) = children
                    .iter()
                    .map(|child| self.inline_item(*child, depth))
                    .collect::<Option<Vec<_>>>()
            {
                let text = format!("{{ {} }}", items.join(" "));
                return if list_layout == ListLayout::Inline {
                    ValueLayout::inline(text)
                } else {
                    ValueLayout::width_sensitive(text)
                };
            }
            if children.len() == 1
                && children[0].kind() == CstKind::Property
                && let Some(property) = self.inline_property(children[0], depth)
            {
                return ValueLayout::width_sensitive(format!("{{ {property} }}"));
            }
        }

        let pack = match list_layout {
            ListLayout::Wrap => Some(1),
            ListLayout::WrapPairs => Some(2),
            _ => None,
        };
        let header_comment = children.first().filter(|comment| {
            comment.kind() == CstKind::Comment
                && ((!has_scalar_items && list_layout == ListLayout::Auto)
                    || self
                        .file
                        .source()
                        .get(node.range().start() as usize..comment.range().start() as usize)
                        .is_some_and(|prefix| !contains_line_break(prefix)))
        });
        let body_children = if header_comment.is_some() {
            &children[1..]
        } else {
            &children[..]
        };
        let opener = header_comment.map_or_else(
            || "{".to_owned(),
            |comment| format!("{{ {}", self.text(*comment)),
        );
        ValueLayout::Expanded {
            opener,
            body: self.sequence(body_children, depth.saturating_add(1), pack),
            closer: format!("{}}}", indent(depth)),
        }
    }

    fn inline_item(&self, node: CstNode<'_>, depth: usize) -> Option<String> {
        match node.kind() {
            CstKind::BareValue => Some(canonical_scalar(self.text(node)).into_owned()),
            CstKind::QuotedString => match self.quoted(node, depth, false, false) {
                ValueLayout::Inline { text, .. } if !contains_line_break(&text) => Some(text),
                _ => None,
            },
            CstKind::Property => self.inline_property(node, depth),
            CstKind::HeaderBlock => {
                let (header, block) = header_parts(node)?;
                match self.block(block, depth, false, false) {
                    ValueLayout::Inline { text, .. } if !contains_line_break(&text) => {
                        Some(format!("{} {text}", canonical_keyword(self.text(header))))
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

    fn inline_property(&self, node: CstNode<'_>, depth: usize) -> Option<String> {
        let (key, operator, value) = property_parts(node)?;
        let value = match self.value(value, depth, false, false) {
            ValueLayout::Inline { text, .. } if !contains_line_break(&text) => text,
            _ => return None,
        };
        let property = format!(
            "{} {} {value}",
            canonical_keyword(self.text(key)),
            self.text(operator).trim()
        );
        fits_line(&property).then_some(property)
    }

    fn compact_item(&self, node: CstNode<'_>, depth: usize) -> Option<String> {
        match node.kind() {
            CstKind::Comment | CstKind::Bom => None,
            CstKind::BareValue => Some(canonical_scalar(self.text(node)).into_owned()),
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
                    canonical_keyword(self.text(key)),
                    self.text(operator).trim()
                ))
            }
            CstKind::HeaderBlock => {
                let (header, block) = header_parts(node)?;
                let value = match self.block(block, depth, true, false) {
                    ValueLayout::Inline { text, .. } => text,
                    _ => return None,
                };
                Some(format!("{} {value}", canonical_keyword(self.text(header))))
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

    fn parameter(&self, node: CstNode<'_>, depth: usize, compact: bool) -> ValueLayout {
        let children: Vec<_> = node.children().collect();
        let Some(condition) = children
            .first()
            .filter(|node| node.kind() == CstKind::ParameterCondition)
        else {
            return ValueLayout::inline(self.text(node).trim());
        };
        let body = &children[1..];
        let condition = self.text(*condition);
        let has_comment = node_has_comment(node);
        if (compact || !has_comment)
            && let Some(items) = body
                .iter()
                .map(|child| self.compact_item(*child, depth))
                .collect::<Option<Vec<_>>>()
        {
            return ValueLayout::inline(format!("[[{condition}]{}]", items.join(" ")));
        }
        ValueLayout::Expanded {
            opener: format!("[[{condition}]"),
            body: self.sequence(body, depth.saturating_add(1), None),
            closer: format!("{}]", indent(depth)),
        }
    }

    fn quoted(
        &self,
        node: CstNode<'_>,
        depth: usize,
        compact: bool,
        force_expand: bool,
    ) -> ValueLayout {
        let source = self.text(node);
        let Some(script) = quoted_script(source, self.quoted_script_depth) else {
            if is_asset_path_spelling(source) {
                return ValueLayout::inline(normalize_asset_path_separators(source));
            }
            return ValueLayout::inline(source);
        };
        let inner = PdcFormatter::new(&script.parsed, self.quoted_script_depth + 1, None);
        let lines = inner.document_lines();
        if lines.is_empty() {
            return ValueLayout::inline(source);
        }
        if let [line] = lines.as_slice()
            && (compact || (!force_expand && !contains_line_break(line)))
        {
            return ValueLayout::width_sensitive(format!(
                "\"{}\"",
                encode_quoted_script_text(line)
            ));
        }
        if compact {
            return ValueLayout::inline(source);
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

    fn text(&self, node: CstNode<'_>) -> &str {
        self.file.text(node.range()).unwrap_or("")
    }

    fn same_source_line(&self, left: CstNode<'_>, right: CstNode<'_>) -> bool {
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

/// Canonical spelling of a bare scalar: the fixed keyword capitals, then
/// asset-path separator normalization for path-shaped values.
fn canonical_scalar(text: &str) -> Cow<'_, str> {
    let keyword = canonical_keyword(text);
    if is_asset_path_spelling(keyword) {
        normalize_asset_path_separators(keyword)
    } else {
        Cow::Borrowed(keyword)
    }
}

/// Canonical spelling of the fixed script keywords Paradox convention writes in capitals.
/// Any other token is returned unchanged; matching is case-insensitive so mixed spellings
/// normalize to the canonical form. A leading byte order mark (the lexer attaches it to the
/// first key token of a BOM-prefixed file) is preserved and skipped for matching.
pub fn canonical_keyword(text: &str) -> &str {
    match text {
        _ if text.eq_ignore_ascii_case("and") => "AND",
        _ if text.eq_ignore_ascii_case("or") => "OR",
        _ if text.eq_ignore_ascii_case("not") => "NOT",
        _ if text.eq_ignore_ascii_case("root") => "ROOT",
        _ if text.eq_ignore_ascii_case("from") => "FROM",
        _ if text.eq_ignore_ascii_case("prev") => "PREV",
        _ if text.eq_ignore_ascii_case("this") => "THIS",
        _ if text.eq_ignore_ascii_case("\u{feff}and") => "\u{feff}AND",
        _ if text.eq_ignore_ascii_case("\u{feff}or") => "\u{feff}OR",
        _ if text.eq_ignore_ascii_case("\u{feff}not") => "\u{feff}NOT",
        _ if text.eq_ignore_ascii_case("\u{feff}root") => "\u{feff}ROOT",
        _ if text.eq_ignore_ascii_case("\u{feff}from") => "\u{feff}FROM",
        _ if text.eq_ignore_ascii_case("\u{feff}prev") => "\u{feff}PREV",
        _ if text.eq_ignore_ascii_case("\u{feff}this") => "\u{feff}THIS",
        _ => text,
    }
}

fn property_parts(node: CstNode<'_>) -> Option<(CstNode<'_>, CstNode<'_>, CstNode<'_>)> {
    let key = node.children().find(|child| child.kind() == CstKind::Key)?;
    let operator = node
        .children()
        .find(|child| child.kind() == CstKind::Operator)?;
    let value = node
        .children()
        .find(|child| child.kind() == CstKind::Value)?;
    Some((key, operator, value))
}

fn header_parts(node: CstNode<'_>) -> Option<(CstNode<'_>, CstNode<'_>)> {
    let header = node.children().next()?;
    let block = node
        .children()
        .find(|child| child.kind() == CstKind::Block)?;
    Some((header, block))
}

fn is_scalar_node(node: CstNode<'_>) -> bool {
    matches!(node.kind(), CstKind::BareValue | CstKind::QuotedString)
}

fn node_has_comment(node: CstNode<'_>) -> bool {
    node.kind() == CstKind::Comment || node.children().any(node_has_comment)
}

pub(super) struct QuotedScript {
    pub(super) parsed: ParsedFile,
}

pub(super) fn quoted_script(source: &str, depth: usize) -> Option<QuotedScript> {
    let multiline = contains_line_break(source);
    if depth >= MAX_QUOTED_SCRIPT_DEPTH || (!multiline && !source.contains('{')) {
        return None;
    }
    let script = parse_quoted_payload(source)?;
    let parsed = script.parsed();
    if !parsed.errors().is_empty() {
        return None;
    }
    let is_script = if multiline {
        has_semantic_item(parsed.root())
    } else {
        has_inline_script_block(parsed.root())
    };
    is_script.then(|| QuotedScript {
        parsed: parsed.clone(),
    })
}

fn has_inline_script_block(root: CstNode<'_>) -> bool {
    // A block assignment signals Script even on one line. Loose scalars can be prose.
    let mut has_block = false;
    for node in root.children() {
        match node.kind() {
            CstKind::Property => {
                has_block |= property_parts(node)
                    .and_then(|(_, _, value)| value.child(0))
                    .is_some_and(|value| {
                        matches!(value.kind(), CstKind::Block | CstKind::HeaderBlock)
                    });
            }
            CstKind::HeaderBlock | CstKind::ParameterBlock | CstKind::Comment | CstKind::Bom => {}
            _ => return false,
        }
    }
    has_block
}

fn has_semantic_item(root: CstNode<'_>) -> bool {
    root.children().any(|node| {
        matches!(
            node.kind(),
            CstKind::Property | CstKind::HeaderBlock | CstKind::ParameterBlock
        )
    })
}
