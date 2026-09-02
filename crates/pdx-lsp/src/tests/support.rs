use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::*;
use pdx_engine::{
    AnalysisHost, IndexCache, SourceRoot, SourceRootId, SourceRootKind, WorkspaceChange,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Creates a temporary directory for use as a cross-platform workspace root.
pub(crate) fn temp_workspace_dir() -> (std::path::PathBuf, String) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("pdx-lsp-test-{nonce}"));
    fs::create_dir_all(&dir).expect("create temp workspace");
    let canonical = fs::canonicalize(&dir).expect("canonicalize temp workspace");
    (canonical.clone(), path_to_uri(&canonical))
}

/// Canonicalizes a path and returns its file:// URI, matching the format used by
/// workspace scanning so that URI-keyed maps can be compared directly.
pub(crate) fn canonical_uri(path: &std::path::Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path_to_uri(&canonical)
}

pub(crate) fn eu4_server(options: InitializeOptions) -> Result<LspServer, LspError> {
    LspServer::try_new_with_rules(
        options,
        pdx_game::eu4::first_party_rules()?,
        pdx_game::eu4::profile(),
    )
}

pub(crate) fn frame(value: Value) -> Vec<u8> {
    let body = serde_json::to_vec(&value).expect("test JSON should serialize");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend(body);
    framed
}

pub(crate) fn frames(values: impl IntoIterator<Item = Value>) -> Vec<u8> {
    values.into_iter().flat_map(frame).collect()
}

pub(crate) type ReadAction = Option<Box<dyn FnOnce() + Send>>;

pub(crate) struct ScriptedReader {
    steps: VecDeque<(Vec<u8>, ReadAction)>,
    current: Cursor<Vec<u8>>,
}

impl ScriptedReader {
    pub(crate) fn new(steps: impl IntoIterator<Item = (Value, ReadAction)>) -> Self {
        Self {
            steps: steps
                .into_iter()
                .map(|(value, action)| (frame(value), action))
                .collect(),
            current: Cursor::new(Vec::new()),
        }
    }
}

impl Read for ScriptedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if usize::try_from(self.current.position()).unwrap_or(usize::MAX)
            >= self.current.get_ref().len()
        {
            let Some((bytes, action)) = self.steps.pop_front() else {
                return Ok(0);
            };
            if let Some(action) = action {
                action();
            }
            self.current = Cursor::new(bytes);
        }
        self.current.read(buffer)
    }
}

/// Write handle mirroring transport output into a shared buffer so test code
/// can observe frames while `run_transport` is still running. The event loop
/// is the only writer, so frames always land whole; readers consider only
/// complete frames.
#[derive(Clone, Default)]
pub(crate) struct SharedOutput(Arc<Mutex<Vec<u8>>>);

impl SharedOutput {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns every complete frame written so far; a trailing partially
    /// written frame is ignored until its bytes have landed.
    pub(crate) fn frames(&self) -> Vec<Value> {
        let bytes = self.0.lock().expect("shared output lock").clone();
        let mut cursor = Cursor::new(&bytes[..]);
        let mut decoded = Vec::new();
        while let Some(value) = read_message(&mut cursor).ok().flatten() {
            decoded.push(value);
        }
        decoded
    }

    /// Blocks until a complete frame matching `predicate` has been written.
    /// The short poll interval is efficiency only: release is caused by the
    /// frame being written, never by timing, so the resulting interleaving
    /// does not depend on machine speed.
    pub(crate) fn wait_for(&self, predicate: impl Fn(&Value) -> bool) {
        while !self.frames().iter().any(&predicate) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Returns the raw bytes written so far.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0.lock().expect("shared output lock").clone()
    }
}

