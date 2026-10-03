//! Native benchmark, real-server latency, resource, profile and comparison workflows.
pub mod client;
pub mod memory;
pub mod sampler;
pub mod workflow;
pub const HELP: &str = "tools perf bench [--repeat N] [-- CARGO_ARGS...]\ntools perf baseline NAME [--skip-bench] [--skip-sweep] [--repeat N] [--force]\ntools perf ab NAME [--bench-only] [--sweep-only] [--fail-over PCT]\ntools perf sweep --vanilla-source DIR --vanilla-cache PATH --output DIR\ntools perf probe|compare [--workspace DIR] [--server PATH] [--cache PATH | --no-cache] [--output PATH]\ntools perf memory --before BINARY --after BINARY --source DIR --output DIR [--repeat N] [--require-identical-diagnostics]\ntools perf profile bench:NAME|sweep [--flavor samply|perf] [-- ARGS...]\ntools perf init|status|import-corpus|control [options]";
pub fn execute(arguments: &[String]) -> Result<String, String> {
    let Some((name, args)) = arguments.split_first() else {
        return Ok(HELP.into());
    };
    match name.as_str() {
        "--help" | "help" | "-h" => Ok(HELP.into()),
        "memory" => memory::execute(args),
        "probe" | "compare" => client::execute(name, args),
        _ => workflow::execute(name, args),
    }
}
pub fn median(values: &[f64]) -> Result<f64, String> {
    if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return Err("missing or non-finite measurement".into());
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    Ok(if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    })
}
