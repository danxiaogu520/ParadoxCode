//! Thin Zed extension entry point for the Phase 1 language frontend.
//!
//! The extension registers no EU4 semantic logic. It owns only the Zed package boundary; grammar
//! metadata and queries are loaded from the adjacent language directories. Language-server
//! process discovery is deliberately left for Phase 2.

use zed_extension_api as zed;

struct ParadoxCodeExtension;

const LANGUAGE_SERVER_ID: &str = "pdx-ls";

impl zed::Extension for ParadoxCodeExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return Err(format!("unsupported language server: {language_server_id}"));
        }
        let settings = zed::settings::LspSettings::for_worktree(LANGUAGE_SERVER_ID, worktree)
            .unwrap_or_default();
        let (configured_path, configured_args) =
            settings.binary.map_or((None, None), |binary| (binary.path, binary.arguments));
        let binary =
            configured_path.or_else(|| worktree.which(LANGUAGE_SERVER_ID)).ok_or_else(|| {
                "pdx-ls was not found; install it or configure lsp.pdx-ls.binary.path".to_owned()
            })?;
        let args = configured_args.unwrap_or_default();
        Ok(zed::Command::new(binary).args(args))
    }
}

zed::register_extension!(ParadoxCodeExtension);

/// Returns the extension's development identifier.
#[must_use]
pub const fn extension_id() -> &'static str {
    "paradoxcode"
}
