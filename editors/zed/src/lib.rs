//! Zed extension entry point and checksummed `pdx-ls` release installer.

use std::fs;
use std::io::Read;

use flate2::read::{DeflateDecoder, GzDecoder};
use sha2::{Digest, Sha256};
use zed_extension_api as zed;

struct ParadoxCodeExtension;

const LANGUAGE_SERVER_ID: &str = "pdx-ls";
const REPOSITORY: &str = "danxiaogu520/ParadoxCode";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_CHECKSUM_BYTES: usize = 1_024;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES: usize = 128 * 1024 * 1024;
// Python's USTAR writer pads the header, payload, and end markers to a 20-block record.
const MAX_TAR_OVERHEAD_BYTES: usize = 20 * 512;

#[derive(Clone, Copy)]
enum ArchiveKind {
    TarGz,
    Zip,
}

struct Artifact {
    target: &'static str,
    binary: &'static str,
    kind: ArchiveKind,
}

fn platform_artifact(platform: (zed::Os, zed::Architecture)) -> zed::Result<Artifact> {
    use zed::{Architecture, Os};
    let artifact = match platform {
        (Os::Linux, Architecture::X8664) => Artifact {
            target: "x86_64-unknown-linux-gnu",
            binary: "pdx-ls",
            kind: ArchiveKind::TarGz,
        },
        (Os::Linux, Architecture::Aarch64) => Artifact {
            target: "aarch64-unknown-linux-gnu",
            binary: "pdx-ls",
            kind: ArchiveKind::TarGz,
        },
        (Os::Mac, Architecture::X8664) => Artifact {
            target: "x86_64-apple-darwin",
            binary: "pdx-ls",
            kind: ArchiveKind::TarGz,
        },
        (Os::Mac, Architecture::Aarch64) => Artifact {
            target: "aarch64-apple-darwin",
            binary: "pdx-ls",
            kind: ArchiveKind::TarGz,
        },
        (Os::Windows, Architecture::X8664) => Artifact {
            target: "x86_64-pc-windows-msvc",
            binary: "pdx-ls.exe",
            kind: ArchiveKind::Zip,
        },
        _ => return Err("ParadoxCode does not publish pdx-ls for this platform".to_owned()),
    };
    Ok(artifact)
}

fn archive_name(artifact: &Artifact) -> String {
    let extension = match artifact.kind {
        ArchiveKind::TarGz => "tar.gz",
        ArchiveKind::Zip => "zip",
    };
    format!("pdx-ls-v{VERSION}-{}.{extension}", artifact.target)
}

