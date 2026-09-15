//! Headless frame tracing behind `PDC_TRACE=<path>`.
//!
//! The editor's debug mode traces the protocol from the client side; this
//! module serves the headless repro drivers that speak framed JSON-RPC to pdc
//! directly. Setting `PDC_TRACE` to a file path appends one line per transport
//! frame — direction, method, id, size — with request/response round-trip
//! times correlated by id. `PDC_TRACE_FULL=1` additionally appends a params
//! preview truncated to 2 KiB.
//!
//! The writer owns a dedicated thread fed by an unbounded channel, so callers
//! on the event loop only ever pay one channel send per frame. The handle is
//! initialized once from the environment; tests never set the variable, so
//! in-process parallel servers observe a permanent no-op.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::mpsc::Sender;
use std::time::Instant;

use serde_json::Value;

/// Preview cap for `PDC_TRACE_FULL` param dumps.
const PREVIEW_LIMIT: usize = 2 * 1024;

/// One transport frame enqueued to the writer thread.
pub(crate) struct FrameRecord {
    /// True when the server wrote the frame, false when it read one.
    outgoing: bool,
    method: Option<String>,
    id: Option<String>,
    size: usize,
    at: Instant,
    preview: Option<String>,
}

impl FrameRecord {
    pub(crate) fn new(outgoing: bool, message: &Value, size: usize, with_preview: bool) -> Self {
        let preview = with_preview.then(|| {
            message
                .get("params")
                .or_else(|| message.get("result"))
                .map(|params| truncate_preview(&params.to_string()))
        });
        Self {
            outgoing,
            method: message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned),
            id: message.get("id").map(|id| id.to_string()),
            size,
            at: Instant::now(),
            preview: preview.flatten(),
        }
    }

    /// A frame with an id but no method is a response to an earlier request.
    fn is_response(&self) -> bool {
        self.method.is_none() && self.id.is_some()
    }
}

fn truncate_preview(value: &str) -> String {
    if value.len() <= PREVIEW_LIMIT {
        return value.to_owned();
    }
    let mut truncated = value[..PREVIEW_LIMIT].to_owned();
    truncated.push('…');
    truncated
}

/// Wall-clock stamp for one line; epoch milliseconds keep driver-side logs
/// joinable without pulling in a date-formatting dependency.
fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

/// Formats one trace line. `elapsed_ms` is the request round-trip time when
/// this record answers an earlier request.
pub(crate) fn frame_line(record: &FrameRecord, elapsed_ms: Option<u128>) -> String {
    let direction = if record.outgoing { "→" } else { "←" };
    let mut line = match (&record.method, &record.id) {
        (Some(method), Some(id)) => format!("{direction} {method} id={id}"),
        (Some(method), None) => format!("{direction} {method}"),
        (None, Some(id)) => format!("{direction} response id={id}"),
        (None, None) => format!("{direction} frame"),
    };
    if let Some(elapsed) = elapsed_ms {
        line.push_str(&format!(" ({elapsed}ms)"));
    }
    line.push(' ');
    line.push_str(&format!("{}b", record.size));
    if let Some(preview) = &record.preview {
        line.push_str(&format!(" params={preview}"));
    }
    format!("{} {}", epoch_millis(), line)
}

/// Correlates responses to their requests per origin, so response lines can
/// carry the round-trip time. Ids of client requests and server requests live
/// in separate maps: both sides pick ids independently and may collide.
#[derive(Default)]
pub(crate) struct RequestTimings {
    client: HashMap<String, Instant>,
    server: HashMap<String, Instant>,
}

impl RequestTimings {
    /// Records one frame and returns the round-trip milliseconds when it
    /// answers a still-open request.
    pub(crate) fn note(&mut self, record: &FrameRecord) -> Option<u128> {
        match (record.outgoing, record.is_response()) {
            (false, false) if record.id.is_some() => {
                if let Some(id) = &record.id {
                    self.client.insert(id.clone(), record.at);
                }
                None
            }
            (true, false) if record.id.is_some() => {
                if let Some(id) = &record.id {
                    self.server.insert(id.clone(), record.at);
                }
                None
            }
            (true, true) => record
                .id
                .as_ref()
                .and_then(|id| self.client.remove(id))
                .map(|started| started.elapsed().as_millis()),
            (false, true) => record
                .id
                .as_ref()
                .and_then(|id| self.server.remove(id))
                .map(|started| started.elapsed().as_millis()),
            _ => None,
        }
    }
}

