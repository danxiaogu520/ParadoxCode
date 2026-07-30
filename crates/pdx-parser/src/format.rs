//! Canonical, editor-neutral formatting for the PDX text frontends.
//!
//! The formatter is intentionally non-configurable. Script uses tabs, LF line endings,
//! recursive block layout, and no layout blank lines. Ordinary scalar spelling is preserved;
//! multiline quoted strings are formatted recursively only when their decoded payload is
//! demonstrably valid, non-empty Script.

use crate::{CstKind, CstNode, FileFormat, ParsedFile, TokenKind, parse};
use pdx_text::TextRange;
use unicode_width::UnicodeWidthChar;

const LINE_WIDTH: usize = 120;
const TAB_WIDTH: usize = 4;
const MAX_QUOTED_SCRIPT_DEPTH: usize = 64;

/// Why formatting did not produce edits.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FormatSkipReason {
    /// The parser reported an error, so a rewrite cannot be proven safe.
    UnsafeSyntax,
    /// The canonical output failed structural, token, or idempotence validation.
    SafetyValidationFailed,
}

/// A single source edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    /// Source range to replace.
    pub range: TextRange,
    /// Replacement text.
    pub replacement: String,
}

/// Formatter result that can safely contain no edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatResult {
    /// Safe, non-overlapping edits in source order.
    pub edits: Vec<TextEdit>,
    /// Explicit reason when formatting was skipped.
    pub skipped: Option<FormatSkipReason>,
}

/// Formats a parsed document using the fixed canonical style.
#[must_use]
pub fn format(file: &ParsedFile) -> FormatResult {
    if !file.errors().is_empty() {
        return skipped(FormatSkipReason::UnsafeSyntax);
    }
    let formatted = canonical_text(file);
    if formatted == file.source() {
        return FormatResult { edits: Vec::new(), skipped: None };
    }

    let reparsed = parse(file.format(), &formatted);
    if !reparsed.errors().is_empty()
        || !equivalent(file, &reparsed, 0)
        || canonical_text(&reparsed) != formatted
    {
        return skipped(FormatSkipReason::SafetyValidationFailed);
    }

    let Some(edits) = minimal_edits(file, &reparsed) else {
        return skipped(FormatSkipReason::SafetyValidationFailed);
    };
    FormatResult { edits, skipped: None }
}

fn skipped(reason: FormatSkipReason) -> FormatResult {
    FormatResult { edits: Vec::new(), skipped: Some(reason) }
}

fn canonical_text(file: &ParsedFile) -> String {
    match file.format() {
        FileFormat::Script => PdxFormatter::new(file, 0).document(),
        FileFormat::Localisation => format_localisation(file),
    }
}

#[derive(Clone, Debug)]
enum ValueLayout {
    Inline { text: String, width_sensitive: bool },
    Expanded { opener: String, body: Vec<String>, closer: String },
}

impl ValueLayout {
    fn inline(text: impl Into<String>) -> Self {
        Self::Inline { text: text.into(), width_sensitive: false }
    }

    fn width_sensitive(text: impl Into<String>) -> Self {
        Self::Inline { text: text.into(), width_sensitive: true }
    }
}

struct PdxFormatter<'file> {
    file: &'file ParsedFile,
    quoted_script_depth: usize,
}

