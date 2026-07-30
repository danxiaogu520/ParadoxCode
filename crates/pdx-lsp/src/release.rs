//! Deterministic server release packaging and verification.
//!
//! Replaces the Python release toolchain: `server_release_contract.py`,
//! `package-server-release.py`, `verify-server-release.py`, and
//! `test-package-server-release.py`.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONTRACT_PATH: &str = "editors/zed/server-distribution.json";

/// One target's archive and executable naming contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerArtifact {
    pub target: String,
    pub archive_template: String,
    pub checksum_template: String,
    pub binary: String,
}

impl ServerArtifact {
    pub fn archive_name(&self, version: &str) -> String {
        self.archive_template.replace("{version}", version)
    }

    pub fn checksum_name(&self, version: &str) -> String {
        self.checksum_template.replace("{archive}", &self.archive_name(version))
    }

    pub fn archive_kind(&self) -> Result<ArchiveKind, ReleaseError> {
        if self.archive_template.ends_with(".tar.gz") {
            Ok(ArchiveKind::TarGz)
        } else if self.archive_template.ends_with(".zip") {
            Ok(ArchiveKind::Zip)
        } else {
            Err(ReleaseError::UnsupportedArchive(self.archive_template.clone()))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

/// Installer-compatible release size limits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLimits {
    pub checksum_bytes: u64,
    pub archive_bytes: u64,
    pub executable_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DistributionContract {
    schema_version: u32,
    binary: String,
    limits: ServerLimits,
    artifacts: std::collections::BTreeMap<String, DistributionArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DistributionArtifact {
    archive: String,
    checksum: String,
    binary: String,
}

/// Loads and validates the server distribution contract.
pub fn load_contract(root: &Path) -> Result<(ServerLimits, Vec<ServerArtifact>), ReleaseError> {
    let path = root.join(CONTRACT_PATH);
    let text = fs::read_to_string(&path)
        .map_err(|error| ReleaseError::Io { path: path.clone(), error })?;
    let contract: DistributionContract = serde_json::from_str(&text)
        .map_err(|error| ReleaseError::Json { path: path.clone(), error })?;
    if contract.schema_version != 1 {
        return Err(ReleaseError::Contract(format!(
            "unsupported schema version: {}",
            contract.schema_version
        )));
    }
    if contract.binary != "pdx-ls" {
        return Err(ReleaseError::Contract(format!(
            "unexpected binary name: {}",
            contract.binary
        )));
    }
    if contract.limits.checksum_bytes == 0
        || contract.limits.archive_bytes == 0
        || contract.limits.executable_bytes == 0
    {
        return Err(ReleaseError::Contract("size limits must be positive".to_owned()));
    }
    let expected_targets: BTreeSet<&str> = [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ]
    .into_iter()
    .collect();
    let actual_targets: BTreeSet<&str> = contract.artifacts.keys().map(String::as_str).collect();
    if actual_targets != expected_targets {
        return Err(ReleaseError::Contract(format!(
            "target matrix mismatch: expected {:?}, found {:?}",
            expected_targets, actual_targets
        )));
    }
    let mut artifacts = Vec::new();
    for (target, entry) in &contract.artifacts {
        let rendered = entry.archive.replace("{version}", "0.0.0");
        if entry.archive.matches("{version}").count() != 1
            || rendered.contains('{')
            || rendered.contains('}')
            || !is_plain_filename(&rendered)
            || entry.checksum != "{archive}.sha256"
            || !is_plain_filename(&entry.binary)
        {
            return Err(ReleaseError::Contract(format!(
                "{target}: invalid artifact contract"
            )));
        }
        artifacts.push(ServerArtifact {
            target: target.clone(),
            archive_template: entry.archive.clone(),
            checksum_template: entry.checksum.clone(),
            binary: entry.binary.clone(),
        });
    }
    artifacts.sort_by(|left, right| left.target.cmp(&right.target));
    Ok((contract.limits, artifacts))
}

fn is_plain_filename(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
}

/// Creates a deterministic tar.gz archive.
pub fn create_tar_gz(binary: &[u8], executable: &str) -> Result<Vec<u8>, ReleaseError> {
    let mut output = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut output, flate2::Compression::best());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary.len() as u64);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("").map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        header.set_groupname("").map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        header.set_path(executable).map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        header.set_cksum();
        archive
            .append_data(&mut header, executable, io::Cursor::new(binary))
            .map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        let encoder = archive.into_inner().map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        encoder.finish().map_err(|error| ReleaseError::Packaging(error.to_string()))?;
    }
    Ok(output)
}

/// Creates a deterministic zip archive.
pub fn create_zip(binary: &[u8], executable: &str) -> Result<Vec<u8>, ReleaseError> {
    let mut output = Vec::new();
    {
        let mut archive =
            zip::ZipWriter::new(io::Cursor::new(&mut output));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755)
            .last_modified_time(zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).map_err(|error| ReleaseError::Packaging(error.to_string()))?);
        archive
            .start_file(executable, options)
            .map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        use std::io::Write;
        archive
            .write_all(binary)
            .map_err(|error| ReleaseError::Packaging(error.to_string()))?;
        archive
            .finish()
            .map_err(|error| ReleaseError::Packaging(error.to_string()))?;
    }
    Ok(output)
}

