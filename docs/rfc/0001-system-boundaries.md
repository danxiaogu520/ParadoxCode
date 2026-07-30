# RFC 0001：系统边界与 Crate 依赖

- 状态：Accepted
- MVP：EU4 v0.1

> 2026-07-20 amendment：EU4-only 约束已被 [RFC 0013](0013-generic-engine-eu4-first.md) 取代。当前产品仍只交付 EU4，但 workspace、index、analysis、LSP 和规则 runtime 将迁移为通用引擎，EU4 专有行为进入游戏 profile。
>
> 2026-07-21 amendment：第 5–6 条中的外部规则 artifact 分发边界已由
> [RFC 0014](0014-embedded-first-party-rules.md) 取代；crate 依赖方向仍然有效。
>
> 2026-07-22 amendment：`pdx-cwt` 和所有 CWT 输入已由
> [RFC 0015](0015-first-party-rule-source.md) 废止；维护者工具现为 `pdx-rulec`。

## 问题

ParadoxCode 同时包含 EU4 parser、semantic analysis、workspace index、LSP 和编辑器扩展。如果不先限制依赖方向，EU4 规则、协议类型和可变工作区状态会迅速渗透到全部模块。

## 决策

Rust workspace 初始包含：

| Crate | 职责 |
|---|---|
| `pdx-text` | range、position、line index、URI/path 基础类型 |
| `pdx-parser` | 硬编码 EU4 Rust parser、source text、typed CST、syntax errors、canonical formatter |
| `pdx-rules` | 通用 SQLite schema、canonical hash、只读 runtime model 与查询 API |
| `pdx-game` | 安装发现、EU4 profile（eu4 模块）、bootstrap catalog |
| `pdx-rulec` | 第一方规则源码严格校验与 artifact/manifest 编译器 |
| `pdx-hir` | 路径和规则感知的语义 lowering |
| `pdx-workspace` | VFS、source roots、cache、snapshot、index |
| `pdx-analysis` | diagnostics、completion、navigation、rename 查询 |
| `pdx-lsp` | `pdx` 与 `pdx-ls` binary 入口、LSP transport 和协议适配；`pdx-rulec` 是独立维护者 binary |

## 依赖约束

1. 运行时依赖沿 `text/rules -> syntax/hir -> workspace -> analysis -> lsp` 方向。
2. `pdx-rulec` 只依赖 `pdx-rules`，任何 analysis runtime crate 都不反向依赖维护者编译器。
3. 格式化逻辑位于 `pdx-parser` 的 `format` 模块，只依赖 text 和 CST 类型。
4. 只有 `pdx-lsp` 可以在公开 API 中使用 LSP protocol types。
5. EU4 规则数据库是由第一方源码生成的 SQLite artifact；通用加载位于 `pdx-rules`，官方 composition root 将其嵌入 binary。
6. Zed extension 不链接 analysis crate，不携带或传递 semantic rules。
7. 核心 API 不接受绝对游戏目录作为隐式全局；workspace configuration 显式传入。
8. 所有 Cargo package/独立模块使用 `pdx-` 前缀；Rust identifier 必须使用下划线时采用 `pdx_*`。binary 固定为 `pdx`、`pdx-ls` 与维护者工具 `pdx-rulec`。

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
crates/                  `pdx-*` Rust 核心与第一方规则编译器
grammars/                Zed editor-side Tree-sitter grammars
rules/                   第一方 JSON source 与生成的 eu4.pdxrules/manifest
editors/                 薄客户端，Cargo package 仍以 `pdx-` 命名
docs/                    架构与 RFC
tests/                   跨 crate fixtures/integration
fuzz/                    fuzz targets
reference/               只读参考仓库，不参与构建
```

## 结果

- CLI 可以在没有 LSP 的情况下复用分析。
- Zed 和未来其他编辑器客户端不会产生两套 EU4 语义逻辑。
- 第一方规则编译器、通用 `RuleSet` schema 和 EU4 profile 可独立测试。
- crate 数量略多，但每个边界均对应不同的变化原因。

## 拒绝的方案

- 单一 `pdx-core` 巨型 crate：早期方便，后期无法阻止协议和游戏逻辑耦合。
- 在只有 EU4 一个真实实现时设计复杂插件 ABI 或大量行为 trait：缺乏第二个样本，容易形成错误抽象。
- 在 Zed extension 内做语义分析：无法复用、难测试且受扩展运行环境限制。
