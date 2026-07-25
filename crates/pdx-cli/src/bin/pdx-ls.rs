fn main() -> Result<(), pdx_lsp::LspError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--version" || argument == "-V") {
        println!("pdx-ls 0.1.0");
        return Ok(());
    }
    if !args.is_empty() {
        return Err(pdx_lsp::LspError::Protocol(format!("unknown pdx-ls argument: {}", args[0])));
    }
    let rules = pdx_game_eu4::first_party_rules()?;
    let profile = pdx_game_eu4::profile();
    match pdx_game::UserPaths::platform() {
        Ok(user_paths) => pdx_lsp::LspServer::run_stdio_with_profile_and_auto_vanilla(
            pdx_lsp::InitializeOptions,
            rules,
            profile,
            pdx_lsp::AutoVanillaConfiguration {
                descriptor: pdx_game_eu4::INSTALL_DESCRIPTOR,
                user_paths,
            },
        ),
        Err(error) => {
            eprintln!(
                "pdx-ls: automatic Vanilla discovery is disabled because user paths could not be resolved: {error}"
            );
            pdx_lsp::LspServer::run_stdio_with_profile(pdx_lsp::InitializeOptions, rules, profile)
        }
    }
}