impl<'file> PdxFormatter<'file> {
    fn new(file: &'file ParsedFile, quoted_script_depth: usize) -> Self {
        Self { file, quoted_script_depth }
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
        let has_bom = children.first().is_some_and(|node| node.kind() == CstKind::Bom);
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
                    ValueLayout::Expanded { opener, body, closer } => {
                        let mut lines = vec![format!("{}{}", indent(depth), opener)];
                        lines.extend(body);
                        lines.push(closer);
                        lines
                    }
                }
            }
            CstKind::Comment => vec![format!("{}{}", indent(depth), self.text(node))],
            CstKind::Value => {
                node.children().first().map_or_else(Vec::new, |child| self.item(child, depth))
            }
            _ => vec![format!("{}{}", indent(depth), self.text(node).trim())],
        }
    }

    fn property(&self, node: &CstNode, depth: usize) -> Vec<String> {
        let Some((key, operator, value)) = property_parts(node) else {
            return vec![format!("{}{}", indent(depth), self.text(node).trim())];
        };
        let prefix = format!("{}{} {} ", indent(depth), self.text(key), self.text(operator).trim());
        let mut layout = self.value(value, depth, false, false);
        if let ValueLayout::Inline { text, width_sensitive: true } = &layout
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
        if let ValueLayout::Inline { text, width_sensitive: true } = &layout
            && !fits_line(&format!("{prefix}{text}"))
        {
            layout = self.block(block, depth, false, true);
        }
        compose(prefix, layout)
    }

    fn parameter_item(&self, node: &CstNode, depth: usize) -> Vec<String> {
        match self.parameter(node, depth, false) {
            ValueLayout::Inline { text, .. } => vec![format!("{}{}", indent(depth), text)],
            ValueLayout::Expanded { opener, body, closer } => {
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
            ValueLayout::Inline { text, width_sensitive } => {
                ValueLayout::Inline { text: format!("{header} {text}"), width_sensitive }
            }
            ValueLayout::Expanded { opener, body, closer } => {
                ValueLayout::Expanded { opener: format!("{header} {opener}"), body, closer }
            }
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

        let has_comment = children.iter().any(|child| child.kind() == CstKind::Comment);
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

        let header_comment = children.first().filter(|node| node.kind() == CstKind::Comment);
        let body_children = if header_comment.is_some() { &children[1..] } else { children };
        let opener = header_comment
            .map_or_else(|| "{".to_owned(), |comment| format!("{{ {}", self.text(comment)));
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
                Some(format!("{} {} {value}", self.text(key), self.text(operator).trim()))
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
        let Some(condition) =
            node.children().first().filter(|node| node.kind() == CstKind::ParameterCondition)
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
            return ValueLayout::width_sensitive(format!("\"{}\"", encode_payload(&lines[0])));
        }
        ValueLayout::Expanded {
            opener: "\"".to_owned(),
            body: lines
                .into_iter()
                .map(|line| format!("{}{}", indent(depth.saturating_add(1)), encode_payload(&line)))
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
        ValueLayout::Expanded { opener, body, closer } => {
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
    let operator = children.iter().find(|child| child.kind() == CstKind::Operator)?;
    let value = children.iter().find(|child| child.kind() == CstKind::Value)?;
    Some((key, operator, value))
}

fn header_parts(node: &CstNode) -> Option<(&CstNode, &CstNode)> {
    let children = node.children();
    let header = children.first()?;
    let block = children.iter().find(|child| child.kind() == CstKind::Block)?;
    Some((header, block))
}

fn is_scalar_node(node: &CstNode) -> bool {
    matches!(node.kind(), CstKind::BareValue | CstKind::QuotedString)
}

fn node_has_comment(node: &CstNode) -> bool {
    node.kind() == CstKind::Comment || node.children().iter().any(node_has_comment)
}

struct QuotedScript {
    parsed: ParsedFile,
}

fn quoted_script(source: &str, depth: usize) -> Option<QuotedScript> {
    if depth >= MAX_QUOTED_SCRIPT_DEPTH || !contains_line_break(source) {
        return None;
    }
    let payload = quoted_payload(source)?;
    let decoded = decode_payload(payload)?;
    let parsed = parse(FileFormat::Script, &decoded);
    if !parsed.errors().is_empty() || !has_semantic_item(parsed.root()) {
        return None;
    }
    Some(QuotedScript { parsed })
}

fn has_semantic_item(root: &CstNode) -> bool {
    root.children().iter().any(|node| {
        matches!(node.kind(), CstKind::Property | CstKind::HeaderBlock | CstKind::ParameterBlock)
    })
}

fn quoted_payload(source: &str) -> Option<&str> {
    source.strip_prefix('"')?.strip_suffix('"')
}

fn decode_payload(payload: &str) -> Option<String> {
    let mut decoded = String::with_capacity(payload.len());
    let mut characters = payload.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escaped = characters.next()?;
        match escaped {
            '"' | '\\' => decoded.push(escaped),
            _ => {
                decoded.push('\\');
                decoded.push(escaped);
            }
        }
    }
    Some(decoded)
}

fn encode_payload(payload: &str) -> String {
    let mut encoded = String::with_capacity(payload.len());
    for character in payload.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            _ => encoded.push(character),
        }
    }
    encoded
}

