use crate::{lsp, report};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SCRIPT_PATH: &str = "events/perf.txt";
pub const EFFECTS_PATH: &str = "common/scripted_effects/perf.txt";
pub const EFFECTS: &str = "perf_effect = { add_prestige = 1 }\nperf_embed = { $BODY$ }\n";
pub const ROUND_DEFINITION: &str = "common/scripted_effects/perf_round.txt";
pub const DISK_DEFINITION: &str = "perf_round = { add_prestige = $PERF_DISK$ }\n";
pub const TERRAIN_PATH: &str = "map/terrain.txt";
pub const GFX_PATH: &str = "interface/perf.gfx";
pub const GFX: &str = "spriteTypes = { spriteType = { name = \"perf_picture\" texturefile = \"gfx/event_pictures/perf.dds\" } }\n";
pub const LOC_PATH: &str = "localisation/perf_l_english.yml";
pub const LOC: &str =
    "\u{feff}l_english:\n perf_title:0 \"Performance 中文\"\n perf_desc:0 \"😀 $perf_title$\"\n";
pub const TERRAIN: &str = "categories = { demo = { type = foresst } }\n";
pub const MISSION: &str =
    "perf_tree = { slot = 1 perf_mission = { position = 1 icon = mission_perf } }\n";
const SCRIPT: &str = "# 编辑 😀\r\nnamespace = perf\r\ncountry_event = {\r\n id = perf.1\r\n title = perf_title\r\n picture = perf_picture\r\n immediate = { log = \"编辑😀\" perf_effect = yes add_prestige = 1 perf_embed = { BODY = \"add_prestige = 2\" } }\r\n}\r\n";

pub struct Fixture {
    _temp: tempfile::TempDir,
    pub workspace: PathBuf,
    pub profile: PathBuf,
    pub dependency_index: PathBuf,
    pub dependency_files: usize,
    pub vanilla_digest: Option<String>,
    pub options: Value,
    pub script: String,
    pub callers: Vec<(String, String)>,
    pub digest: String,
}

impl Fixture {
    pub fn new(files: usize, entries: usize, cache: Option<&Path>) -> Result<Self, String> {
        let temp = tempfile::Builder::new()
            .prefix("pdc-perf-suite-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let workspace = temp.path().join("project");
        let profile = temp.path().join("profile");
        let dependency = temp.path().join("dependency");
        let dependency_index = temp.path().join("dependency.pdcindex");
        let mut hash = Sha256::new();
        let mut write = |root: &Path, name: &str, text: &str| -> Result<(), String> {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::write(path, text).map_err(|e| e.to_string())?;
            hash.update(name.as_bytes());
            hash.update([0]);
            hash.update(text.as_bytes());
            hash.update([0]);
            Ok(())
        };
        let mut script = SCRIPT.to_owned();
        for i in 0..entries {
            script.push_str(&format!(
                "country_event = {{ id = perf.{} immediate = {{ perf_effect = yes }} }}\r\n",
                i + 2
            ));
        }
        write(&workspace, SCRIPT_PATH, &script)?;
        write(&workspace, TERRAIN_PATH, TERRAIN)?;
        write(&workspace, EFFECTS_PATH, EFFECTS)?;
        write(&workspace, ROUND_DEFINITION, DISK_DEFINITION)?;
        let mut callers = Vec::new();
        for slot in 0..3 {
            let path = format!("events/perf_caller_{slot}.txt");
            let mut caller = String::new();
            for event in 0..=entries {
                let body = if event == slot * entries / 2 {
                    "log = \"编辑😀\" perf_round = { P }"
                } else {
                    "add_prestige = 1"
                };
                caller.push_str(&format!(
                    "country_event = {{ id = caller{slot}.{event} immediate = {{ {body} }} }}\r\n"
                ));
            }
            write(
                &workspace,
                &path,
                &caller.replace("perf_round = { P }", "perf_round = { PERF_DISK = 1 }"),
            )?;
            callers.push((path, caller));
        }
        write(&workspace, LOC_PATH, LOC)?;
        write(&workspace, "missions/perf.txt", MISSION)?;
        write(&workspace, GFX_PATH, GFX)?;
        // Server-side preview resolves the path. Pixel decoding belongs to the editor.
        write(&workspace, "gfx/event_pictures/perf.dds", "fixture")?;
        for i in 0..files {
            write(
                &dependency,
                &format!("events/dep_{i:05}.txt"),
                &format!("country_event = {{ id = dependency.{i} }}\n"),
            )?;
        }
        let mut options = json!({"modDirectory":workspace,"vanillaMode":"disabled","workspaceWideDiagnostics":false,
            "dependencies":[{"id":"perf-dependency","path":dependency,"index":dependency_index}]});
        let vanilla_digest = if let Some(cache) = cache {
            let copy = temp.path().join("vanilla.pdcindex");
            fs::copy(cache, &copy).map_err(|e| format!("copy Vanilla cache: {e}"))?;
            options["vanillaMode"] = json!("auto");
            options["vanillaIndexCache"] = json!(copy);
            Some(report::file_hash(&copy)?)
        } else {
            None
        };
        fs::create_dir_all(&profile).map_err(|e| e.to_string())?;
        Ok(Self {
            _temp: temp,
            workspace,
            profile,
            dependency_index,
            dependency_files: files,
            vanilla_digest,
            options,
            script,
            callers,
            digest: hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
    }

    pub fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        // Keep discovery settings, persistent parse trees and regenerated indexes local to this pair.
        for name in [
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
        ] {
            command.env(name, &self.profile);
        }
        #[cfg(target_os = "macos")]
        command.env("HOME", &self.profile);
        command
    }

    pub fn document(&self, relative: &str) -> Result<Value, String> {
        Ok(json!({"uri":lsp::uri(&self.workspace.join(relative))?}))
    }

    pub fn position(&self, needle: &str) -> Value {
        let offset = self.script.find(needle).expect("owned fixture anchor") + needle.len();
        let prefix = &self.script[..offset];
        json!({"line":prefix.bytes().filter(|b| *b == b'\n').count(),
            "character":prefix.rsplit('\n').next().unwrap().encode_utf16().count()})
    }
}
