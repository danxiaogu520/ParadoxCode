#![no_main]

use libfuzzer_sys::fuzz_target;
use transcode::{EscapeSet, Profile, decode_file, encode_file};

fuzz_target!(|data: &[u8]| {
    // The script profile decodes any byte stream: escape markers are
    // self-describing and every other byte maps through CP1252.
    let decoded = decode_file(data, Profile::Script).expect("script profile never fails");
    assert!(
        decoded
            .broken_sequences
            .iter()
            .all(|&index| index < data.len())
    );

    // Localisation decoding is gated on valid UTF-8 and nothing else.
    match decode_file(data, Profile::Localisation) {
        Ok(_) => assert!(std::str::from_utf8(data).is_ok()),
        Err(_) => assert!(std::str::from_utf8(data).is_err()),
    }

    // Text without marker code points encodes to bytes that decode back to
    // itself exactly, for both profiles and both escape sets. Text containing
    // a raw marker code point is outside the readable-text contract.
    if let Ok(text) = std::str::from_utf8(data)
        && !text
            .chars()
            .any(|character| matches!(character, '\u{10}'..='\u{13}'))
    {
        for profile in [Profile::Localisation, Profile::Script] {
            for escape_set in [EscapeSet::Paratranz, EscapeSet::DllFull] {
                let Ok(encoded) = encode_file(text, profile, escape_set) else {
                    continue;
                };
                let round =
                    decode_file(&encoded, profile).expect("a successful encode always decodes");
                assert_eq!(round.text, text);
                assert!(round.broken_sequences.is_empty());
            }
        }
    }
});
