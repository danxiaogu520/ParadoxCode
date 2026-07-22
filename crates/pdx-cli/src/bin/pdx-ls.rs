fn main() -> Result<(), pdx_lsp::LspError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--version" || argument == "-V") {
        println!("pdx-ls 0.1.0");
        return Ok(());
    }
    if !args.is_empty() {
        return Err(pdx_lsp::LspError::Protocol(format!("unknown pdx-ls argument: {}", args[0])));
    }
    pdx_lsp::LspServer::run_stdio_with_profile(
        pdx_lsp::InitializeOptions,
        pdx_game_eu4::first_party_rules()?,
        pdx_game_eu4::profile(),
    )
}
