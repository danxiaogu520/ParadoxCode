// This byte stream deliberately bypasses workspace indexing and JSON-RPC frame limits.
fn transcode_stdio(profile: &str, direction: &str) -> Result<(), pdc::LspError> {
    use std::io::{Read, Write};
    use transcode::{Direction, EscapeSet, FileConversionError, Profile};
    let profile = match profile {
        "localisation" => Profile::Localisation,
        "script" => Profile::Script,
        _ => return Err(pdc::LspError::Protocol("unknown transcode profile".into())),
    };
    let direction = match direction {
        "encode" => Direction::Encode(EscapeSet::Paratranz),
        "decode" => Direction::Decode,
        _ => {
            return Err(pdc::LspError::Protocol(
                "unknown transcode direction".into(),
            ));
        }
    };
    let mut input = Vec::new();
    std::io::stdin().lock().read_to_end(&mut input)?;
    match transcode::convert_editor_buffer(&input, profile, direction) {
        Ok(bytes) => std::io::stdout().lock().write_all(&bytes)?,
        Err(error) => {
            let refusal = match error {
                FileConversionError::InvalidUtf8 => serde_json::json!({"refused":"invalidUtf8"}),
                FileConversionError::BrokenEscape { byte_index } => {
                    serde_json::json!({"refused":"brokenEscape","offsets":[byte_index]})
                }
                FileConversionError::Unencodable { error } => {
                    let points: Vec<_> = error.unencodable.iter().take(8).map(|point|
                        serde_json::json!({"codePoint":point.code_point,"byteIndex":point.byte_index})
                    ).collect();
                    serde_json::json!({"refused":"unencodable","count":error.unencodable.len(),"points":points})
                }
            };
            eprintln!("{refusal}");
            std::process::exit(2);
        }
    }
    Ok(())
}

fn main() -> Result<(), pdc::LspError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [command, profile, direction] = args.as_slice()
        && command == "--transcode-stdio"
    {
        return transcode_stdio(profile, direction);
    }
    let started = std::time::Instant::now();
    let process_message = format!("paradoxcode process started (pid {})", std::process::id());
    eprintln!("paradoxcode: {process_message}");
    let mut startup_messages = vec![process_message];
    if args
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("paradoxcode {}", engine::LSP_VERSION);
        return Ok(());
    }
    if !args.is_empty() {
        return Err(pdc::LspError::Protocol(format!(
            "unknown paradoxcode argument: {}",
            args[0]
        )));
    }
    let profile_message = format!("game profile selected: {}", pdc::INSTALL_DESCRIPTOR.game_id);
    eprintln!("paradoxcode: {profile_message}");
    startup_messages.push(profile_message);

    let rules_started = std::time::Instant::now();
    let loading_message = format!(
        "loading compiled first-party {} rules",
        pdc::INSTALL_DESCRIPTOR.game_id
    );
    eprintln!("paradoxcode: {loading_message}");
    startup_messages.push(loading_message);
    let rules = match pdc::runtime_rules() {
        Ok(rules) => {
            let ready_message = format!(
                "first-party rules ready in {:.1} ms (hash {})",
                rules_started.elapsed().as_secs_f64() * 1000.0,
                rules.rule_hash().to_hex()
            );
            eprintln!("paradoxcode: {ready_message}");
            startup_messages.push(ready_message);
            rules
        }
        Err(error) => {
            eprintln!(
                "pdc: first-party rules failed after {:.1} ms: {error}",
                rules_started.elapsed().as_secs_f64() * 1000.0
            );
            return Err(error.into());
        }
    };
    let profile = rules.profile().clone();

    match game::UserPaths::platform() {
        Ok(user_paths) => {
            let paths_message = format!(
                "user paths resolved: config={}, cache={}",
                user_paths.config_file.display(),
                user_paths.cache_root.display()
            );
            eprintln!("paradoxcode: {paths_message}");
            startup_messages.push(paths_message);
            user_paths.remove_legacy_rule_caches(pdc::INSTALL_DESCRIPTOR.game_id);

            let transport_message =
                "stdio JSON-RPC transport starting; waiting for initialize".to_owned();
            eprintln!("paradoxcode: {transport_message}");
            startup_messages.push(transport_message);
            let server = pdc::LspServer::run_stdio_with_profile_and_auto_vanilla_with_startup_log(
                pdc::InitializeOptions,
                rules,
                profile,
                pdc::AutoVanillaConfiguration {
                    descriptor: pdc::INSTALL_DESCRIPTOR,
                    user_paths,
                    source_override: None,
                },
                startup_messages,
            );
            eprintln!(
                "pdc: stdio transport ended after {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            server
        }
        Err(error) => {
            eprintln!(
                "pdc: user cache paths could not be resolved; vanilla auto-discovery is disabled: {error}"
            );
            let fallback_message = format!(
                "user cache paths unavailable; vanilla auto-discovery is disabled: {error}"
            );
            startup_messages.push(fallback_message);
            let transport_message =
                "stdio JSON-RPC transport starting; waiting for initialize".to_owned();
            eprintln!("paradoxcode: {transport_message}");
            startup_messages.push(transport_message);
            let result = pdc::LspServer::run_stdio_with_profile_and_startup_log(
                pdc::InitializeOptions,
                rules,
                profile,
                startup_messages,
            );
            eprintln!(
                "pdc: stdio transport ended after {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            result
        }
    }
}
