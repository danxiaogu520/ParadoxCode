//! Quote/comment-scoped transcoding: escape triples live only inside quoted
//! strings, everything else (comments, code) stays readable.
//!
//! The transparent editor workflow shows readable text everywhere and encodes
//! on save; whole-file encoding would escape comment CJK too, turning every
//! other editor's view of the comments into mojibake. Scoped files therefore
//! carry two byte shapes in one stream: escaped strings (the profile's usual
//! form — CP1252-mapped triple payloads for scripts, UTF-8 triple characters
//! for localisation) and verbatim UTF-8 outside strings.
//!
//! The scanner is deliberately encoding-blind. Structural bytes (`"`, `#`,
//! `\n`, `\`) are ASCII, and every one of them is an escape-set member, so
//! triple payloads never contain them — scanning raw bytes yields the same
//! spans as scanning text, and the spans are stable across encode/decode,
//! which is what closes the round trip.

use crate::cp1252;
use crate::escape::{decode_file, encode_file};
use crate::{Classification, EncodeError, EscapeSet, Profile, classify_file};

/// Maximal quoted spans outside comments: byte ranges including the quotes,
/// over the scanned buffer. Strings never span lines; an unterminated quote
/// closes at end of line (or end of input). `#` inside a string is literal
/// text, `\"` never closes a string.
#[must_use]
pub fn scan_string_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut in_string = false;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte == b'\\' && index + 1 < bytes.len() {
                index += 2;
                continue;
            }
            if byte == b'"' {
                spans.push((start, index + 1));
                in_string = false;
            } else if byte == b'\n' {
                spans.push((start, index));
                in_string = false;
            }
        } else if byte == b'#' {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        } else if byte == b'"' {
            in_string = true;
            start = index;
        }
        index += 1;
    }
    if in_string {
        spans.push((start, bytes.len()));
    }
    spans
}

/// How a buffer relates to the scoped form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopedForm {
    /// Readable or ASCII with no escape markers: show as-is; a save encodes
    /// quoted CJK (if any) into the scoped form.
    Plain,
    /// Whole-file escaped (legacy form): the whole-file decoder applies.
    WholeEscaped,
    /// Escape triples confined to quoted spans: the scoped decoder applies.
    /// Readable CJK may share the spans — a save converges them to escaped.
    Scoped,
    /// Escape markers outside any quoted span: damaged content, refuse to
    /// transform and report.
    Damaged,
}

/// Dispatch for a buffer under scoped semantics.
///
/// The decisive signal is the comment gap: a scoped file holds readable
/// UTF-8 CJK between its string spans, which the CP1252 byte layer of a
/// legacy escaped file can never produce (escaped comments are triples).
/// Files without that signal — ASCII/readable gaps, or fully escaped ones —
/// keep the legacy whole-file path, whose decode result is identical for
/// them anyway.
#[must_use]
pub fn scoped_form(bytes: &[u8], profile: Profile) -> ScopedForm {
    match classify_file(bytes, profile) {
        Classification::Ascii | Classification::Readable => ScopedForm::Plain,
        Classification::Escaped => {
            if readable_cjk_gap(bytes) {
                ScopedForm::Scoped
            } else {
                ScopedForm::WholeEscaped
            }
        }
        Classification::Mixed => {
            if readable_cjk_gap(bytes) {
                return ScopedForm::Scoped;
            }
            // Localisation input that is not valid UTF-8 can never decode;
            // report it as damaged instead of a form the decoder refuses.
            if profile == Profile::Localisation && std::str::from_utf8(bytes).is_err() {
                return ScopedForm::Damaged;
            }
            if marker_outside_spans(bytes).is_empty() {
                ScopedForm::Scoped
            } else {
                ScopedForm::Damaged
            }
        }
    }
}

