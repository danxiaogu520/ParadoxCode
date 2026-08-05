//! Game installation discovery, user-local configuration, and EU4 game profile.
//!
//! This crate provides platform-agnostic installation search and the EU4 semantic profile
//! consumed by the language server. The installation discovery logic is game-agnostic; the
//! EU4 profile lives in the `eu4` module.

pub mod eu4;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_DISCOVERY_DEPTH: usize = 128;

/// Static, data-only facts needed to locate one supported game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GameInstallDescriptor {
    /// Stable identity shared with the game's rules artifact and semantic profile.
    pub game_id: &'static str,
    /// Human-readable name used in CLI and editor messages.
    pub display_name: &'static str,
    /// Executable paths relative to the installation root, selected by target platform.
    pub executable_paths: PlatformExecutablePaths,
    /// Directories that must all exist below a valid installation root.
    pub validation_directories: &'static [&'static str],
    /// Directory names used by Steam and common-location discovery.
    pub installation_directory_names: &'static [&'static str],
}

/// Platform-specific executable markers, using `/` separators below the installation root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlatformExecutablePaths {
    /// Windows executable markers.
    pub windows: &'static [&'static str],
    /// Linux executable markers.
    pub linux: &'static [&'static str],
    /// macOS bundle or executable markers.
    pub macos: &'static [&'static str],
}

impl PlatformExecutablePaths {
    fn current(self) -> &'static [&'static str] {
        #[cfg(target_os = "windows")]
        {
            self.windows
        }
        #[cfg(target_os = "linux")]
        {
            self.linux
        }
        #[cfg(target_os = "macos")]
        {
            self.macos
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            &[]
        }
    }
}

/// Search breadth selected by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryDepth {
    /// Inspect platform and Steam candidates without recursively walking entire disks.
    Quick,
    /// Recursively inspect local fixed and removable storage.
    Deep,
}

/// Inputs for one installation search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryOptions {
    /// Search breadth.
    pub depth: DiscoveryDepth,
    /// Explicit roots or candidate directories supplied by the user.
    pub roots: Vec<PathBuf>,
    /// Whether platform defaults and mounted local volumes should be included.
    pub include_platform_locations: bool,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            depth: DiscoveryDepth::Quick,
            roots: Vec::new(),
            include_platform_locations: true,
        }
    }
}

/// Cooperative cancellation for potentially long deep searches.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryToken(Arc<AtomicBool>);

impl DiscoveryToken {
    /// Creates a live token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Reports whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Result of scanning for one supported game.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryReport {
    /// Canonical, validated installation roots in deterministic order.
    pub installations: Vec<PathBuf>,
    /// Directories that could not be read. Discovery continues after these failures.
    pub unreadable_directories: usize,
    /// Whether the caller cancelled the traversal.
    pub cancelled: bool,
}

/// Validates the agreed minimal installation shape without executing or hashing game files.
#[must_use]
pub fn validate_installation(root: &Path, descriptor: &GameInstallDescriptor) -> bool {
    validate_installation_with_executables(
        root,
        descriptor,
        descriptor.executable_paths.current().iter(),
    )
}

/// Validates an explicitly supplied source directory using any platform's executable marker.
///
/// Vanilla source data is platform-independent, and an explicit path may point to a mounted
/// installation from another platform (for example, a Windows Steam library mounted in WSL).
/// Automatic discovery intentionally continues to use [`validate_installation`] so it does not
/// select foreign-platform installations from generic search locations.
#[must_use]
pub fn validate_installation_for_source(root: &Path, descriptor: &GameInstallDescriptor) -> bool {
    let executables = descriptor
        .executable_paths
        .windows
        .iter()
        .chain(descriptor.executable_paths.linux)
        .chain(descriptor.executable_paths.macos);
    validate_installation_with_executables(root, descriptor, executables)
}