impl Write for SharedOutput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("shared output lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One-shot latch used by scan-gate tests: the gate sets it once the scan
/// worker is parked, and a `ScriptedReader` action waits on it so the messages
/// that must race the scan are only delivered after the scan is verifiably in
/// flight.
#[derive(Clone, Default)]
pub(crate) struct Latch(Arc<(Mutex<bool>, Condvar)>);

impl Latch {
    pub(crate) fn set(&self) {
        let mut set = self.0.0.lock().expect("latch lock");
        *set = true;
        self.0.1.notify_all();
    }

    pub(crate) fn wait(&self) {
        let mut set = self.0.0.lock().expect("latch lock");
        while !*set {
            set = self.0.1.wait(set).expect("latch lock");
        }
    }
}

/// Matches the successful response to request `id` — the release condition the
/// shutdown-race tests gate the initial scan on.
pub(crate) fn response_written(id: i64) -> impl Fn(&Value) -> bool {
    move |value| value["id"] == id && value.get("result").is_some()
}

/// Builds the deterministic scan-completion gate used by the shutdown-race
/// regression tests. The first background scan worker to finish parks after
/// its scan completed but before the completion is reported to the event
/// loop, and resumes once `release_after` matches a frame the transport has
/// already written (typically the `shutdown` response) — so the scan
/// verifiably completes after that frame and verifiably overlaps every
/// message delivered before it. Revision-race retry scans pass through
/// freely so shutdown drains still converge. The returned latch fires when
/// the worker parks.
pub(crate) fn scan_completion_gate(
    output: &SharedOutput,
    release_after: impl Fn(&Value) -> bool + Send + Sync + 'static,
) -> (Arc<dyn Fn() + Send + Sync>, Latch) {
    let parked = Latch::default();
    let gate_parked = parked.clone();
    let output = output.clone();
    let spent = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(move || {
        if spent.swap(true, Ordering::AcqRel) {
            return;
        }
        gate_parked.set();
        output.wait_for(&release_after);
    });
    (gate, parked)
}

pub(crate) fn decode_frames(bytes: &[u8]) -> Vec<Value> {
    let mut cursor = Cursor::new(bytes);
    let mut decoded = Vec::new();
    while let Some(value) = super::read_message(&mut cursor).expect("test frame is valid") {
        decoded.push(value);
    }
    decoded
}

pub(crate) fn typed_result<T: DeserializeOwned>(responses: &[Value], id: i64) -> T {
    let value = responses
        .iter()
        .find(|value| value["id"] == id)
        .unwrap_or_else(|| panic!("missing response {id}"));
    serde_json::from_value(value["result"].clone())
        .unwrap_or_else(|error| panic!("response {id} is not valid LSP: {error}"))
}

pub(crate) fn stale_cache_fixture(container: &std::path::Path) -> std::path::PathBuf {
    let workspace = container.join("workspace");
    let vanilla = container.join("vanilla");
    fs::create_dir_all(&workspace).expect("workspace directory");
    fs::create_dir_all(&vanilla).expect("Vanilla directory");
    let vanilla = fs::canonicalize(&vanilla).expect("canonical Vanilla directory");
    let cache_path = container.join("vanilla.pdxindex");

    let bootstrap_rules = pdx_game::eu4::bootstrap_rules();
    let mut stale_host = AnalysisHost::with_profile(bootstrap_rules, pdx_game::eu4::profile());
    stale_host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla,
    )]));
    stale_host.refresh_source_roots().expect("scan Vanilla");
    let stale_cache = IndexCache::from_snapshot(&stale_host.snapshot()).expect("stale cache");
    stale_cache.save(&cache_path).expect("save stale cache");
    cache_path
}

/// Builds a cache whose rule hash matches the embedded first-party rules.
pub(crate) fn valid_cache_fixture(container: &std::path::Path) -> std::path::PathBuf {
    let workspace = container.join("workspace");
    let vanilla = container.join("vanilla");
    fs::create_dir_all(&workspace).expect("workspace directory");
    fs::create_dir_all(&vanilla).expect("Vanilla directory");
    let vanilla = fs::canonicalize(&vanilla).expect("canonical Vanilla directory");
    let cache_path = container.join("vanilla.pdxindex");

    let rules = pdx_game::eu4::first_party_rules().expect("embedded rules");
    let mut host = AnalysisHost::with_profile(rules, pdx_game::eu4::profile());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(0),
        SourceRootKind::Vanilla,
        vanilla,
    )]));
    host.refresh_source_roots().expect("scan Vanilla");
    let cache = IndexCache::from_snapshot(&host.snapshot()).expect("cache");
    cache.save(&cache_path).expect("save cache");
    cache_path
}
