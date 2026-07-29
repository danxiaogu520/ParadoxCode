#![no_main]

use libfuzzer_sys::fuzz_target;
use pdx_syntax::{FileFormat, SyntaxEdit, parse};
use pdx_text::TextRange;

fuzz_target!(|data: &[u8]| {
    let Ok(seed) = std::str::from_utf8(data) else { return };
    let mut current = parse(FileFormat::PdxScript, seed);
    for (index, chunk) in data.chunks(3).take(16).enumerate() {
        let text = String::from_utf8_lossy(chunk);
        let edit = if index % 3 == 0 {
            SyntaxEdit::full(text.into_owned())
        } else {
            let end = u32::try_from(current.source().len()).unwrap_or(u32::MAX);
            let offset =
                if end == 0 { 0 } else { u32::from(chunk.first().copied().unwrap_or(0)) % end };
            SyntaxEdit::ranged(TextRange::empty(offset), text.into_owned())
        };
        let Ok(next) = current.apply_edit(&edit) else { return };
        let full = parse(FileFormat::PdxScript, next.source());
        assert_eq!(next.root(), full.root());
        assert_eq!(next.tokens(), full.tokens());
        assert_eq!(next.errors(), full.errors());
        current = next;
    }
});