/// Writes a file atomically via a temporary alongside the target.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), ReleaseError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ReleaseError::Io { path: parent.to_owned(), error })?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ReleaseError::Packaging(format!("invalid output path: {}", path.display())))?;
    let temp_name = format!(".{file_name}.{}.tmp", std::process::id());
    let temp_path = path.with_file_name(temp_name);
    fs::write(&temp_path, contents)
        .map_err(|error| ReleaseError::Io { path: temp_path.clone(), error })?;
    fs::rename(&temp_path, path)
        .map_err(|error| ReleaseError::Io { path: temp_path, error })?;
    Ok(())
}

/// Packages one target's release archive and SHA-256 sidecar.
pub fn package_target(
    version: &str,
    artifact: &ServerArtifact,
    binary_path: &Path,
    output_dir: &Path,
    limits: &ServerLimits,
) -> Result<(PathBuf, PathBuf), ReleaseError> {
    let binary = fs::read(binary_path)
        .map_err(|error| ReleaseError::Io { path: binary_path.to_owned(), error })?;
    if binary.len() as u64 > limits.executable_bytes {
        return Err(ReleaseError::Limits(format!(
            "server binary exceeds the distribution executable size limit ({} > {})",
            binary.len(),
            limits.executable_bytes
        )));
    }
    let archive_bytes = match artifact.archive_kind()? {
        ArchiveKind::TarGz => create_tar_gz(&binary, &artifact.binary)?,
        ArchiveKind::Zip => create_zip(&binary, &artifact.binary)?,
    };
    if archive_bytes.len() as u64 > limits.archive_bytes {
        return Err(ReleaseError::Limits(format!(
            "server archive exceeds the distribution archive size limit ({} > {})",
            archive_bytes.len(),
            limits.archive_bytes
        )));
    }
    let archive_name = artifact.archive_name(version);
    let archive_path = output_dir.join(&archive_name);
    atomic_write(&archive_path, &archive_bytes)?;

    let digest = format!("{:x}", Sha256::digest(&archive_bytes));
    let sidecar_contents = format!("{digest}  {archive_name}\n");
    if sidecar_contents.len() as u64 > limits.checksum_bytes {
        return Err(ReleaseError::Limits(format!(
            "checksum sidecar exceeds limit ({} > {})",
            sidecar_contents.len(),
            limits.checksum_bytes
        )));
    }
    let sidecar_path = output_dir.join(artifact.checksum_name(version));
    atomic_write(&sidecar_path, sidecar_contents.as_bytes())?;
    Ok((archive_path, sidecar_path))
}

