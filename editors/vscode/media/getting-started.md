# ParadoxCode quick path

This walkthrough keeps the first run deliberately small:

1. Trust the workspace.
2. Open the Mod folder.
3. Wait for the matching `pdx-ls` binary to download and start.
4. Let Vanilla discovery finish, or choose the EU4 installation once if discovery needs help.
5. Open an EU4 script or Localisation file and inspect Problems, completion, hover, and navigation.
6. Open a mission file to try the read-only mission-tree preview.

No `.pdx/project.toml`, server download, rule path, or cache path is required for a normal
Marketplace installation. The extension and the language server keep machine-local cache data;
your Mod files are never copied into that cache.

## 如果你使用中文

首次使用只需要：信任工作区、打开 Mod 文件夹、等待 `pdx-ls` 自动准备，然后等待 Vanilla
索引建立。如果自动找不到 EU4，再选择包含 `eu4.exe`、`common`、`events`、`missions`、
`decisions` 和 `localisation` 的安装目录。之后直接打开脚本或本地化文件即可获得诊断、补全、悬停和跳转。

任务树预览只读地显示服务器提供的布局；打开 `common/missions` 下的任务文件即可开始。
