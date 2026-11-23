//! Each trial starts from the disk template, opens a caller and ends after both overlays close.
use super::fixture::{DISK_DEFINITION, Fixture, ROUND_DEFINITION, TERRAIN, TERRAIN_PATH};
use super::{Session, SessionSettings, process_sample, record};
use crate::perf::completion::normalize;
use serde_json::{Value, json};
use std::time::Instant;

fn labels(result: &Value, expected: &str, stale: &str) -> Result<usize, String> {
    let items = result["items"]
        .as_array()
        .ok_or("completion has no item array")?;
    if !items.iter().any(|i| i["label"] == expected) || items.iter().any(|i| i["label"] == stale) {
        return Err(format!(
            "caller completion did not resolve the current template parameter {expected}"
        ));
    }
    Ok(items.len())
}

fn result(response: &mut Value) -> Result<Value, String> {
    if let Some(error) = response.get("error") {
        return Err(format!("unexpected RPC error: {error}"));
    }
    response
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| "missing RPC result".into())
}

fn trial(s: &mut Session<'_>, f: &Fixture, index: usize, group: &str) -> Result<Value, String> {
    let (path, source) = &f.callers[index % f.callers.len()];
    let caller = f.document(path)?;
    let provider = f.document(ROUND_DEFINITION)?;
    let offset =
        source.find("perf_round = { P").expect("owned call site") + "perf_round = { ".len();
    let before = &source[..offset];
    let line = before.bytes().filter(|b| *b == b'\n').count();
    let column = before.rsplit('\n').next().unwrap().encode_utf16().count();
    let position = |width: usize| json!({"line":line,"character":column+width});
    let params = |width: usize| json!({"textDocument":caller,"position":position(width)});
    let caller_path = f.workspace.join(path);
    let provider_path = f.workspace.join(ROUND_DEFINITION);
    s.client.open(&provider_path, DISK_DEFINITION, 1)?;
    let opened = Instant::now();
    s.client.open(&caller_path, source, 1)?;
    let id = s
        .client
        .send_request("textDocument/completion", params(1))?;
    let (mut frame, at) = s.client.wait_response(id, s.timeout)?;
    let count = labels(&result(&mut frame)?, "PERF_DISK", "PERF_LIVE")?;
    record(
        s.measurements,
        &format!("{group}/open-to-first-completion"),
        at.duration_since(opened),
        Some(count),
    );

    let mut definition = DISK_DEFINITION.replace("PERF_DISK", "PERF_LIVE");
    definition.push_str("perf_round_marker = { add_prestige = 2 }\n");
    s.client.notify("textDocument/didChange",json!({"textDocument":{"uri":provider["uri"],"version":2},"contentChanges":[{"text":definition}]}))?;
    // Observe the edited provider before timing the consumer. A stale cross-file result
    // is a correctness failure even if its completion list has the expected cardinality.
    let symbols = s.client.request(
        "textDocument/documentSymbol",
        json!({"textDocument":provider}),
    )?;
    if !symbols
        .as_array()
        .is_some_and(|items| items.iter().any(|i| i["name"] == "perf_round_marker"))
    {
        return Err("edited template definition was not visible".into());
    }

    let mut obsolete = Vec::new();
    let mut width = 1;
    for (step, prefix) in ["PE", "PER"].into_iter().enumerate() {
        s.client.notify("textDocument/didChange",json!({"textDocument":{"uri":caller["uri"],"version":step+2},
            "contentChanges":[{"range":{"start":position(0),"end":position(width)},"text":prefix}]}))?;
        width = prefix.len();
        let id = s
            .client
            .send_request("textDocument/completion", params(width))?;
        s.client.notify("$/cancelRequest", json!({"id":id}))?;
        obsolete.push(id);
    }
    let background = s.client.send_request(
        "pdc/textDiagnostics",
        json!({"files":[{"path":TERRAIN_PATH,"text":TERRAIN}]}),
    )?;
    let last_edit = json!({"textDocument":{"uri":caller["uri"],"version":4},"contentChanges":[{"range":{"start":position(0),"end":position(width)},"text":"PERF_"}]});
    let final_params = params(5);
    let started = Instant::now();
    s.client.notify("textDocument/didChange", last_edit)?;
    let id = s
        .client
        .send_request("textDocument/completion", final_params.clone())?;
    let (mut frame, at) = s.client.wait_response(id, s.timeout)?;
    let newest = result(&mut frame)?;
    let count = labels(&newest, "PERF_LIVE", "PERF_DISK")?;
    let (mut diagnostic, diagnosed_at) = s.client.wait_response(background, s.timeout)?;
    let diagnostic_cancelled = diagnostic["error"]["code"] == -32800;
    if !diagnostic_cancelled
        && !result(&mut diagnostic)?
            .pointer("/0/diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|ds| ds.iter().any(|d| d["code"] == "InvalidValue"))
    {
        return Err("background diagnostics lost the fixture error".into());
    }
    if normalize(newest) != normalize(s.client.request("textDocument/completion", final_params)?) {
        return Err("last edit and settled completions differ".into());
    }
    record(
        s.measurements,
        &format!("{group}/last-edit-to-completion"),
        at.duration_since(started),
        Some(count),
    );
    let mut cancelled = 0;
    for id in obsolete {
        let (mut frame, _) = s.client.wait_response(id, s.timeout)?;
        if frame["error"]["code"] == -32800 {
            cancelled += 1;
        } else if !result(&mut frame)?["items"].is_array() {
            return Err("unexpected obsolete completion response".into());
        }
    }

    let since = s.client.notifications.len();
    s.client.close(&caller_path)?;
    s.client.close(&provider_path)?;
    for doc in [&caller, &provider] {
        s.client.wait_notification_since(
            "textDocument/publishDiagnostics",
            since,
            s.timeout,
            |p| {
                p["uri"] == doc["uri"]
                    && p.get("version").is_none()
                    && p["diagnostics"] == json!([])
            },
        )?;
    }
    let rollback = s
        .client
        .request("workspace/symbol", json!({"query":"perf_round_marker"}))?;
    if !rollback
        .as_array()
        .is_some_and(|items| items.iter().all(|i| i["name"] != "perf_round_marker"))
    {
        return Err("closed template overlay still contributes its symbol".into());
    }
    // Earlier notifications have been consumed, including both close acknowledgements.
    s.client.notifications.clear();
    Ok(
        json!({"cycle":index+1,"site":index%f.callers.len(),"position":position(5),"cancelled":cancelled,"superseded_requests":2,
        "diagnostics_pending_at_last_edit":diagnosed_at>=started,"diagnostics_cancelled":diagnostic_cancelled}),
    )
}

pub(super) fn exercise(
    s: &mut Session<'_>,
    f: &Fixture,
    log: &mut Value,
    settings: &SessionSettings,
    persist: &mut impl FnMut(&Value) -> Result<(), String>,
) -> Result<(), String> {
    log["edit_trials"] = json!([]);
    for i in 0..settings.edit_rounds {
        let observation =
            trial(s, f, i, "round").map_err(|e| format!("edit trial {}: {e}", i + 1))?;
        log["edit_trials"].as_array_mut().unwrap().push(observation);
    }
    if settings.soak_cycles == 0 {
        return Ok(());
    }
    let start = Instant::now();
    let pid = s.client.pid();
    let checkpoint = |cycle: usize| {
        std::thread::sleep(settings.idle);
        json!({"cycle":cycle,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"process":process_sample(pid)})
    };
    log["soak"] =
        json!({"checkpoints":[checkpoint(0)],"completed_cycles":0,"cancelled_requests":0});
    persist(log)?;
    let stride = settings.soak_cycles.div_ceil(8);
    let mut cancelled = 0;
    let mut window_start = 0;
    for i in 0..settings.soak_cycles {
        let observed = trial(s, f, i, "soak").map_err(|e| format!("soak cycle {}: {e}", i + 1))?;
        cancelled += observed["cancelled"].as_u64().unwrap();
        let completed = i + 1;
        log["soak"]["completed_cycles"] = json!(completed);
        log["soak"]["cancelled_requests"] = json!(cancelled);
        if completed.is_multiple_of(stride) || completed == settings.soak_cycles {
            let mut point = checkpoint(completed);
            let window = &s.measurements["soak/last-edit-to-completion"].samples_ms;
            // This Metrics map can already contain earlier sessions' observations.
            let count = completed - window_start;
            point["cycles_in_window"] = json!(count);
            point["median_last_edit_ms"] =
                json!(crate::perf::median(&window[window.len() - count..])?);
            point["cancelled_requests_total"] = json!(cancelled);
            log["soak"]["checkpoints"]
                .as_array_mut()
                .unwrap()
                .push(point);
            window_start = completed;
            persist(log)?;
            eprintln!(
                "soak checkpoint {completed}/{} cycles",
                settings.soak_cycles
            );
        }
    }
    let checkpoints = log["soak"]["checkpoints"].as_array().unwrap();
    let bytes = |c: &Value| {
        c.pointer("/process/working_set_bytes")
            .and_then(Value::as_i64)
    };
    let drift = bytes(checkpoints.last().unwrap())
        .zip(bytes(&checkpoints[0]))
        .map(|(end, start)| end - start);
    let latency_change = (checkpoints.len() >= 3).then(|| {
        let first = checkpoints[1]["median_last_edit_ms"].as_f64().unwrap();
        checkpoints.last().unwrap()["median_last_edit_ms"]
            .as_f64()
            .unwrap()
            - first
    });
    log["soak"]["last_minus_initial_bytes"] = json!(drift);
    log["soak"]["last_minus_first_window_ms"] = json!(latency_change);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_file_trials_reject_stale_candidates_even_at_equal_cardinality() {
        let stale = json!({"items":[{"label":"PERF_DISK"}]});
        let current = json!({"items":[{"label":"PERF_LIVE"}]});
        let mixed = json!({"items":[{"label":"PERF_LIVE"},{"label":"PERF_DISK"}]});
        assert!(labels(&stale, "PERF_LIVE", "PERF_DISK").is_err());
        assert!(labels(&mixed, "PERF_LIVE", "PERF_DISK").is_err());
        assert_eq!(labels(&current, "PERF_LIVE", "PERF_DISK").unwrap(), 1);
    }
}