fn fetch(url: &str, maximum: usize, label: &str) -> zed::Result<Vec<u8>> {
    let request = zed::http_client::HttpRequest::builder()
        .method(zed::http_client::HttpMethod::Get)
        .url(url)
        .redirect_policy(zed::http_client::RedirectPolicy::FollowAll)
        .build()?;
    let stream = request.fetch_stream()?;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next_chunk()? {
        let length = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| format!("downloaded {label} exceeds the safety limit"))?;
        if length > maximum {
            return Err(format!(
                "downloaded {label} exceeds the {maximum}-byte safety limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_limited(reader: impl Read, maximum: usize, label: &str) -> zed::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut output)
        .map_err(|error| format!("failed to decompress {label}: {error}"))?;
    if output.len() > maximum {
        return Err(format!(
            "decompressed {label} exceeds the {maximum}-byte safety limit"
        ));
    }
    Ok(output)
}

fn expected_checksum(sidecar: &[u8], archive: &str) -> zed::Result<String> {
    let text = std::str::from_utf8(sidecar)
        .map_err(|_| "release checksum sidecar is not UTF-8".to_owned())?
        .trim();
    let (digest, name) = text
        .split_once("  ")
        .ok_or_else(|| "release checksum sidecar is malformed".to_owned())?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) || name != archive
    {
        return Err("release checksum sidecar does not match the selected archive".to_owned());
    }
    Ok(digest.to_ascii_lowercase())
}

fn extract_tar_gz(archive: &[u8], binary: &str) -> zed::Result<Vec<u8>> {
    extract_tar_gz_with_limit(archive, binary, MAX_EXECUTABLE_BYTES)
}

fn extract_tar_gz_with_limit(
    archive: &[u8],
    binary: &str,
    maximum_executable_bytes: usize,
) -> zed::Result<Vec<u8>> {
    let maximum_tar_bytes = maximum_executable_bytes.saturating_add(MAX_TAR_OVERHEAD_BYTES);
    let tar = read_limited(
        GzDecoder::new(archive),
        maximum_tar_bytes,
        "server tar archive",
    )?;
    if tar.len() < 1024 {
        return Err("server tar archive is truncated".to_owned());
    }
    let header = &tar[..512];
    let name_end = header[..100]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(100);
    let name = std::str::from_utf8(&header[..name_end])
        .map_err(|_| "server tar member name is not UTF-8".to_owned())?;
    let checksum_text = std::str::from_utf8(&header[148..156])
        .map_err(|_| "server tar checksum is malformed".to_owned())?
        .trim_matches(['\0', ' ']);
    let checksum = u64::from_str_radix(checksum_text, 8)
        .map_err(|_| "server tar checksum is malformed".to_owned())?;
    let actual_checksum = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum::<u64>();
    let size_text = std::str::from_utf8(&header[124..136])
        .map_err(|_| "server tar size is malformed".to_owned())?
        .trim_matches(['\0', ' ']);
    let size = usize::from_str_radix(size_text, 8)
        .map_err(|_| "server tar size is malformed".to_owned())?;
    let end = 512_usize
        .checked_add(size)
        .ok_or_else(|| "server tar member is too large".to_owned())?;
    let padded_end = end
        .checked_add(511)
        .map(|value| value / 512 * 512)
        .ok_or_else(|| "server tar member is too large".to_owned())?;
    if name != binary
        || checksum != actual_checksum
        || &header[257..263] != b"ustar\0"
        || &header[263..265] != b"00"
        || !matches!(header[156], 0 | b'0')
        || size > maximum_executable_bytes
        || end > tar.len()
        || padded_end > tar.len()
        || tar[padded_end..].iter().any(|byte| *byte != 0)
    {
        return Err("server tar archive must contain only the expected executable".to_owned());
    }
    Ok(tar[512..end].to_vec())
}

fn little_u16(bytes: &[u8], offset: usize) -> zed::Result<u16> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "server zip archive is truncated".to_owned())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn little_u32(bytes: &[u8], offset: usize) -> zed::Result<u32> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "server zip archive is truncated".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn extract_zip(archive: &[u8], binary: &str) -> zed::Result<Vec<u8>> {
    if little_u32(archive, 0)? != 0x0403_4b50 {
        return Err("server zip local header is missing".to_owned());
    }
    let flags = little_u16(archive, 6)?;
    let method = little_u16(archive, 8)?;
    let expected_crc = little_u32(archive, 14)?;
    let compressed_size = usize::try_from(little_u32(archive, 18)?)
        .map_err(|_| "server zip member is too large".to_owned())?;
    let expected_size = usize::try_from(little_u32(archive, 22)?)
        .map_err(|_| "server zip member is too large".to_owned())?;
    if expected_size > MAX_EXECUTABLE_BYTES {
        return Err("server zip executable exceeds the safety limit".to_owned());
    }
    let name_length = usize::from(little_u16(archive, 26)?);
    let extra_length = usize::from(little_u16(archive, 28)?);
    let name_start = 30_usize;
    let name_end = name_start
        .checked_add(name_length)
        .ok_or_else(|| "server zip member is too large".to_owned())?;
    let data_start = name_end
        .checked_add(extra_length)
        .ok_or_else(|| "server zip member is too large".to_owned())?;
    let data_end = data_start
        .checked_add(compressed_size)
        .ok_or_else(|| "server zip member is too large".to_owned())?;
    let name = std::str::from_utf8(
        archive
            .get(name_start..name_end)
            .ok_or_else(|| "server zip archive is truncated".to_owned())?,
    )
    .map_err(|_| "server zip member name is not UTF-8".to_owned())?;
    let compressed = archive
        .get(data_start..data_end)
        .ok_or_else(|| "server zip archive is truncated".to_owned())?;
    let eocd = archive
        .len()
        .checked_sub(22)
        .ok_or_else(|| "server zip end record is missing".to_owned())?;
    let central_size = usize::try_from(little_u32(archive, eocd + 12)?)
        .map_err(|_| "server zip central directory is too large".to_owned())?;
    let central_offset = usize::try_from(little_u32(archive, eocd + 16)?)
        .map_err(|_| "server zip central directory is too large".to_owned())?;
    let central_name_length = usize::from(little_u16(archive, central_offset + 28)?);
    let central_extra_length = usize::from(little_u16(archive, central_offset + 30)?);
    let central_comment_length = usize::from(little_u16(archive, central_offset + 32)?);
    let central_name_start = central_offset
        .checked_add(46)
        .ok_or_else(|| "server zip central directory is too large".to_owned())?;
    let central_name_end = central_name_start
        .checked_add(central_name_length)
        .ok_or_else(|| "server zip central directory is too large".to_owned())?;
    let central_end = central_name_end
        .checked_add(central_extra_length)
        .and_then(|value| value.checked_add(central_comment_length))
        .ok_or_else(|| "server zip central directory is too large".to_owned())?;
    let central_name = std::str::from_utf8(
        archive
            .get(central_name_start..central_name_end)
            .ok_or_else(|| "server zip central directory is truncated".to_owned())?,
    )
    .map_err(|_| "server zip central member name is not UTF-8".to_owned())?;
    if name != binary
        || central_name != binary
        || flags & 0x0009 != 0
        || little_u32(archive, central_offset)? != 0x0201_4b50
        || little_u16(archive, central_offset + 8)? != flags
        || little_u16(archive, central_offset + 10)? != method
        || little_u32(archive, central_offset + 16)? != expected_crc
        || little_u32(archive, central_offset + 20)? != little_u32(archive, 18)?
        || little_u32(archive, central_offset + 24)? != little_u32(archive, 22)?
        || central_offset != data_end
        || central_end != eocd
        || central_size != central_end - central_offset
        || little_u32(archive, eocd)? != 0x0605_4b50
        || little_u16(archive, eocd + 4)? != 0
        || little_u16(archive, eocd + 6)? != 0
        || little_u16(archive, eocd + 8)? != 1
        || little_u16(archive, eocd + 10)? != 1
        || little_u16(archive, eocd + 20)? != 0
        || little_u32(archive, central_offset + 42)? != 0
    {
        return Err("server zip archive must contain only the expected executable".to_owned());
    }
    let mut output = Vec::with_capacity(expected_size);
    match method {
        0 => output.extend_from_slice(compressed),
        8 => {
            output = read_limited(
                DeflateDecoder::new(compressed),
                MAX_EXECUTABLE_BYTES,
                "server zip executable",
            )?;
        }
        _ => return Err("server zip uses an unsupported compression method".to_owned()),
    }
    if output.len() != expected_size {
        return Err("server zip executable size does not match its header".to_owned());
    }
    if crc32fast::hash(&output) != expected_crc {
        return Err("server zip executable checksum does not match its header".to_owned());
    }
    Ok(output)
}

fn extract(archive: &[u8], artifact: &Artifact) -> zed::Result<Vec<u8>> {
    match artifact.kind {
        ArchiveKind::TarGz => extract_tar_gz(archive, artifact.binary),
        ArchiveKind::Zip => extract_zip(archive, artifact.binary),
    }
}

// Direct `releases/download` URLs avoid the unauthenticated GitHub REST API quota
// (60 requests/hour/IP) that the tag lookup previously exhausted.
fn release_asset_url(artifact: &Artifact) -> String {
    format!(
        "https://github.com/{REPOSITORY}/releases/download/v{VERSION}/{}",
        archive_name(artifact)
    )
}

fn sha256_file(path: &str) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn cached_server_is_valid(binary_path: &str) -> bool {
    let checksum_path = format!("{binary_path}.sha256");
    let Ok(metadata) = fs::symlink_metadata(binary_path) else {
        return false;
    };
    if !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > u64::try_from(MAX_EXECUTABLE_BYTES).unwrap_or(u64::MAX)
    {
        return false;
    }
    let Ok(checksum_metadata) = fs::symlink_metadata(&checksum_path) else {
        return false;
    };
    if !checksum_metadata.is_file() || checksum_metadata.len() > 128 {
        return false;
    }
    let Ok(expected) = fs::read_to_string(checksum_path) else {
        return false;
    };
    let expected = expected.trim();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    sha256_file(binary_path).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))
}

