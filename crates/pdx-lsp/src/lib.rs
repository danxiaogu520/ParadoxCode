//! Minimal JSON-RPC/LSP runtime for the generic PDX language server.
//!
//! The crate owns transport framing, protocol state, document versioning, URI and position
//! conversion, and result freshness checks. Parser and language-feature logic remains in the
//! editor-neutral workspace and analysis crates.

pub mod check;
pub mod cli;
pub mod release;

mod dependency;
mod initialize;
mod protocol;
mod requests;
mod server;
mod text;
mod transport;
mod uri;
mod vanilla;
mod workspace;

pub use pdx_game::eu4::{
    INSTALL_DESCRIPTOR, first_party_rules, first_party_rules_cached, first_party_rules_ephemeral,
    profile,
};

pub use initialize::{AutoVanillaConfiguration, InitializeOptions};
pub use protocol::LspError;
pub use server::{LspServer, ServerState};
pub use uri::{UriError, path_to_uri, uri_to_path};

pub(crate) const JSON_RPC_VERSION: &str = "2.0";
pub(crate) const INTERNAL_ERROR: i64 = -32603;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const SERVER_NOT_INITIALIZED: i64 = -32002;
pub(crate) const REQUEST_CANCELLED: i64 = -32800;
pub(crate) const DIAGNOSTIC_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
pub(crate) const PROJECT_CONFIG_MAX_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
pub(crate) const MAX_LSP_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_COMPLETION_RESULTS: usize = 512;
pub(crate) const MAX_WORKSPACE_SYMBOL_RESULTS: usize = 256;
pub(crate) const MAX_WORKSPACE_DIAGNOSTIC_FILES: usize = 128;
/// Maximum number of closed Current Mod files that one workspace validation pass publishes.
/// Explicit validation still counts every file; the cap only protects the JSON-RPC client from
/// a notification storm on very large mods.
pub(crate) const MAX_WORKSPACE_DIAGNOSTIC_PUBLICATIONS: usize = 2_000;
/// Maximum number of stale closed-file diagnostic entries cleared by one pass.
pub(crate) const MAX_WORKSPACE_DIAGNOSTIC_CLEARS: usize = 2_000;
pub(crate) const MAX_PUBLISHED_DIAGNOSTICS: usize = 1_000;
pub(crate) const WATCHED_FILES_REGISTRATION_ID: &str = "pdx-source-roots";
pub(crate) const WATCHED_FILES_REQUEST_ID: &str = "pdx/register-source-root-watchers";

// Keep the implementation details available to the in-crate JSON-RPC corpus without exposing
// them as part of the library's external API.
#[cfg(test)]
pub(crate) use initialize::prepare_initialize_candidate;
#[cfg(test)]
pub(crate) use pdx_analysis::CancellationToken;
#[cfg(test)]
pub(crate) use pdx_engine::DocumentId;
#[cfg(test)]
pub(crate) use protocol::{
    RequestId, cancel_initialize_from_notification, cancel_request_from_notification,
    diagnostic_result_counts,
};
#[cfg(test)]
pub(crate) use requests::{bounded_results, strip_snippet_placeholders};
#[cfg(test)]
pub(crate) use server::{InFlightInitialize, InFlightRequest, IndexSetupCancellation};
#[cfg(test)]
pub(crate) use text::changed_document_len;
#[cfg(test)]
pub(crate) use transport::read_message;
#[cfg(test)]
pub(crate) use vanilla::{
    IndexCacheLoadRequest, apply_user_vanilla_configuration, run_auto_vanilla_setup_with_options,
    run_index_cache_load_with_options,
};
#[cfg(test)]
pub(crate) use workspace::{ResolvedSourceRoots, resolve_source_roots};

#[cfg(test)]
mod tests;
