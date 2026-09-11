//! The CP1252 high-range mapping (bytes `0x80`–`0x9F` to Unicode) shared by both
//! file profiles. The five undefined slots (`0x81 0x8D 0x8F 0x90 0x9D`) have no
//! mapping and pass through as C1 control characters, mirroring the reference
//! implementations.

/// The 27 CP1252 bytes above ASCII that map outside Latin-1, paired with their
/// Unicode code points. Sorted by byte for lookup.
pub const CP1252_MAP: [(u8, u32); 27] = [
    (0x80, 0x20AC),
    (0x82, 0x201A),
    (0x83, 0x0192),
    (0x84, 0x201E),
    (0x85, 0x2026),
    (0x86, 0x2020),
    (0x87, 0x2021),
    (0x88, 0x02C6),
    (0x89, 0x2030),
    (0x8A, 0x0160),
    (0x8B, 0x2039),
    (0x8C, 0x0152),
    (0x8E, 0x017D),
    (0x91, 0x2018),
    (0x92, 0x2019),
    (0x93, 0x201C),
    (0x94, 0x201D),
    (0x95, 0x2022),
    (0x96, 0x2013),
    (0x97, 0x2014),
    (0x98, 0x02DC),
    (0x99, 0x2122),
    (0x9A, 0x0161),
    (0x9B, 0x203A),
    (0x9C, 0x0153),
    (0x9E, 0x017E),
    (0x9F, 0x0178),
];

/// Returns the single CP1252 byte for one of the 27 mapped code points (`ä`-class
/// characters stay single bytes in script files), or `None` for everything else.
pub(crate) fn mapped_byte(code_point: u32) -> Option<u8> {
    CP1252_MAP
        .iter()
        .find(|(_, mapped_code_point)| *mapped_code_point == code_point)
        .map(|(byte, _)| *byte)
}

/// Maps a byte to the code point it denotes inside an escape triple: table entries
/// for the CP1252 high range, the byte value itself everywhere else (including the
/// five undefined C1 slots).
pub(crate) fn byte_to_char(byte: u8) -> u32 {
    for (mapped, code_point) in CP1252_MAP {
        if mapped == byte {
            return code_point;
        }
    }
    u32::from(byte)
}

/// Inverse of [`byte_to_char`] for escape-triple members: table entries reverse,
/// code points below `0x100` truncate to their byte value, anything else is corrupt
/// input and truncates deterministically.
pub(crate) fn char_to_byte(code_point: u32) -> u8 {
    for (mapped, mapped_code_point) in CP1252_MAP {
        if mapped_code_point == code_point {
            return mapped;
        }
    }
    if code_point < 0x100 {
        code_point as u8
    } else {
        (code_point & 0xFF) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_is_well_formed() {
        let mut sorted = CP1252_MAP;
        sorted.sort();
        assert_eq!(sorted, CP1252_MAP, "table must stay byte-sorted");
        for (byte, code_point) in CP1252_MAP {
            assert!((0x80..=0x9F).contains(&byte));
            assert!(code_point >= 0x100);
            assert_eq!(byte_to_char(byte), code_point);
            assert_eq!(char_to_byte(code_point), byte);
        }
    }

    #[test]
    fn passthrough_ranges() {
        for byte in 0u8..=0x7F {
            assert_eq!(byte_to_char(byte), u32::from(byte));
            assert_eq!(char_to_byte(u32::from(byte)), byte);
        }
        for byte in [0x81, 0x8D, 0x8F, 0x90, 0x9D] {
            assert_eq!(byte_to_char(byte), u32::from(byte), "undefined C1 slot");
        }
        for byte in 0xA0..=0xFF {
            assert_eq!(byte_to_char(byte), u32::from(byte), "Latin-1 range");
        }
    }
}