fn validate_installation_with_executables<'a>(
    root: &Path,
    descriptor: &GameInstallDescriptor,
    mut executables: impl Iterator<Item = &'a &'static str>,
) -> bool {
    root.is_dir()
        && executables.any(|relative| join_portable(root, relative).is_file())
        && descriptor
            .validation_directories
            .iter()
            .all(|relative| join_portable(root, relative).is_dir())
}

/// Finds validated installations using deterministic, symlink-safe traversal.
#[must_use]
pub fn discover_installations(
    descriptor: &GameInstallDescriptor,
    options: &DiscoveryOptions,
    cancellation: &DiscoveryToken,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    let mut candidates = BTreeSet::new();
    for root in &options.roots {
        candidates.insert(root.clone());
    }
    if options.include_platform_locations {
        candidates.extend(quick_candidates(descriptor));
    }

    match options.depth {
        DiscoveryDepth::Quick => {
            for candidate in candidates {
                if cancellation.is_cancelled() {
                    report.cancelled = true;
                    break;
                }
                add_candidate(candidate, descriptor, &mut report.installations);
            }
        }
        DiscoveryDepth::Deep => {
            if options.include_platform_locations {
                candidates.extend(local_volume_roots());
            }
            let excluded = excluded_volume_roots();
            let explicit_roots = options.roots.iter().cloned().collect::<BTreeSet<_>>();
            let mut visited = BTreeSet::new();
            for root in candidates {
                let exclusions = if explicit_roots.contains(&root) {
                    None
                } else {
                    Some(&excluded)
                };
                walk_root(
                    &root,
                    descriptor,
                    cancellation,
                    exclusions,
                    0,
                    &mut visited,
                    &mut report,
                );
                if report.cancelled {
                    break;
                }
            }
        }
    }
    report.installations.sort();
    report.installations.dedup();
    report
}

fn add_candidate(
    candidate: PathBuf,
    descriptor: &GameInstallDescriptor,
    installations: &mut Vec<PathBuf>,
) {
    if validate_installation(&candidate, descriptor) {
        if let Ok(path) = fs::canonicalize(candidate) {
            installations.push(path);
        }
        return;
    }
    for name in descriptor.installation_directory_names {
        let child = candidate.join(name);
        if validate_installation(&child, descriptor)
            && let Ok(path) = fs::canonicalize(child)
        {
            installations.push(path);
        }
    }
}

fn walk_root(
    root: &Path,
    descriptor: &GameInstallDescriptor,
    cancellation: &DiscoveryToken,
    excluded: Option<&BTreeSet<PathBuf>>,
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
    report: &mut DiscoveryReport,
) {
    if depth > MAX_DISCOVERY_DEPTH {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(root) else {
        report.unreadable_directories = report.unreadable_directories.saturating_add(1);
        return;
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() || should_skip_directory(root) {
        return;
    }
    let canonical = match fs::canonicalize(root) {
        Ok(path) => path,
        Err(_) => {
            report.unreadable_directories = report.unreadable_directories.saturating_add(1);
            return;
        }
    };
    if excluded.is_some_and(|excluded| excluded.contains(&canonical)) {
        return;
    }
    if !visited.insert(canonical.clone()) {
        return;
    }
    if cancellation.is_cancelled() {
        report.cancelled = true;
        return;
    }
    if descriptor
        .executable_paths
        .current()
        .iter()
        .any(|relative| join_portable(&canonical, relative).is_file())
        && validate_installation(&canonical, descriptor)
    {
        report.installations.push(canonical);
        return;
    }
    let entries = match fs::read_dir(&canonical) {
        Ok(entries) => entries,
        Err(_) => {
            report.unreadable_directories = report.unreadable_directories.saturating_add(1);
            return;
        }
    };
    let mut children = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() && !kind.is_symlink())
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        walk_root(
            &child,
            descriptor,
            cancellation,
            excluded,
            depth.saturating_add(1),
            visited,
            report,
        );
        if report.cancelled {
            return;
        }
    }
}

fn join_portable(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(root.to_owned(), |path, part| path.join(part))
}

