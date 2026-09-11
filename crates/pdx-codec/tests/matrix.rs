//! Full byte coverage matrix: every escape-set byte value in both triple positions,
//! the 27 CP1252 characters, ASCII/BOM fixed points, and the decode universality
//! across escape-set variants.

use pdx_codec::{
    CP1252_MAP, Classification, DLL_FULL_ESCAPE_BYTES, EscapeSet, PARATRANZ_ESCAPE_BYTES, Profile,
    classify_file, classify_text, decode_file, decode_text, encode_file, encode_text,
};

const SETS: [EscapeSet; 2] = [EscapeSet::Paratranz, EscapeSet::DllFull];

#[test]
fn low_position_matrix() {
    // 0x4E00 + b keeps the high byte fixed at 0x4E and sweeps the low byte.
    for byte in 0u8..=255 {
        let code_point = 0x4E00 + u32::from(byte);
        let text = char::from_u32(code_point).unwrap().to_string();
        for set in SETS {
            let encoded = encode_text(&text, set).unwrap();
            assert_eq!(
                decode_text(&encoded).text,
                text,
                "low byte {byte:#04x}, set {set:?}"
            );
        }
        let bytes = encode_file(&text, Profile::Script, EscapeSet::Paratranz).unwrap();
        assert_eq!(
            decode_file(&bytes, Profile::Script).unwrap().text,
            text,
            "script low byte {byte:#04x}"
        );
    }
}

#[test]
fn high_position_matrix() {
    // High bytes 0x10..=0xFF are the only ones reachable from 4-digit code points.
    // 0xD8..=0xDF would land in the surrogate range, which valid text never contains.
    for byte in (0x10u8..=0xFF).filter(|byte| !(0xD8..=0xDF).contains(byte)) {
        let code_point = (u32::from(byte) << 8) | 0x61;
        let text = char::from_u32(code_point).unwrap().to_string();
        for set in SETS {
            let encoded = encode_text(&text, set).unwrap();
            assert_eq!(
                decode_text(&encoded).text,
                text,
                "high byte {byte:#04x}, set {set:?}"
            );
        }
        let bytes = encode_file(&text, Profile::Script, EscapeSet::Paratranz).unwrap();
        assert_eq!(
            decode_file(&bytes, Profile::Script).unwrap().text,
            text,
            "script high byte {byte:#04x}"
        );
    }
}

#[test]
fn every_escape_set_member_actually_escapes_in_both_positions() {
    for set in SETS {
        for &byte in set.bytes() {
            let low_char = char::from_u32(0x4E00 + u32::from(byte)).unwrap();
            let low_encoded = encode_text(&low_char.to_string(), set).unwrap();
            assert_eq!(
                low_encoded.chars().count(),
                3,
                "set {set:?} low {byte:#04x}"
            );
            assert!(decode_text(&low_encoded).broken_sequences.is_empty());

            if byte >= 0x10 {
                let high_char = char::from_u32((u32::from(byte) << 8) | 0x61).unwrap();
                let high_encoded = encode_text(&high_char.to_string(), set).unwrap();
                assert_eq!(
                    high_encoded.chars().count(),
                    3,
                    "set {set:?} high {byte:#04x}"
                );
            }
        }
    }
}

#[test]
fn escape_triples_never_contain_raw_set_members() {
    for set in SETS {
        for byte in (0x10u8..=0xFF).filter(|byte| !(0xD8..=0xDF).contains(byte)) {
            let code_point = (u32::from(byte) << 8) | 0x62;
            let text = char::from_u32(code_point).unwrap().to_string();
            let encoded = encode_text(&text, set).unwrap();
            let members: Vec<u32> = encoded.chars().map(u32::from).collect();
            assert_eq!(members.len(), 3);
            if members[0] == 0x12 || members[0] == 0x13 {
                assert_ne!(members[2], u32::from(byte), "high {byte:#04x} stayed raw");
            }
        }
        for byte in 0u8..=255 {
            let code_point = 0x4F00 + u32::from(byte);
            let text = char::from_u32(code_point).unwrap().to_string();
            let encoded = encode_text(&text, set).unwrap();
            let members: Vec<u32> = encoded.chars().map(u32::from).collect();
            if members.len() == 3 && (members[0] == 0x11 || members[0] == 0x13) {
                assert_ne!(members[1], u32::from(byte), "low {byte:#04x} stayed raw");
            }
        }
    }
}

