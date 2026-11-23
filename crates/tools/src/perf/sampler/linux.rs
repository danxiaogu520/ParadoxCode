//! Linux CPU ticks retain sub-second resolution on short editor sessions.
use serde_json::{Value, json};
use std::sync::OnceLock;

fn sysconf(key: &str) -> Result<u64, String> {
    let output = crate::process::capture(crate::process::command("getconf").arg(key))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("invalid getconf {key}"))
}

pub(super) fn sample(pid: u32) -> Result<Value, String> {
    static UNITS: OnceLock<Result<(u64, u64), String>> = OnceLock::new();
    let &(hz, page) = UNITS
        .get_or_init(|| Ok((sysconf("CLK_TCK")?, sysconf("PAGESIZE")?)))
        .as_ref()
        .map_err(Clone::clone)?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    let (_, fields) = stat.rsplit_once(')').ok_or("invalid process stat record")?;
    let fields: Vec<_> = fields.split_ascii_whitespace().collect();
    // The first field following the parenthesised process name is field 3 (state).
    let field = |i: usize| {
        fields
            .get(i)
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("invalid process stat field")
    };
    Ok(
        json!({"cpu_seconds":(field(11)? + field(12)?) as f64/hz as f64, "working_set_bytes":field(21)?*page}),
    )
}
