//! Compile a checked Rules IR artifact for tooling or build-time embedding.
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("build") {
        return Err(
            "usage: bake-ir build --source <directory> --manifest <path> [--output <path>]".into(),
        );
    }
    let mut source = None;
    let mut manifest = None;
    let mut output = None;
    while let Some(flag) = args.next() {
        let destination = match flag.as_str() {
            "--source" => &mut source,
            "--manifest" => &mut manifest,
            "--output" => &mut output,
            _ => return Err(format!("unexpected option: {flag}")),
        };
        if destination.is_some() {
            return Err(format!("duplicate option: {flag}"));
        }
        let path = args
            .next()
            .filter(|value| !value.starts_with("--"))
            .ok_or_else(|| format!("missing path for {flag}"))?;
        *destination = Some(PathBuf::from(path));
    }
    let source = source.ok_or("missing required option: --source")?;
    let manifest = manifest.ok_or("missing required option: --manifest")?;
    // Compile and check everything before touching either output path.
    let result = rules::bake::compile(&source)?;
    let metadata =
        serde_json::to_string_pretty(&result.manifest).map_err(|error| error.to_string())? + "\n";
    if let Some(output) = output {
        std::fs::write(output, result.bytes).map_err(|error| error.to_string())?;
    }
    std::fs::write(manifest, metadata).map_err(|error| error.to_string())?;
    println!(
        "compiled {} schemas / {} fields (rule_hash={})",
        result.manifest.schema_count, result.manifest.field_count, result.manifest.rule_hash
    );
    Ok(())
}
