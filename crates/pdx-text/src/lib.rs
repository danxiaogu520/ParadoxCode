//! Editor-neutral text primitives.
//!
//! This crate deliberately has no dependency on an editor protocol or EU4 semantic rules. It is
//! the lowest layer in the `ParadoxCode` dependency graph.

use std::fmt;

/// A UTF-8 byte offset into a source document.
pub type TextSize = u32;

/// A half-open UTF-8 byte range in a source document.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    start: TextSize,
    end: TextSize,
}

impl TextRange {
    /// Creates a range. Returns `None` when the end precedes the start.
    #[must_use]
    pub const fn new(start: TextSize, end: TextSize) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// Creates an empty range at an offset.
    #[must_use]
    pub const fn empty(offset: TextSize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// Returns the start offset.
    #[must_use]
    pub const fn start(self) -> TextSize {
        self.start
    }

    /// Returns the end offset.
    #[must_use]
    pub const fn end(self) -> TextSize {
        self.end
    }

    /// Returns the byte length.
    #[must_use]
    pub const fn len(self) -> TextSize {
        self.end - self.start
    }

    /// Returns whether the range is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A zero-based editor position. `character` is measured in UTF-16 code units.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Position {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based UTF-16 character offset within the line.
    pub character: u32,
}

impl Position {
    /// Creates a position.
    #[must_use]
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// A half-open editor position range measured in UTF-16 code units.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PositionRange {
    /// Start position.
    pub start: Position,
    /// End position.
    pub end: Position,
}

impl PositionRange {
    /// Creates a position range.
    #[must_use]
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}

/// Converts between UTF-8 byte offsets and UTF-16 editor positions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineIndex {
    line_starts: Vec<TextSize>,
    text_len: TextSize,
}

impl LineIndex {
    /// Builds an index for `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n'
                && let Ok(next) = TextSize::try_from(offset + 1)
            {
                line_starts.push(next);
            }
        }
        let text_len = TextSize::try_from(text.len()).unwrap_or(TextSize::MAX);
        Self {
            line_starts,
            text_len,
        }
    }

    /// Returns the byte offset for an editor position, if it is on a UTF-8 boundary.
    #[must_use]
    pub fn offset(&self, text: &str, position: Position) -> Option<TextSize> {
        let start =
            usize::try_from(*self.line_starts.get(usize::try_from(position.line).ok()?)?).ok()?;
        let end = self
            .line_starts
            .get(usize::try_from(position.line).ok()?.saturating_add(1))
            .copied()
            .unwrap_or(self.text_len);
        let end = usize::try_from(end).ok()?;
        let line = text.get(start..end)?;
        let mut utf16 = 0_u32;
        for (byte_offset, character) in line.char_indices() {
            if utf16 == position.character {
                return TextSize::try_from(start + byte_offset).ok();
            }
            utf16 = utf16.checked_add(u32::try_from(character.len_utf16()).ok()?)?;
            if utf16 > position.character {
                return None;
            }
        }
        (utf16 == position.character)
            .then_some(start + line.len())
            .and_then(|offset| TextSize::try_from(offset).ok())
    }

    /// Returns the editor position for a UTF-8 byte offset.
    #[must_use]
    pub fn position(&self, text: &str, offset: TextSize) -> Option<Position> {
        if offset > self.text_len {
            return None;
        }
        let offset = usize::try_from(offset).ok()?;
        let line = self
            .line_starts
            .partition_point(|start| usize::try_from(*start).is_ok_and(|start| start <= offset))
            .saturating_sub(1);
        let start = usize::try_from(*self.line_starts.get(line)?).ok()?;
        let slice = text.get(start..offset)?;
        let character = slice.chars().try_fold(0_u32, |sum, character| {
            sum.checked_add(u32::try_from(character.len_utf16()).ok()?)
        })?;
        Some(Position::new(u32::try_from(line).ok()?, character))
    }

    /// Returns the UTF-16 editor range corresponding to a UTF-8 byte range.
    #[must_use]
    pub fn position_range(&self, text: &str, range: TextRange) -> Option<PositionRange> {
        Some(PositionRange::new(
            self.position(text, range.start())?,
            self.position(text, range.end())?,
        ))
    }

    /// Returns the number of indexed lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

