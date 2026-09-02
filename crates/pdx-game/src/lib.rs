//! Game installation discovery, user-local configuration, and EU4 game profile.
//!
//! This crate provides platform-agnostic installation search and the EU4 semantic profile
//! consumed by the language server. The installation discovery logic is game-agnostic; the
//! EU4 profile lives in the `eu4` module.

pub mod eu4;

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

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
    /// Steam application identity used to read per-library install manifests.
    pub steam_app_id: Option<u32>,
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

    /// Every platform marker, used for explicit sources and cross-platform WSL mounts.
    fn any_platform(self) -> impl Iterator<Item = &'static &'static str> {
        self.windows.iter().chain(self.linux).chain(self.macos)
    }
}

impl GameInstallDescriptor {
    /// Whether a launcher-reported executable (a registry value or manifest entry) names
    /// one of this game's marker executables. Comparison uses only the file name.
    #[must_use]
    fn matches_executable_file(&self, reported: &str) -> bool {
        let Some(name) = Path::new(reported).file_name().and_then(OsStr::to_str) else {
            return false;
        };
        self.executable_paths.any_platform().any(|marker| {
            Path::new(marker)
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|marker| marker.eq_ignore_ascii_case(name))
        })
    }
}

/// Inputs for one installation search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryOptions {
    /// Explicit roots or candidate directories supplied by the user.
    pub roots: Vec<PathBuf>,
    /// Whether launcher metadata and common platform locations should be included.
    pub include_platform_locations: bool,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            include_platform_locations: true,
        }
    }
}

/// Cooperative cancellation for discovery scans.
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

/// Provenance of one installation candidate, ordered by selection priority.
///
/// Launcher metadata is preferred over guessed common locations because it names the
/// exact directory the launcher installed to; caller-supplied roots express explicit
/// user intent and win over everything.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CandidateSource {
    /// A caller-supplied root (`--root`, `--source`, or an editor-guided directory).
    Explicit,
    /// A Steam library app manifest resolved the exact install directory.
    SteamManifest,
    /// An Epic Games launcher manifest resolved the install directory.
    Epic,
    /// A GOG registry entry resolved the install directory.
    Gog,
    /// A Steam library location came from launcher metadata; the directory was guessed.
    SteamLibrary,
    /// A common-location or mounted-volume guess.
    Guessed,
}

impl CandidateSource {
    /// Stable label persisted in the user configuration as `resolved_via`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::SteamManifest => "steam-appmanifest",
            Self::Epic => "epic-manifest",
            Self::Gog => "gog-registry",
            Self::SteamLibrary => "steam-library",
            Self::Guessed => "common-location",
        }
    }
}

/// One validated installation with the provenance needed for deterministic selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredInstallation {
    /// Canonical, validated installation root.
    pub path: PathBuf,
    /// How the candidate was located.
    pub source: CandidateSource,
    /// Launcher-reported build identifier (the Steam `buildid`), when available.
    pub game_build: Option<String>,
    /// Modification time of the newest present marker executable.
    pub marker_modified: Option<SystemTime>,
}

/// Deterministic outcome of [`select_installation`].
#[derive(Clone, Debug)]
pub struct InstallationSelection<'a> {
    /// The installation to configure.
    pub selected: &'a DiscoveredInstallation,
    /// Validated alternatives in report order.
    pub alternatives: Vec<&'a DiscoveredInstallation>,
}

/// Selects one installation deterministically when several validated candidates exist.
///
/// Caller-supplied roots win over launcher metadata, which wins over common-location
/// guesses. Within one source tier the most recently updated marker executable is
/// preferred, and paths break remaining ties. Any validated installation of the same
/// game version can serve as the Vanilla source, so resolution never blocks setup on
/// user interaction; alternatives are reported so the user can override with
/// `--source` or an editor setting.
#[must_use]
pub fn select_installation(
    installations: &[DiscoveredInstallation],
) -> Option<InstallationSelection<'_>> {
    let selected = installations.iter().min_by_key(|installation| {
        (
            installation.source,
            Reverse(
                installation
                    .marker_modified
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            ),
            installation.path.clone(),
        )
    })?;
    Some(InstallationSelection {
        selected,
        alternatives: installations
            .iter()
            .filter(|candidate| !std::ptr::eq(*candidate, selected))
            .collect(),
    })
}