fn should_skip_directory(path: &Path) -> bool {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    #[cfg(target_os = "windows")]
    {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "$recycle.bin" | "system volume information" | "recovery"
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        matches!(name, "proc" | "sys" | "dev" | "run" | ".Trash" | ".Trashes")
    }
}

fn quick_candidates(descriptor: &GameInstallDescriptor) -> BTreeSet<PathBuf> {
    let mut candidates = BTreeSet::new();
    for steam in steam_roots() {
        candidates.extend(steam_library_candidates(&steam, descriptor));
    }
    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(variable) {
                let root = PathBuf::from(root);
                candidates.insert(root.clone());
                candidates.insert(root.join("Steam/steamapps/common"));
                candidates.insert(root.join("GOG Galaxy/Games"));
            }
        }
        candidates.insert(PathBuf::from(r"C:\GOG Games"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_directory() {
            candidates.insert(home.join("GOG Games"));
            candidates.insert(home.join("Games"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.insert(PathBuf::from("/Applications"));
        if let Some(home) = home_directory() {
            candidates.insert(home.join("Applications"));
        }
    }
    candidates
}

fn steam_roots() -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    #[cfg(target_os = "windows")]
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(root) = std::env::var_os(variable) {
            roots.insert(PathBuf::from(root).join("Steam"));
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(home) = home_directory() {
        roots.insert(home.join(".steam/steam"));
        roots.insert(home.join(".local/share/Steam"));
        roots.insert(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = home_directory() {
        roots.insert(home.join("Library/Application Support/Steam"));
    }
    roots
}

fn steam_library_candidates(
    steam_root: &Path,
    descriptor: &GameInstallDescriptor,
) -> BTreeSet<PathBuf> {
    let mut libraries = BTreeSet::from([steam_root.to_owned()]);
    let manifest = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = fs::read_to_string(manifest) {
        for line in text.lines() {
            let fields = line
                .split('"')
                .enumerate()
                .filter_map(|(index, field)| (index % 2 == 1).then_some(field))
                .collect::<Vec<_>>();
            if fields.len() >= 2
                && (fields[0].eq_ignore_ascii_case("path")
                    || (fields[0].bytes().all(|byte| byte.is_ascii_digit())
                        && looks_like_filesystem_path(fields[1])))
            {
                libraries.insert(PathBuf::from(fields[1].replace(r"\\", r"\")));
            }
        }
    }
    let mut candidates = BTreeSet::new();
    for library in libraries {
        let common = library.join("steamapps/common");
        candidates.insert(common.clone());
        for name in descriptor.installation_directory_names {
            candidates.insert(common.join(name));
        }
    }
    candidates
}

fn looks_like_filesystem_path(value: &str) -> bool {
    value.starts_with('/') || value.starts_with(r"\\") || value.as_bytes().get(1) == Some(&b':')
}

fn local_volume_roots() -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(system_drive) = std::env::var_os("SystemDrive") {
            roots.insert(PathBuf::from(format!(
                "{}\\",
                system_drive.to_string_lossy()
            )));
        }
        if let Ok(output) = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-CimInstance Win32_LogicalDisk | Where-Object { $_.DriveType -in 2,3 } | ForEach-Object { $_.DeviceID }",
            ])
            .output()
            && output.status.success()
        {
            for drive in String::from_utf8_lossy(&output.stdout).lines() {
                let drive = drive.trim();
                if drive.len() == 2 && drive.ends_with(':') {
                    roots.insert(PathBuf::from(format!("{drive}\\")));
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        roots.insert(PathBuf::from("/"));
        if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.len() >= 3 && is_local_filesystem(fields[2]) {
                    roots.insert(PathBuf::from(fields[1].replace(r"\040", " ")));
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = Command::new("/sbin/mount").output()
            && output.status.success()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Some((_, mounted)) = line.split_once(" on ") else {
                    continue;
                };
                let Some((mount, details)) = mounted.rsplit_once(" (") else {
                    continue;
                };
                if !details.starts_with("nfs")
                    && !details.starts_with("smbfs")
                    && !details.starts_with("webdav")
                    && !details.starts_with("devfs")
                {
                    roots.insert(PathBuf::from(mount));
                }
            }
        }
    }
    roots
}

fn excluded_volume_roots() -> BTreeSet<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let mut roots = BTreeSet::new();
        if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.len() >= 3
                    && !is_local_filesystem(fields[2])
                    && let Ok(path) = fs::canonicalize(fields[1].replace(r"\040", " "))
                {
                    roots.insert(path);
                }
            }
        }
        roots
    }
    #[cfg(not(target_os = "linux"))]
    {
        BTreeSet::new()
    }
}

#[cfg(target_os = "linux")]
fn is_local_filesystem(kind: &str) -> bool {
    !matches!(
        kind,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "tmpfs"
            | "cgroup"
            | "cgroup2"
            | "overlay"
            | "squashfs"
            | "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "fuse.sshfs"
            | "iso9660"
    )
}

#[cfg(not(target_os = "windows"))]
fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Outcome retained to ensure an automatic discovery attempt never repeats on every startup.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryOutcome {
    /// Exactly one installation was configured and indexed.
    Configured,
    /// No valid installation was found.
    NotFound,
    /// More than one valid installation requires user selection.
    MultipleCandidates,
    /// Discovery found a source but indexing or persistence failed.
    Failed,
}

/// Per-game machine-local state.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameUserConfiguration {
    /// Whether automatic quick discovery has already run.
    pub auto_discovery_attempted: bool,
    /// Last automatic or manual outcome.
    pub discovery_outcome: Option<DiscoveryOutcome>,
    /// Validated source directory retained for explicit refresh.
    pub vanilla_source: Option<PathBuf>,
    /// Persistent cache consumed by language servers.
    pub vanilla_cache: Option<PathBuf>,
}

/// Versioned user-level configuration shared by editors and projects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfiguration {
    /// On-disk schema version.
    pub version: u32,
    /// Per-game state keyed by stable game ID.
    pub games: BTreeMap<String, GameUserConfiguration>,
}

