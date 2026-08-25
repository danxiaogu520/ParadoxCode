fn main() -> Result<(), pdx_lsp::LspError> {
    let started = std::time::Instant::now();
    let process_message = format!("pdx-ls process started (pid {})", std::process::id());
    eprintln!("pdx-ls: {process_message}");
    let mut startup_messages = vec![process_message];
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
    let profile_message = format!(
        "game profile selected: {}",
        pdx_lsp::INSTALL_DESCRIPTOR.game_id
    );
    eprintln!("pdx-ls: {profile_message}");
    startup_messages.push(profile_message);
    match pdx_game::UserPaths::platform() {
        Ok(user_paths) => {
            let paths_message = format!(
                "user paths resolved: config={}, cache={}",
                user_paths.config_file.display(),
                user_paths.cache_root.display()
            );
            eprintln!("pdx-ls: {paths_message}");
            startup_messages.push(paths_message);

            let rules_path = user_paths.rules_cache(pdx_lsp::INSTALL_DESCRIPTOR.game_id);
            let rules_started = std::time::Instant::now();
            let loading_message = format!(
                "loading first-party {} rules cache from {}",
                pdx_lsp::INSTALL_DESCRIPTOR.game_id,
                rules_path.display()
            );
            eprintln!("pdx-ls: {loading_message}");
            startup_messages.push(loading_message);
            let rules = match pdx_lsp::first_party_rules_cached(&rules_path) {
                Ok(rules) => {
                    let ready_message = format!(
                        "first-party rules ready in {:.1} ms (hash {})",
                        rules_started.elapsed().as_secs_f64() * 1000.0,
                        rules.rule_hash().to_hex()
                    );
                    eprintln!("pdx-ls: {ready_message}");
                    startup_messages.push(ready_message);
                    rules
                }
                Err(error) => {
                    eprintln!(
                        "pdx-ls: first-party rules failed after {:.1} ms: {error}",
                        rules_started.elapsed().as_secs_f64() * 1000.0
                    );
                    return Err(error.into());
                }
            };
            let profile = rules.profile().clone();
            let transport_message =
                "stdio JSON-RPC transport starting; waiting for initialize".to_owned();
            eprintln!("pdx-ls: {transport_message}");
            startup_messages.push(transport_message);
            let server =
                pdx_lsp::LspServer::run_stdio_with_profile_and_auto_vanilla_with_startup_log(
                    pdx_lsp::InitializeOptions,
                    rules,
                    profile,
                    pdx_lsp::AutoVanillaConfiguration {
                        descriptor: pdx_lsp::INSTALL_DESCRIPTOR,
                        user_paths,
                        source_override: None,
                    },
                    startup_messages,
                );
            let result = server;
            eprintln!(
                "pdx-ls: stdio transport ended after {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            result
        }
        Err(error) => {
            eprintln!(
                "pdx-ls: user cache paths could not be resolved; compiled rules will not be persisted: {error}"
            );
            let fallback_message = format!(
                "user cache paths unavailable; using process-local rules artifact: {error}"
            );
            startup_messages.push(fallback_message);
            let rules_started = std::time::Instant::now();
            let rules = match pdx_lsp::first_party_rules_ephemeral() {
                Ok(rules) => {
                    let ready_message = format!(
                        "process-local first-party rules ready in {:.1} ms (hash {})",
                        rules_started.elapsed().as_secs_f64() * 1000.0,
                        rules.rule_hash().to_hex()
                    );
                    eprintln!("pdx-ls: {ready_message}");
                    startup_messages.push(ready_message);
                    rules
                }
                Err(error) => {
                    eprintln!(
                        "pdx-ls: process-local rules failed after {:.1} ms: {error}",
                        rules_started.elapsed().as_secs_f64() * 1000.0
                    );
                    return Err(error.into());
                }
            };
            let profile = rules.profile().clone();
            let transport_message =
                "stdio JSON-RPC transport starting; waiting for initialize".to_owned();
            eprintln!("pdx-ls: {transport_message}");
            startup_messages.push(transport_message);
            let result = pdx_lsp::LspServer::run_stdio_with_profile_and_startup_log(
                pdx_lsp::InitializeOptions,
                rules,
                profile,
                startup_messages,
            );
            eprintln!(
                "pdx-ls: stdio transport ended after {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            result
        }
    }
}
