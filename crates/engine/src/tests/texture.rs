//! `TextureCatalog` ground-truth tests: harvest, multi-root resolution, DLC
//! pack-relative keys, extension-drift fallback, and watcher-driven rebuilds.

use super::*;
use text::AbsPath;

fn temp_root(label: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("engine-texture-{label}-{nonce}"));
    fs::create_dir_all(&root).expect("fixture root");
    root
}

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
        source_root(1, SourceRootKind::CurrentMod, &mod_root),
        source_root(2, SourceRootKind::Vanilla, &game_root),
    ];
    let catalog = TextureCatalog::build(&roots);

    // Case, backslashes, doubled separators, and a leading slash all fold to the
    // same normalized key; the mod root wins over the game root.
    let resolution = catalog
        .resolve(&roots, "\"\\\\gfx//Interface\\SHARED.dds\"")
        .expect("normalized hit");
    assert_eq!(resolution.hit.root_kind, SourceRootKind::CurrentMod);
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

    let roots = [source_root(1, SourceRootKind::CurrentMod, &mod_root)];
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
fn snapshot_rebuilds_the_texture_catalog_when_watched_assets_change() {
    let mod_root = temp_root("watch");
    fs::create_dir_all(mod_root.join("gfx/interface")).expect("dir");
    fs::write(mod_root.join("interface.gfx"), "spriteTypes = {}\n").expect("gfx source");
    fs::write(mod_root.join("gfx/interface/initial.dds"), b"").expect("initial");

    let mut host = eu4_host();
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![source_root(
        1,
        SourceRootKind::CurrentMod,
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