impl Default for UserConfiguration {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            games: BTreeMap::new(),
        }
    }
}

impl UserConfiguration {
    /// Loads a configuration, returning defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Self, UserConfigError> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(UserConfigError::Io(error)),
        };
        let mut text = String::new();
        file.take(MAX_CONFIG_BYTES + 1).read_to_string(&mut text)?;
        if text.len() as u64 > MAX_CONFIG_BYTES {
            return Err(UserConfigError::TooLarge);
        }
        let config = toml::from_str::<Self>(&text)?;
        if config.version != CONFIG_VERSION {
            return Err(UserConfigError::UnsupportedVersion(config.version));
        }
        Ok(config)
    }

    /// Atomically writes the complete configuration in a deterministic representation.
    pub fn save(&self, path: &Path) -> Result<(), UserConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(UserConfigError::UnsupportedVersion(self.version));
        }
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| {
                UserConfigError::InvalidPath("configuration path has no parent".to_owned())
            })?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| UserConfigError::Io(error.error))?;
        Ok(())
    }
}

/// Platform-appropriate shared configuration and cache locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserPaths {
    /// User configuration file.
    pub config_file: PathBuf,
    /// Root under which per-game cache files are stored.
    pub cache_root: PathBuf,
}

impl UserPaths {
    /// Resolves standard platform locations without creating them.
    pub fn platform() -> Result<Self, UserConfigError> {
        #[cfg(target_os = "windows")]
        {
            let config = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .ok_or(UserConfigError::MissingEnvironment("APPDATA"))?;
            let cache = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or(UserConfigError::MissingEnvironment("LOCALAPPDATA"))?;
            Ok(Self {
                config_file: config.join("ParadoxCode/config.toml"),
                cache_root: cache.join("ParadoxCode/cache"),
            })
        }
        #[cfg(target_os = "macos")]
        {
            let home = home_directory().ok_or(UserConfigError::MissingEnvironment("HOME"))?;
            Ok(Self {
                config_file: home.join("Library/Application Support/ParadoxCode/config.toml"),
                cache_root: home.join("Library/Caches/ParadoxCode"),
            })
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let home = home_directory().ok_or(UserConfigError::MissingEnvironment("HOME"))?;
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"));
            Ok(Self {
                config_file: config.join("paradoxcode/config.toml"),
                cache_root: cache.join("paradoxcode"),
            })
        }
    }