/// Validates one release archive and its sidecar.
pub fn verify_archive(
    archive_path: &Path,
    sidecar_path: &Path,
    artifact: &ServerArtifact,
    limits: &ServerLimits,
) -> Result<(), ReleaseError> {
    if !archive_path.is_file() {
        return Err(ReleaseError::Verification(format!(
            "{} is not a regular file",
            archive_path.display()
        )));
    }
    let archive_bytes = fs::read(archive_path)
        .map_err(|error| ReleaseError::Io { path: archive_path.to_owned(), error })?;
    if archive_bytes.len() as u64 > limits.archive_bytes {
        return Err(ReleaseError::Verification(format!(
            "archive exceeds size limit: {}",
            archive_path.display()
        )));
    }
    let sidecar_bytes = fs::read(sidecar_path)
        .map_err(|error| ReleaseError::Io { path: sidecar_path.to_owned(), error })?;
    if sidecar_bytes.len() as u64 > limits.checksum_bytes {
        return Err(ReleaseError::Verification(format!(
            "sidecar exceeds size limit: {}",
            sidecar_path.display()
        )));
    }
    let sidecar_text = String::from_utf8_lossy(&sidecar_bytes);
    let (digest_str, name_in_sidecar) = sidecar_text
        .trim()
        .split_once("  ")
        .ok_or_else(|| {
            ReleaseError::Verification(format!("malformed sidecar: {}", sidecar_path.display()))
        })?;
    let expected_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if name_in_sidecar != expected_name {
        return Err(ReleaseError::Verification(format!(
            "sidecar filename mismatch: {name_in_sidecar} vs {expected_name}"
        )));
    }
    let expected_digest = format!("{:x}", Sha256::digest(&archive_bytes));
    if digest_str != expected_digest {
        return Err(ReleaseError::Verification(format!(
            "checksum mismatch for {}",
            archive_path.display()
        )));
    }
    // Check archive internal structure.
    match artifact.archive_kind()? {
        ArchiveKind::TarGz => verify_tar_gz(archive_path, &artifact.binary, limits)?,
        ArchiveKind::Zip => verify_zip(archive_path, &artifact.binary, limits)?,
    }
    Ok(())
}

fn verify_tar_gz(path: &Path, binary: &str, limits: &ServerLimits) -> Result<(), ReleaseError> {
    let file = fs::File::open(path)
        .map_err(|error| ReleaseError::Io { path: path.to_owned(), error })?;
    let decompressed = flate2::read::GzDecoder::new(io::BufReader::new(file));
    let mut archive = tar::Archive::new(decompressed);
    let entries: Vec<_> = archive
        .entries()
        .map_err(|error| ReleaseError::Verification(format!("tar error: {error}")))?
        .filter_map(|entry| entry.ok())
        .collect();
    if entries.len() != 1 {
        return Err(ReleaseError::Verification(format!(
            "{}: expected one entry, found {}",
            path.display(),
            entries.len()
        )));
    }
    if entries[0].path().map_err(|error| ReleaseError::Verification(error.to_string()))?.as_ref()
        != Path::new(binary)
    {
        return Err(ReleaseError::Verification(format!(
            "{}: expected executable name {binary}",
            path.display()
        )));
    }
    if !entries[0].header().entry_type().is_file() {
        return Err(ReleaseError::Verification(format!(
            "{}: entry is not a regular file",
            path.display()
        )));
    }
    if entries[0].header().mode().unwrap_or(0) & 0o777 != 0o755 {
        return Err(ReleaseError::Verification(format!(
            "{}: executable mode is not 0755",
            path.display()
        )));
    }
    if entries[0].header().size().unwrap_or(0) > limits.executable_bytes {
        return Err(ReleaseError::Verification(format!(
            "{}: executable exceeds size limit",
            path.display()
        )));
    }
    Ok(())
}

fn verify_zip(path: &Path, binary: &str, limits: &ServerLimits) -> Result<(), ReleaseError> {
    let file = fs::File::open(path)
        .map_err(|error| ReleaseError::Io { path: path.to_owned(), error })?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|error| ReleaseError::Verification(error.to_string()))?;
    if archive.len() != 1 {
        return Err(ReleaseError::Verification(format!(
            "{}: expected one entry, found {}",
            path.display(),
            archive.len()
        )));
    }
    let entry = archive.by_index(0).map_err(|error| ReleaseError::Verification(error.to_string()))?;
    if entry.name() != binary {
        return Err(ReleaseError::Verification(format!(
            "{}: expected executable name {binary}",
            path.display()
        )));
    }
    if entry.is_dir() || entry.is_symlink() {
        return Err(ReleaseError::Verification(format!(
            "{}: entry is not a regular file",
            path.display()
        )));
    }
    if entry.size() > limits.executable_bytes {
        return Err(ReleaseError::Verification(format!(
            "{}: executable exceeds size limit",
            path.display()
        )));
    }
    Ok(())
}