/// Result of scanning for one supported game.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryReport {
    /// Canonical, validated installations in deterministic order.
    pub installations: Vec<DiscoveredInstallation>,
    /// Whether the caller cancelled the scan.
    pub cancelled: bool,
}

/// One unvalidated candidate directory with its provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    path: PathBuf,
    source: CandidateSource,
    /// Whether a foreign-platform executable marker may satisfy validation (WSL mounts).
    cross_platform: bool,
    game_build: Option<String>,
}

impl Candidate {
    fn new(path: PathBuf, source: CandidateSource) -> Self {
        Self {
            path,
            source,
            cross_platform: false,
            game_build: None,
        }
    }
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

/// Finds validated installations using launcher metadata and common locations.
///
/// The scan never walks disks: every candidate comes from launcher metadata (Steam
/// libraries and app manifests, Epic manifests, GOG registry entries), fixed common
/// locations, or caller-supplied roots. Metadata is only a hint; every candidate must
/// still pass full installation validation before entering the report.
#[must_use]
pub fn discover_installations(
    descriptor: &GameInstallDescriptor,
    options: &DiscoveryOptions,
    cancellation: &DiscoveryToken,
) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    // The same path can arrive from several sources; keep the strongest provenance so
    // deterministic selection prefers launcher metadata over guesses.
    let mut candidates: BTreeMap<PathBuf, Candidate> = BTreeMap::new();
    for root in &options.roots {
        merge_candidate(
            &mut candidates,
            Candidate::new(root.clone(), CandidateSource::Explicit),
        );
    }
    if options.include_platform_locations {
        for candidate in platform_candidates(descriptor) {
            merge_candidate(&mut candidates, candidate);
        }
    }
    for (_, candidate) in candidates {
        if cancellation.is_cancelled() {
            report.cancelled = true;
            break;
        }
        add_candidate(&candidate, descriptor, &mut report.installations);
    }
    report
        .installations
        .sort_by(|left, right| (&left.path, left.source).cmp(&(&right.path, right.source)));
    report
        .installations
        .dedup_by(|left, right| left.path == right.path);
    report
}

/// Inserts a candidate, replacing a weaker provenance for the same path.
fn merge_candidate(candidates: &mut BTreeMap<PathBuf, Candidate>, candidate: Candidate) {
    match candidates.entry(candidate.path.clone()) {
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            if candidate.source < existing.source {
                *existing = candidate;
            }
        }
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(candidate);
        }
    }
}

fn add_candidate(
    candidate: &Candidate,
    descriptor: &GameInstallDescriptor,
    installations: &mut Vec<DiscoveredInstallation>,
) {
    if let Some(installation) = validated_installation(&candidate.path, descriptor, candidate) {
        installations.push(installation);
        return;
    }
    for name in descriptor.installation_directory_names {
        let child = candidate.path.join(name);
        if let Some(installation) = validated_installation(&child, descriptor, candidate) {
            installations.push(installation);
        }
    }
}

/// Validates one directory, recording provenance and marker mtime on success.
fn validated_installation(
    root: &Path,
    descriptor: &GameInstallDescriptor,
    candidate: &Candidate,
) -> Option<DiscoveredInstallation> {
    let executables: Vec<&'static &'static str> = if candidate.cross_platform {
        descriptor.executable_paths.any_platform().collect()
    } else {
        descriptor.executable_paths.current().iter().collect()
    };
    if !validate_installation_with_executables(root, descriptor, executables.iter().copied()) {
        return None;
    }
    Some(DiscoveredInstallation {
        path: canonicalize_clean(root)?,
        source: candidate.source,
        game_build: candidate.game_build.clone(),
        marker_modified: marker_modified(root, &executables),
    })
}

