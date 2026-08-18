# ParadoxCode for VS Code

EU4 language tooling backed by `pdx-ls`（与 Zed 扩展共用同一个语言服务器），外加 **任务树实时预览**（mission tree preview）——在编辑 `missions/*.txt` 时，侧边面板实时渲染任务树并把诊断信号直接画在节点上，点击节点跳转到对应文本块。

> A VS Code extension for EU4 modding, sharing the same `pdx-ls` server as the
> Zed extension, with a live mission-tree preview on top.

## 功能 / Features

- 语言服务器：诊断 / 补全 / hover / 定义跳转 / 重命名 / 格式化（与 Zed 扩展同源）
- **任务树实时预览**：打开 EU4 任务文件后，点编辑器标题栏的预览图标（悬停 “Open Mission Tree Preview to the Side”，与 Markdown 预览同款按钮）或执行命令 `ParadoxCode: Open Mission Tree Preview to the Side` 打开面板；编辑当前
  EU4 任务文件时实时刷新（150ms 防抖），点击节点/组跳转对应文本块；节点用**游戏
  真实纹理渲染**（EMT 式：任务图标 + 帧贴图 + 白色粗体标题），依赖箭头逐 glyph 贴
  游戏贴图（EMT 兼容几何，由 pdx-ls 计算）；未配置/未找到游戏安装时自动回退为
  简化示意图样式。任务标题解析本地化 `{id}_title`（优先英文定义，缺失时回退原始 id）
- 错误/警告/根任务颜色编码（纹理模式下以描边叠加）；平移 / 滚轮缩放（光标锚定）/ 双击自适应

## 通用配置 / Universal configuration

一份 `.pdx/project.toml` 放在项目根目录，Zed 与 VS Code 共用同一份配置：**pdx-ls 自动发现并消费
它的 source 字段，两端扩展读取它的 `[server]` 表来决定 pdx-ls 二进制**。无需任何编辑器侧配置：

```toml
# project root/.pdx/project.toml
mod_directory = "."
vanilla_index_cache = ".pdx/vanilla.pdxindex"

[[dependencies]]
id = "Chinese Language Mod for 1.37"
path = "C:/.../workshop/content/236850/2976470733"
index = ".pdx/cache/dep.pdxindex"   # 可选：该依赖的持久缓存

[server]
binary = "C:/Code/ParadoxCode/target/release/pdx-ls.exe"
```

- **source 字段**（`mod_directory` / `vanilla_index_cache` / `dependencies`）：由 pdx-ls
  自动发现并消费（工作区即 Current Mod 时 `mod_directory` 可省略）。
- **`[server]` 表**：只被扩展读取（pdx-ls 二进制路径）。优先级：编辑器显式设置 >
  `[server].binary` > `$PATH`。
- 文件不存在 → 回退默认（workspace 即 Current Mod）；文件存在但非法 → **大声失败**（不静默忽略）。

## 行为一致性 / Behavior parity with Zed

两个扩展刻意保持行为一致，配置也共享同一套语义：

| 概念 | Zed 扩展 | VS Code 扩展 | 服务器消费 |
| --- | --- | --- | --- |
| 语言 | `Europa Universalis IV`（tree-sitter） | `eu4`（filenamePatterns） | — |
| 服务器二进制 | `pdx-ls`（`[server].binary`，回退编辑器设置/安装） | `pdx-ls`（`[server].binary`，回退 `paradoxcode.pdxLsPath`/PATH） | `pdx-ls` |
| 项目配置（自动发现 `.pdx/project.toml`） | `lsp.pdx-ls.initialization_options.projectConfig`（可选覆盖） | `paradoxcode.projectConfig`（可选覆盖） | `initializationOptions.projectConfig` |
| Mod 目录 | `.modDirectory` | `paradoxcode.modDirectory` | `modDirectory` |
| Vanilla 索引缓存 | `.vanillaIndexCache` | `paradoxcode.vanillaIndexCache` | `vanillaIndexCache` |
| 依赖 Mod 列表 | `.dependencies` | `paradoxcode.dependencies` | `dependencies` |
| 游戏安装目录（预览纹理） | `gameDirectory` | `paradoxcode.gameDirectory` | `gameDirectory` |

`paradoxcode.*` 设置直接映射为 LSP `initializationOptions` 的**同名同义** JSON——与 Zed 的初始化
选项 schema 完全一致，因此两端服务器行为天然一致。**日常配置放 `.pdx/project.toml`（通用源），
两端编辑器选项只在需要覆盖时使用**。

差异是有意的：VS Code 扩展额外提供**任务树预览**（实时可视化），这是 Zed 扩展当前
没有的增量；语法高亮 Zed 用 tree-sitter，VS Code 用基础 TextMate（完整对账为后续项）。

## 开发 / Development

```bash
npm install
npm run check        # TypeScript 类型检查 + pdx-ls 数据契约冒烟（node scripts/smoke.mjs）
npm run compile      # 输出到 out/
```

冒烟脚本 `scripts/smoke.mjs` 直接用真实 `pdx-ls`（stdio JSON-RPC）验证
`pdx/missionPreview` 的返回结构（节点/箭头 glyph 集/组/外部引用/诊断/纹理表），不需要 VSCode 宿主。

## 运行 / Run

1. 确保 `pdx-ls` 可执行：配置 `paradoxcode.pdxLsPath` 指向 `pdx-ls`（或 `cargo build -p pdx-lsp --bin pdx-ls` 后加入 PATH），或在项目 `.pdx/project.toml` 的 `[server].binary` 写明路径。
2. （可选）配置 `paradoxcode.gameDirectory` 指向 EU4 安装根目录（含 `eu4.exe` 的目录），预览即可渲染真实游戏纹理；留空时 pdx-ls 启动时自动发现一次，找不到则使用简化示意图样式。
2. 在 VSCode 中打开扩展目录（F5，Extension Development Host），或 `vsce package` 后安装.
3. 扩展在窗口启动时自动激活并拉起 `pdx-ls`（`onStartupFinished`）：状态栏左侧显示 `PDX ●`（运行）/ `◐`（启动中）/ `○`（未运行），输出面板 **ParadoxCode** 通道记录激活、二进制来源与启动日志。
4. 打开一个 `missions/*.txt` 文件，点编辑器标题栏右侧的预览图标按钮（仅在 EU4 文件上出现，悬停
   “Open Mission Tree Preview to the Side”），或执行命令 `ParadoxCode: Open Mission Tree Preview to the Side`。

## 当前限制 / Limits（v1）

- 配置为手动指定服务器路径；Zed 的 checksum 自动下载安装器未移植（后续项）。
- 语法高亮为基础级别；tree-sitter 对账为后续项。
- 预览为单文档范围；跨文件前置引用显示为 "↥ id" 桩，不解析其他文件。
- 纹理模式不显示任务图标（frame 缺失/解码失败时回退示意图样式）；任务图标来自
  游戏 `interface/missionicons_*.gfx` 与 `gfx/interface/missions`，DLC 未装时对应
  图标缺失（回退为帧内空白，与游戏一致）。