fn remove_cache_file(path: &str) -> zed::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove invalid server cache entry `{path}`: {error}"
        )),
    }
}

fn ensure_install_directory(path: &str) -> zed::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "server cache directory `{path}` is not a regular directory"
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| format!("failed to create server cache: {error}"))
        }
        Err(error) => Err(format!(
            "failed to inspect server cache directory `{path}`: {error}"
        )),
    }
}

fn install_server(language_server_id: &zed::LanguageServerId) -> zed::Result<String> {
    let artifact = platform_artifact(zed::current_platform())?;
    let install_dir = format!("pdx-ls-v{VERSION}-{}", artifact.target);
    let binary_path = format!("{install_dir}/{}", artifact.binary);
    let local_checksum_path = format!("{binary_path}.sha256");
    if cached_server_is_valid(&binary_path) {
        if !matches!(artifact.kind, ArchiveKind::Zip) {
            zed::make_file_executable(&binary_path)?;
        }
        return Ok(binary_path);
    }
    remove_cache_file(&binary_path)?;
    remove_cache_file(&local_checksum_path)?;

    zed::set_language_server_installation_status(
        language_server_id,
        &zed::LanguageServerInstallationStatus::Downloading,
    );
    let result = (|| {
        let archive = archive_name(&artifact);
        let archive_url = release_asset_url(&artifact);
        let expected = expected_checksum(
            &fetch(
                &format!("{archive_url}.sha256"),
                MAX_CHECKSUM_BYTES,
                "checksum sidecar",
            )?,
            &archive,
        )?;
        let bytes = fetch(&archive_url, MAX_ARCHIVE_BYTES, "server archive")?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != expected {
            return Err(format!("checksum verification failed for `{archive}`"));
        }
        let executable = extract(&bytes, &artifact)?;
        if executable.is_empty() {
            return Err("downloaded pdx-ls executable is empty".to_owned());
        }
        let executable_checksum = format!("{:x}\n", Sha256::digest(&executable));
        ensure_install_directory(&install_dir)?;
        let temporary = format!("{binary_path}.tmp");
        let temporary_checksum = format!("{local_checksum_path}.tmp");
        remove_cache_file(&temporary)?;
        remove_cache_file(&temporary_checksum)?;
        fs::write(&temporary, executable)
            .map_err(|error| format!("failed to write downloaded server: {error}"))?;
        fs::write(&temporary_checksum, executable_checksum)
            .map_err(|error| format!("failed to write downloaded server checksum: {error}"))?;
        if !matches!(artifact.kind, ArchiveKind::Zip) {
            zed::make_file_executable(&temporary)?;
        }
        fs::rename(&temporary, &binary_path)
            .map_err(|error| format!("failed to install downloaded server: {error}"))?;
        fs::rename(&temporary_checksum, &local_checksum_path)
            .map_err(|error| format!("failed to install downloaded server checksum: {error}"))?;
        Ok(binary_path)
    })();
    let status = match &result {
        Ok(_) => zed::LanguageServerInstallationStatus::None,
        Err(error) => zed::LanguageServerInstallationStatus::Failed(error.clone()),
    };
    zed::set_language_server_installation_status(language_server_id, &status);
    result
}

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
        let (configured_path, configured_args) = settings
            .binary
            .map_or((None, None), |binary| (binary.path, binary.arguments));
        // The shared `.pdx/project.toml` `[server].binary` is the single
        // editor-agnostic way to point both Zed and VS Code at pdx-ls; it is
        // consulted after an explicit editor setting and before PATH.
        let shared = shared_server_binary(worktree)?;
        let binary = configured_path
            .or(shared)
            .or_else(|| worktree.which(LANGUAGE_SERVER_ID))
            .map_or_else(|| install_server(language_server_id), Ok)?;
        let args = configured_args.unwrap_or_default();
        Ok(zed::Command::new(binary).args(args))
    }

    fn language_server_initialization_options_schema(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        if language_server_id.as_ref() != LANGUAGE_SERVER_ID {
            return None;
        }
        Some(initialization_options_schema())
    }
}

