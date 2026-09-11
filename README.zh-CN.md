# ParadoxCode

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/danxiaogu520/ParadoxCode)](https://github.com/danxiaogu520/ParadoxCode/releases)
[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/paradoxcode.paradoxcode-vscode)](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ParadoxCode 是一个独立、开源的 P 社（Paradox）模组语言工具包。它以「泛型 PDX 语言引擎、EU4 优先」为产品方向：引擎层（工作区、索引、分析、LSP）保持跨游戏可复用，而《欧陆风云 IV》的路径、作用域、命令、符号与特殊语义全部收拢在 EU4 profile 中。当前版本面向 VS Code 中的 EU4 模组开发。

ParadoxCode **与 Paradox Interactive 无任何关联，也未获得其背书**。《欧陆风云 IV》与 Paradox Interactive 均为其各自权利人的商标。

## 功能特性

- 容错的 Paradox 脚本与 EU4 本地化解析器。语法错误不会阻断分析：解析器始终生成损失感知（loss-aware）语法树，无法识别的结构降级为 `Unknown*` 节点而不会崩溃。
- 由经过校验的第一方 EU4 规则数据库驱动的语法与语义诊断。
- 诊断使用稳定的 PascalCase ID（例如 `UnknownKey`、`InvalidValue`、`WrongScope`、
  `UnknownLocalisationKey`）；严重度在分析层类型化，LSP 元数据提供确定性，不暴露内部规则来源
  或旧版本迁移字段。
- 规则诊断可提供有界的 `textDocument/codeAction` 快速修复，例如为拼写接近且唯一的静态
  枚举值生成“你是否想要”替换。
- EU4 profile 会从支持的 scripted localisation 目录拼写中发现定义，将 `name` 字段作为
  `defined_text` 编入索引，并用于本地化命令补全与基于注册表的未知命令警告；查询按路径
  分片并使用哈希查表，大型 Vanilla 索引无需遍历全部符号。
- 补全、悬停、跳转定义、查找引用、文档/工作区符号。
- 完整语义 token 支持增量 `full/delta` 响应，并使用有界、按快照版本校验的缓存；编辑与工作区刷新时自动失效。
- 支持按视口请求 `textDocument/semanticTokens/range`，在分类前剪枝范围外的语法树子树，使可见编辑器高亮成本与视口相关。
- 规则证明的作用域转换可通过有界的 `textDocument/inlayHint` 注解显示。
- 冲突感知的重命名（仅限可写的 Mod 源）。
- 保守的格式化器，拒绝改写不安全或残缺的文件。
- 跨「未保存缓冲区 → 当前 Mod → 有序依赖 Mod → 本地持久化 Vanilla 索引」的工作区解析。
- stdio 语言服务器（`pdc`），支持取消、过期结果保护与不可变分析快照，并能对活跃 Mod 根做定向文件监听更新。
- VS Code 扩展：零配置、带校验和的服务器自动安装，首次使用引导（walkthrough），以及实时任务树预览（贴图节点、缩放、源码跳转、PNG/JSON 导出）。
- 精确版本服务器下载：SHA-256 校验、受限解压、有界流式传输与自校验可执行缓存。

## 快速开始

### VS Code

从 [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode) 安装 **ParadoxCode - EU4 Language Tools**（或在命令面板执行 `ext install paradoxcode.paradoxcode-vscode`）。然后：

1. 打开（或新建）一个工作区并**信任**它。
2. 打开 EU4 Mod 中的文件，例如 `common/`、`events/`、`decisions/`、`missions/`、`history/`、`interface/`。
3. 首次使用时，扩展会自动下载与你平台匹配的 `pdc` 发布版本，校验其 SHA-256 校验和，缓存并启动它。无需任何语言服务器配置。
4. 如果未自动发现你的 EU4 安装目录，请使用 **Choose EU4 Installation / Vanilla Data**，选择包含 `eu4.exe` 以及 `common`、`events`、`missions`、`decisions`、`localisation` 的文件夹。

VS Code 的 **Get Started** 页面提供 **Start using ParadoxCode** 引导，覆盖上述全部流程。

### pdc 独立二进制

Linux（x86_64、aarch64）、macOS（x86_64、aarch64）与 Windows（x86_64）的独立 `pdc` 二进制以 `.tar.gz` / `.zip` 归档形式附在每个 [GitHub Release](https://github.com/danxiaogu520/ParadoxCode/releases) 上，并带有 `.sha256` 校验文件。语言服务器内嵌第一方 EU4 规则源，绝不导入外部规则文件。

## 项目状态

**最新版本：v0.3.2**（2026-09-11）。EU4 分析与索引功能已实现、测试，并通过标签驱动的发布流水线发布（见[发布](#发布)）。本版本将悬停重构为一等展示层（分类标题、scope 表、多语言并行本地化预览），让补全、悬停与诊断共用同一份逐站点动态参数推导，依据 wiki 与 vanilla 证据重建了 EU4 变量模型与修正器数据，并把大型工作区的稳态诊断延迟压到一秒以内。0.x 仍在早期成熟期，欢迎早期使用者通过 issue 模板反馈问题，以便在下一个版本中修复。

当前范围的已知限制：

- CSV 文件仅作为语法占位/不透明资源处理，尚未提供 CSV 解析器。
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
    -> VS Code
```

引擎/profile 边界保证工作区、索引、分析、LSP 与发布基础设施保持游戏中立，而 EU4 的路径、作用域、命令、符号与特殊语义留在 EU4 profile 中。crate 依赖方向是严格单向的：

```text
text
  -> parser -> engine -> ide -> pdc
game（EU4 profile）-> parser + text + rules
rules -> bake
rules + game -> engine / ide
```

## 从源码构建

前置条件：**Rust 1.98 或更新版本**，以及 **Node.js 24 LTS**（用于 VS Code 扩展工具链）。

```bash
git clone https://github.com/danxiaogu520/ParadoxCode.git
cd ParadoxCode
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
```

显式运行质量门禁套件，或只诊断某个分组（`core`、`vscode`、`release`、`fuzz`、`core-fast`、`perf`）。仓库不使用提交钩子；CI 会在每个 pull request 上运行同样的门禁：

```bash
bash scripts/check-quality-gates.sh
```

Pull Request CI 使用 `core-fast` 分组：保留正确性检查，但不编译或运行 benchmark 目标。
优化后的 benchmark 套件仍保留在 `perf` 分组中，由定时或手动触发的 Performance workflow 运行。
CI 还会运行编辑器、fuzz 与依赖检查；fuzz 只绑定其直接运行时依赖，
Windows release 构建则与 Windows 测试和 clippy 并行执行。分支保护应将 `Required CI checks`
作为稳定的聚合必需检查。

使用 `bake` 校验并编译开发者维护的第一方规则源；产物可放入被忽略的构建目录以供检视：

```bash
cargo run -p rules --bin bake -- build \
  --source rules/eu4 \
  --output target/rules/eu4.pdcrules \
  --manifest target/rules/manifest.json
```

官方 `pdc` 二进制内嵌第一方 JSON 规则源，并在首次使用或源 `rule_hash` 变化时，在用户缓存中生成经过校验的 SQLite 规则工件。生成工件不会提交到仓库。

EU4 规则源按职责拆分：`catalog/` 保存文件类别、符号描述符与规范化记录，`semantic/` 按
effect、trigger、modifier、on_action 以及 event、decision、mission、history 等目录语义组织规则，
`types/`、`values/`、`localisation/` 保存支撑表，`profile/` 保存 EU4 的扫描路径、符号、作用域、
动态值和语义继承配置。`rules/eu4/manifest.json` 显式列出全部片段；编译器把它们合并成一个
逻辑模型，profile 与语义规则共同参与同一个规范 `rule_hash`。

`pdc` 要求现代 LSP 客户端在 initialize 请求中提供至少一个 `workspaceFolders` 条目。仅发送已弃用
`rootUri` 的旧客户端不受支持，并会收到 `INVALID_PARAMS`；请升级编辑器或语言客户端。

## 开发环境

可从配置路径或 `PATH` 启动 `pdc`。编辑器配置位于 VS Code 扩展的 `paradoxcode.*` 设置中。
本文档所述方式面向贡献者，并非最终安装体验。

`pdc` 会自动发现、校验、索引并记住本地 EU4 安装。首次启动时，若没有显式缓存或之前的尝试记录，会执行一次非阻塞的快速探测：读取启动器元数据（Steam 库清单、Epic 清单、GOG 注册表）和常见位置，只执行一次。若未产生候选，请将游戏目录设置指向安装位置（VS Code：`paradoxcode.gameDirectory`）并重新加载；缓存随后自动构建并保持更新，安装变更时后台重建索引。

大型依赖 Mod 可以只索引一次，然后在每次启动时从持久缓存加载，而无需重新扫描。
`id` 必须与编辑器中配置的依赖 id 一致。

在设置了 `index` 时，依赖不会实时扫描；修改依赖后，删除过期的缓存文件并重启语言服务器（命令面板 `pdc: restart`），缓存会自动重建。删除 `index` 字段可回退到实时扫描。

使用下面的开发脚本，对照该 Vanilla 缓存对完整 Current Mod 做一次可重复的诊断遍历。它会通过真实的 `pdc` 传输逐文件打开相关资源，并把 JSON 与 Markdown 报告写入被忽略的 `diagnostic-reports/` 目录：

```bash
bash scripts/diagnose-current-mod.sh \
  --mod /path/to/current-mod \
  --vanilla-cache /path/to/vanilla.pdcindex
```

发现错误时命令以非零码退出；使用 `--fail-on warning` 或 `--fail-on none` 调整自动化阈值。全部选项见 `--help`。

## 仓库布局

| 路径 | 用途 |
| --- | --- |
| `crates/text` | 文本、范围、位置与路径原语 |
| `crates/parser` | 损失感知解析器与规范化格式化器 |
| `crates/rules` | 泛型规则 schema、运行时与第一方编译器（`bake`） |
| `crates/game` | EU4 profile：游戏发现、本地配置与 EU4 任务模型 |
| `crates/vfs` | 源根、工作区扫描与稳定的文档数据模型 |
| `crates/hir` | 规则感知的语义降阶（定义、作用域、模板） |
| `crates/index` | 工作区符号索引分片与逐文件分析管线 |
| `crates/engine` | 分析宿主、不可变快照、查询缓存与 `.pdcindex` 持久化 |
| `crates/ide` | 编辑器中立的分析查询（诊断、补全、导航、重命名） |
| `crates/pdc` | `pdc` 语言服务器：LSP 生命周期与协议边界 |
| `crates/tools` | 仓库工具链（`check`、`release`、缓存构建），供 CI 与维护者使用 |
| `editors/vscode/` | VS Code 扩展：服务器引导、引导流程、任务树预览 |
| `rules/eu4/` | 权威第一方 EU4 规则树（catalog、semantic、支撑表与 profile） |
| `fuzz/` | 解析、编辑、格式化与 HIR 模糊测试目标 |
| `scripts/` | 可复现的质量检查与诊断工作流 |

当前第一方 EU4 规则面向游戏版本 **1.37.5**（8,525 条语义规则、121 个文件类别、2,667 个符号描述符）。
`rules/manifest.json` 记录 schema/source 版本、规范 `rule_hash` 与工件校验和。

## 发布

发布由标签驱动：推送 `v0.x.y` 标签后，流水线会构建并验证全部五个原生 `pdc` 归档、创建不可变的 GitHub Release，并打包和附加 VSIX。Visual Studio Marketplace 发布暂时改为手动：从 Release 下载附加的 VSIX，再通过发布者管理页面上传。版本历史与各版本变更记录在 [CHANGELOG.md](CHANGELOG.md)；完整发布检查清单见 [RELEASING.md](RELEASING.md)。

## 贡献

欢迎贡献。请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md) 了解构建/测试环境、提交信息约定以及仓库强制执行的工程不变量（禁止 `unsafe`、稳定身份、唯一规则源等）。

## 安全

请勿在公开 issue 中报告安全漏洞。如何私下报告以及如何处理，见 [SECURITY.md](SECURITY.md)。

## 许可

ParadoxCode 源代码以 [MIT 许可证](LICENSE) 发布。仓库不重新分发 EU4 游戏文件、用户 Vanilla 缓存或外部规则语料。规则维护与再分发边界由 `bake` 校验与仓库质量门禁保证。