/// Modification time of the newest present marker executable.
fn marker_modified(root: &Path, executables: &[&'static &'static str]) -> Option<SystemTime> {
    executables
        .iter()
        .filter(|relative| join_portable(root, relative).is_file())
        .filter_map(|relative| fs::metadata(join_portable(root, relative)).ok())
        .filter_map(|metadata| metadata.modified().ok())
        .max()
}

/// Collects launcher-metadata and common-location candidates for one game.
fn platform_candidates(descriptor: &GameInstallDescriptor) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for root in steam_roots() {
        candidates.extend(steam_library_candidates(&root, descriptor));
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    candidates.extend(epic_candidates(descriptor));
    #[cfg(target_os = "windows")]
    candidates.extend(gog_candidates(descriptor));
    for path in guessed_locations() {
        candidates.push(Candidate::new(path, CandidateSource::Guessed));
    }
    #[cfg(target_os = "linux")]
    candidates.extend(wsl_candidates(descriptor));
    candidates
}

fn steam_roots() -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    #[cfg(target_os = "windows")]
    {
        // Steam installs to arbitrary drives; the registry is the authoritative location.
        if let Some(path) = registry_string_value(r"HKCU\Software\Valve\Steam", "SteamPath")
            .or_else(|| {
                registry_string_value(r"HKLM\SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath")
            })
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
        {
            roots.insert(path);
        }
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(root) = std::env::var_os(variable) {
                roots.insert(PathBuf::from(root).join("Steam"));
            }
        }
        for drive in probed_drive_roots() {
            roots.insert(drive.join("SteamLibrary"));
            roots.insert(drive.join("Steam"));
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

/// Enumerates present drive letters by direct probing, without spawning a process.
#[cfg(target_os = "windows")]
fn probed_drive_roots() -> Vec<PathBuf> {
    (b'C'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", char::from(letter))))
        .filter(|root| root.is_dir())
        .collect()
}

/// Common fixed locations and per-drive well-known folder names.
fn guessed_locations() -> BTreeSet<PathBuf> {
    let mut locations = BTreeSet::new();
    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = std::env::var_os(variable) {
                let root = PathBuf::from(root);
                locations.insert(root.clone());
                locations.insert(root.join("GOG Galaxy/Games"));
                locations.insert(root.join("Epic Games"));
            }
        }
        locations.insert(PathBuf::from(r"C:\GOG Games"));
        for drive in probed_drive_roots() {
            for name in ["GOG Games", "Games", "Epic Games"] {
                locations.insert(drive.join(name));
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_directory() {
            locations.insert(home.join("GOG Games"));
            locations.insert(home.join("Games"));
        }
        for mount in media_roots() {
            locations.insert(mount);
        }
    }
    #[cfg(target_os = "macos")]
    {
        locations.insert(PathBuf::from("/Applications"));
        if let Some(home) = home_directory() {
            locations.insert(home.join("Applications"));
            locations.insert(home.join("Games"));
        }
        for volume in volume_roots() {
            locations.insert(volume);
        }
    }
    locations
}

/// Mounted volumes under `/Volumes`, excluding the live system volume.
#[cfg(target_os = "macos")]
fn volume_roots() -> Vec<PathBuf> {
    fs::read_dir("/Volumes")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
                .filter(|path| path.file_name().and_then(OsStr::to_str) != Some("Macintosh HD"))
                .collect()
        })
        .unwrap_or_default()
}

