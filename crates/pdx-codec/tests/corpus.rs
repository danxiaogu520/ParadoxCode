//! Golden corpus tests: the EDG-KTP localisation file pair (the user's own mod,
//! verified against the deployed paratranz converter on 2026-09-11) plus synthetic
//! script fixtures reproducing the phenomena found in the 310-file workshop corpus
//! (history/countries of workshop mod 3047072888; the empirical round-trip report
//! lives in docs/eu4-cjk-localisation-design.md §3.1).

use pdx_codec::{
    Classification, EscapeSet, Profile, classify_file, decode_file, decode_text, encode_file,
    encode_text,
};

const MASTER: &[u8] = include_bytes!("corpus/edg_ktp_master.yml");
const RELEASE: &[u8] = include_bytes!("corpus/edg_ktp_release.yml");

#[test]
fn edg_ktp_release_decodes_to_the_master() {
    let decoded =
        decode_file(RELEASE, Profile::Localisation).expect("release file must be valid UTF-8");
    let master = std::str::from_utf8(MASTER).expect("master must be valid UTF-8");
    assert_eq!(decoded.text, master);
    assert!(decoded.broken_sequences.is_empty());
}

#[test]
fn edg_ktp_master_encodes_to_the_release_byte_for_byte() {
    let master = std::str::from_utf8(MASTER).expect("master must be valid UTF-8");
    let encoded = encode_file(master, Profile::Localisation, EscapeSet::Paratranz)
        .expect("master must contain only encodable code points");
    assert_eq!(encoded, RELEASE);
}

#[test]
fn edg_ktp_classification() {
    assert_eq!(
        classify_file(MASTER, Profile::Localisation),
        Classification::Readable
    );
    assert_eq!(
        classify_file(RELEASE, Profile::Localisation),
        Classification::Escaped
    );
}

#[test]
fn edg_ktp_text_layer_matches_the_file_layer() {
    let master = std::str::from_utf8(MASTER).unwrap();
    let text_encoded = encode_text(master, EscapeSet::Paratranz).unwrap();
    assert_eq!(text_encoded.as_bytes(), RELEASE);
    let release_text = std::str::from_utf8(RELEASE).unwrap();
    assert_eq!(decode_text(release_text).text, master);
}

/// A history/countries-style script fixture: BOM-less single-byte stream with
/// escaped names and a CP1252 comment, shaped like the workshop corpus files.
#[test]
fn script_fixture_round_trips() {
    let readable = "1500.1.1 = {\r\n\tmonarch = {\r\n\t\tname = \"\u{541B}\u{58EB}\u{5766}\u{4E01}\"\r\n\t\tdynasty = \"\u{5DF4}\u{5217}\u{5965}\u{7565}\"\r\n\t}\r\n}\r\n# K\u{f6}nigreich: caf\u{e9} r\u{e9}sum\u{e9}\r\n";
    let bytes = encode_file(readable, Profile::Script, EscapeSet::Paratranz).unwrap();

    assert!(
        !bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "script profile never adds a BOM"
    );
    assert!(bytes.contains(&0xF6), "CP1252 ö stays a single byte");
    assert!(bytes.contains(&0xE9), "CP1252 é stays a single byte");

    let decoded = decode_file(&bytes, Profile::Script).unwrap();
    assert_eq!(decoded.text, readable);
    assert!(decoded.broken_sequences.is_empty());
    assert_eq!(
        classify_file(&bytes, Profile::Script),
        Classification::Escaped
    );

    let re_encoded = encode_file(&decoded.text, Profile::Script, EscapeSet::Paratranz).unwrap();
    assert_eq!(
        re_encoded, bytes,
        "byte-exact round trip within the canonical profile"
    );
}

/// Three of the 310 corpus files were produced by a historical tool whose escape
/// set included 0x3A; the markers are self-describing so they still decode, and
/// re-encoding normalizes them to the canonical form.
#[test]
fn script_0x3a_variant_decodes_and_normalizes() {
    // 为 U+4E3A (low byte 0x3A): variant tool -> [0x11, 0x48, 0x4E]
    // (marker +1, low + 0x0E); canonical -> [0x10, 0x3A, 0x4E].
    let variant: [u8; 5] = [b'"', 0x11, 0x48, 0x4E, b'"'];
    let decoded = decode_file(&variant, Profile::Script).unwrap();
    assert_eq!(decoded.text, "\"\u{4E3A}\"");
    assert!(decoded.broken_sequences.is_empty());

    let canonical = encode_file(&decoded.text, Profile::Script, EscapeSet::Paratranz).unwrap();
    assert_eq!(
        canonical,
        vec![b'"', 0x10, 0x3A, 0x4E, b'"'],
        "normalized, still correct"
    );

    // Three variant triples clear the classification threshold; a single one
    // intentionally does not (stray control bytes are not evidence).
    let variant_stream: [u8; 9] = [
        0x11, 0x48, 0x4E, // U+4E3A
        0x11, 0x48, 0x52, // U+523A
        0x11, 0x48, 0x62, // U+623A
    ];
    assert_eq!(
        classify_file(&variant_stream, Profile::Script),
        Classification::Escaped
    );
    let stream_decoded = decode_file(&variant_stream, Profile::Script).unwrap();
    assert_eq!(stream_decoded.text, "\u{4E3A}\u{523A}\u{623A}");
}

/// HSN-class comment ambiguity from the corpus: a historical tool encoded the
/// CP1252 character ‘ (U+2018) as a triple; both its triple form and the canonical
/// single byte decode to the same text.
#[test]
fn script_cp1252_triple_ambiguity_both_decode() {
    // U+2018 canonical script form: single byte 0x91.
    let single = decode_file(&[0x91], Profile::Script).unwrap();
    // Historical triple form: U+2018 encoded as a triple [0x10, 0x18, 0x20]
    // (low 0x18 = 0x2018 & 0xFF, high 0x20 = 0x2018 >> 8).
    let triple = decode_file(&[0x10, 0x18, 0x20], Profile::Script).unwrap();
    assert_eq!(single.text, "\u{2018}");
    assert_eq!(triple.text, "\u{2018}");
}
