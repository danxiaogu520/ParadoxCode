# ParadoxCode

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/danxiaogu520/ParadoxCode)](https://github.com/danxiaogu520/ParadoxCode/releases)
[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/paradoxcode.paradoxcode-vscode)](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ParadoxCode 是一个独立、开源的 P 社（Paradox）模组语言工具包。它以「泛型 PDX 语言引擎、EU4 优先」为产品方向：引擎层（工作区、索引、分析、LSP）保持跨游戏可复用，而《欧陆风云 IV》的路径、作用域、命令、符号与特殊语义全部收拢在 EU4 profile 中。当前版本面向 VS Code 与 Zed 中的 EU4 模组开发。

ParadoxCode **与 Paradox Interactive 无任何关联，也未获得其背书**。《欧陆风云 IV》与 Paradox Interactive 均为其各自权利人的商标。

## 功能特性

- 容错的 Paradox 脚本与 EU4 本地化解析器。语法错误不会阻断分析：解析器始终生成损失感知（loss-aware）语法树，无法识别的结构降级为 `Unknown*` 节点而不会崩溃。
- 由经过校验的第一方 EU4 规则数据库驱动的语法与语义诊断。
- 补全、悬停、跳转定义、查找引用、文档/工作区符号。
- 冲突感知的重命名（仅限可写的 Mod 源）。
- 保守的格式化器，拒绝改写不安全或残缺的文件。
- 跨「未保存缓冲区 → 当前 Mod → 有序依赖 Mod → 本地持久化 Vanilla 索引」的工作区解析。
- stdio 语言服务器（`pdx-ls`），支持取消、过期结果保护与不可变分析快照，并能对活跃 Mod 根做定向文件监听更新。
- VS Code 扩展：零配置、带校验和的服务器自动安装，首次使用引导（walkthrough），以及实时任务树预览（贴图节点、缩放、源码跳转、PNG/JSON 导出）。
- 轻量 Zed 扩展，提供 Tree-sitter 高亮；编辑器高亮是 Tree-sitter 的唯一用途——运行时解析器是纯 Rust 实现，不链接 Tree-sitter C。
- 精确版本服务器下载：SHA-256 校验、受限解压、有界流式传输与自校验可执行缓存。

## 快速开始

### VS Code

从 [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode) 安装 **ParadoxCode - EU4 Language Tools**（或在命令面板执行 `ext install paradoxcode.paradoxcode-vscode`）。然后：

1. 打开（或新建）一个工作区并**信任**它。
2. 打开 EU4 Mod 中的文件，例如 `common/`、`events/`、`decisions/`、`missions/`、`history/`、`interface/`。
3. 首次使用时，扩展会自动下载与你平台匹配的 `pdx-ls` 发布版本，校验其 SHA-256 校验和，缓存并启动它。无需任何语言服务器配置。
4. 如果未自动发现你的 EU4 安装目录，请使用 **Choose EU4 Installation / Vanilla Data**，选择包含 `eu4.exe` 以及 `common`、`events`、`missions`、`decisions`、`localisation` 的文件夹。

VS Code 的 **Get Started** 页面提供 **Start using ParadoxCode** 引导，覆盖上述全部流程。

### Zed

Zed 扩展在本仓库中开发（`editors/zed`），正在等待 [`zed-industries/extensions`](https://github.com/zed-industries/extensions) 注册表的审核。在它上架之前，请以开发扩展方式安装：指向本仓库检出目录的 `editors/zed` 子目录。推荐的语言设置见 `editors/zed/recommended-settings.json`。

### pdx-ls 独立二进制

Linux（x86_64、aarch64）、macOS（x86_64、aarch64）与 Windows（x86_64）的独立 `pdx` / `pdx-ls` 二进制以 `.tar.gz` / `.zip` 归档形式附在每个 [GitHub Release](https://github.com/danxiaogu520/ParadoxCode/releases) 上，并带有 `.sha256` 校验文件。语言服务器内嵌第一方 EU4 规则源，绝不导入外部规则文件。

## 项目状态

**最新版本：v0.1.2**（2026-08-20）。核心 EU4 语言功能已实现、测试，并通过标签驱动的发布流水线发布（见[发布](#发布)）。0.1.x 仍在早期成熟期，欢迎早期使用者通过 issue 模板反馈问题，以便在下一个版本中修复。

当前范围的已知限制：

- CSV 文件仅作为语法占位/不透明资源处理，尚未提供 CSV 解析器。
- Zed 扩展尚未上架 Zed 扩展画廊（注册表审核中）。
- EU4 是唯一已实现的游戏 profile。引擎按设计保持游戏中立，但尚不存在第二个 profile，因此不对其他游戏的时间表做任何承诺。

## 架构

```text
源文本
    -> 损失感知语法树
    -> profile 与规则感知的 HIR
    -> 按文件独立的索引分片
    -> 不可变工作区快照
    -> 编辑器中立的分析
    -> LSP 适配层
    -> Zed / VS Code
```

引擎/profile 边界保证工作区、索引、分析、LSP 与发布基础设施保持游戏中立，而 EU4 的路径、作用域、命令、符号与特殊语义留在 EU4 profile 中。crate 依赖方向是严格单向的：

```text
pdx-text
  -> pdx-parser -> pdx-engine -> pdx-analysis -> pdx-lsp
pdx-game（EU4 profile）-> pdx-parser + pdx-text + pdx-rules
pdx-rules -> pdx-bake
pdx-rules + pdx-game -> pdx-engine / pdx-analysis
```

## 从源码构建

前置条件：**Rust 1.97 或更新版本**，以及 **Node.js 24 LTS**（用于 Tree-sitter 语料检查）。

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

安装一次仓库 Git 钩子（此后每次提交都会运行质量门禁）：

```bash
bash scripts/install-git-hooks.sh
```

显式运行质量门禁套件，或只诊断某个分组（`core`、`grammars`、`zed`、`vscode`、`release`、`fuzz`）：

```bash
bash scripts/check-quality-gates.sh
```

使用 `pdx-bake` 校验并编译开发者维护的第一方规则源；产物可放入被忽略的构建目录以供检视：

```bash
cargo run -p pdx-rules --bin pdx-bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdxrules \
  --manifest target/rules/manifest.json
```

官方 `pdx-ls` 二进制内嵌第一方 JSON 规则源，并在首次使用或源 `rule_hash` 变化时，在用户缓存中生成经过校验的 SQLite 规则工件。生成工件不会提交到仓库。

## 开发环境

可从配置路径或 `PATH` 启动 `pdx-ls`。工作区源根通过 `.pdx/project.toml` 配置，或在 Zed 中通过 `.zed/settings.json` 的 `lsp.pdx-ls.initialization_options` 配置。本文档所述方式面向贡献者，并非最终安装体验。

让 ParadoxCode 自动发现、校验、索引并记住本地 EU4 安装：

```bash
pdx setup vanilla
```

首次 `pdx-ls` 启动时，若没有项目覆盖配置或之前的尝试记录，也会执行一次非阻塞的快速探测。如果常见位置没有产生唯一候选，请运行 `pdx setup vanilla --deep` 或用 `--source` 指定目录。正常启动时不会重复搜索。

使用底层命令在显式位置构建或手动刷新缓存：

```bash
pdx index vanilla \
  --source /path/to/eu4 \
  --output /path/to/vanilla.pdxindex
```

大型依赖 Mod 可以只索引一次，然后在每次启动时从持久缓存加载，而无需重新扫描：

```bash
pdx index dependency \
  --id gui-xu \
  --source /path/to/dependency-mod \
  --output /path/to/dependency.pdxindex
```

`id` 必须与编辑器中配置的依赖 id 一致。在 Zed 中，缓存在 `.zed/settings.json` 中声明；`pdx-ls` 会在后台加载它，并在文件缺失时自动重建（规则哈希变化时与 Vanilla 缓存一样重新生成）：

```json
{
  "lsp": {
    "pdx-ls": {
      "initialization_options": {
        "dependencies": [
          {
            "id": "gui-xu",
            "path": "/path/to/dependency-mod",
            "index": "/path/to/dependency.pdxindex"
          }
        ]
      }
    }
  }
}
```

在设置了 `index` 时，依赖不会实时扫描；修改依赖后，请用 `pdx index dependency` 重建缓存并重启语言服务器（命令面板 `pdx-ls: restart`）。删除 `index` 字段可回退到实时扫描。

使用下面的开发脚本，对照该 Vanilla 缓存对完整 Current Mod 做一次可重复的诊断遍历。它会通过真实的 `pdx-ls` 传输逐文件打开相关资源，并把 JSON 与 Markdown 报告写入被忽略的 `diagnostic-reports/` 目录：

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/current-mod \
  --vanilla-cache /path/to/vanilla.pdxindex
```

发现错误时命令以非零码退出；使用 `--fail-on warning` 或 `--fail-on none` 调整自动化阈值。全部选项见 `--help`。

## 仓库布局

| 路径 | 用途 |
| --- | --- |
| `crates/pdx-text` | 文本、范围、位置与路径原语 |
| `crates/pdx-parser` | 损失感知解析器与规范化格式化器 |
| `crates/pdx-rules` | 泛型规则 schema、运行时与第一方编译器（`pdx-bake`） |
| `crates/pdx-game` | EU4 profile：游戏发现、本地配置与 EU4 任务模型 |
| `crates/pdx-engine` | VFS、源根、索引分片与不可变快照 |
| `crates/pdx-analysis` | 编辑器中立的分析查询（诊断、补全、导航、重命名） |
| `crates/pdx-lsp` | LSP 生命周期、协议边界与 CLI 入口（`pdx`、`pdx-ls`） |
| `editors/vscode/` | VS Code 扩展：服务器引导、引导流程、任务树预览 |
| `editors/zed/` | 轻量 Zed 扩展、语言元数据与查询 |
| `grammars/` | 仅编辑用的 Tree-sitter 语法与语料测试 |
| `rules/eu4/` | 权威第一方 EU4 规则源（JSON）与生成的清单 |
| `fuzz/` | 解析、编辑、格式化与 HIR 模糊测试目标 |
| `scripts/` | 可复现的质量检查与诊断工作流 |

当前第一方 EU4 规则面向游戏版本 **1.37.5**（8,577 条语义规则、117 个文件类别、2,674 个符号描述符）。

## 发布

发布由标签驱动：推送 `v0.x.y` 标签后，流水线会构建并验证全部五个原生 `pdx-ls` 归档、创建不可变的 GitHub Release，并打包和附加 VSIX。Visual Studio Marketplace 发布暂时改为手动：从 Release 下载附加的 VSIX，再通过发布者管理页面上传。版本历史与各版本变更记录在 [CHANGELOG.md](CHANGELOG.md)；完整发布检查清单见 [RELEASING.md](RELEASING.md)。

## 贡献

欢迎贡献。请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 了解构建/测试环境、提交信息约定以及仓库强制执行的工程不变量（禁止 `unsafe`、稳定身份、唯一规则源等）。

## 安全

请勿在公开 issue 中报告安全漏洞。如何私下报告以及如何处理，见 [SECURITY.md](SECURITY.md)。

## 许可

ParadoxCode 源代码以 [MIT 许可证](LICENSE) 发布。仓库不重新分发 EU4 游戏文件、用户 Vanilla 缓存或外部规则语料。规则维护与再分发边界由 `pdx-bake` 校验与仓库质量门禁保证。