/// Everything `frame()` needs after one-time environment initialization.
struct TraceState {
    sender: Sender<FrameRecord>,
    /// Whether frames carry a truncated params preview (`PDC_TRACE_FULL=1`).
    full: bool,
}

static TRACE: OnceLock<Option<TraceState>> = OnceLock::new();

fn trace_state() -> Option<&'static TraceState> {
    TRACE.get_or_init(init).as_ref()
}

fn init() -> Option<TraceState> {
    let path = std::env::var_os("PDC_TRACE").filter(|value| !value.is_empty())?;
    let full = std::env::var_os("PDC_TRACE_FULL").is_some_and(|value| !value.is_empty());
    let file: File = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "paradoxcode: PDC_TRACE could not open {}: {error}",
                path.display()
            );
            return None;
        }
    };
    let (sender, receiver) = std::sync::mpsc::channel::<FrameRecord>();
    std::thread::Builder::new()
        .name("pdc-trace".to_owned())
        .spawn(move || {
            let mut file = file;
            let mut timings = RequestTimings::default();
            // One write+flush per line: no buffered tail can be lost when the
            // process exits, and an explicitly-enabled debug tool can afford
            // the syscall.
            for record in receiver {
                let elapsed = timings.note(&record);
                let _ = writeln!(file, "{}", frame_line(&record, elapsed));
                let _ = file.flush();
            }
        })
        .ok()?;
    Some(TraceState { sender, full })
}

/// Records one transport frame; a permanent no-op unless `PDC_TRACE` named a
/// writable file at first use.
pub(crate) fn frame(outgoing: bool, message: &Value, size: usize) {
    if let Some(state) = trace_state() {
        let _ = state
            .sender
            .send(FrameRecord::new(outgoing, message, size, state.full));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(outgoing: bool, message: &Value, size: usize) -> FrameRecord {
        FrameRecord::new(outgoing, message, size, false)
    }

    #[test]
    fn frame_lines_carry_direction_method_id_and_size() {
        let request = record(
            false,
            &json!({"jsonrpc":"2.0","id":12,"method":"textDocument/completion","params":{}}),
            1834,
        );
        let line = frame_line(&request, None);
        assert!(
            line.ends_with("← textDocument/completion id=12 1834b"),
            "{line}"
        );

        let notification = record(true, &json!({"jsonrpc":"2.0","method":"$/progress"}), 412);
        let line = frame_line(&notification, None);
        assert!(line.ends_with("→ $/progress 412b"), "{line}");

        let response = record(true, &json!({"jsonrpc":"2.0","id":12,"result":[]}), 91);
        let line = frame_line(&response, Some(34));
        assert!(line.ends_with("→ response id=12 (34ms) 91b"), "{line}");
    }

    #[test]
    fn response_correlation_tracks_each_direction_independently() {
        let mut timings = RequestTimings::default();
        // Client request id 1 and server request id 1 coexist.
        timings.note(&record(
            false,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            10,
        ));
        timings.note(&record(
            true,
            &json!({"jsonrpc":"2.0","id":1,"method":"window/workDoneProgress/create","params":{}}),
            10,
        ));
        // The outgoing initialize response answers the client request.
        let elapsed = timings.note(&record(
            true,
            &json!({"jsonrpc":"2.0","id":1,"result":{}}),
            10,
        ));
        assert!(elapsed.is_some());
        // An unmatched late response resolves nothing.
        let late = timings.note(&record(
            true,
            &json!({"jsonrpc":"2.0","id":1,"result":{}}),
            10,
        ));
        assert!(late.is_none());
    }

    #[test]
    fn previews_are_truncated_to_the_cap() {
        let record = FrameRecord::new(
            true,
            &json!({"jsonrpc":"2.0","id":7,"method":"x","params":{"blob":"y".repeat(5000)}}),
            6000,
            true,
        );
        let line = frame_line(&record, None);
        assert!(line.contains("params="), "{line}");
        assert!(line.len() < PREVIEW_LIMIT + 200, "{line}");
    }
}
