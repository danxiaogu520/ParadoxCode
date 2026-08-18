//! Model → script text rendering.
//!
//! Rendering is *normalizing*: known fields are written in a fixed order with the
//! file's detected indentation, while opaque blocks and unknown fields are written
//! back verbatim. Re-rendering an already rendered tree is stable (idempotent), and
//! editing one tree never touches the other trees' bytes (see [`apply_tree_edit`]).

use pdx_text::TextRange;

use crate::model::{Block, Mission, MissionTree, RawField};

/// Indentation style detected from the source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Indent {
    Tab,
    Spaces(u8),
}

/// Block separation style: Paradox's own files separate logical blocks with
/// blank lines; generic files are compact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockSpacing {
    Compact,
    Spacious,
}

/// Rendering style for one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteStyle {
    pub indent: Indent,
    pub newline: &'static str,
    pub spacing: BlockSpacing,
}

impl Default for WriteStyle {
    fn default() -> Self {
        Self {
            indent: Indent::Tab,
            newline: "\n",
            spacing: BlockSpacing::Compact,
        }
    }
}

impl WriteStyle {
    fn indent_text(self, level: usize) -> String {
        match self.indent {
            Indent::Tab => "\t".repeat(level),
            Indent::Spaces(n) => " ".repeat(usize::from(n) * level),
        }
    }
}

/// Detects indentation and line endings from an existing file so write-back blends
/// into the user's style instead of imposing one.
#[must_use]
pub fn detect_style(source: &str) -> WriteStyle {
    let probe_len = source.len().min(64 * 1024);
    let probe = &source[..probe_len];
    let crlf = probe.matches("\r\n").count();
    let lf_only = probe.matches('\n').count() - crlf;
    let newline = if crlf > lf_only { "\r\n" } else { "\n" };
    WriteStyle {
        indent: detect_indent(source),
        newline,
        spacing: detect_spacing(source),
    }
}

/// Picks block spacing from the presence of blank lines in the file.
fn detect_spacing(source: &str) -> BlockSpacing {
    let probe_len = source.len().min(256 * 1024);
    let probe = &source[..probe_len];
    let blank_lines = probe.matches("\n\n").count() + probe.matches("\r\n\r\n").count();
    if blank_lines > 0 {
        BlockSpacing::Spacious
    } else {
        BlockSpacing::Compact
    }
}

fn detect_indent(source: &str) -> Indent {
    let start = usize::from(source.starts_with('\u{feff}'));
    for line in source[start..].lines() {
        let trimmed_start = line
            .char_indices()
            .find(|(_, c)| *c != '\t' && *c != ' ')
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let trimmed = &line[trimmed_start..];
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed_start == 0 {
            continue;
        }
        let prefix = &line[..trimmed_start];
        if prefix.contains('\t') {
            return Indent::Tab;
        }
        return Indent::Spaces(u8::try_from(prefix.len()).unwrap_or(4));
    }
    Indent::Tab
}

/// Renders a single mission block at tree-field depth (block at 1, fields at 2),
/// matching the layout produced inside [`render_tree`].
#[must_use]
pub fn render_mission_block(mission: &Mission, style: &WriteStyle) -> String {
    let mut out = String::new();
    let ind1 = style.indent_text(1);
    let ind2 = style.indent_text(2);
    render_mission(&mut out, &ind1, &ind2, mission, style);
    out
}

/// Renders one tree as a full `id = { ... }` block.
#[must_use]
pub fn render_tree(tree: &MissionTree, style: &WriteStyle) -> String {
    let mut out = String::new();
    out.push_str(&tree.id);
    out.push_str(" = {");
    out.push_str(style.newline);
    if style.spacing == BlockSpacing::Spacious {
        out.push_str(style.newline);
    }

    let ind1 = style.indent_text(1);
    let ind2 = style.indent_text(2);

    scalar_field(&mut out, &ind1, "slot", &tree.slot.to_string(), style);
    scalar_field(
        &mut out,
        &ind1,
        "generic",
        if tree.generic { "yes" } else { "no" },
        style,
    );
    if let Some(ai) = tree.ai {
        scalar_field(&mut out, &ind1, "ai", if ai { "yes" } else { "no" }, style);
    }
    if let Some(block) = &tree.potential_on_load {
        block_field(&mut out, &ind1, "potential_on_load", block, style);
    }
    if let Some(block) = &tree.potential {
        block_field(&mut out, &ind1, "potential", block, style);
    }
    if let Some(shield) = tree.has_country_shield {
        scalar_field(
            &mut out,
            &ind1,
            "has_country_shield",
            if shield { "yes" } else { "no" },
            style,
        );
    }
    for field in &tree.unknown {
        raw_field(&mut out, &ind1, field, style);
    }
    if !tree.missions.is_empty() && style.spacing == BlockSpacing::Spacious {
        blank_line(&mut out, &ind1, style);
    }
    for (i, mission) in tree.missions.iter().enumerate() {
        if i > 0 && style.spacing == BlockSpacing::Spacious {
            blank_line(&mut out, &ind1, style);
        }
        render_mission(&mut out, &ind1, &ind2, mission, style);
    }

    out.push('}');
    out
}