/// Whether any between-spans run is unescaped, valid UTF-8, and contains raw
/// CJK — the fingerprint of readable scoped comments.
fn readable_cjk_gap(bytes: &[u8]) -> bool {
    let spans = scan_string_spans(bytes);
    let mut cursor = 0usize;
    for &(start, end) in &spans {
        if gap_is_readable_cjk(&bytes[cursor..start]) {
            return true;
        }
        cursor = end;
    }
    gap_is_readable_cjk(&bytes[cursor..])
}

fn gap_is_readable_cjk(gap: &[u8]) -> bool {
    if gap.is_empty() || gap.iter().any(|byte| is_marker(*byte)) {
        return false;
    }
    match std::str::from_utf8(gap) {
        Ok(text) => text
            .chars()
            .any(|character| crate::is_raw_cjk(u32::from(character))),
        Err(_) => false,
    }
}

/// Input byte offsets of escape markers that sit outside every quoted span,
/// computed by walking the gaps between spans linearly.
#[must_use]
pub fn marker_outside_spans(bytes: &[u8]) -> Vec<usize> {
    let spans = scan_string_spans(bytes);
    let mut markers = Vec::new();
    let mut cursor = 0usize;
    for &(start, end) in &spans {
        for (offset, byte) in bytes[cursor..start].iter().enumerate() {
            if is_marker(*byte) {
                markers.push(cursor + offset);
            }
        }
        cursor = end;
    }
    for (offset, byte) in bytes[cursor..].iter().enumerate() {
        if is_marker(*byte) {
            markers.push(cursor + offset);
        }
    }
    markers
}

/// Successful scoped decode: readable text plus the input byte offsets of
/// markers that were not resolved — orphans inside spans (passed through) and
/// markers outside spans (damage evidence).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopedDecoded {
    /// The decoded readable text.
    pub text: String,
    /// Input byte offsets of orphan markers inside quoted spans.
    pub in_span_broken: Vec<usize>,
    /// Input byte offsets of markers outside any quoted span.
    pub out_of_span_markers: Vec<usize>,
}

/// Decodes a scoped-escaped buffer: escape triples resolve only inside quoted
/// spans; a segment without markers reads as verbatim UTF-8 when valid (scoped
/// comments and not-yet-encoded strings hold readable UTF-8 CJK) and falls
/// back to the CP1252 character layer otherwise, matching files written by
/// foreign tools.
///
/// # Errors
/// Returns [`InvalidUtf8`](crate::InvalidUtf8) only for the localisation
/// profile when the file is not valid UTF-8; the script profile never fails.
pub fn scoped_decode_file(
    bytes: &[u8],
    profile: Profile,
) -> Result<ScopedDecoded, crate::InvalidUtf8> {
    if profile == Profile::Localisation && std::str::from_utf8(bytes).is_err() {
        return Err(crate::InvalidUtf8);
    }
    let spans = scan_string_spans(bytes);
    let mut text = String::with_capacity(bytes.len());
    let mut in_span_broken = Vec::new();
    let mut out_of_span_markers = Vec::new();
    let mut cursor = 0usize;
    for &(start, end) in &spans {
        decode_gap(
            &bytes[cursor..start],
            profile,
            cursor,
            &mut text,
            &mut out_of_span_markers,
        );
        let (run, broken) = decode_run(&bytes[start..end], profile, start);
        in_span_broken.extend(broken);
        text.push_str(&run);
        cursor = end;
    }
    decode_gap(
        &bytes[cursor..],
        profile,
        cursor,
        &mut text,
        &mut out_of_span_markers,
    );
    Ok(ScopedDecoded {
        text,
        in_span_broken,
        out_of_span_markers,
    })
}

