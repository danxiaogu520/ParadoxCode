use std::collections::VecDeque;
use std::fs;
use std::io::{Cursor, Read};

use super::*;
use pdx_engine::{
    AnalysisHost, SourceRoot, SourceRootId, SourceRootKind, VanillaIndexCache, WorkspaceChange,
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
    let stale_cache =
        VanillaIndexCache::from_snapshot(&stale_host.snapshot()).expect("stale cache");
    stale_cache.save(&cache_path).expect("save stale cache");
    cache_path
}