#[test]
fn cp1252_characters_round_trip_in_both_profiles() {
    for (byte, code_point) in CP1252_MAP {
        let text = char::from_u32(code_point).unwrap().to_string();

        // Script: every mapped character stays a single byte — including the eight
        // Latin-Extended letters inside U+0100..=U+0FFF (Š, œ, …), which only the
        // single-byte path can round-trip.
        let bytes = encode_file(&text, Profile::Script, EscapeSet::Paratranz).unwrap();
        assert_eq!(bytes, vec![byte], "CP1252 byte {byte:#04x}");
        assert_eq!(decode_file(&bytes, Profile::Script).unwrap().text, text);

        // Localisation: characters at or above U+1000 take the triple path; the
        // eight low-plane letters are rejected there (their triples would mangle).
        if code_point >= 0x1000 {
            let encoded = encode_text(&text, EscapeSet::Paratranz).unwrap();
            assert_eq!(
                decode_text(&encoded).text,
                text,
                "CP1252 U+{code_point:04X}"
            );
        } else {
            let error = encode_text(&text, EscapeSet::Paratranz).unwrap_err();
            assert_eq!(error.unencodable.len(), 1, "CP1252 U+{code_point:04X}");
        }
    }
}

#[test]
fn ascii_and_bom_are_fixed_points() {
    // § (U+00A7) is below U+0100: the text layer passes it through unchanged.
    let ascii = "l_english:\r\n KEY:0 \"value with \u{a7}Y color \u{a7}! and [Root.GetName]\"\r\n";
    assert_eq!(encode_text(ascii, EscapeSet::Paratranz).unwrap(), ascii);
    assert_eq!(decode_text(ascii).text, ascii);

    // The script byte layer is Latin-1-shaped, so only pure ASCII is byte-identical
    // to its UTF-8 form (§ becomes the single byte 0xA7 instead of C2 A7).
    let pure_ascii = "1500.1.1 = {\r\n\tname = \"Constantine\"\r\n}\r\n";
    assert_eq!(
        encode_file(pure_ascii, Profile::Script, EscapeSet::Paratranz).unwrap(),
        pure_ascii.as_bytes()
    );

    let bom_input = "\u{FEFF}中";
    let bom_encoded = encode_text(bom_input, EscapeSet::Paratranz).unwrap();
    assert!(bom_encoded.starts_with('\u{FEFF}'));
    assert_eq!(decode_text(&bom_encoded).text, bom_input);
}

#[test]
fn decode_accepts_variant_escape_streams() {
    // 0x21 variant (deployed paratranz inline converter): U+4E21 encoded with
    // low 0x21 escaped even though 0x21 is not in the canonical set.
    let variant: Vec<u32> = vec![0x11, 0x2F, 0x4E]; // low 0x21 + 0x0E = 0x2F
    let variant_text: String = variant
        .iter()
        .map(|cp| char::from_u32(*cp).unwrap())
        .collect();
    assert_eq!(decode_text(&variant_text).text, "\u{4E21}");
}

#[test]
fn dll_full_output_decodes_with_the_canonical_decoder() {
    // Every DLL-full-only escaped byte also appears unescaped in canonical output,
    // so canonical decode must handle both shapes.
    for byte in [0x3Au8, 0x3C, 0x3E, 0x3F, 0x7C, 0x2A] {
        let code_point = 0x4E00 + u32::from(byte);
        let text = char::from_u32(code_point).unwrap().to_string();
        let dll = encode_text(&text, EscapeSet::DllFull).unwrap();
        let canonical = encode_text(&text, EscapeSet::Paratranz).unwrap();
        assert_ne!(dll, canonical, "byte {byte:#04x}");
        assert_eq!(decode_text(&dll).text, text);
        assert_eq!(decode_text(&canonical).text, text);
    }
}

#[test]
fn classification_survives_a_full_round_trip() {
    let readable = "\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}";
    assert_eq!(classify_text(readable), Classification::Readable);
    let encoded = encode_text(readable, EscapeSet::Paratranz).unwrap();
    assert_eq!(classify_text(&encoded), Classification::Escaped);
    assert_eq!(
        classify_file(encoded.as_bytes(), Profile::Localisation),
        Classification::Escaped
    );
    let decoded = decode_text(&encoded);
    assert_eq!(classify_text(&decoded.text), Classification::Readable);
}

#[test]
fn escape_sets_are_documented_supersets() {
    for byte in PARATRANZ_ESCAPE_BYTES {
        assert!(
            DLL_FULL_ESCAPE_BYTES.contains(&byte),
            "{byte:#04x} missing from dll-full"
        );
    }
    assert_eq!(
        DLL_FULL_ESCAPE_BYTES.len() - PARATRANZ_ESCAPE_BYTES.len(),
        6
    );
}
