// Persistent caches are reusable only by the same analyzer build.
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    println!("cargo:rerun-if-changed={}", directory.display());
    let mut entries = std::fs::read_dir(directory)
        .expect("build input directory")
        .map(|entry| entry.expect("build input").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let metadata = std::fs::symlink_metadata(&path).expect("build input metadata");
        assert!(
            !metadata.file_type().is_symlink(),
            "analyzer build inputs must not contain symlinks: {}",
            path.display()
        );
        if metadata.is_dir() {
            // Cargo output and dependencies are not analyzer source inputs.
            if path
                .file_name()
                .is_some_and(|name| name == "target" || name == "node_modules")
            {
                continue;
            }
            collect(&path, files);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "rs" || ext == "json")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            files.push(path);
        }
    }
}
fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}
/// Emits the analyzer build stamp for a cache-owning crate.
pub fn generate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    collect(&root.join("crates"), &mut files);
    collect(&root.join("rules"), &mut files);
    files.sort();
    let mut hasher = Sha256::new();
    field(&mut hasher, b"paradoxcode/analyzer-build/v1");
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        field(
            &mut hasher,
            path.strip_prefix(&root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace(std::path::MAIN_SEPARATOR, "/")
                .as_bytes(),
        );
        field(
            &mut hasher,
            &std::fs::read(&path).expect("read build input"),
        );
    }
    for name in [
        "TARGET",
        "PROFILE",
        "OPT_LEVEL",
        "DEBUG",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_CFG_TARGET_FEATURE",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
        field(&mut hasher, name.as_bytes());
        field(
            &mut hasher,
            std::env::var(name).unwrap_or_default().as_bytes(),
        );
    }
    let compiler = std::process::Command::new(std::env::var_os("RUSTC").expect("RUSTC"))
        .arg("-vV")
        .output()
        .expect("compiler identity");
    assert!(compiler.status.success(), "compiler identity failed");
    field(&mut hasher, &compiler.stdout);
    let identity = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    println!("cargo:rustc-env=PDC_ANALYZER_BUILD_ID={identity}");
}