    /// Returns the stable cache location for one game.
    #[must_use]
    pub fn vanilla_cache(&self, game_id: &str) -> PathBuf {
        self.cache_root.join(game_id).join("vanilla.pdxindex")
    }
}

/// User-configuration I/O and validation failures.
#[derive(Debug)]
pub enum UserConfigError {
    /// A required platform environment variable was absent.
    MissingEnvironment(&'static str),
    /// The requested path cannot be represented safely.
    InvalidPath(String),
    /// Configuration exceeds the fixed read limit.
    TooLarge,
    /// Configuration schema is newer or otherwise unsupported.
    UnsupportedVersion(u32),
    /// Filesystem access failed.
    Io(std::io::Error),
    /// TOML decoding failed.
    Decode(toml::de::Error),
    /// TOML encoding failed.
    Encode(toml::ser::Error),
}

impl fmt::Display for UserConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnvironment(name) => {
                write!(formatter, "required environment variable {name} is not set")
            }
            Self::InvalidPath(message) => formatter.write_str(message),
            Self::TooLarge => formatter.write_str("user configuration exceeds 1 MiB"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported user configuration version: {version}"
                )
            }
            Self::Io(error) => write!(formatter, "user configuration I/O error: {error}"),
            Self::Decode(error) => write!(formatter, "invalid user configuration TOML: {error}"),
            Self::Encode(error) => write!(formatter, "cannot encode user configuration: {error}"),
        }
    }
}

impl std::error::Error for UserConfigError {}

impl From<std::io::Error> for UserConfigError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<toml::de::Error> for UserConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::Decode(error)
    }
}

impl From<toml::ser::Error> for UserConfigError {
    fn from(error: toml::ser::Error) -> Self {
        Self::Encode(error)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        DiscoveryDepth, DiscoveryOptions, DiscoveryOutcome, DiscoveryToken, GameInstallDescriptor,
        PlatformExecutablePaths, UserConfiguration, UserPaths, discover_installations,
        validate_installation, validate_installation_for_source,
    };

    const TEST_GAME: GameInstallDescriptor = GameInstallDescriptor {
        game_id: "test",
        display_name: "Test Game",
        executable_paths: PlatformExecutablePaths {
            windows: &["test-game.exe"],
            linux: &["test-game"],
            macos: &["Test.app/Contents/MacOS/test-game"],
        },
        validation_directories: &["common", "events", "missions", "decisions", "localisation"],
        installation_directory_names: &["Test Game"],
    };

