#![no_main]

use libfuzzer_sys::fuzz_target;
use pdx_format::format;
use pdx_syntax::{FileFormat, TokenKind, parse};

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else { return };
    let parsed = parse(FileFormat::PdxScript, source);
    let result = format(&parsed);
    if let Some(edit) = result.edits.first() {
        let reparsed = parse(FileFormat::PdxScript, &edit.replacement);
        assert!(reparsed.errors().is_empty() || !parsed.errors().is_empty());
        let before: Vec<_> = parsed
            .tokens()
            .iter()
            .copied()
            .filter(|token| !matches!(token.kind(), TokenKind::Comment))
            .filter_map(|token| parsed.text(token.range()))
            .collect();
        let after: Vec<_> = reparsed
            .tokens()
            .iter()
            .copied()
            .filter(|token| !matches!(token.kind(), TokenKind::Comment))
            .filter_map(|token| reparsed.text(token.range()))
            .collect();
        assert_eq!(before, after);
        assert_eq!(format(&reparsed).edits.len(), 0);
    }
});
