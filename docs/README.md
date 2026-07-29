# ParadoxCode 文档

ParadoxCode 是通用 PDX Mod 语言引擎，当前按 EU4-first 路线交付。仓库仍处于
alpha；实现状态以根目录的 [执行计划](../plan.md) 为准，不以文档数量、代码骨架或单项
测试推断发布完成度。

## 如何使用这组文档

不同文档只承担一种职责：

| 文档 | 职责 | 是否描述当前进度 |
| --- | --- | --- |
| [项目 README](../README.md) | 面向用户和新贡献者的项目入口 | 仅概述 |
| [MVP 验收基线](mvp.md) | v0.1 能力、非目标、阶段退出条件 | 否 |
| [执行计划](../plan.md) | 当前状态、剩余工作、验证顺序 | 是 |
| [总体架构](architecture.md) | 当前数据流、依赖边界和运行时不变量 | 仅记录已落地架构 |
| RFC | 已接受设计及其修订历史 | 否 |
| ADR | 较小且稳定的实现决策 | 否 |
| 调研和 spike | 历史证据与早期结论 | 否 |

发生冲突时，当前用户要求优先，其次是已接受 RFC 与总体架构，再其次是执行计划。
被取代的决策和历史调研不约束当前实现。

## 使用与维护

- [Workspace 配置](configuration.md)
- [发布流程与检查清单](releasing.md)
- [贡献指南](../CONTRIBUTING.md)
- [规则源码与生成物](../rules/README.md)
- [Zed 扩展开发](../editors/zed/README.md)
- [Grammar 说明](../grammars/README.md)
- [测试说明](../tests/README.md)
- [Fuzz 说明](../fuzz/README.md)

## 当前设计

- [总体架构](architecture.md)
- [EU4 v0.1 MVP 验收基线](mvp.md)
- [RFC 0013：通用 PDX 引擎与 EU4-first](rfc/0013-generic-engine-eu4-first.md)
- [RFC 0014：内嵌第一方规则与发布所有权](rfc/0014-embedded-first-party-rules.md)
- [RFC 0015：第一方规则源码与编译器](rfc/0015-first-party-rule-source.md)
- [ADR 0001：固定规范格式与 quoted script](adr/0001-canonical-formatting-and-quoted-scripts.md)
- [ADR 0002：EU4 language 与 Script format 命名](adr/0002-eu4-language-and-script-format-names.md)

## 当前调研

- [PDX Block 与共享语义上下文实证调研](research/semantic-context-block-schema.md)：基于真实
  大型 EU4 Mod 的只读研究，提出 `SemanticContext + BlockSchema + body context` 模型；
  结论尚未成为已接受 RFC。

## 基础 RFC

| RFC | 主题 |
| --- | --- |
| [0001](rfc/0001-system-boundaries.md) | 系统边界与 crate 依赖 |
| [0002](rfc/0002-syntax-cst.md) | 语法、CST 与编辑更新 |
| [0003](rfc/0003-workspace-vfs.md) | Workspace、VFS 与覆盖解析 |
| [0004](rfc/0004-eu4-rules-schema.md) | EU4 规则数据库与 runtime schema |
| [0005](rfc/0005-hir-scope.md) | HIR 与 scope |
| [0006](rfc/0006-symbol-index.md) | Symbol 与 reference index |
| [0007](rfc/0007-diagnostics-completion.md) | Diagnostics 与 completion |
| [0008](rfc/0008-formatter.md) | 安全格式化 |
| [0009](rfc/0009-lsp-runtime.md) | LSP runtime |
| [0010](rfc/0010-zed-integration.md) | Zed 集成 |
| [0011](rfc/0011-testing-quality.md) | 测试、fuzz 与质量门禁 |

RFC 0001–0011 是首轮设计基线；其中与后续 RFC 冲突的段落以文首 amendment 和 RFC
0013–0015 为准。

## 历史资料

以下文件保留审计价值，但不定义当前产品行为：

- [已取代：EU4-only 架构决策](decision-eu4-only.md)
- [已取代：RFC 0012 CWT 一次性导入](rfc/0012-cwt-rule-compiler.md)
- [CWTools、EU4 Config 与 Jomini 调研](reference-study.md)
- [Phase 0：server 分发 spike](spikes/phase0-server-distribution.md)
- [Phase 0：Zed grammar spike](spikes/phase0-zed-grammar.md)

文档示例必须是 ParadoxCode 自有设计示例，不得直接复制游戏文件或参考项目测试
corpus。