fn format_localisation(file: &ParsedFile) -> String {
    let mut lines = Vec::new();
    let mut bom = false;
    for node in file.root().children() {
        match node.kind() {
            CstKind::Bom => bom = true,
            CstKind::Comment => lines.push(file.text(node.range()).unwrap_or("").to_owned()),
            CstKind::LanguageHeader => {
                let key = node.children().first().and_then(|child| file.text(child.range()));
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
    if lines.is_empty() { String::new() } else { format!("{}\n", lines.join("\n")) }
}

fn format_localisation_entry(file: &ParsedFile, node: &CstNode) -> String {
    let children = node.children();
    let key = children
        .iter()
        .find(|child| child.kind() == CstKind::LocalisationKey)
        .and_then(|child| file.text(child.range()))
        .unwrap_or("");
    let version = children
        .iter()
        .find(|child| child.kind() == CstKind::Version)
        .and_then(|child| file.text(child.range()));
    let value_node = children
        .iter()
        .find(|child| matches!(child.kind(), CstKind::LocalisationString | CstKind::UnquotedValue));
    let value = value_node.and_then(|child| file.text(child.range())).unwrap_or("");
    let has_colon = value_node.is_some_and(|value_node| {
        let range = TextRange::new(
            children
                .iter()
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
    if let Some(comment) = children.iter().find(|child| child.kind() == CstKind::Comment) {
        line.push(' ');
        line.push_str(file.text(comment.range()).unwrap_or(""));
    }
    line
}

fn equivalent(original: &ParsedFile, formatted: &ParsedFile, depth: usize) -> bool {
    if original.format() != formatted.format()
        || !formatted.errors().is_empty()
        || !same_tree_shape(original.root(), formatted.root())
        || original.tokens().len() != formatted.tokens().len()
    {
        return false;
    }
    original.tokens().iter().zip(formatted.tokens()).all(|(before, after)| {
        if before.kind() != after.kind() {
            return false;
        }
        let Some(before_text) = original.text(before.range()) else {
            return false;
        };
        let Some(after_text) = formatted.text(after.range()) else {
            return false;
        };
        if before.kind() == TokenKind::Quoted
            && let Some(before_script) = quoted_script(before_text, depth)
        {
            let Some(after_payload) = quoted_payload(after_text).and_then(decode_payload) else {
                return false;
            };
            let after_script = parse(FileFormat::Script, &after_payload);
            return equivalent(&before_script.parsed, &after_script, depth.saturating_add(1));
        }
        before_text == after_text
    })
}

fn same_tree_shape(left: &CstNode, right: &CstNode) -> bool {
    left.kind() == right.kind()
        && left.children().len() == right.children().len()
        && left
            .children()
            .iter()
            .zip(right.children())
            .all(|(left, right)| same_tree_shape(left, right))
}

fn minimal_edits(original: &ParsedFile, formatted: &ParsedFile) -> Option<Vec<TextEdit>> {
    if original.tokens().len() != formatted.tokens().len() {
        return None;
    }
    let mut edits = Vec::new();
    let mut before_end = 0_usize;
    let mut after_end = 0_usize;
    for (before, after) in original.tokens().iter().zip(formatted.tokens()) {
        if before.kind() != after.kind() {
            return None;
        }
        let before_start = usize::try_from(before.range().start()).ok()?;
        let after_start = usize::try_from(after.range().start()).ok()?;
        push_changed_range(
            &mut edits,
            original.source(),
            before_end,
            before_start,
            formatted.source().get(after_end..after_start)?,
        )?;

        let before_text = original.text(before.range())?;
        let after_text = formatted.text(after.range())?;
        if before_text != after_text {
            if before.kind() != TokenKind::Quoted || quoted_script(before_text, 0).is_none() {
                return None;
            }
            push_minimal_token_edit(&mut edits, before_text, after_text, before_start)?;
        }
        before_end = usize::try_from(before.range().end()).ok()?;
        after_end = usize::try_from(after.range().end()).ok()?;
    }
    push_changed_range(
        &mut edits,
        original.source(),
        before_end,
        original.source().len(),
        formatted.source().get(after_end..)?,
    )?;
    Some(edits)
}

fn push_changed_range(
    edits: &mut Vec<TextEdit>,
    source: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Option<()> {
    if source.get(start..end)? == replacement {
        return Some(());
    }
    edits.push(TextEdit { range: text_range(start, end)?, replacement: replacement.to_owned() });
    Some(())
}

fn push_minimal_token_edit(
    edits: &mut Vec<TextEdit>,
    before: &str,
    after: &str,
    absolute_start: usize,
) -> Option<()> {
    let mut prefix =
        before.bytes().zip(after.bytes()).take_while(|(left, right)| left == right).count();
    while prefix > 0 && (!before.is_char_boundary(prefix) || !after.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let max_suffix = before.len().min(after.len()).saturating_sub(prefix);
    let mut suffix = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take(max_suffix)
        .take_while(|(left, right)| left == right)
        .count();
    while suffix > 0
        && (!before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix))
    {
        suffix -= 1;
    }

    let start = absolute_start.checked_add(prefix)?;
    let end = absolute_start.checked_add(before.len().saturating_sub(suffix))?;
    let replacement_end = after.len().saturating_sub(suffix);
    edits.push(TextEdit {
        range: text_range(start, end)?,
        replacement: after.get(prefix..replacement_end)?.to_owned(),
    });
    Some(())
}

fn text_range(start: usize, end: usize) -> Option<TextRange> {
    TextRange::new(u32::try_from(start).ok()?, u32::try_from(end).ok()?)
}

fn indent(depth: usize) -> String {
    "\t".repeat(depth)
}

fn fits_line(line: &str) -> bool {
    !contains_line_break(line) && display_width(line) <= LINE_WIDTH
}

fn display_width(text: &str) -> usize {
    text.chars().fold(0_usize, |column, character| {
        if character == '\t' {
            column.saturating_add(TAB_WIDTH - column % TAB_WIDTH)
        } else {
            column.saturating_add(UnicodeWidthChar::width(character).unwrap_or(0))
        }
    })
}

fn contains_line_break(text: &str) -> bool {
    text.contains(['\r', '\n'])
}

#[cfg(test)]
mod tests {
    use crate::{FileFormat, SyntaxErrorKind, parse};

    use super::{FormatSkipReason, TextEdit, format};

    fn apply(source: &str, edits: &[TextEdit]) -> String {
        let mut output = source.to_owned();
        for edit in edits.iter().rev() {
            let start = usize::try_from(edit.range.start()).expect("start");
            let end = usize::try_from(edit.range.end()).expect("end");
            output.replace_range(start..end, &edit.replacement);
        }
        output
    }

    fn formatted(format_kind: FileFormat, source: &str) -> String {
        let parsed = parse(format_kind, source);
        assert!(parsed.errors().is_empty(), "errors: {:?}", parsed.errors());
        let result = format(&parsed);
        assert!(result.skipped.is_none(), "skipped: {:?}", result.skipped);
        apply(source, &result.edits)
    }

    #[test]
    fn canonical_block_layout_is_recursive_and_idempotent() {
        let source = "outer={inner={factor=1}}\nlist={one\ntwo\nthree}\nmany={a=1 b=2}\n";
        let expected = concat!(
            "outer = { inner = { factor = 1 } }\n",
            "list = { one two three }\n",
            "many = {\n",
            "\ta = 1\n",
            "\tb = 2\n",
            "}\n",
        );
        let output = formatted(FileFormat::Script, source);
        assert_eq!(output, expected);
        assert!(format(&parse(FileFormat::Script, &output)).edits.is_empty());
    }

    #[test]
    fn comments_expand_blocks_and_first_leading_comment_joins_opener() {
        let source = "root = {\n\n# header\n# second\nchild=yes # tail\n}\n";
        let expected = "root = { # header\n\t# second\n\tchild = yes # tail\n}\n";
        assert_eq!(formatted(FileFormat::Script, source), expected);
    }

    #[test]
    fn parameter_blocks_are_compact_unless_they_contain_comments() {
        let compact = "[[name]\na=1\nb={x=1 y=2}\n]\n";
        assert_eq!(formatted(FileFormat::Script, compact), "[[name]a = 1 b = { x = 1 y = 2 }]\n");
        let commented = "[[name]\n# note\na=1\n]\n";
        assert_eq!(formatted(FileFormat::Script, commented), "[[name]\n\t# note\n\ta = 1\n]\n");
    }

    #[test]
    fn quoted_script_collapses_or_expands_and_opaque_text_is_unchanged() {
        let source = concat!(
            "first_limit = \"\n\thas_disaster = example\n\"\n",
            "first_effect = \"\n\ta = yes\n\tb = { x = 1 }\n\"\n",
            "description = \"first prose line\n\nsecond prose line\"\n",
        );
        let expected = concat!(
            "first_limit = \"has_disaster = example\"\n",
            "first_effect = \"\n",
            "\ta = yes\n",
            "\tb = { x = 1 }\n",
            "\"\n",
            "description = \"first prose line\n\nsecond prose line\"\n",
        );
        assert_eq!(formatted(FileFormat::Script, source), expected);
    }

    #[test]
    fn quoted_script_supports_escaped_nested_quotes() {
        let source = "effect = \"\n\tname = \\\"quoted\\\"\n\tvalue = yes\n\"\n";
        let expected = "effect = \"\n\tname = \\\"quoted\\\"\n\tvalue = yes\n\"\n";
        assert_eq!(formatted(FileFormat::Script, source), expected);
    }

    #[test]
    fn formatting_uses_tabs_lf_no_blank_lines_and_one_final_newline() {
        let source = "\u{feff}root = {\r\n  child = yes\r\n\r\n}\r\n\r\n";
        assert_eq!(formatted(FileFormat::Script, source), "\u{feff}root = { child = yes }\n");
    }

    #[test]
    fn line_width_expands_properties_but_never_scalar_only_blocks() {
        let long_key = "界".repeat(58);
        let property_source = format!("root = {{ {long_key} = yes }}\n");
        let property_expected = format!("root = {{\n\t{long_key} = yes\n}}\n");
        assert_eq!(formatted(FileFormat::Script, &property_source), property_expected);

        let scalar = "value".repeat(30);
        let scalar_source = format!("list = {{\n{scalar}\n}}\n");
        assert_eq!(
            formatted(FileFormat::Script, &scalar_source),
            format!("list = {{ {scalar} }}\n")
        );
    }

    #[test]
    fn opaque_multiline_scalar_preserves_internal_crlf_and_blank_lines() {
        let source = "description = \"first\r\n\r\nsecond\"\r\n";
        assert_eq!(
            formatted(FileFormat::Script, source),
            "description = \"first\r\n\r\nsecond\"\n"
        );
    }

    #[test]
    fn localisation_is_canonical_but_values_remain_opaque() {
        let source = "\u{feff}  l_english:  \r\n\r\n hello:0 \"  text  \"   \r\n# note\r\n";
        let expected = "\u{feff}l_english:\nhello:0 \"  text  \"\n# note\n";
        assert_eq!(formatted(FileFormat::Localisation, source), expected);
    }

    #[test]
    fn unsafe_syntax_does_not_generate_edits() {
        let parsed = parse(FileFormat::Script, "broken = \"unfinished");
        assert!(
            parsed.errors().iter().any(|error| error.kind == SyntaxErrorKind::UnterminatedString)
        );
        let result = format(&parsed);
        assert!(result.edits.is_empty());
        assert_eq!(result.skipped, Some(FormatSkipReason::UnsafeSyntax));
    }

    #[test]
    fn formatter_emits_precise_non_overlapping_edits() {
        let source = "root={a=1 b=2}";
        let parsed = parse(FileFormat::Script, source);
        let result = format(&parsed);
        assert!(result.skipped.is_none());
        assert!(result.edits.len() > 1);
        assert!(result.edits.windows(2).all(|pair| pair[0].range.end() <= pair[1].range.start()));
        assert_eq!(apply(source, &result.edits), "root = {\n\ta = 1\n\tb = 2\n}\n");
    }
}
