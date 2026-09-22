//! `TextureCatalog` ground-truth tests: harvest, multi-root resolution, DLC
//! pack-relative keys, extension-drift fallback, and watcher-driven rebuilds.

use super::*;
use text::AbsPath;

fn source_root(id: u32, kind: SourceRootKind, path: &std::path::Path) -> SourceRoot {
    SourceRoot::new(SourceRootId::new(id), kind, AbsPath::normalize(path))
}

#[test]
fn catalog_normalizes_spellings_and_prioritizes_earlier_roots() {
    let mod_root = temp_root("mod");
    let game_root = temp_root("game");
    fs::create_dir_all(mod_root.join("gfx/interface")).expect("mod dir");
    fs::write(mod_root.join("gfx/interface/shared.dds"), b"").expect("mod texture");
    fs::create_dir_all(game_root.join("gfx/interface")).expect("game dir");
    fs::write(game_root.join("gfx/interface/shared.dds"), b"").expect("game texture");
    fs::write(game_root.join("gfx/interface/only_game.tga"), b"").expect("game texture");

    let roots = [
        source_root(1, SourceRootKind::Project, &mod_root),
        source_root(2, SourceRootKind::Vanilla, &game_root),
    ];
    let catalog = TextureCatalog::build(&roots);

    // Case, backslashes, doubled separators, and a leading slash all fold to the
    // same normalized key; the mod root wins over the game root.
    let resolution = catalog
        .resolve(&roots, "\"\\\\gfx//Interface\\SHARED.dds\"")
        .expect("normalized hit");
    assert_eq!(resolution.hit.root_kind, SourceRootKind::Project);
    assert!(!resolution.extension_fallback);

    let game_only = catalog
        .resolve(&roots, "gfx/interface/only_game.tga")
        .expect("game hit");
    assert_eq!(game_only.hit.root_kind, SourceRootKind::Vanilla);
}

#[test]
fn catalog_accepts_extension_drift_between_tga_and_dds() {
    let game_root = temp_root("drift");
    fs::create_dir_all(game_root.join("gfx/interface")).expect("dir");
    fs::write(game_root.join("gfx/interface/shipped.dds"), b"").expect("dds");

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);

    let resolution = catalog
        .resolve(&roots, "gfx/interface/shipped.tga")
        .expect("drift fallback");
    assert!(resolution.extension_fallback);
    assert!(
        resolution
            .hit
            .path
            .as_path()
            .to_string_lossy()
            .ends_with("shipped.dds")
    );
    // The drift works in both directions.
    fs::write(game_root.join("gfx/interface/other.tga"), b"").expect("tga");
    let catalog = TextureCatalog::build(&roots);
    let reverse = catalog
        .resolve(&roots, "gfx/interface/other.dds")
        .expect("reverse drift");
    assert!(reverse.extension_fallback);
}

#[test]
fn catalog_keys_dlc_pack_entries_pack_relative() {
    let game_root = temp_root("dlc");
    let pack = game_root.join("builtin_dlc/dlc_immersion/gfx");
    fs::create_dir_all(&pack).expect("pack dir");
    fs::write(pack.join("pack_only.dds"), b"").expect("pack texture");

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);

    // The pack's own .gfx files reference the texture without the pack prefix.
    let resolution = catalog
        .resolve(&roots, "gfx/pack_only.dds")
        .expect("pack-relative hit");
    assert!(resolution.hit.path.as_path().starts_with(&game_root));
}

#[test]
fn catalog_probes_game_root_relative_paths_outside_harvested_directories() {
    let mod_root = temp_root("probe");
    fs::create_dir_all(mod_root.join("map/terrain")).expect("dir");
    fs::write(mod_root.join("map/terrain/offroad.bmp"), b"").expect("file");

    let roots = [source_root(1, SourceRootKind::Project, &mod_root)];
    let catalog = TextureCatalog::build(&roots);

    assert!(
        catalog.resolve(&roots, "map/terrain/offroad.bmp").is_some(),
        "game-root-relative probe"
    );
    assert!(catalog.resolve(&roots, "map/terrain/missing.bmp").is_none());
}

#[test]
fn catalog_serves_completion_prefixes_and_missing_suggestions() {
    let game_root = temp_root("suggest");
    fs::create_dir_all(game_root.join("gfx/interface")).expect("dir");
    fs::write(game_root.join("gfx/interface/a.dds"), b"").expect("a");
    fs::write(game_root.join("gfx/interface/b.tga"), b"").expect("b");
    fs::create_dir_all(game_root.join("gfx/map")).expect("dir");
    fs::write(game_root.join("gfx/map/c.png"), b"").expect("c");

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);

    assert_eq!(
        catalog.paths_with_prefix("gfx/interface/"),
        vec!["gfx/interface/a.dds", "gfx/interface/b.tga"]
    );
    let siblings = catalog.sibling_paths("gfx/interface/typo.dds", 8);
    assert_eq!(siblings, vec!["gfx/interface/a.dds", "gfx/interface/b.tga"]);
    assert!(catalog.resolve(&roots, "gfx/interface/typo.dds").is_none());
}

