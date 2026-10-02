use std::path::{Path, PathBuf};

fn watch(directory: &Path) {
    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in std::fs::read_dir(directory).expect("rules directory") {
        let path = entry.expect("rule entry").path();
        let metadata = std::fs::symlink_metadata(&path).expect("rule metadata");
        assert!(
            !metadata.file_type().is_symlink(),
            "rules directory must not contain symlinks: {}",
            path.display()
        );
        if metadata.is_dir() {
            watch(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn main() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4-v2");
    watch(&source);
    let baked =
        rules::bake::compile(&source).expect("first-party rule checks must pass before embedding");
    let profile = rules::bundle::load_directory(&source)
        .expect("first-party game configuration")
        .game
        .profile;
    let install = profile
        .install
        .as_ref()
        .expect("game.json must declare install data");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(output.join("eu4.ir.json"), baked.bytes).expect("write compiled arena");
    std::fs::write(
        output.join("eu4.install.rs"),
        install_declaration(&profile.game_id, install),
    )
    .expect("write installation descriptor");
}

fn install_declaration(game_id: &str, install: &rules::ProfileInstallSpec) -> String {
    format!(
        "/// Installation recognition facts compiled from game.json.\n\
         pub const INSTALL_DESCRIPTOR: crate::GameInstallDescriptor = crate::GameInstallDescriptor {{\n\
             game_id: {game_id:?},\n\
             display_name: {display_name:?},\n\
             executable_paths: crate::PlatformExecutablePaths {{\n\
                 windows: &{windows:?}, linux: &{linux:?}, macos: &{macos:?},\n\
             }},\n\
             validation_directories: &{directories:?},\n\
             installation_directory_names: &{names:?},\n\
             steam_app_id: {steam_app_id:?},\n\
         }};\n",
        display_name = install.display_name,
        windows = install.executable_paths.windows,
        linux = install.executable_paths.linux,
        macos = install.executable_paths.macos,
        directories = install.validation_directories,
        names = install.installation_directory_names,
        steam_app_id = install.steam_app_id,
    )
}