zed::register_extension!(ParadoxCodeExtension);

/// Returns the `[server].binary` of the shared `.pdx/project.toml` in the
/// worktree, or `Ok(None)` when the file is absent. A present-but-invalid
/// file fails loudly — the shared config must never be silently ignored.
fn shared_server_binary(worktree: &zed::Worktree) -> zed::Result<Option<String>> {
    let text = match worktree.read_text_file(".pdx/project.toml") {
        Ok(text) => text,
        Err(_) => return Ok(None),
    };
    parse_shared_server_binary(&text)
}

/// Extracts `[server].binary` from shared `.pdx/project.toml` text. The rest
/// of the file belongs to pdx-ls (which auto-discovers the same file); the
/// extension only needs the server path to launch it.
fn parse_shared_server_binary(text: &str) -> zed::Result<Option<String>> {
    let value: toml::Value =
        toml::from_str(text).map_err(|error| format!("invalid .pdx/project.toml: {error}"))?;
    let binary = value
        .get("server")
        .and_then(|server| server.get("binary"))
        .and_then(|binary| binary.as_str())
        .map(str::to_owned);
    Ok(binary)
}

/// JSON Schema for `lsp.pdx-ls.initialization_options`.
///
/// Mirrors the `WorkspaceInitializationOptions` accepted by `pdx-ls`: `projectConfig`,
/// `modDirectory`, `vanillaIndexCache`, and the dependency list with its optional persistent
/// index cache. Zed uses this schema to offer completion, validation, and documentation while
/// the user edits `.zed/settings.json`.
fn initialization_options_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "projectConfig": {
                "type": "string",
                "description": "Path to a `.pdx/project.toml` whose fields are overridden by the inline options below. When absent, a `.pdx/project.toml` next to the workspace root is discovered automatically, so Zed and VS Code share the same project configuration with no per-editor setup."
            },
            "modDirectory": {
                "type": "string",
                "description": "Directory of the current Mod being edited. Defaults to the workspace root."
            },
            "vanillaIndexCache": {
                "type": "string",
                "description": "Path to a persistent Vanilla index cache (`.pdxindex`). When absent, `pdx-ls` attempts automatic discovery once."
            },
            "dependencies": {
                "type": "array",
                "description": "Dependency Mods in order from lowest to highest priority.",
                "items": {
                    "type": "object",
                    "required": ["id", "path"],
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Stable dependency identity used for symbol resolution."
                        },
                        "path": {
                            "type": "string",
                            "description": "Directory of the dependency Mod."
                        },
                        "index": {
                            "type": "string",
                            "description": "Optional persistent index cache (`.pdxindex`). When set, the dependency is not scanned live; the cache is loaded and rebuilt in the background when missing. Rebuild after changing the dependency by running `pdx index dependency --id <id> --source <path> --output <cache>` and restarting the language server."
                        }
                    }
                }
            },
            "gameDirectory": {
                "type": "string",
                "description": "EU4 installation root (the folder containing `eu4.exe`). Backs the VS Code mission-tree preview with real game textures; the Zed extension does not render a preview. When empty, the server attempts one-time automatic discovery."
            }
        }
    })
}

