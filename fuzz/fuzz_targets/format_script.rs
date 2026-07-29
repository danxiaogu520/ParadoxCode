#![no_main]

use libfuzzer_sys::fuzz_target;
use pdx_format::format;
use pdx_syntax::{FileFormat, TokenKind, parse};

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else { return };
    let parsed = parse(FileFormat::Script, source);
    let result = format(&parsed);
    if result.edits.is_empty() {
        return;
    }
    assert!(parsed.errors().is_empty());
    assert!(result.edits.windows(2).all(|edits| edits[0].range.end() <= edits[1].range.start()));
    let mut output = source.to_owned();
    for edit in result.edits.iter().rev() {
        let start = edit.range.start() as usize;
        let end = edit.range.end() as usize;
        assert!(start <= end && output.get(start..end).is_some());
        output.replace_range(start..end, &edit.replacement);
    }
    let reparsed = parse(FileFormat::Script, &output);
    assert!(reparsed.errors().is_empty());
    assert_eq!(parsed.tokens().len(), reparsed.tokens().len());
    for (before, after) in parsed.tokens().iter().zip(reparsed.tokens()) {
        assert_eq!(before.kind(), after.kind());
        if before.kind() != TokenKind::Quoted
            || !parsed.text(before.range()).is_some_and(|text| text.contains('\n'))
        {
            assert_eq!(parsed.text(before.range()), reparsed.text(after.range()));
        }
    }
    assert!(format(&reparsed).edits.is_empty());
});