/// Removable-media mount points: `/media/<user>/<volume>` and `/run/media/<user>/<volume>`.
#[cfg(target_os = "linux")]
fn media_roots() -> Vec<PathBuf> {
    fn children(root: &str) -> Vec<PathBuf> {
        fs::read_dir(root)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn nested(roots: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut volumes = Vec::new();
        for user in roots {
            let mut added = false;
            for volume in fs::read_dir(&user)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .map(|entry| entry.path())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
            {
                volumes.push(volume);
                added = true;
            }
            if !added {
                volumes.push(user);
            }
        }
        volumes
    }
    let mut roots = nested(children("/media"));
    roots.extend(children("/run/media").into_iter().flat_map(|user| {
        fs::read_dir(&user)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }));
    roots
}

/// Windows drive mounts (`/mnt/<letter>`) visible from WSL.
#[cfg(target_os = "linux")]
fn wsl_drive_mounts() -> Vec<PathBuf> {
    fs::read_dir("/mnt")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name().to_string_lossy().len() == 1
                        && entry.file_type().is_ok_and(|kind| kind.is_dir())
                })
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether this Linux environment is the Windows Subsystem for Linux.
#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    fs::read_to_string("/proc/version")
        .map(|version| version.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Cross-platform candidates from Windows Steam libraries mounted under WSL.
///
/// The mounted installations carry a Windows executable marker, so validation accepts
/// any platform's marker for these candidates only; ordinary locations stay on the
/// current-platform contract.
#[cfg(target_os = "linux")]
fn wsl_candidates(descriptor: &GameInstallDescriptor) -> Vec<Candidate> {
    if !is_wsl() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for mount in wsl_drive_mounts() {
        for relative in ["Program Files (x86)/Steam", "Program Files/Steam", "Steam"] {
            let root = mount.join(relative);
            if root.is_dir() {
                candidates.extend(steam_library_candidates(&root, descriptor).into_iter().map(
                    |mut candidate| {
                        candidate.cross_platform = true;
                        candidate
                    },
                ));
            }
        }
    }
    candidates
}

/// Builds candidates for one Steam root, preferring app manifests over name guesses.
fn steam_library_candidates(
    steam_root: &Path,
    descriptor: &GameInstallDescriptor,
) -> Vec<Candidate> {
    let mut libraries = vec![steam_root.to_owned()];
    if let Ok(text) = fs::read_to_string(steam_root.join("steamapps/libraryfolders.vdf")) {
        libraries.extend(parse_steam_libraries(&text));
    }
    let mut candidates = Vec::new();
    for library in libraries {
        let common = library.join("steamapps/common");
        match steam_app_manifest(&library, descriptor) {
            // The manifest is authoritative: it reports whether the library actually
            // holds a complete installation and which directory it lives in.
            Some(SteamAppManifestState::Installed {
                installdir,
                buildid,
            }) => {
                candidates.push(Candidate {
                    path: common.join(installdir),
                    source: CandidateSource::SteamManifest,
                    cross_platform: false,
                    game_build: buildid,
                });
            }
            // Recorded but not fully installed; this library is skipped entirely.
            Some(SteamAppManifestState::NotInstalled) => {}
            // No manifest knowledge for this library; fall back to directory names.
            None => {
                candidates.push(Candidate::new(
                    common.clone(),
                    CandidateSource::SteamLibrary,
                ));
                for name in descriptor.installation_directory_names {
                    candidates.push(Candidate::new(
                        common.join(name),
                        CandidateSource::SteamLibrary,
                    ));
                }
            }
        }
    }
    candidates
}

enum SteamAppManifestState {
    Installed {
        installdir: String,
        buildid: Option<String>,
    },
    NotInstalled,
}

/// Reads the game's `appmanifest_<id>.acf` in one library, when present.
fn steam_app_manifest(
    library: &Path,
    descriptor: &GameInstallDescriptor,
) -> Option<SteamAppManifestState> {
    let app_id = descriptor.steam_app_id?;
    let text =
        fs::read_to_string(library.join(format!("steamapps/appmanifest_{app_id}.acf"))).ok()?;
    let manifest = parse_steam_app_manifest(&text)?;
    if manifest.state_flags & 4 == 0 {
        return Some(SteamAppManifestState::NotInstalled);
    }
    Some(SteamAppManifestState::Installed {
        installdir: manifest.installdir,
        buildid: manifest.buildid,
    })
}

struct SteamAppManifest {
    installdir: String,
    state_flags: u32,
    buildid: Option<String>,
}

/// Minimal `appmanifest_<id>.acf` reader covering quoted key/value lines only.
fn parse_steam_app_manifest(text: &str) -> Option<SteamAppManifest> {
    let mut installdir = None;
    let mut state_flags = None;
    let mut buildid = None;
    for line in text.lines() {
        let fields = quoted_fields(line);
        if fields.len() == 2 {
            match fields[0] {
                "installdir" => installdir = Some(fields[1].replace(r"\\", r"\")),
                "StateFlags" => state_flags = fields[1].parse().ok(),
                "buildid" => buildid = Some(fields[1].to_owned()),
                _ => {}
            }
        }
    }
    Some(SteamAppManifest {
        installdir: installdir?,
        state_flags: state_flags?,
        buildid,
    })
}

/// Extracts additional library paths from `libraryfolders.vdf` text, accepting both the
/// current `path` fields and legacy numeric entries.
fn parse_steam_libraries(text: &str) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for line in text.lines() {
        let fields = quoted_fields(line);
        if fields.len() >= 2
            && (fields[0].eq_ignore_ascii_case("path")
                || (fields[0].bytes().all(|byte| byte.is_ascii_digit())
                    && looks_like_filesystem_path(fields[1])))
        {
            libraries.push(PathBuf::from(fields[1].replace(r"\\", r"\")));
        }
    }
    libraries
}

/// Splits one line into its double-quoted fields.
fn quoted_fields(line: &str) -> Vec<&str> {
    line.split('"')
        .enumerate()
        .filter_map(|(index, field)| (index % 2 == 1).then_some(field))
        .collect()
}

/// Resolves installations from Epic Games launcher manifests.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn epic_candidates(descriptor: &GameInstallDescriptor) -> Vec<Candidate> {
    #[cfg(target_os = "windows")]
    let Some(base) = std::env::var_os("ProgramData").map(PathBuf::from) else {
        return Vec::new();
    };
    #[cfg(target_os = "macos")]
    let Some(base) = home_directory().map(|home| home.join("Library/Application Support/Epic"))
    else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(base.join("EpicGamesLauncher/Data/Manifests")) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let is_manifest = entry
            .path()
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("item"));
        if !is_manifest {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(location) = epic_install_location(&text, descriptor) {
            candidates.push(Candidate::new(location, CandidateSource::Epic));
        }
    }
    candidates
}

/// Extracts the install location from one Epic `.item` manifest when it matches the game.
#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn epic_install_location(manifest: &str, descriptor: &GameInstallDescriptor) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(manifest).ok()?;
    let matches_game = value
        .get("LaunchExecutable")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|executable| descriptor.matches_executable_file(executable))
        || value
            .get("DisplayName")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(descriptor.display_name));
    if !matches_game {
        return None;
    }
    value
        .get("InstallLocation")
        .and_then(serde_json::Value::as_str)
        .filter(|location| !location.is_empty())
        .map(PathBuf::from)
}