#[test]
fn catalog_browses_children_by_directory_level() {
    let game_root = temp_root("browse");
    fs::create_dir_all(game_root.join("gfx/interface/assets")).expect("assets dir");
    fs::create_dir_all(game_root.join("gfx/map")).expect("map dir");
    fs::create_dir_all(game_root.join("tutorial")).expect("tutorial dir");
    fs::write(game_root.join("tutorial/first.dds"), b"").expect("tutorial");
    fs::write(game_root.join("gfx/top.dds"), b"").expect("top file");
    fs::write(game_root.join("gfx/interface/a.dds"), b"").expect("a");
    fs::write(game_root.join("gfx/interface/assets/deep.dds"), b"").expect("deep");
    fs::write(game_root.join("gfx/map/c.png"), b"").expect("c");

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);

    // An empty prefix lists the top-level directories, not an alphabetical
    // head of every file in the catalog.
    let top = catalog.children_with_prefix("");
    assert_eq!(top.directories, vec!["gfx/", "tutorial/"]);
    assert!(top.files.is_empty(), "no top-level files: {:?}", top.files);

    // One level down: subdirectories and the files sitting directly in gfx/.
    let gfx = catalog.children_with_prefix("gfx/");
    assert_eq!(gfx.directories, vec!["gfx/interface/", "gfx/map/"]);
    assert_eq!(gfx.files, vec!["gfx/top.dds"]);

    // Deeper files only appear once their own directory is browsed; the
    // fragment after the last slash filters both children kinds.
    let interface = catalog.children_with_prefix("gfx/interface/");
    assert_eq!(interface.directories, vec!["gfx/interface/assets/"]);
    assert_eq!(interface.files, vec!["gfx/interface/a.dds"]);
    let filtered = catalog.children_with_prefix("gfx/interface/a");
    assert_eq!(filtered.directories, vec!["gfx/interface/assets/"]);
    assert_eq!(filtered.files, vec!["gfx/interface/a.dds"]);
    let exact = catalog.children_with_prefix("gfx/interface/as");
    assert_eq!(exact.directories, vec!["gfx/interface/assets/"]);
    assert!(exact.files.is_empty());

    // Spelling normalization (case, backslashes, doubled separators) applies
    // to the browse prefix exactly as it does to resolution.
    let spelled = catalog.children_with_prefix("\\\\GFX//Interface\\");
    assert_eq!(spelled.files, vec!["gfx/interface/a.dds"]);
}

#[test]
fn catalog_browse_files_respect_the_prefix_cap() {
    let game_root = temp_root("cap");
    fs::create_dir_all(game_root.join("gfx/interface")).expect("dir");
    for index in 0..260 {
        fs::write(
            game_root.join(format!("gfx/interface/f{index:03}.dds")),
            b"",
        )
        .expect("texture");
    }

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);
    let children = catalog.children_with_prefix("gfx/interface/");
    assert_eq!(children.files.len(), crate::texture::MAX_PREFIX_RESULTS);
    assert_eq!(
        children.files.first().copied(),
        Some("gfx/interface/f000.dds")
    );
}

#[test]
fn snapshot_rebuilds_the_texture_catalog_when_watched_assets_change() {
    let mod_root = temp_root("watch");
    fs::create_dir_all(mod_root.join("gfx/interface")).expect("dir");
    fs::write(mod_root.join("interface.gfx"), "spriteTypes = {}\n").expect("gfx source");
    fs::write(mod_root.join("gfx/interface/initial.dds"), b"").expect("initial");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![source_root(
        1,
        SourceRootKind::Project,
        &mod_root,
    )]));
    host.refresh_source_roots().expect("scan");
    let before = host.snapshot();
    assert!(
        before
            .resolve_texture_path("gfx/interface/initial.dds")
            .is_some()
    );
    assert!(
        before
            .resolve_texture_path("gfx/interface/added_later.dds")
            .is_none()
    );

    let added = mod_root.join("gfx/interface/added_later.dds");
    fs::write(&added, b"").expect("added texture");
    host.apply_disk_file_changes(&[DiskFileChange::new(
        AbsPath::normalize(&added),
        DiskFileChangeKind::Created,
    )])
    .expect("watched change");
    let after = host.snapshot();
    assert!(
        after
            .resolve_texture_path("gfx/interface/added_later.dds")
            .is_some(),
        "catalog invalidated and rebuilt for a watched asset"
    );
}

