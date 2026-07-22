use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "build") {
        let source = option(&args[1..], "--source")?;
        let output = option(&args[1..], "--output")?;
        let manifest = option(&args[1..], "--manifest")?;
        let result =
            pdx_rulec::compile(&source, &output, &manifest).map_err(|error| error.to_string())?;
        println!("compiled {} rules (rule_hash={})", result.semantic_rule_count, result.rule_hash);
        return Ok(());
    }
    Err("usage: pdx-rulec build --source <rules/eu4> --output <rules/eu4.pdxrules> --manifest <rules/manifest.json>".to_owned())
}

fn option(args: &[String], flag: &str) -> Result<PathBuf, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| PathBuf::from(&pair[1]))
        .ok_or_else(|| format!("missing required option: {flag}"))
}
