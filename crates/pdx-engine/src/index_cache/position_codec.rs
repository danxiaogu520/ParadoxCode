//! Compact per-file encoding of UTF-16 navigation positions for cached files.
//!
//! Entries are stored sorted by `(range_start, range_end)`, so start offsets and start lines
//! are non-decreasing and small unsigned deltas dominate. A typical entry shrinks from six
//! `i64` columns plus SQLite row overhead to roughly 6-10 bytes.
//!
//! Layout (all integers little-endian LEB128):
//! - `u8` codec version, currently 1
//! - varint entry count
//! - per entry:
//!   - varint `range_start - previous range_start` (first entry: absolute)
//!   - varint `range_end - range_start` (range length)
//! - varint `start_line` (absolute; definition positions come from the HIR selection range,
//!   so lines can regress relative to the sorted ranges)
//!   - zigzag `start_character` (absolute)
//!   - varint `end_line - start_line`
//!   - zigzag `end_character - start_character`

use pdx_text::{Position, PositionRange, TextRange};

use super::{IndexCacheError, MAX_CACHE_SYMBOLS};

const CODEC_VERSION: u8 = 1;

pub(super) fn encode(entries: &[(TextRange, PositionRange)]) -> Result<Vec<u8>, IndexCacheError> {
    if entries.len() > MAX_CACHE_SYMBOLS {
        return Err(IndexCacheError::LimitExceeded(
            "navigation position",
            MAX_CACHE_SYMBOLS,
        ));
    }
    let mut payload = Vec::with_capacity(entries.len().saturating_mul(10).saturating_add(8));
    payload.push(CODEC_VERSION);
    push_varint(
        &mut payload,
        u64::try_from(entries.len()).unwrap_or(u64::MAX),
    );
    let mut previous_range: Option<(u32, u32)> = None;
    let mut previous_start = 0u32;
    for (range, position) in entries {
        let start = range.start();
        let end = range.end();
        if end < start {
            return Err(IndexCacheError::InvalidData(format!(
                "navigation range end {end} precedes start {start}"
            )));
        }
        if position.start > position.end {
            return Err(IndexCacheError::InvalidData(
                "navigation position end precedes start".to_owned(),
            ));
        }
        if let Some((previous_start, previous_end)) = previous_range
            && (start < previous_start || (start == previous_start && end <= previous_end))
        {
            return Err(IndexCacheError::InvalidData(
                "navigation positions are not strictly ordered".to_owned(),
            ));
        }
        let start_delta = start.checked_sub(previous_start).ok_or_else(|| {
            IndexCacheError::InvalidData("navigation range start regressed".to_owned())
        })?;
        push_varint(&mut payload, u64::from(start_delta));
        push_varint(&mut payload, u64::from(end - start));
        push_varint(&mut payload, u64::from(position.start.line));
        push_zigzag(&mut payload, i64::from(position.start.character));
        push_varint(
            &mut payload,
            u64::from(position.end.line - position.start.line),
        );
        push_zigzag(
            &mut payload,
            i64::from(position.end.character) - i64::from(position.start.character),
        );
        previous_range = Some((start, end));
        previous_start = start;
    }
    Ok(payload)
}

pub(super) fn decode(payload: &[u8]) -> Result<Vec<(TextRange, PositionRange)>, IndexCacheError> {
    let mut cursor = Cursor {
        bytes: payload,
        offset: 0,
    };
    let version = cursor
        .read_u8()
        .ok_or_else(|| IndexCacheError::InvalidData("empty navigation position payload".into()))?;
    if version != CODEC_VERSION {
        return Err(IndexCacheError::InvalidData(format!(
            "unsupported navigation position codec version {version}"
        )));
    }
    let count = cursor.read_varint()?;
    if count > u64::try_from(MAX_CACHE_SYMBOLS).unwrap_or(u64::MAX) {
        return Err(IndexCacheError::LimitExceeded(
            "navigation position",
            MAX_CACHE_SYMBOLS,
        ));
    }
    let mut entries = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    let mut previous_range: Option<(u32, u32)> = None;
    let mut previous_start = 0u32;
    for _ in 0..count {
        let start_delta = u32::try_from(cursor.read_varint()?).map_err(|_| {
            IndexCacheError::InvalidData("navigation range start exceeds u32".to_owned())
        })?;
        let start = previous_start.checked_add(start_delta).ok_or_else(|| {
            IndexCacheError::InvalidData("navigation range start overflows".to_owned())
        })?;
        let length = u32::try_from(cursor.read_varint()?).map_err(|_| {
            IndexCacheError::InvalidData("navigation range length exceeds u32".to_owned())
        })?;
        let end = start.checked_add(length).ok_or_else(|| {
            IndexCacheError::InvalidData("navigation range end overflows".to_owned())
        })?;
        let start_line = u32::try_from(cursor.read_varint()?).map_err(|_| {
            IndexCacheError::InvalidData("navigation start line exceeds u32".to_owned())
        })?;
        let start_character = cursor.read_zigzag()?;
        let start_character = u32::try_from(start_character).map_err(|_| {
            IndexCacheError::InvalidData("navigation start character exceeds u32".to_owned())
        })?;
        let end_line_delta = u32::try_from(cursor.read_varint()?).map_err(|_| {
            IndexCacheError::InvalidData("navigation end line delta exceeds u32".to_owned())
        })?;
        let end_line = start_line.checked_add(end_line_delta).ok_or_else(|| {
            IndexCacheError::InvalidData("navigation end line overflows".to_owned())
        })?;
        let end_character = i64::from(start_character) + cursor.read_zigzag()?;
        let end_character = u32::try_from(end_character).map_err(|_| {
            IndexCacheError::InvalidData("navigation end character exceeds u32".to_owned())
        })?;
        if let Some((previous_start, previous_end)) = previous_range
            && (start < previous_start || (start == previous_start && end <= previous_end))
        {
            return Err(IndexCacheError::InvalidData(
                "navigation positions are not strictly ordered".to_owned(),
            ));
        }
        let range = TextRange::new(start, end)
            .ok_or_else(|| IndexCacheError::InvalidData("invalid navigation range".to_owned()))?;
        entries.push((
            range,
            PositionRange::new(
                Position::new(start_line, start_character),
                Position::new(end_line, end_character),
            ),
        ));
        previous_range = Some((start, end));
        previous_start = start;
    }
    if cursor.offset != payload.len() {
        return Err(IndexCacheError::InvalidData(
            "navigation position payload has trailing bytes".to_owned(),
        ));
    }
    Ok(entries)
}