/// Queries GOG's per-game registry tree for this game's install directory.
#[cfg(target_os = "windows")]
fn gog_candidates(descriptor: &GameInstallDescriptor) -> Vec<Candidate> {
    let Some(output) = Command::new("reg.exe")
        .args(["query", r"HKLM\SOFTWARE\WOW6432Node\GOG.com\Games", "/s"])
        .output()
        .ok()
        .filter(|output| output.status.success())
    else {
        return Vec::new();
    };
    gog_installs_from_registry(&String::from_utf8_lossy(&output.stdout), descriptor)
}

/// Extracts this game's install roots from GOG `reg query ... /s` output.
#[cfg(any(target_os = "windows", test))]
fn gog_installs_from_registry(text: &str, descriptor: &GameInstallDescriptor) -> Vec<Candidate> {
    parse_registry_tree(text)
        .into_iter()
        .filter_map(|(_subkey, values)| {
            let value = |name: &str| {
                values
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.as_str())
            };
            let exe = value("exe")?;
            if !descriptor.matches_executable_file(exe) {
                return None;
            }
            let root = value("path")
                .map(PathBuf::from)
                .or_else(|| Path::new(exe).parent().map(Path::to_path_buf))?;
            Some(Candidate::new(root, CandidateSource::Gog))
        })
        .collect()
}

