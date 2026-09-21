//! Differential vector generator for the TypeScript twin of this crate
//! (`editors/vscode/src/transcode.ts`).
//!
//! Emits one JSON object per line on stdout; `editors/vscode/scripts/
//! transcode.mjs` feeds every vector through the TypeScript implementation
//! and compares. The format is deliberately dumb — hex strings, integers, and
//! short enum names only — so it stays hand-formatted without a JSON dependency
//! in this std-only crate.
//!
//! Sections:
//! * `cp` — one line per code point `0..=0xFFFF` (plus beyond-BMP samples):
//!   single-character encode outcomes for all three lanes.
//! * `seq` — deterministic pseudo-random and structured byte strings with
//!   their script/localisation decode outcomes and classifications.
//! * `text` — deterministic pseudo-random code-point strings with their
//!   text-layer encode/decode/classify outcomes.
//!
//! Regenerate nothing: the generator is seeded and stable, so the same vectors
//! pin both implementations on every run.

use std::io::Write;

use transcode::{
    Classification, EscapeSet, Profile, classify_file, classify_text, decode_file, decode_text,
    encode_file, encode_text,
};

/// xorshift64* — deterministic, no dependencies, good enough for adversarial
/// byte patterns (the transcoder has no statistical requirements).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// Code points of a string as space-separated hex, so control characters and
/// lone surrogates survive JSON without an escape layer.
fn cps(text: &str) -> String {
    text.chars()
        .map(|character| format!("{:04X}", u32::from(character)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn offsets(values: &[usize]) -> String {
    values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Encode outcome for one lane: hex bytes, or `R<kind>` for a refusal.
fn outcome(text: &str, profile: Profile, set: EscapeSet) -> String {
    match encode_file(text, profile, set) {
        Ok(bytes) => hex(&bytes),
        Err(error) => {
            let kind = error.unencodable[0].kind;
            let discriminant = match kind {
                transcode::UnencodableKind::MangledLowPlane => 0,
                transcode::UnencodableKind::BeyondBmp => 1,
            };
            format!("R{discriminant}")
        }
    }
}

fn classification_name(classification: Classification) -> &'static str {
    match classification {
        Classification::Ascii => "ascii",
        Classification::Readable => "readable",
        Classification::Escaped => "escaped",
        Classification::Mixed => "mixed",
    }
}

fn emit(line: String) {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}").expect("write vector");
}

fn main() {
    // --- cp sweep: single-character encode for every BMP code point ----------
    for code_point in 0..=0xFFFFu32 {
        let character = match char::from_u32(code_point) {
            Some(character) => character,
            // Lone surrogates are not scalars; the TS side skips the same rows.
            None => {
                emit(format!(
                    "{{\"v\":\"cp\",\"cp\":{code_point},\"loc\":\"SURROGATE\",\"scr\":\"SURROGATE\",\"dll\":\"SURROGATE\"}}"
                ));
                continue;
            }
        };
        let text = character.to_string();
        emit(format!(
            "{{\"v\":\"cp\",\"cp\":{code_point},\"loc\":\"{}\",\"scr\":\"{}\",\"dll\":\"{}\"}}",
            outcome(&text, Profile::Localisation, EscapeSet::Paratranz),
            outcome(&text, Profile::Script, EscapeSet::Paratranz),
            outcome(&text, Profile::Script, EscapeSet::DllFull),
        ));
    }
    for code_point in [0x10000u32, 0x1F600, 0x10FFFF] {
        let text = char::from_u32(code_point).expect("sample").to_string();
        emit(format!(
            "{{\"v\":\"cp\",\"cp\":{code_point},\"loc\":\"{}\",\"scr\":\"{}\",\"dll\":\"{}\"}}",
            outcome(&text, Profile::Localisation, EscapeSet::Paratranz),
            outcome(&text, Profile::Script, EscapeSet::Paratranz),
            outcome(&text, Profile::Script, EscapeSet::DllFull),
        ));
    }

    // --- byte sequences: random + structured ---------------------------------
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    fn emit_sequence(bytes: &[u8]) {
        let script = decode_file(bytes, Profile::Script).expect("script decode never fails");
        match std::str::from_utf8(bytes) {
            Ok(_) => {
                let localisation =
                    decode_file(bytes, Profile::Localisation).expect("checked utf-8");
                emit(format!(
                    "{{\"v\":\"seq\",\"b\":\"{}\",\"st\":\"{}\",\"sb\":\"[{}]\",\"sc\":\"{}\",\"ok\":1,\"lt\":\"{}\",\"lb\":\"[{}]\",\"lc\":\"{}\"}}",
                    hex(bytes),
                    cps(&script.text),
                    offsets(&script.broken_sequences),
                    classification_name(classify_file(bytes, Profile::Script)),
                    cps(&localisation.text),
                    offsets(&localisation.broken_sequences),
                    classification_name(classify_file(bytes, Profile::Localisation)),
                ));
            }
            Err(_) => {
                emit(format!(
                    "{{\"v\":\"seq\",\"b\":\"{}\",\"st\":\"{}\",\"sb\":\"[{}]\",\"sc\":\"{}\",\"ok\":0}}",
                    hex(bytes),
                    cps(&script.text),
                    offsets(&script.broken_sequences),
                    classification_name(classify_file(bytes, Profile::Script)),
                ));
            }
        }
    }
    for _ in 0..6000 {
        let length = (rng.next() % 49) as usize;
        let bytes: Vec<u8> = (0..length)
            .map(|_| {
                let roll = rng.next();
                // Bias a third of the bytes toward marker values so triple
                // boundaries, orphans, and adjacent markers all show up.
                if roll % 10 < 3 {
                    (0x10 + (roll >> 8) % 4) as u8
                } else {
                    (roll >> 16) as u8
                }
            })
            .collect();
        emit_sequence(&bytes);
    }
    // Structured cases: every byte value once, marker runs, truncated triples.
    emit_sequence(&(0..=255u8).collect::<Vec<_>>());
    emit_sequence(&[0x10; 32]);
    emit_sequence(&[0x10, 0x11, 0x12, 0x13]);
    emit_sequence(&[b'a', 0x10, b'b', 0x11, 0x12]);
    for cut in 0..6 {
        let mut bytes = vec![b'#', 0x12, 0x34, 0x56, 0x13, 0x21];
        bytes.truncate(bytes.len() - cut);
        emit_sequence(&bytes);
    }

    // --- text layer: random code-point strings --------------------------------
    // Pool mixes ASCII, markers, CJK, CP1252-mapped letters, and refusal
    // ranges so every branch of encode/decode/classify is exercised.
    const POOL: [u32; 17] = [
        b'a' as u32,
        b'"' as u32,
        0x10,
        0x13,
        0x2E80,
        0x4E2A,
        0x9FFF,
        0xFF01,
        0x0160,
        0x0153,
        0x0100,
        0x0FFF,
        0x1000,
        0x5C3A,
        0xFEFF,
        0x20AC,
        0x00E4,
    ];
    for _ in 0..3000 {
        let length = (rng.next() % 41) as usize;
        let text: String = (0..length)
            .map(|_| char::from_u32(POOL[(rng.next() as usize) % POOL.len()]).expect("pool scalar"))
            .collect();
        match encode_text(&text, EscapeSet::Paratranz) {
            Ok(encoded) => {
                let decoded = decode_text(&encoded);
                emit(format!(
                    "{{\"v\":\"text\",\"t\":\"{}\",\"e\":\"{}\",\"d\":\"{}\",\"db\":\"[{}]\",\"c\":\"{}\"}}",
                    cps(&text),
                    cps(&encoded),
                    cps(&decoded.text),
                    offsets(&decoded.broken_sequences),
                    classification_name(classify_text(&text)),
                ));
            }
            // Refusal rows keep the kind/byteIndex pairs so the TS side's
            // UTF-16-vs-code-point bookkeeping is compared too.
            Err(error) => {
                let refusals = error
                    .unencodable
                    .iter()
                    .map(|point| {
                        let discriminant = match point.kind {
                            transcode::UnencodableKind::MangledLowPlane => 0,
                            transcode::UnencodableKind::BeyondBmp => 1,
                        };
                        format!("{discriminant}@{}", point.byte_index)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                emit(format!(
                    "{{\"v\":\"text\",\"t\":\"{}\",\"err\":\"{refusals}\",\"c\":\"{}\"}}",
                    cps(&text),
                    classification_name(classify_text(&text)),
                ));
            }
        }
    }

    // --- scoped layer: quote/comment-aware encode and decode -----------------
    // Text vectors exercise the scoped encoder (including iron-rule refusals
    // from pooled markers); their successful bytes double as decode+form
    // vectors, and byte-salad vectors stress the scanner and dispatch.
    const SCOPED_POOL: [u32; 15] = [
        0x22, 0x23, 0x5C, 0x0A, 0x61, 0x4E2D, 0x6587, 0x0160, 0x0100, 0x1F600, 0x10, 0x13, 0x20AC,
        0x00E9, 0x30,
    ];
    let scoped_profile = |roll: u64| {
        if roll.is_multiple_of(2) {
            Profile::Script
        } else {
            Profile::Localisation
        }
    };
    let scoped_form_name = |form: transcode::ScopedForm| match form {
        transcode::ScopedForm::Plain => "plain",
        transcode::ScopedForm::WholeEscaped => "whole",
        transcode::ScopedForm::Scoped => "scoped",
        transcode::ScopedForm::Damaged => "damaged",
    };
    let emit_scoped_bytes = |bytes: &[u8], profile: Profile| {
        let form_name = scoped_form_name(transcode::scoped_form(bytes, profile));
        match transcode::scoped_decode_file(bytes, profile) {
            Ok(decoded) => {
                emit(format!(
                    "{{\"v\":\"scd\",\"p\":\"{}\",\"b\":\"{}\",\"d\":\"{}\",\"ib\":\"[{}]\",\"ob\":\"[{}]\",\"f\":\"{form_name}\"}}",
                    profile_name(profile),
                    hex(bytes),
                    cps(&decoded.text),
                    offsets(&decoded.in_span_broken),
                    offsets(&decoded.out_of_span_markers),
                ));
            }
            Err(_) => {
                emit(format!(
                    "{{\"v\":\"scd\",\"p\":\"{}\",\"b\":\"{}\",\"d\":\"ERR\",\"ib\":\"[]\",\"ob\":\"[]\",\"f\":\"{form_name}\"}}",
                    profile_name(profile),
                    hex(bytes),
                ));
            }
        }
    };
    for _ in 0..4000 {
        let length = (rng.next() % 33) as usize;
        let text: String = (0..length)
            .map(|_| {
                char::from_u32(SCOPED_POOL[(rng.next() as usize) % SCOPED_POOL.len()])
                    .expect("pool scalar")
            })
            .collect();
        let profile = scoped_profile(rng.next());
        match transcode::scoped_encode_file(&text, profile, EscapeSet::Paratranz) {
            Ok(bytes) => {
                emit(format!(
                    "{{\"v\":\"sct\",\"p\":\"{}\",\"t\":\"{}\",\"e\":\"{}\"}}",
                    profile_name(profile),
                    cps(&text),
                    hex(&bytes),
                ));
                emit_scoped_bytes(&bytes, profile);
            }
            Err(transcode::ScopedEncodeError::AlreadyEscaped { positions }) => {
                let joined = offsets(&positions);
                emit(format!(
                    "{{\"v\":\"sct\",\"p\":\"{}\",\"t\":\"{}\",\"e\":\"E[{}]\"}}",
                    profile_name(profile),
                    cps(&text),
                    joined,
                ));
            }
            Err(transcode::ScopedEncodeError::Unencodable { error }) => {
                let refusals = error
                    .unencodable
                    .iter()
                    .map(|point| {
                        let discriminant = match point.kind {
                            transcode::UnencodableKind::MangledLowPlane => 0,
                            transcode::UnencodableKind::BeyondBmp => 1,
                        };
                        format!("{discriminant}@{}", point.byte_index)
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                emit(format!(
                    "{{\"v\":\"sct\",\"p\":\"{}\",\"t\":\"{}\",\"e\":\"R{refusals}\"}}",
                    profile_name(profile),
                    cps(&text),
                ));
            }
        }
    }
    // Byte salad: structural bytes, markers, high bytes, and spliced UTF-8
    // CJK runs so the scanner, gap detection, and dispatch all diverge on
    // realistic-looking damage.
    let chunks: [&[u8]; 12] = [
        &[0x22],
        &[0x23],
        &[0x5C],
        &[0x0A],
        b"a",
        &[0x10],
        &[0x11],
        &[0x80, 0x81],
        "中".as_bytes(),
        "文".as_bytes(),
        "é".as_bytes(),
        "€".as_bytes(),
    ];
    for _ in 0..3000 {
        let count = (rng.next() % 25) as usize;
        let mut bytes = Vec::new();
        for _ in 0..count {
            let roll = rng.next();
            if roll.is_multiple_of(5) {
                bytes.push((roll >> 8) as u8);
            } else {
                bytes.extend_from_slice(chunks[(roll as usize) % chunks.len()]);
            }
        }
        let profile = scoped_profile(rng.next());
        emit_scoped_bytes(&bytes, profile);
    }
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Script => "s",
        Profile::Localisation => "l",
    }
}