fn push_varint(output: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            output.push(byte);
            return;
        }
        output.push(byte | 0x80);
    }
}

fn push_zigzag(output: &mut Vec<u8>, value: i64) {
    push_varint(output, ((value << 1) ^ (value >> 63)) as u64);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn read_u8(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.offset)?;
        self.offset = self.offset.saturating_add(1);
        Some(byte)
    }

    fn read_varint(&mut self) -> Result<u64, IndexCacheError> {
        let mut value = 0u64;
        for shift in (0..u64::BITS as usize).step_by(7) {
            let byte = self.read_u8().ok_or_else(|| {
                IndexCacheError::InvalidData("truncated navigation position varint".to_owned())
            })?;
            if shift >= u64::BITS as usize - 7 && byte > 1 {
                return Err(IndexCacheError::InvalidData(
                    "navigation position varint exceeds u64".to_owned(),
                ));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(IndexCacheError::InvalidData(
            "navigation position varint exceeds u64".to_owned(),
        ))
    }

    fn read_zigzag(&mut self) -> Result<i64, IndexCacheError> {
        let value = self.read_varint()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        start: u32,
        end: u32,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> (TextRange, PositionRange) {
        (
            TextRange::new(start, end).expect("range"),
            PositionRange::new(
                Position::new(start_line, start_character),
                Position::new(end_line, end_character),
            ),
        )
    }

    #[test]
    fn round_trip_preserves_entries_across_line_boundaries() {
        let entries = vec![
            entry(0, 4, 0, 0, 0, 4),
            entry(5, 40, 0, 5, 1, 12),
            entry(41, 42, 1, 13, 1, 14),
            entry(100, 200, 2, 0, 4, 7),
        ];
        let payload = encode(&entries).expect("encode");
        assert_eq!(decode(&payload).expect("decode"), entries);
    }

    #[test]
    fn empty_entries_round_trip() {
        let payload = encode(&[]).expect("encode");
        assert_eq!(decode(&payload).expect("decode"), Vec::new());
    }

    #[test]
    fn large_values_round_trip() {
        let entries = vec![entry(
            u32::MAX - 5,
            u32::MAX,
            u32::MAX - 1,
            u32::MAX - 3,
            u32::MAX,
            u32::MAX,
        )];
        let payload = encode(&entries).expect("encode");
        assert_eq!(decode(&payload).expect("decode"), entries);
    }

    #[test]
    fn decode_rejects_empty_and_unknown_payloads() {
        assert!(matches!(decode(&[]), Err(IndexCacheError::InvalidData(_))));
        let mut payload = vec![CODEC_VERSION + 1, 0];
        assert!(matches!(
            decode(&payload),
            Err(IndexCacheError::InvalidData(_))
        ));
        payload = vec![CODEC_VERSION, 1, 0x80];
        assert!(matches!(
            decode(&payload),
            Err(IndexCacheError::InvalidData(_))
        ));
    }

    #[test]
    fn duplicate_entries_are_rejected_and_same_start_is_allowed() {
        // Exact duplicates cannot be produced by the writer and must be rejected.
        let duplicates = vec![entry(10, 20, 0, 10, 0, 20), entry(10, 20, 0, 10, 0, 20)];
        assert!(matches!(
            encode(&duplicates),
            Err(IndexCacheError::InvalidData(_))
        ));
        // The same start with a longer range is a valid ordering.
        let same_start = vec![entry(10, 20, 0, 10, 0, 20), entry(10, 21, 0, 10, 0, 21)];
        let payload = encode(&same_start).expect("encode");
        assert_eq!(decode(&payload).expect("decode"), same_start);
    }

    #[test]
    fn positions_can_regress_lines_across_sorted_ranges() {
        // A definition range can start early while its HIR selection position is far later,
        // so the next entry is on an earlier line than the previous position.
        let entries = vec![
            entry(181, 5053, 184, 23, 184, 44),
            entry(210, 213, 6, 11, 6, 14),
        ];
        let payload = encode(&entries).expect("encode");
        assert_eq!(decode(&payload).expect("decode"), entries);
    }

    #[test]
    fn encode_rejects_position_end_preceding_start() {
        let bad = (
            TextRange::new(10, 20).expect("range"),
            PositionRange::new(Position::new(1, 2), Position::new(1, 1)),
        );
        assert!(matches!(
            encode(&[bad]),
            Err(IndexCacheError::InvalidData(_))
        ));
    }
}
