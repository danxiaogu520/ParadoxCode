fn main() -> Result<(), pdx_lsp::LspError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("pdx-ls {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if !args.is_empty() {
        return Err(pdx_lsp::LspError::Protocol(format!(
            "unknown pdx-ls argument: {}",
            args[0]
        )));
    }
    let profile = pdx_lsp::profile();
    match pdx_game::UserPaths::platform() {
        Ok(user_paths) => {
            let rules = pdx_lsp::first_party_rules_cached(
                &user_paths.rules_cache(pdx_lsp::INSTALL_DESCRIPTOR.game_id),
            )?;
            pdx_lsp::LspServer::run_stdio_with_profile_and_auto_vanilla(
                pdx_lsp::InitializeOptions,
                rules,
                profile,
                pdx_lsp::AutoVanillaConfiguration {
                    descriptor: pdx_lsp::INSTALL_DESCRIPTOR,
                    user_paths,
                    source_override: None,
                },
            )
        }
        Err(error) => {
            eprintln!(
                "pdx-ls: user cache paths could not be resolved; compiled rules will not be persisted: {error}"
            );
            let rules = pdx_lsp::first_party_rules_ephemeral()?;
            pdx_lsp::LspServer::run_stdio_with_profile(pdx_lsp::InitializeOptions, rules, profile)
        }
    }
}