/// Writes a real STORED-method zip with the given (name, payload) members so
/// the central-directory harvest is exercised against authentic archive bytes.
fn write_stored_zip(path: &std::path::Path, members: &[(&str, &[u8])]) {
    fn le16(value: u16) -> [u8; 2] {
        value.to_le_bytes()
    }
    fn le32(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }
    let mut body = Vec::new();
    let mut central = Vec::new();
    for (name, payload) in members {
        let local_offset = body.len() as u32;
        body.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]); // local signature
        body.extend_from_slice(&le16(20)); // version needed
        body.extend_from_slice(&le16(0)); // flags
        body.extend_from_slice(&le16(0)); // stored
        body.extend_from_slice(&le16(0)); // time
        body.extend_from_slice(&le16(0)); // date
        body.extend_from_slice(&le32(0)); // crc
        body.extend_from_slice(&le32(payload.len() as u32));
        body.extend_from_slice(&le32(payload.len() as u32));
        body.extend_from_slice(&le16(name.len() as u16));
        body.extend_from_slice(&le16(0)); // extra
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(payload);
        central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
        central.extend_from_slice(&le16(20)); // version made by
        central.extend_from_slice(&le16(20)); // version needed
        central.extend_from_slice(&le16(0)); // flags
        central.extend_from_slice(&le16(0)); // stored
        central.extend_from_slice(&le16(0)); // time
        central.extend_from_slice(&le16(0)); // date
        central.extend_from_slice(&le32(0)); // crc
        central.extend_from_slice(&le32(payload.len() as u32));
        central.extend_from_slice(&le32(payload.len() as u32));
        central.extend_from_slice(&le16(name.len() as u16));
        central.extend_from_slice(&le16(0)); // extra
        central.extend_from_slice(&le16(0)); // comment
        central.extend_from_slice(&le16(0)); // disk start
        central.extend_from_slice(&le16(0)); // internal attrs
        central.extend_from_slice(&le32(0)); // external attrs
        central.extend_from_slice(&le32(local_offset));
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = body.len() as u32;
    let central_size = central.len() as u32;
    let mut zip = body;
    zip.extend_from_slice(&central);
    zip.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
    zip.extend_from_slice(&le16(0)); // disk
    zip.extend_from_slice(&le16(0)); // central directory disk
    zip.extend_from_slice(&le16(members.len() as u16));
    zip.extend_from_slice(&le16(members.len() as u16));
    zip.extend_from_slice(&le32(central_size));
    zip.extend_from_slice(&le32(central_offset));
    zip.extend_from_slice(&le16(0)); // comment
    fs::write(path, zip).expect("write zip");
}

#[test]
fn catalog_harvests_dlc_zip_members_record_only() {
    let game_root = temp_root("dlczip");
    let pack_dir = game_root.join("dlc/dlc999_probe");
    fs::create_dir_all(&pack_dir).expect("pack dir");
    write_stored_zip(
        &pack_dir.join("dlc999.zip"),
        &[
            ("gfx/event_pictures/pic.dds", b"pixels"),
            ("music/track.mp3", b"audio"),
            ("interface/readme.txt", b"text"),
        ],
    );
    // No disk copy of the packed key exists here; the duplicate-priority case
    // is covered by `catalog_prefers_disk_files_over_packed_members` below.
    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);

    // The packed member resolves with archive provenance.
    let resolution = catalog
        .resolve(&roots, "gfx/event_pictures/pic.dds")
        .expect("packed member resolves");
    assert!(
        resolution
            .hit
            .path
            .as_path()
            .to_string_lossy()
            .ends_with("dlc999.zip")
    );
    assert_eq!(
        resolution.hit.archive_member.as_deref(),
        Some("gfx/event_pictures/pic.dds")
    );
    assert_eq!(resolution.hit.root_kind, SourceRootKind::Vanilla);

    // Non-catalog extensions stay out, and so does a corrupt archive.
    assert!(catalog.resolve(&roots, "music/track.mp3").is_none());
    fs::write(pack_dir.join("broken.zip"), b"not a zip at all").expect("broken zip");
    let rebuilt = TextureCatalog::build(&roots);
    assert!(
        rebuilt
            .resolve(&roots, "gfx/event_pictures/pic.dds")
            .is_some()
    );

    // The packed member's directories join the browse index.
    let children = rebuilt.children_with_prefix("gfx/");
    assert!(
        children.directories.contains(&"gfx/event_pictures/"),
        "packed directory must be browsable: {:?}",
        children.directories
    );
}

#[test]
fn catalog_prefers_disk_files_over_packed_members() {
    let game_root = temp_root("ziporder");
    let pack_dir = game_root.join("dlc/dlc998_order");
    fs::create_dir_all(&pack_dir).expect("pack dir");
    write_stored_zip(
        &pack_dir.join("dlc998.zip"),
        &[("gfx/shared.dds", b"packed")],
    );
    fs::create_dir_all(game_root.join("gfx")).expect("dir");
    fs::write(game_root.join("gfx/shared.dds"), b"disk").expect("disk copy");

    let roots = [source_root(1, SourceRootKind::Vanilla, &game_root)];
    let catalog = TextureCatalog::build(&roots);
    let resolution = catalog.resolve(&roots, "gfx/shared.dds").expect("resolves");
    // Component-wise suffix: disk paths carry platform separators.
    assert!(
        resolution
            .hit
            .path
            .as_path()
            .ends_with(std::path::Path::new("gfx").join("shared.dds"))
    );
    assert_eq!(resolution.hit.archive_member, None);
}