/// A normalized logical path relative to an EU4 workspace root.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LogicalPath(String);

impl LogicalPath {
    /// Parses and validates an EU4 logical path.
    pub fn parse(path: &str) -> Result<Self, LogicalPathError> {
        let normalized = path.replace('\\', "/");
        let mut components = Vec::new();
        for component in normalized.trim_start_matches('/').split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    if components.pop().is_none() {
                        return Err(LogicalPathError::EscapesRoot(path.to_owned()));
                    }
                }
                value => components.push(value.to_owned()),
            }
        }
        if components.iter().any(|component| component.contains('\0')) {
            return Err(LogicalPathError::Nul(path.to_owned()));
        }
        Ok(Self(components.join("/")))
    }

    /// Returns the normalized path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A logical path that cannot be represented safely inside a workspace root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalPathError {
    /// `..` would escape the logical root.
    EscapesRoot(String),
    /// NUL is not valid in a filesystem path.
    Nul(String),
}

impl fmt::Display for LogicalPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EscapesRoot(path) => write!(formatter, "logical path escapes root: {path}"),
            Self::Nul(path) => write!(formatter, "logical path contains NUL: {path}"),
        }
    }
}

impl std::error::Error for LogicalPathError {}

impl fmt::Display for LogicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, LogicalPath, Position, TextRange};

    #[test]
    fn range_rejects_reversed_bounds() {
        assert!(TextRange::new(4, 3).is_none());
        assert_eq!(TextRange::new(3, 4).map(TextRange::len), Some(1));
    }

    #[test]
    fn line_index_round_trips_utf16_positions() {
        let text = "a\n汉😀z";
        let index = LineIndex::new(text);
        let position = Position::new(1, 3);
        let offset = index.offset(text, position).expect("valid position");
        assert_eq!(index.position(text, offset), Some(position));
    }

    #[test]
    fn line_index_handles_crlf_emoji_and_combining_characters() {
        let text = "head\r\n汉😀e\u{301}\r\ntail";
        let index = LineIndex::new(text);
        let start_of_second_line = Position::new(1, 0);
        let second_line_offset = index
            .offset(text, start_of_second_line)
            .expect("line start");
        assert_eq!(
            index.position(text, second_line_offset),
            Some(start_of_second_line)
        );

        let after_combining_mark = Position::new(1, 5);
        let offset = index
            .offset(text, after_combining_mark)
            .expect("valid UTF-16 position");
        assert_eq!(index.position(text, offset), Some(after_combining_mark));
        assert!(
            index.offset(text, Position::new(1, 2)).is_none(),
            "must reject half an emoji"
        );
    }

    #[test]
    fn line_index_converts_ranges_to_utf16_positions() {
        let text = "head\r\n汉😀e\u{301}\r\ntail";
        let index = LineIndex::new(text);
        let start = u32::try_from(text.find("😀").expect("emoji")).expect("offset");
        let range = TextRange::new(start, start + 4).expect("emoji range");
        assert_eq!(
            index.position_range(text, range),
            Some(super::PositionRange::new(
                Position::new(1, 1),
                Position::new(1, 3),
            ))
        );
    }

    #[test]
    fn logical_path_normalizes_separators() {
        assert_eq!(
            LogicalPath::parse("\\common\\events\\x.txt")
                .expect("normalized path")
                .as_str(),
            "common/events/x.txt"
        );
    }

    #[test]
    fn logical_path_rejects_escape_and_normalizes_dots() {
        assert_eq!(
            LogicalPath::parse("common/./events/../events/x.txt")
                .unwrap()
                .as_str(),
            "common/events/x.txt"
        );
        assert!(LogicalPath::parse("../../x.txt").is_err());
    }
}