/// Returns the extension's development identifier.
#[must_use]
pub const fn extension_id() -> &'static str {
    "paradoxcode"
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::initialization_options_schema;
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder};

    use sha2::{Digest, Sha256};

    use super::{
        MAX_ARCHIVE_BYTES, MAX_CHECKSUM_BYTES, MAX_EXECUTABLE_BYTES, MAX_TAR_OVERHEAD_BYTES,
        VERSION, archive_name, cached_server_is_valid, expected_checksum, extract_tar_gz,
        extract_tar_gz_with_limit, extract_zip, parse_shared_server_binary, platform_artifact,
        read_limited, release_asset_url,
    };

    #[test]
    fn shared_server_binary_reads_the_server_table() {
        let config = r#"
mod_directory = "mod"
[[dependencies]]
id = "Chinese Language Mod for 1.37"
path = "deps/han"

[server]
binary = "C:/Code/ParadoxCode/target/release/pdx-ls.exe"
"#;
        assert_eq!(
            parse_shared_server_binary(config).expect("parse"),
            Some("C:/Code/ParadoxCode/target/release/pdx-ls.exe".to_owned())
        );
        // Backslashes are parsed like TOML, not regex-matched.
        let windows = r#"[server]
binary = "C:\\tools\\pdx-ls.exe""#;
        assert_eq!(
            parse_shared_server_binary(windows).expect("parse"),
            Some("C:\\tools\\pdx-ls.exe".to_owned())
        );
    }

    #[test]
    fn shared_server_binary_is_absent_without_a_server_table() {
        let config = r#"mod_directory = "mod""#;
        assert_eq!(parse_shared_server_binary(config).expect("parse"), None);
    }

    #[test]
    fn shared_server_binary_fails_loudly_on_invalid_toml() {
        assert!(parse_shared_server_binary("mod_directory = [").is_err());
    }

    #[cfg(unix)]
    use super::{ensure_install_directory, remove_cache_file};

    const PAYLOAD: &[u8] = b"pdx-ls test payload";

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("pdx-zed-contract-{}-{nonce}", std::process::id()));
            fs::create_dir(&path).expect("create contract test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove contract test directory");
        }
    }

    fn tar_gz(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut tar = vec![0_u8; 1024];
        tar[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}\0", payload.len());
        tar[124..136].copy_from_slice(size.as_bytes());
        tar[156] = b'0';
        tar[257..263].copy_from_slice(b"ustar\0");
        tar[263..265].copy_from_slice(b"00");
        let checksum = tar[..512]
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum::<u64>();
        tar[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        tar.splice(512..512, payload.iter().copied());
        let padding = (512 - payload.len() % 512) % 512;
        tar.splice(512 + payload.len()..512 + payload.len(), vec![0; padding]);
        let record_padding =
            (MAX_TAR_OVERHEAD_BYTES - tar.len() % MAX_TAR_OVERHEAD_BYTES) % MAX_TAR_OVERHEAD_BYTES;
        tar.extend(std::iter::repeat_n(0, record_padding));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar).expect("encode tar fixture");
        encoder.finish().expect("finish tar fixture")
    }

    fn zip(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(payload).expect("encode zip fixture");
        let compressed = encoder.finish().expect("finish zip fixture");
        let mut archive = Vec::new();
        archive.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&20_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&8_u16.to_le_bytes());
        archive.extend_from_slice(&[0; 4]);
        archive.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        archive.extend_from_slice(
            &u32::try_from(compressed.len())
                .expect("compressed size")
                .to_le_bytes(),
        );
        archive.extend_from_slice(&u32::try_from(payload.len()).expect("size").to_le_bytes());
        archive.extend_from_slice(
            &u16::try_from(name.len())
                .expect("name length")
                .to_le_bytes(),
        );
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(name.as_bytes());
        archive.extend_from_slice(&compressed);
        let central_offset = u32::try_from(archive.len()).expect("central offset");
        archive.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&20_u16.to_le_bytes());
        archive.extend_from_slice(&20_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&8_u16.to_le_bytes());
        archive.extend_from_slice(&[0; 4]);
        archive.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        archive.extend_from_slice(
            &u32::try_from(compressed.len())
                .expect("compressed size")
                .to_le_bytes(),
        );
        archive.extend_from_slice(&u32::try_from(payload.len()).expect("size").to_le_bytes());
        archive.extend_from_slice(
            &u16::try_from(name.len())
                .expect("name length")
                .to_le_bytes(),
        );
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u32.to_le_bytes());
        archive.extend_from_slice(&0_u32.to_le_bytes());
        archive.extend_from_slice(name.as_bytes());
        let central_size = u32::try_from(archive.len()).expect("central end") - central_offset;
        archive.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive.extend_from_slice(&1_u16.to_le_bytes());
        archive.extend_from_slice(&1_u16.to_le_bytes());
        archive.extend_from_slice(&central_size.to_le_bytes());
        archive.extend_from_slice(&central_offset.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive
    }

    #[test]
    fn initialization_options_schema_describes_the_pdx_ls_workspace_options() {
        let schema = initialization_options_schema();
        let properties = schema["properties"].as_object().expect("schema properties");
        for key in [
            "projectConfig",
            "modDirectory",
            "vanillaIndexCache",
            "dependencies",
        ] {
            assert!(properties.contains_key(key), "missing {key}");
        }
        let dependencies = properties["dependencies"].clone();
        assert_eq!(dependencies["type"], "array");
        let items = dependencies["items"].as_object().expect("dependency items");
        assert_eq!(items["required"], serde_json::json!(["id", "path"]));
        for key in ["id", "path", "index"] {
            assert!(
                items["properties"]
                    .as_object()
                    .expect("item properties")
                    .contains_key(key)
            );
        }
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn checksum_sidecar_is_strictly_bound_to_the_archive_name() {
        let digest = "a".repeat(64);
        assert_eq!(
            expected_checksum(
                format!("{digest}  pdx-ls-v0.1.0-test.tar.gz\n").as_bytes(),
                "pdx-ls-v0.1.0-test.tar.gz",
            ),
            Ok(digest)
        );
        assert!(
            expected_checksum(b"aaaaaaaa  another.tar.gz\n", "pdx-ls-v0.1.0-test.tar.gz").is_err()
        );
    }

    #[test]
    fn release_urls_use_direct_download_paths_without_the_github_api() {
        let artifact = platform_artifact((
            zed_extension_api::Os::Windows,
            zed_extension_api::Architecture::X8664,
        ))
        .expect("supported platform");
        let url = release_asset_url(&artifact);
        assert_eq!(
            url,
            format!(
                "https://github.com/{}/releases/download/v{VERSION}/{}",
                super::REPOSITORY,
                archive_name(&artifact)
            )
        );
        assert!(!url.contains("api.github.com"));
        assert_eq!(
            format!("{url}.sha256"),
            format!(
                "https://github.com/{}/releases/download/v{VERSION}/{}.sha256",
                super::REPOSITORY,
                archive_name(&artifact)
            )
        );
    }

    #[test]
    fn rust_platform_mapping_matches_the_canonical_distribution_contract() {
        use zed_extension_api::{Architecture, Os};

        let contract: serde_json::Value =
            serde_json::from_str(include_str!("../server-distribution.json"))
                .expect("parse distribution contract");
        assert_eq!(contract["limits"]["checksum_bytes"], MAX_CHECKSUM_BYTES);
        assert_eq!(contract["limits"]["archive_bytes"], MAX_ARCHIVE_BYTES);
        assert_eq!(contract["limits"]["executable_bytes"], MAX_EXECUTABLE_BYTES);
        let artifacts = contract["artifacts"].as_object().expect("artifact table");
        let cases = [
            ((Os::Linux, Architecture::X8664), "x86_64-unknown-linux-gnu"),
            (
                (Os::Linux, Architecture::Aarch64),
                "aarch64-unknown-linux-gnu",
            ),
            ((Os::Mac, Architecture::X8664), "x86_64-apple-darwin"),
            ((Os::Mac, Architecture::Aarch64), "aarch64-apple-darwin"),
            ((Os::Windows, Architecture::X8664), "x86_64-pc-windows-msvc"),
        ];
        for (platform, target) in cases {
            let artifact = platform_artifact(platform).expect("supported platform");
            let contract = &artifacts[target];
            assert_eq!(artifact.target, target);
            assert_eq!(contract["binary"], artifact.binary);
            assert_eq!(
                contract["archive"]
                    .as_str()
                    .expect("archive template")
                    .replace("{version}", VERSION),
                archive_name(&artifact)
            );
            assert_eq!(contract["checksum"], "{archive}.sha256");
        }
        assert_eq!(artifacts.len(), cases.len());
        assert!(platform_artifact((Os::Windows, Architecture::Aarch64)).is_err());
    }

    #[test]
    fn decompression_reader_stops_at_the_safety_limit() {
        assert_eq!(
            read_limited(&b"four"[..], 4, "fixture"),
            Ok(b"four".to_vec())
        );
        assert!(read_limited(&b"five!"[..], 4, "fixture").is_err());
    }

    #[test]
    fn restricted_extractors_accept_only_the_expected_executable() {
        let valid_tar = tar_gz("pdx-ls", PAYLOAD);
        assert_eq!(extract_tar_gz(&valid_tar, "pdx-ls"), Ok(PAYLOAD.to_vec()));
        assert!(extract_tar_gz(&tar_gz("../pdx-ls", PAYLOAD), "pdx-ls").is_err());
        let mut corrupt_tar = valid_tar;
        corrupt_tar[20] ^= 1;
        assert!(extract_tar_gz(&corrupt_tar, "pdx-ls").is_err());

        let valid_zip = zip("pdx-ls.exe", PAYLOAD);
        assert_eq!(extract_zip(&valid_zip, "pdx-ls.exe"), Ok(PAYLOAD.to_vec()));
        assert!(extract_zip(&zip("nested/pdx-ls.exe", PAYLOAD), "pdx-ls.exe").is_err());
        let mut corrupt_zip = valid_zip;
        let central_offset = corrupt_zip
            .windows(4)
            .position(|bytes| bytes == 0x0201_4b50_u32.to_le_bytes())
            .expect("central directory");
        corrupt_zip[14] ^= 1;
        corrupt_zip[central_offset + 16] ^= 1;
        assert!(extract_zip(&corrupt_zip, "pdx-ls.exe").is_err());
    }

    #[test]
    fn tar_container_overhead_does_not_reduce_the_executable_limit() {
        let payload = vec![b'x'; 512];
        assert_eq!(
            extract_tar_gz_with_limit(&tar_gz("pdx-ls", &payload), "pdx-ls", payload.len()),
            Ok(payload.clone())
        );
        let oversized = vec![b'x'; payload.len() + 1];
        assert!(
            extract_tar_gz_with_limit(&tar_gz("pdx-ls", &oversized), "pdx-ls", payload.len())
                .is_err()
        );
    }

    #[test]
    fn release_archive_shapes_are_accepted_by_the_rust_extractors() {
        assert_eq!(
            extract_tar_gz(&tar_gz("pdx-ls", PAYLOAD), "pdx-ls"),
            Ok(PAYLOAD.to_vec())
        );
        assert_eq!(
            extract_zip(&zip("pdx-ls.exe", PAYLOAD), "pdx-ls.exe"),
            Ok(PAYLOAD.to_vec())
        );
    }

    #[test]
    fn cached_server_requires_a_matching_local_executable_checksum() {
        let root = TestDirectory::new();
        let binary = root.0.join("pdx-ls");
        let binary_path = binary.to_str().expect("UTF-8 test path");
        fs::write(&binary, PAYLOAD).expect("write cached executable");
        assert!(!cached_server_is_valid(binary_path));
        fs::write(
            format!("{binary_path}.sha256"),
            format!("{:x}\n", Sha256::digest(PAYLOAD)),
        )
        .expect("write cached checksum");
        assert!(cached_server_is_valid(binary_path));
        fs::write(&binary, b"corrupt").expect("corrupt cached executable");
        assert!(!cached_server_is_valid(binary_path));
        fs::write(&binary, PAYLOAD).expect("restore cached executable");
        fs::write(format!("{binary_path}.sha256"), vec![b'a'; 129])
            .expect("write oversized checksum metadata");
        assert!(!cached_server_is_valid(binary_path));
    }

    #[cfg(unix)]
    #[test]
    fn server_cache_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = TestDirectory::new();
        let target = root.0.join("target-pdx-ls");
        let binary = root.0.join("pdx-ls");
        fs::write(&target, PAYLOAD).expect("write symlink target");
        symlink(&target, &binary).expect("link cached executable");
        fs::write(
            format!("{}.sha256", binary.display()),
            format!("{:x}\n", Sha256::digest(PAYLOAD)),
        )
        .expect("write cached checksum");
        assert!(!cached_server_is_valid(
            binary.to_str().expect("UTF-8 test path")
        ));
        remove_cache_file(binary.to_str().expect("UTF-8 test path")).expect("remove cache link");
        assert_eq!(fs::read(&target).expect("read symlink target"), PAYLOAD);

        let target_directory = root.0.join("target-directory");
        let linked_directory = root.0.join("linked-directory");
        fs::create_dir(&target_directory).expect("create directory target");
        symlink(&target_directory, &linked_directory).expect("link cache directory");
        assert!(
            ensure_install_directory(linked_directory.to_str().expect("UTF-8 test path")).is_err()
        );
    }
}
