# ParadoxCode 文档入口

本文档目录只维护与当前代码和当前产品边界相关的内容。历史实现日志、已废止方案和不可复现研究不作为当前规范。

## 当前入口

- [当前架构](architecture.md)：数据流、crate 边界、workspace、规则、并发和安全不变量。
- [规则语义矩阵](rule-semantics-matrix.md)：第一方 JSON 字段与 runtime/profile/analysis 的当前消费关系。
- [性能与复现](performance-report.md)：当前 benchmark、LSP 性能脚本和测量边界。
- [RFC 索引](rfc/README.md)：按编号查看当前契约；以下 RFC 已清理为当前实现说明。

## 模块文档

| 模块 | 文档 |
|---|---|
| `pdx-text` | [`crates/pdx-text/README.md`](../crates/pdx-text/README.md) |
| `pdx-parser` | [`crates/pdx-parser/README.md`](../crates/pdx-parser/README.md) |
| `pdx-rules` | [`crates/pdx-rules/README.md`](../crates/pdx-rules/README.md) |
| `pdx-game` | [`crates/pdx-game/README.md`](../crates/pdx-game/README.md) |
| `pdx-engine` | [`crates/pdx-engine/README.md`](../crates/pdx-engine/README.md) |
| `pdx-analysis` | [`crates/pdx-analysis/README.md`](../crates/pdx-analysis/README.md) |
| `pdx-mission-model` | [`docs/rfc/0014-mission-tree-editor.md`](rfc/0014-mission-tree-editor.md)（EU4 任务树预览数据面：模型/CST 提取/几何/校验） |
| `pdx-lsp` | [`crates/pdx-lsp/README.md`](../crates/pdx-lsp/README.md) |
| Zed extension | [`editors/zed/README.md`](../editors/zed/README.md) |
| VS Code extension | [`editors/vscode/README.md`](../editors/vscode/README.md) |
| Tree-sitter grammar | [`grammars/README.md`](../grammars/README.md) |
| Fuzz targets | [`fuzz/README.md`](../fuzz/README.md) |

## RFC 状态

| 文档 | 状态 | 当前用途 |
|---|---|---|
| RFC 0001 | Current | 系统边界与 crate 依赖 |
| RFC 0002 | Current | Script/Localisation CST 与编辑更新 |
| RFC 0003 | Current | workspace、source roots、overlay、Vanilla cache |
| RFC 0004 | Current | SQLite runtime schema、matcher、canonical hash |
| RFC 0005 | Partial | HIR、scope facts、参数和保守 lowering |
| RFC 0006 | Partial | shard、resolution、navigation、rename |
| RFC 0007 | Partial | diagnostics、completion、hover |
| RFC 0008 | Current | 安全 formatter |
| RFC 0009 | Current | LSP transport、生命周期、并发和 capabilities |
| RFC 0010 | Current | Zed extension、server 获取和启动 |
| RFC 0011 | Current | 测试、fuzz、benchmark 和 CI quality gates |
| RFC 0012 | Current | 通用 engine 与 EU4-first 边界 |
| RFC 0013 | Current | 第一方规则 source、compiler 和 runtime authority |
| RFC 0014 | Superseded | EU4 任务树编辑器：独立 GPUI 应用已退役，转为 VS Code 实时预览（`pdx/missionPreview` + `editors/vscode` webview） |

RFC 中的 `Current` 表示内容与当前代码契约一致；`Partial` 表示设计和实现已经存在，但仍有明确未完成边界；`Superseded` 不定义新的行为。

## 文档维护规则

- 当前行为写入架构或对应 RFC，不追加日期化实现日志。
- 设计尚未落地时必须标为“当前限制”或 `Partial`，不能写成已实现。
- 规则 authority 只由 RFC 0013 定义；workspace/cache 细节由 RFC 0003 定义；协议细节由 RFC 0009 定义。
- 代码、测试、CLI 或规则 schema 改变时，同一变更同步修正文档。
- 性能数字必须带测量环境和复现命令，不作为跨机器验收阈值。
