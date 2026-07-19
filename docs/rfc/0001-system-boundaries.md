# RFC 0001：系统边界与 Crate 依赖

- 状态：Accepted
- MVP：EU4 v0.1

> 2026-07-20 amendment：EU4-only 约束已被 [RFC 0013](0013-generic-engine-eu4-first.md) 取代。当前产品仍只交付 EU4，但 workspace、index、analysis、LSP 和规则 runtime 将迁移为通用引擎，EU4 专有行为进入游戏 profile。

## 问题

ParadoxCode 同时包含 EU4 parser、semantic analysis、workspace index、LSP 和编辑器扩展。如果不先限制依赖方向，EU4 规则、协议类型和可变工作区状态会迅速渗透到全部模块。

## 决策

Rust workspace 初始包含：

| Crate | 职责 |
|---|---|
| `pdx-text` | range、position、line index、URI/path 基础类型 |
| `pdx-syntax` | 硬编码 EU4 Rust parser、source text、typed CST、syntax errors |
| `pdx-eu4` | EU4 SQLite schema、canonical hash、只读 runtime model 与查询 API |
| `pdx-cwt` | MVP 一次性 CWT importer；未来的规则数据库 CRUD 管理工具 |
| `pdx-hir` | 路径和规则感知的语义 lowering |
| `pdx-workspace` | VFS、source roots、cache、snapshot、index |
| `pdx-analysis` | diagnostics、completion、navigation、rename 查询 |
| `pdx-format` | 编辑器无关格式化 |
| `pdx-lsp` | LSP transport 和协议适配 |
| `pdx-cli` | `pdx` 与 `pdx-ls` binary 入口；`pdx-cwt` crate 提供同名维护者 binary |

`pdx-cli` 可以提供两个 binary target，也可以在实现 spike 后拆为独立 binary crate；这不改变核心边界。

## 依赖约束

1. 运行时依赖沿 `text -> syntax/eu4 -> hir -> workspace -> analysis -> lsp` 方向。
2. `pdx-cwt` 依赖 `pdx-text` 和 `pdx-eu4`，但任何 analysis runtime crate 都不反向依赖 `pdx-cwt`。
3. `pdx-format` 只依赖 text/syntax 和格式配置，不依赖 workspace index。
4. 只有 `pdx-lsp` 可以在公开 API 中使用 LSP protocol types。
5. EU4 规则数据库是独立 SQLite artifact；`pdx-eu4` 可以包含 EU4 专用类型和规则适配，但不把规则复制进 `pdx-ls` binary。
6. Zed extension 不链接 analysis crate；它携带 `eu4.pdxrules` 并显式传给 server。
7. 核心 API 不接受绝对游戏目录作为隐式全局；workspace configuration 显式传入。
8. 所有 Cargo package/独立模块使用 `pdx-` 前缀；Rust identifier 必须使用下划线时采用 `pdx_*`。binary 固定为 `pdx`、`pdx-ls` 与维护者工具 `pdx-cwt`。

## 分析门面

核心使用 host/snapshot 模型：

```rust
pub struct AnalysisHost { /* mutable owner */ }
pub struct AnalysisSnapshot { /* immutable Arc-backed view */ }

impl AnalysisHost {
    pub fn apply_change(&mut self, change: WorkspaceChange);
    pub fn snapshot(&self) -> AnalysisSnapshot;
}
```

语言查询只定义在 snapshot 上。查询返回 editor-neutral DTO，例如 `Location`, `Completion`, `WorkspaceEditPlan`，由 `pdx-lsp` 转换成协议类型。

## 仓库布局

```text
crates/                  `pdx-*` Rust 核心与 CWT importer
grammars/                Zed editor-side Tree-sitter grammars
rules/                   提交的 eu4.pdxrules、manifest 与测试 metadata
editors/                 薄客户端，Cargo package 仍以 `pdx-` 命名
docs/                    架构与 RFC
tests/                   跨 crate fixtures/integration
fuzz/                    fuzz targets
reference/               只读参考仓库，不参与构建
```

## 结果

- CLI 可以在没有 LSP 的情况下复用分析。
- Zed 和未来其他编辑器客户端不会产生两套 EU4 语义逻辑。
- 一次性 CWT importer 和 Eu4Rules schema 可独立测试。
- crate 数量略多，但每个边界均对应不同的变化原因。

## 拒绝的方案

- 单一 `pdx-core` 巨型 crate：早期方便，后期无法阻止协议和游戏逻辑耦合。
- 在只有 EU4 一个真实实现时设计复杂插件 ABI 或大量行为 trait：缺乏第二个样本，容易形成错误抽象。
- 在 Zed extension 内做语义分析：无法复用、难测试且受扩展运行环境限制。