/// Decodes one quoted span: with markers it takes the profile's triple decode
/// (orphan markers come back as offsets); without markers it is readable
/// content — verbatim UTF-8 when valid, CP1252-mapped otherwise.
fn decode_run(segment: &[u8], profile: Profile, base: usize) -> (String, Vec<usize>) {
    if segment.iter().any(|byte| is_marker(*byte)) {
        // Localisation input was validated as UTF-8 above and script decode
        // never fails, so the triple decode cannot error here.
        let decoded = decode_file(segment, profile).expect("segment decode cannot fail");
        let broken = decoded
            .broken_sequences
            .iter()
            .map(|offset| offset + base)
            .collect();
        (decoded.text, broken)
    } else {
        match std::str::from_utf8(segment) {
            Ok(run) => (run.to_owned(), Vec::new()),
            Err(_) => (
                segment
                    .iter()
                    .map(|byte| char::from_u32(cp1252::byte_to_char(*byte)).unwrap_or('\u{FFFD}'))
                    .collect(),
                Vec::new(),
            ),
        }
    }
}

/// Appends a between-spans run, recording marker bytes as damage evidence.
fn decode_gap(
    gap: &[u8],
    profile: Profile,
    base: usize,
    text: &mut String,
    out_of_span_markers: &mut Vec<usize>,
) {
    if gap.is_empty() {
        return;
    }
    for (offset, byte) in gap.iter().enumerate() {
        if is_marker(*byte) {
            out_of_span_markers.push(base + offset);
        }
    }
    let (run, _) = decode_run(gap, profile, base);
    text.push_str(&run);
}

/// Scoped encoding refusal: either the buffer already carries escape triples
/// inside strings (pasted transcoded text — encoding again would double-
/// encode), or in-span content holds code points the transcoder refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopedEncodeError {
    /// Byte offsets (into the readable input) of markers found inside quoted
    /// spans.
    AlreadyEscaped { positions: Vec<usize> },
    /// Unencodable in-span code points, offsets rebased to the whole input.
    Unencodable { error: EncodeError },
}

/// Encodes readable text into scoped-escaped bytes: escape triples only inside
/// quoted spans, verbatim UTF-8 outside them. In-span encoding follows the
/// profile's usual byte shape, so the game-side transcoder reads the strings
/// exactly as with whole-file encoding; comments and code stay readable.
///
/// # Errors
/// [`ScopedEncodeError::AlreadyEscaped`] for any in-span escape marker (iron
/// rule ②), [`ScopedEncodeError::Unencodable`] for refused in-span code points.
pub fn scoped_encode_file(
    input: &str,
    profile: Profile,
    escape_set: EscapeSet,
) -> Result<Vec<u8>, ScopedEncodeError> {
    let bytes = input.as_bytes();
    let spans = scan_string_spans(bytes);
    let mut positions = Vec::new();
    for &(start, end) in &spans {
        for (offset, byte) in bytes[start..end].iter().enumerate() {
            if is_marker(*byte) {
                positions.push(start + offset);
            }
        }
    }
    if !positions.is_empty() {
        return Err(ScopedEncodeError::AlreadyEscaped { positions });
    }
    let mut output = Vec::with_capacity(input.len() * 2);
    let mut cursor = 0usize;
    for &(start, end) in &spans {
        output.extend_from_slice(&bytes[cursor..start]);
        let span_text = &input[start..end];
        match encode_file(span_text, profile, escape_set) {
            Ok(mut encoded) => {
                output.append(&mut encoded);
            }
            Err(error) => {
                let error = EncodeError {
                    unencodable: error
                        .unencodable
                        .into_iter()
                        .map(|point| crate::UnencodableCodePoint {
                            byte_index: point.byte_index + start,
                            ..point
                        })
                        .collect(),
                };
                return Err(ScopedEncodeError::Unencodable { error });
            }
        }
        cursor = end;
    }
    output.extend_from_slice(&bytes[cursor..]);
    Ok(output)
}