    fn fixture() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("library/Test Game");
        for directory in TEST_GAME.validation_directories {
            fs::create_dir_all(root.join(directory)).expect("validation directory");
        }
        let executable = super::join_portable(
            &root,
            TEST_GAME
                .executable_paths
                .current()
                .first()
                .expect("supported test platform"),
        );
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture").expect("executable marker");
        temporary
    }

    #[test]
    fn validation_requires_the_executable_and_every_directory() {
        let temporary = fixture();
        let root = temporary.path().join("library/Test Game");
        assert!(validate_installation(&root, &TEST_GAME));
        fs::remove_dir_all(root.join("missions")).expect("remove required directory");
        assert!(!validate_installation(&root, &TEST_GAME));
    }

    #[test]
    fn explicit_source_validation_accepts_a_foreign_platform_marker() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("foreign-platform");
        for directory in TEST_GAME.validation_directories {
            fs::create_dir_all(root.join(directory)).expect("validation directory");
        }
        let current = TEST_GAME.executable_paths.current();
        let foreign = TEST_GAME
            .executable_paths
            .windows
            .iter()
            .chain(TEST_GAME.executable_paths.linux)
            .chain(TEST_GAME.executable_paths.macos)
            .find(|path| !current.iter().any(|current| *path == current))
            .expect("descriptor has a foreign platform marker");
        let marker = root.join(foreign);
        if let Some(parent) = marker
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).expect("marker parent directory");
        }
        fs::write(marker, b"fixture").expect("foreign executable marker");

        assert!(!validate_installation(&root, &TEST_GAME));
        assert!(validate_installation_for_source(&root, &TEST_GAME));
    }

    #[test]
    fn quick_and_deep_discovery_find_only_valid_roots() {
        let temporary = fixture();
        let installation = fs::canonicalize(temporary.path().join("library/Test Game"))
            .expect("canonical installation");
        let quick = discover_installations(
            &TEST_GAME,
            &DiscoveryOptions {
                roots: vec![temporary.path().join("library")],
                include_platform_locations: false,
                ..DiscoveryOptions::default()
            },
            &DiscoveryToken::new(),
        );
        assert_eq!(quick.installations, vec![installation.clone()]);

        let deep = discover_installations(
            &TEST_GAME,
            &DiscoveryOptions {
                depth: DiscoveryDepth::Deep,
                roots: vec![temporary.path().to_owned()],
                include_platform_locations: false,
            },
            &DiscoveryToken::new(),
        );
        assert_eq!(deep.installations, vec![installation]);
    }

    #[cfg(unix)]
    #[test]
    fn deep_discovery_does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = fixture();
        let outside = temporary.path().join("library");
        let scan = temporary.path().join("scan");
        fs::create_dir_all(&scan).expect("scan directory");
        symlink(outside, scan.join("linked-library")).expect("directory symlink");
        let report = discover_installations(
            &TEST_GAME,
            &DiscoveryOptions {
                depth: DiscoveryDepth::Deep,
                roots: vec![scan],
                include_platform_locations: false,
            },
            &DiscoveryToken::new(),
        );
        assert!(report.installations.is_empty());
    }

    #[test]
    fn user_configuration_round_trips_attempt_and_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = UserPaths {
            config_file: temporary.path().join("config/config.toml"),
            cache_root: temporary.path().join("cache"),
        };
        let mut configuration = UserConfiguration::default();
        let game = configuration.games.entry("test".to_owned()).or_default();
        game.auto_discovery_attempted = true;
        game.discovery_outcome = Some(DiscoveryOutcome::Configured);
        game.vanilla_source = Some(temporary.path().join("source"));
        game.vanilla_cache = Some(paths.vanilla_cache("test"));
        configuration
            .save(&paths.config_file)
            .expect("save configuration");
        assert_eq!(
            UserConfiguration::load(&paths.config_file).expect("load configuration"),
            configuration
        );
    }

    #[test]
    fn steam_discovery_accepts_legacy_numeric_library_entries() {
        let temporary = fixture();
        let steam = temporary.path().join("steam");
        let library = temporary.path().join("legacy-library");
        let installation = library.join("steamapps/common/Test Game");
        for directory in TEST_GAME.validation_directories {
            fs::create_dir_all(installation.join(directory)).expect("validation directory");
        }
        let executable = super::join_portable(
            &installation,
            TEST_GAME
                .executable_paths
                .current()
                .first()
                .expect("supported test platform"),
        );
        fs::create_dir_all(executable.parent().expect("executable parent"))
            .expect("executable parent directory");
        fs::write(executable, b"fixture").expect("executable marker");
        fs::create_dir_all(steam.join("steamapps")).expect("Steam metadata directory");
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"LibraryFolders\"\n{{\n  \"1\" \"{}\"\n}}\n",
                library.display()
            ),
        )
        .expect("legacy library metadata");
        let candidates = super::steam_library_candidates(&steam, &TEST_GAME);
        assert!(candidates.contains(&installation));
    }
}