/// Validates a release directory containing all target archives.
pub fn verify_release_directory(
    version: &str,
    directory: &Path,
    artifacts: &[ServerArtifact],
    limits: &ServerLimits,
) -> Result<(), ReleaseError> {
    let mut expected_names = BTreeSet::new();
    for artifact in artifacts {
        let archive_name = artifact.archive_name(version);
        let sidecar_name = artifact.checksum_name(version);
        expected_names.insert(archive_name.clone());
        expected_names.insert(sidecar_name.clone());
        let archive_path = directory.join(&archive_name);
        let sidecar_path = directory.join(&sidecar_name);
        verify_archive(&archive_path, &sidecar_path, artifact, limits)?;
    }
    let actual_names: BTreeSet<String> = fs::read_dir(directory)
        .map_err(|error| ReleaseError::Io { path: directory.to_owned(), error })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    if actual_names != expected_names {
        let extra: Vec<_> = actual_names.difference(&expected_names).collect();
        if !extra.is_empty() {
            return Err(ReleaseError::Verification(format!(
                "unexpected release files: {:?}",
                extra
            )));
        }
    }
    Ok(())
}

/// SemVer validation used by release packaging and verification.
pub fn validate_release_version(version: &str) -> Result<String, ReleaseError> {
    // MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]
    let (major_str, rest) = version.split_once('.').ok_or_else(|| {
        ReleaseError::Version(format!("invalid release version: {version}"))
    })?;
    let (minor_str, patch_str) = rest.split_once('.').ok_or_else(|| {
        ReleaseError::Version(format!("invalid release version: {version}"))
    })?;
    for part in [major_str, minor_str] {
        if part.is_empty() || (part.starts_with('0') && part.len() > 1) {
            return Err(ReleaseError::Version(format!("invalid release version: {version}")));
        }
        part.parse::<u64>().map_err(|_| {
            ReleaseError::Version(format!("invalid release version: {version}"))
        })?;
    }
    let patch_numeric = patch_str.split_once('-').map_or(patch_str, |(num, _)| num);
    if patch_numeric.is_empty() || (patch_numeric.starts_with('0') && patch_numeric.len() > 1) {
        return Err(ReleaseError::Version(format!("invalid release version: {version}")));
    }
    patch_numeric.parse::<u64>().map_err(|_| {
        ReleaseError::Version(format!("invalid release version: {version}"))
    })?;
    Ok(version.to_owned())
}

