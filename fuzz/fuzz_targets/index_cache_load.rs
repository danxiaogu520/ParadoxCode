#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Index cache files are untrusted input: loading arbitrary bytes must
    // surface a structured IndexCacheError or a valid cache, never a panic.
    let path = std::env::temp_dir().join(format!("pdc-fuzz-index-cache-{}", std::process::id()));
    std::fs::write(&path, data).expect("write fuzz cache input");
    let _ = engine::IndexCache::load(&path);
    let _ = std::fs::remove_file(&path);
});
