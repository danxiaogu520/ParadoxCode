#![no_main]

use libfuzzer_sys::fuzz_target;
use pdx_parser::{CstNode, FileFormat, parse};

fn walk(node: &CstNode, source_len: u32) {
    assert!(node.range().end() <= source_len);
    for child in node.children() {
        walk(child, source_len);
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let parsed = parse(FileFormat::Script, source);
        walk(
            parsed.root(),
            u32::try_from(source.len()).unwrap_or(u32::MAX),
        );
        for token in parsed.tokens() {
            assert!(token.range().end() <= u32::try_from(source.len()).unwrap_or(u32::MAX));
        }
    }
});