fn is_marker(byte: u8) -> bool {
    (0x10..=0x13).contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escape::{decode_file, encode_file};

    const P: EscapeSet = EscapeSet::Paratranz;

    fn spans(text: &str) -> Vec<(usize, usize)> {
        scan_string_spans(text.as_bytes())
    }

    #[test]
    fn scanner_finds_strings_and_skips_comments() {
        let text = "# top \"not a string\"\r\nkey = \"value\" # trailing\r\nl_english:\r\n";
        let start = text.find("\"value\"").expect("quoted value");
        assert_eq!(spans(text), vec![(start, start + "\"value\"".len())]);
        assert!(spans("# only comments \" \" here\n").is_empty());
        assert!(spans("plain code without quotes\r\n").is_empty());
    }

    #[test]
    fn scanner_handles_escaped_quotes_and_open_strings() {
        let text = "a = \"say \\\"hi\\\" ok\"\nb = \"";
        let found = spans(text);
        assert_eq!(found.len(), 2);
        assert_eq!(&text[found[0].0..found[0].1], "\"say \\\"hi\\\" ok\"");
        // An unterminated string closes at end of input.
        assert_eq!(&text[found[1].0..found[1].1], "\"");
        // A string left open at end of line closes at the newline.
        let line = "a = \"multi\nb = \"ok\"\n";
        let found = spans(line);
        assert_eq!(found.len(), 2);
        assert_eq!(&line[found[0].0..found[0].1], "\"multi");
        assert_eq!(&line[found[1].0..found[1].1], "\"ok\"");
    }

    #[test]
    fn scoped_script_round_trip_with_readable_comments() {
        let readable =
            "# 注释保持可读\r\ndynasty = \"大明王朝\"\r\n# 另一条注释\r\ntitle = \"帝国\"\r\n";
        let scoped = scoped_encode_file(readable, Profile::Script, P).unwrap();
        // Comments stay readable UTF-8 on disk; strings hold byte triples.
        assert!(scoped.starts_with("# 注释保持可读\r\n".as_bytes()));
        assert_eq!(scoped_form(&scoped, Profile::Script), ScopedForm::Scoped);
        let decoded = scoped_decode_file(&scoped, Profile::Script).unwrap();
        assert_eq!(decoded.text, readable);
        assert!(decoded.in_span_broken.is_empty());
        assert!(decoded.out_of_span_markers.is_empty());
    }

    #[test]
    fn scoped_localisation_round_trip() {
        let readable = "l_english:\r\n edg_key:0 \"发行本\" # 注释\r\n";
        let scoped = scoped_encode_file(readable, Profile::Localisation, P).unwrap();
        assert!(scoped.starts_with("l_english:".as_bytes()));
        assert_eq!(
            scoped_form(&scoped, Profile::Localisation),
            ScopedForm::Scoped
        );
        let decoded = scoped_decode_file(&scoped, Profile::Localisation).unwrap();
        assert_eq!(decoded.text, readable);
    }

    #[test]
    fn scoped_strings_match_whole_file_encoding() {
        // A file whose only non-ASCII content is inside strings must scope-
        // encode to exactly the whole-file form (comments are ASCII here).
        let readable = "dynasty = \"大明王朝\"\r\n";
        assert_eq!(
            scoped_encode_file(readable, Profile::Script, P).unwrap(),
            encode_file(readable, Profile::Script, P).unwrap()
        );
    }

    #[test]
    fn partial_files_self_heal() {
        // One string escaped on disk, one readable, comment readable: scoped
        // decode shows all readable, scoped encode converges both strings.
        let escaped_part = encode_file("title = \"帝国\"\r\n", Profile::Script, P).unwrap();
        let mixed: Vec<u8> = [
            "# 注释\r\n".as_bytes(),
            &escaped_part,
            "dynasty = \"大明王朝\"\r\n".as_bytes(),
        ]
        .concat();
        assert_eq!(scoped_form(&mixed, Profile::Script), ScopedForm::Scoped);
        let decoded = scoped_decode_file(&mixed, Profile::Script).unwrap();
        assert_eq!(
            decoded.text,
            "# 注释\r\ntitle = \"帝国\"\r\ndynasty = \"大明王朝\"\r\n"
        );
        let healed = scoped_encode_file(&decoded.text, Profile::Script, P).unwrap();
        let redecoded = scoped_decode_file(&healed, Profile::Script).unwrap();
        assert_eq!(redecoded.text, decoded.text);
        // And the healed file re-encodes to itself (fixed point).
        assert_eq!(
            scoped_encode_file(&redecoded.text, Profile::Script, P).unwrap(),
            healed
        );
    }

    #[test]
    fn whole_escaped_files_take_the_legacy_path() {
        let legacy = encode_file("# 注释\r\nkey = \"大明\"\r\n", Profile::Script, P).unwrap();
        assert_eq!(
            scoped_form(&legacy, Profile::Script),
            ScopedForm::WholeEscaped
        );
        // The legacy decoder still applies to it.
        assert_eq!(
            decode_file(&legacy, Profile::Script).unwrap().text,
            "# 注释\r\nkey = \"大明\"\r\n"
        );
    }

    #[test]
    fn markers_outside_strings_are_damage() {
        // A stray marker in code position (outside quotes and comments).
        let damaged: Vec<u8> = [b'a', b' ', 0x10, 0x41, 0x42, b'\n'].to_vec();
        assert_eq!(scoped_form(&damaged, Profile::Script), ScopedForm::Damaged);
        assert_eq!(marker_outside_spans(&damaged), vec![2]);
        // The same triple inside a string is scoped content instead.
        let scoped: Vec<u8> = [b'a', b' ', b'"', 0x10, 0x41, 0x42, b'"', b'\n'].to_vec();
        assert_eq!(scoped_form(&scoped, Profile::Script), ScopedForm::Scoped);
        // A marker inside a comment is code-position damage too.
        let commented: Vec<u8> = [b'#', b' ', 0x10, b'\n'].to_vec();
        assert_eq!(
            scoped_form(&commented, Profile::Script),
            ScopedForm::Damaged
        );
    }

    #[test]
    fn iron_rule_refuses_in_span_markers() {
        // A buffer carrying escaped triples inside its strings (as the editor
        // char layer would show pasted transcoded script content) is refused.
        let pasted = encode_file("key = \"大明\"\r\n", Profile::Script, P).unwrap();
        let buffer: String = "# 注释\r\n"
            .chars()
            .chain(
                pasted
                    .iter()
                    .map(|byte| char::from_u32(cp1252::byte_to_char(*byte)).unwrap_or('\u{FFFD}')),
            )
            .collect();
        match scoped_encode_file(&buffer, Profile::Script, P) {
            Err(ScopedEncodeError::AlreadyEscaped { positions }) => {
                assert!(!positions.is_empty());
            }
            other => panic!("expected AlreadyEscaped, got {other:?}"),
        }
    }

    #[test]
    fn unencodable_positions_rebase_to_whole_input() {
        let readable = "# 注释\r\nkey = \"前😀后\"\r\n";
        match scoped_encode_file(readable, Profile::Script, P) {
            Err(ScopedEncodeError::Unencodable { error }) => {
                let expected = "# 注释\r\nkey = \"".len() + "前".chars().next().unwrap().len_utf8();
                assert_eq!(error.unencodable[0].byte_index, expected);
                assert_eq!(error.unencodable[0].code_point, 0x1F600);
            }
            other => panic!("expected Unencodable, got {other:?}"),
        }
    }

    #[test]
    fn in_span_orphans_report_and_pass_through() {
        // Marker inside a string with fewer than two payload bytes left.
        let bytes: Vec<u8> = [b'"', 0x10, b'"', b'\n'].to_vec();
        let decoded = scoped_decode_file(&bytes, Profile::Script).unwrap();
        assert_eq!(decoded.in_span_broken, vec![1]);
        assert_eq!(decoded.text, "\"\u{0010}\"\n");
    }

    #[test]
    fn localisation_invalid_utf8_refused() {
        assert!(scoped_decode_file(&[0xFF, 0xFE], Profile::Localisation).is_err());
    }
}
