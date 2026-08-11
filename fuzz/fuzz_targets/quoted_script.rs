#![no_main]

use libfuzzer_sys::fuzz_target;
use pdx_parser::{encode_quoted_script_text, parse_quoted_script};

fuzz_target!(|data: &[u8]| {
    let Ok(payload) = std::str::from_utf8(data) else {
        return;
    };
    let source = format!("\"{}\"", encode_quoted_script_text(payload));
    let script = parse_quoted_script(&source).expect("encoded payload always has an outer quote");
    assert_eq!(script.parsed().source(), payload);

    let mut previous = 0;
    for decoded in 0..=payload.len() {
        let decoded = u32::try_from(decoded).unwrap_or(u32::MAX);
        let mapped = script
            .source_map()
            .decoded_offset(decoded)
            .expect("every decoded byte boundary is mapped");
        assert!(mapped >= previous);
        assert!(mapped <= u32::try_from(source.len()).unwrap_or(u32::MAX));
        assert_eq!(script.source_map().source_offset(mapped), Some(decoded));
        previous = mapped;
    }
});