/// Reads one `REG_SZ` value through `reg.exe`. Only used during one-time discovery.
#[cfg(target_os = "windows")]
fn registry_string_value(key: &str, value: &str) -> Option<String> {
    let output = Command::new("reg.exe")
        .args(["query", key, "/v", value])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_registry_value(&String::from_utf8_lossy(&output.stdout), value)
}

/// Extracts one `name    REG_SZ    value` entry from `reg.exe query` output.
#[cfg(any(target_os = "windows", test))]
fn parse_registry_value(text: &str, value: &str) -> Option<String> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 3
            && fields[0].eq_ignore_ascii_case(value)
            && fields[1].eq_ignore_ascii_case("REG_SZ")
        {
            let start = line.find(fields[1]).map_or(0, |at| at + fields[1].len());
            return Some(line[start..].trim().to_owned());
        }
    }
    None
}

/// Extracts per-subkey value maps from `reg.exe query ... /s` output.
#[cfg(any(target_os = "windows", test))]
fn parse_registry_tree(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut entries: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            entries.push((line.trim().to_owned(), Vec::new()));
            continue;
        }
        let Some((_subkey, values)) = entries.last_mut() else {
            continue;
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 3 && fields[1].eq_ignore_ascii_case("REG_SZ") {
            let start = line.find(fields[1]).map_or(0, |at| at + fields[1].len());
            values.push((fields[0].to_owned(), line[start..].trim().to_owned()));
        }
    }
    entries
}

fn looks_like_filesystem_path(value: &str) -> bool {
    value.starts_with('/') || value.starts_with(r"\\") || value.as_bytes().get(1) == Some(&b':')
}

fn join_portable(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|part| !part.is_empty())
        .fold(root.to_owned(), |path, part| path.join(part))
}

/// Canonicalizes a path and strips Windows verbatim prefixes from the result.
fn canonicalize_clean(path: &Path) -> Option<PathBuf> {
    fs::canonicalize(path).ok().map(portable_path)
}

/// Strips Windows verbatim (`\\?\`) prefixes so persisted and displayed paths stay portable.
#[must_use]
pub fn portable_path(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest.to_owned());
        }
    }
    path
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
    /// More than one valid installation was found.
    ///
    /// Discovery now resolves multiple candidates deterministically (see
    /// [`select_installation`]); this value remains readable so configurations recorded
    /// by older versions keep loading.
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
    /// Provenance of the configured installation; one of the `CandidateSource` labels.
    pub resolved_via: Option<String>,
    /// Launcher-reported build identifier of the configured installation.
    pub game_build: Option<String>,
}

/// Versioned user-level configuration for local discovery and cache state.
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

