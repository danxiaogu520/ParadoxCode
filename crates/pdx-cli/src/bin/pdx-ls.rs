fn main() -> Result<(), pdx_lsp::LspError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|argument| argument == "--version" || argument == "-V") {
        println!("pdx-ls 0.1.0");
        return Ok(());
    }
    let rules_path = args
        .windows(2)
        .find(|window| window[0] == "--rules")
        .map(|window| std::path::PathBuf::from(&window[1]));
    pdx_lsp::LspServer::run_stdio_for_game(
        pdx_lsp::InitializeOptions { rules_path },
        pdx_game_eu4::GAME_ID,
    )
}