/// Renders one mission block. Mission blocks sit at tree-field depth (1) with
/// their own fields one level deeper (2), matching the game's own formatting.
fn render_mission(
    out: &mut String,
    ind_mission: &str,
    ind_field: &str,
    mission: &Mission,
    style: &WriteStyle,
) {
    out.push_str(ind_mission);
    out.push_str(&mission.id);
    out.push_str(" = {");
    out.push_str(style.newline);

    if let Some(icon) = &mission.icon {
        scalar_field(out, ind_field, "icon", icon, style);
    }
    let mut required = String::from("required_missions = {");
    for id in &mission.required {
        required.push(' ');
        required.push_str(id);
    }
    required.push_str(" }");
    out.push_str(ind_field);
    out.push_str(&required);
    out.push_str(style.newline);
    if let Some(position) = mission.position {
        scalar_field(out, ind_field, "position", &position.to_string(), style);
    }
    if let Some(completed_by) = &mission.completed_by {
        scalar_field(out, ind_field, "completed_by", completed_by, style);
    }
    if let Some(mission_type) = &mission.mission_type {
        scalar_field(out, ind_field, "type", mission_type, style);
    }
    if let Some(block) = &mission.provinces_to_highlight {
        if style.spacing == BlockSpacing::Spacious {
            blank_line(out, ind_field, style);
        }
        block_field(out, ind_field, "provinces_to_highlight", block, style);
    }
    if let Some(block) = &mission.trigger {
        if style.spacing == BlockSpacing::Spacious {
            blank_line(out, ind_field, style);
        }
        block_field(out, ind_field, "trigger", block, style);
    }
    if let Some(block) = &mission.effect {
        if style.spacing == BlockSpacing::Spacious {
            blank_line(out, ind_field, style);
        }
        block_field(out, ind_field, "effect", block, style);
    }
    for field in &mission.unknown {
        raw_field(out, ind_field, field, style);
    }

    out.push_str(ind_mission);
    out.push('}');
    out.push_str(style.newline);
}

fn scalar_field(out: &mut String, indent: &str, name: &str, value: &str, style: &WriteStyle) {
    out.push_str(indent);
    out.push_str(name);
    out.push_str(" = ");
    out.push_str(value);
    out.push_str(style.newline);
}

fn block_field(out: &mut String, indent: &str, name: &str, block: &Block, style: &WriteStyle) {
    out.push_str(indent);
    out.push_str(name);
    out.push_str(" = ");
    out.push_str(&block.text);
    out.push_str(style.newline);
}

fn raw_field(out: &mut String, indent: &str, field: &RawField, style: &WriteStyle) {
    out.push_str(indent);
    out.push_str(&field.name);
    out.push_str(" = ");
    out.push_str(&field.value);
    out.push_str(style.newline);
}

/// Blank line carrying the block's indentation, matching Paradox's own files.
fn blank_line(out: &mut String, indent: &str, style: &WriteStyle) {
    out.push_str(indent);
    out.push_str(style.newline);
}

/// Replaces the byte range of one tree in `source` with `rendered`, leaving all
/// other bytes untouched. The caller must render from the same source the model
/// was loaded from.
#[must_use]
pub fn apply_tree_edit(source: &str, span: TextRange, rendered: &str) -> String {
    let start = span.start() as usize;
    let end = span.end() as usize;
    debug_assert!(source.get(start..end).is_some(), "tree span out of bounds");
    let mut out = String::with_capacity(source.len() + rendered.len());
    out.push_str(&source[..start]);
    out.push_str(rendered);
    out.push_str(&source[end..]);
    out
}