/// Platform-appropriate user-level configuration and cache locations.
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

    /// Returns the stable Vanilla index cache location for one game.
    #[must_use]
    pub fn vanilla_cache(&self, game_id: &str) -> PathBuf {
        self.cache_root.join(game_id).join("vanilla.pdxindex")
    }

    /// Returns the user-local compiled first-party rules artifact location for one game.
    #[must_use]
    pub fn rules_cache(&self, game_id: &str) -> PathBuf {
        self.cache_root.join(game_id).join("rules.pdxrules")
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
        CandidateSource, DiscoveredInstallation, DiscoveryOptions, DiscoveryOutcome,
        DiscoveryToken, GameInstallDescriptor, PlatformExecutablePaths, UserConfiguration,
        UserPaths, discover_installations, select_installation, validate_installation,
        validate_installation_for_source,
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
        steam_app_id: Some(42),
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
    fn discovery_finds_only_valid_roots() {
        let temporary = fixture();
        let installation = super::portable_path(
            fs::canonicalize(temporary.path().join("library/Test Game"))
                .expect("canonical installation"),
        );
        let report = discover_installations(
            &TEST_GAME,
            &DiscoveryOptions {
                roots: vec![temporary.path().join("library")],
                include_platform_locations: false,
            },
            &DiscoveryToken::new(),
        );
        let found = report
            .installations
            .iter()
            .map(|installation| installation.path.clone())
            .collect::<Vec<_>>();
        // Discovered paths are stored without Windows verbatim prefixes so they stay
        // portable across configuration files, messages, and cross-platform mounts.
        assert_eq!(found, vec![installation]);
        assert_eq!(report.installations[0].source, CandidateSource::Explicit);
        assert!(report.installations[0].marker_modified.is_some());
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
        game.resolved_via = Some("steam-appmanifest".to_owned());
        game.game_build = Some("1708067".to_owned());
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
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.path == installation)
        );
    }

    #[test]
    fn steam_libraries_are_parsed_from_current_vdf_path_fields() {
        let text = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"E:\\\\Games\\\\Steam\"\n\t}\n}\n";
        let libraries = super::parse_steam_libraries(text);
        assert_eq!(
            libraries,
            vec![
                std::path::PathBuf::from(r"D:\SteamLibrary"),
                std::path::PathBuf::from(r"E:\Games\Steam"),
            ]
        );
    }

    fn write_steam_installation(root: &std::path::Path) {
        let installation = root.join("steamapps/common/Test Game");
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
    }

    #[test]
    fn an_installed_app_manifest_resolves_the_exact_steam_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let steam = temporary.path().join("steam");
        write_steam_installation(&steam);
        fs::create_dir_all(steam.join("steamapps")).expect("Steam metadata directory");
        fs::write(
            steam.join("steamapps/appmanifest_42.acf"),
            "\"AppState\"\n{\n\t\"installdir\"\t\"Test Game\"\n\t\"StateFlags\"\t\"4\"\n\t\"buildid\"\t\"1708067\"\n}\n",
        )
        .expect("app manifest");

        let candidates = super::steam_library_candidates(&steam, &TEST_GAME);
        let [candidate] = candidates.as_slice() else {
            panic!("the manifest must resolve exactly one candidate: {candidates:?}");
        };
        assert_eq!(candidate.path, steam.join("steamapps/common/Test Game"));
        assert_eq!(candidate.source, CandidateSource::SteamManifest);
        assert_eq!(candidate.game_build.as_deref(), Some("1708067"));
    }

    #[test]
    fn a_not_fully_installed_app_manifest_skips_the_library() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let steam = temporary.path().join("steam");
        write_steam_installation(&steam);
        fs::create_dir_all(steam.join("steamapps")).expect("Steam metadata directory");
        // StateFlags 2 (update required, not fully installed) must suppress the
        // directory-name guesses that would otherwise apply without a manifest.
        fs::write(
            steam.join("steamapps/appmanifest_42.acf"),
            "\"AppState\"\n{\n\t\"installdir\"\t\"Test Game\"\n\t\"StateFlags\"\t\"2\"\n}\n",
        )
        .expect("app manifest");

        let candidates = super::steam_library_candidates(&steam, &TEST_GAME);
        assert!(candidates.is_empty(), "{candidates:?}");
    }

    #[test]
    fn registry_value_output_is_parsed_with_spaces_preserved() {
        let output = "\nHKEY_CURRENT_USER\\Software\\Valve\\Steam\n    SteamPath    REG_SZ    C:\\Program Files (x86)\\Steam\n\n";
        let value = super::parse_registry_value(output, "SteamPath").expect("SteamPath value");
        assert_eq!(value, r"C:\Program Files (x86)\Steam");
        assert!(super::parse_registry_value(output, "Absent").is_none());
    }

    #[test]
    fn gog_registry_entries_are_matched_by_executable_name() {
        let output = "HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\GOG.com\\Games\\1207658924\n\
            gameID    REG_SZ    1207658924\n\
            name    REG_SZ    Some Other Game\n\
            path    REG_SZ    D:\\GOG Games\\Some Other Game\n\
            exe    REG_SZ    D:\\GOG Games\\Some Other Game\\other.exe\n\
            HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\GOG.com\\Games\\1444474061\n\
            gameID    REG_SZ    1444474061\n\
            name    REG_SZ    Test Game\n\
            path    REG_SZ    D:\\GOG Games\\Test Game\n\
            exe    REG_SZ    D:\\GOG Games\\Test Game\\TEST-GAME.EXE\n";
        let candidates = super::gog_installs_from_registry(output, &TEST_GAME);
        let [candidate] = candidates.as_slice() else {
            panic!("exactly the matching entry resolves: {candidates:?}");
        };
        assert_eq!(
            candidate.path,
            std::path::PathBuf::from(r"D:\GOG Games\Test Game")
        );
        assert_eq!(candidate.source, CandidateSource::Gog);
    }

    #[test]
    fn epic_manifests_are_matched_by_launch_executable() {
        let matched = r#"{"DisplayName":"Test Game","LaunchExecutable":"TestFolder/test-game.exe","InstallLocation":"C:/Epic Games/TestFolder"}"#;
        assert_eq!(
            super::epic_install_location(matched, &TEST_GAME),
            Some(std::path::PathBuf::from("C:/Epic Games/TestFolder"))
        );
        let display_only =
            r#"{"DisplayName":"Test Game","InstallLocation":"C:/Epic Games/TestFolder"}"#;
        assert_eq!(
            super::epic_install_location(display_only, &TEST_GAME),
            Some(std::path::PathBuf::from("C:/Epic Games/TestFolder"))
        );
        let other = r#"{"DisplayName":"Other Game","LaunchExecutable":"other.exe","InstallLocation":"C:/Epic Games/Other"}"#;
        assert_eq!(super::epic_install_location(other, &TEST_GAME), None);
    }

    #[test]
    fn selection_prefers_explicit_then_metadata_then_freshness() {
        let explicit = DiscoveredInstallation {
            path: std::path::PathBuf::from("/explicit"),
            source: CandidateSource::Explicit,
            game_build: None,
            marker_modified: Some(std::time::SystemTime::UNIX_EPOCH),
        };
        let fresh_manifest = DiscoveredInstallation {
            path: std::path::PathBuf::from("/fresh"),
            source: CandidateSource::SteamManifest,
            game_build: None,
            marker_modified: Some(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(100),
            ),
        };
        let stale_manifest = DiscoveredInstallation {
            path: std::path::PathBuf::from("/stale"),
            source: CandidateSource::SteamManifest,
            game_build: None,
            marker_modified: Some(std::time::SystemTime::UNIX_EPOCH),
        };
        let guess = DiscoveredInstallation {
            path: std::path::PathBuf::from("/guess"),
            source: CandidateSource::Guessed,
            game_build: None,
            marker_modified: Some(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(9_999),
            ),
        };
        let all = [
            guess.clone(),
            stale_manifest.clone(),
            fresh_manifest.clone(),
            explicit.clone(),
        ];
        let selection = select_installation(&all).expect("a selection exists");
        assert_eq!(selection.selected.path, explicit.path);
        assert_eq!(selection.alternatives.len(), 3);

        let metadata_only_set = [guess, stale_manifest, fresh_manifest.clone()];
        let metadata_only = select_installation(&metadata_only_set).expect("a selection exists");
        assert_eq!(metadata_only.selected.path, fresh_manifest.path);
        assert_eq!(metadata_only.alternatives.len(), 2);

        assert!(select_installation(&[]).is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn verbatim_prefixes_are_stripped_from_canonical_paths() {
        assert_eq!(
            super::portable_path(std::path::PathBuf::from(r"\\?\C:\Games\Test Game")),
            std::path::PathBuf::from(r"C:\Games\Test Game")
        );
        assert_eq!(
            super::portable_path(std::path::PathBuf::from(r"\\?\UNC\server\share\Test Game")),
            std::path::PathBuf::from(r"\\server\share\Test Game")
        );
        assert_eq!(
            super::portable_path(std::path::PathBuf::from(r"C:\Games\Test Game")),
            std::path::PathBuf::from(r"C:\Games\Test Game")
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn canonical_paths_are_returned_unchanged_off_windows() {
        assert_eq!(
            super::portable_path(std::path::PathBuf::from("/games/test")),
            std::path::PathBuf::from("/games/test")
        );
    }
}