/// Errors from release packaging and verification.
#[derive(Debug)]
pub enum ReleaseError {
    Io { path: PathBuf, error: io::Error },
    Json { path: PathBuf, error: serde_json::Error },
    Contract(String),
    Version(String),
    Limits(String),
    Packaging(String),
    Verification(String),
    UnsupportedArchive(String),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => {
                write!(formatter, "I/O error at {}: {error}", path.display())
            }
            Self::Json { path, error } => {
                write!(formatter, "JSON error at {}: {error}", path.display())
            }
            Self::Contract(message) => write!(formatter, "invalid release contract: {message}"),
            Self::Version(message) => write!(formatter, "invalid version: {message}"),
            Self::Limits(message) => write!(formatter, "size limit exceeded: {message}"),
            Self::Packaging(message) => write!(formatter, "packaging error: {message}"),
            Self::Verification(message) => write!(formatter, "release verification failed: {message}"),
            Self::UnsupportedArchive(name) => {
                write!(formatter, "unsupported archive template: {name}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;

    use super::*;

    const PAYLOAD: &[u8] = b"portable pdx-ls fixture\n";

    fn fixture_limits() -> ServerLimits {
        ServerLimits { checksum_bytes: 1024, archive_bytes: 64 * 1024 * 1024, executable_bytes: 128 * 1024 * 1024 }
    }

    fn fixture_artifact(target: &str) -> ServerArtifact {
        match target {
            "x86_64-unknown-linux-gnu" => ServerArtifact {
                target: target.to_owned(),
                archive_template: "pdx-ls-v{version}-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
                checksum_template: "{archive}.sha256".to_owned(),
                binary: "pdx-ls".to_owned(),
            },
            "x86_64-pc-windows-msvc" => ServerArtifact {
                target: target.to_owned(),
                archive_template: "pdx-ls-v{version}-x86_64-pc-windows-msvc.zip".to_owned(),
                checksum_template: "{archive}.sha256".to_owned(),
                binary: "pdx-ls.exe".to_owned(),
            },
            _ => unimplemented!("test target: {target}"),
        }
    }

    #[test]
    fn version_validation() {
        assert!(validate_release_version("1.2.3").is_ok());
        assert!(validate_release_version("0.1.0").is_ok());
        assert!(validate_release_version("1.2.3-alpha.1").is_ok());
        for invalid in ["01.2.3", "1.02.3", "1.2.03", "1.2", "v1.2.3"] {
            assert!(validate_release_version(invalid).is_err(), "{invalid} should be rejected");
        }
    }

    #[test]
    fn tar_gz_roundtrips() {
        let archive = create_tar_gz(PAYLOAD, "pdx-ls").expect("create tar.gz");
        assert!(!archive.is_empty());
        // Decompress via file to avoid in-memory streaming issues.
        let path = std::env::temp_dir().join(format!("pdx-rt-{}.tar.gz", std::process::id()));
        fs::write(&path, &archive).expect("write");
        let file = fs::File::open(&path).expect("open");
        let gz = flate2::read::GzDecoder::new(io::BufReader::new(file));
        let mut tar_archive = tar::Archive::new(gz);
        let entries: Vec<_> = tar_archive.entries().expect("entries").filter_map(|e| e.ok()).collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path().expect("path").as_os_str(), "pdx-ls");
        assert_eq!(entries[0].header().size().unwrap_or(0), PAYLOAD.len() as u64);
        assert_eq!(entries[0].header().mode().unwrap_or(0) & 0o777, 0o755);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn deterministic_zip() {
        let first = create_zip(PAYLOAD, "pdx-ls.exe").expect("create zip");
        let second = create_zip(PAYLOAD, "pdx-ls.exe").expect("create zip again");
        assert_eq!(first, second);
        let cursor = io::Cursor::new(&first);
        let mut archive = zip::ZipArchive::new(cursor).expect("open zip");
        assert_eq!(archive.len(), 1);
        let mut buf = Vec::new();
        archive.by_index(0).expect("entry").read_to_end(&mut buf).expect("read entry");
        assert_eq!(buf, PAYLOAD);
    }

    #[test]
    fn package_and_verify() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-release-test-{nonce}"));
        fs::create_dir_all(&root).expect("temp dir");

        let binary = root.join("fixture");
        fs::write(&binary, PAYLOAD).expect("write fixture");

        let limits = fixture_limits();
        let version = "0.1.0-test.1";

        for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            let artifact = fixture_artifact(target);
            let (archive_path, _sidecar) = package_target(
                version, &artifact, &binary, &root, &limits,
            )
            .expect("package target");
            assert!(archive_path.is_file());

            let sidecar_path = root.join(artifact.checksum_name(version));
            verify_archive(&archive_path, &sidecar_path, &artifact, &limits)
                .expect("verify archive");
        }

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn packaging_rejects_oversized_binary() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-release-oversized-{nonce}"));
        fs::create_dir_all(&root).expect("temp dir");

        let binary = root.join("oversized");
        let oversized = vec![0u8; 1024];
        fs::write(&binary, &oversized).expect("write oversized");

        let limits = ServerLimits { checksum_bytes: 1024, archive_bytes: 1024, executable_bytes: 10 };
        let artifact = fixture_artifact("x86_64-unknown-linux-gnu");
        let result = package_target("0.1.0", &artifact, &binary, &root, &limits);
        assert!(result.is_err());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn verification_rejects_bad_checksum() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-release-bad-checksum-{nonce}"));
        fs::create_dir_all(&root).expect("temp dir");

        let binary = root.join("fixture");
        fs::write(&binary, PAYLOAD).expect("write fixture");

        let limits = fixture_limits();
        let artifact = fixture_artifact("x86_64-unknown-linux-gnu");
        let version = "0.1.0";
        let (archive_path, sidecar_path) =
            package_target(version, &artifact, &binary, &root, &limits).expect("package");

        let bad_sidecar = format!("{}  {}\n", "0".repeat(64), artifact.archive_name(version));
        fs::write(&sidecar_path, bad_sidecar).expect("write bad sidecar");
        assert!(verify_archive(&archive_path, &sidecar_path, &artifact, &limits).is_err());

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn contract_loads_from_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (limits, artifacts) = load_contract(&root).expect("load contract");
        assert_eq!(artifacts.len(), 5);
        assert!(limits.executable_bytes > 0);
        for artifact in &artifacts {
            assert!(artifact.archive_template.contains("{version}"));
            assert_eq!(artifact.checksum_template, "{archive}.sha256");
        }
    }
}
