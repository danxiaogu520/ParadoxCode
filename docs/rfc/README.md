# 当前 RFC 索引

这些 RFC 是当前代码契约的分域说明，不再保留日期化 amendment 或已废止方案。总入口见 [`docs/README.md`](../README.md)，总体数据流和依赖边界见 [`docs/architecture.md`](../architecture.md)。

| RFC | 状态 | 主题 |
|---|---|---|
| [0001](0001-system-boundaries.md) | Current | 系统边界与 crate 依赖 |
| [0002](0002-syntax-cst.md) | Current | Script/Localisation CST 与编辑更新 |
| [0003](0003-workspace-vfs.md) | Current | workspace、source roots、overlay、Vanilla cache |
| [0004](0004-eu4-rules-schema.md) | Current | runtime schema、matcher、canonical hash |
| [0005](0005-hir-scope.md) | Partial | HIR、scope facts、参数和 lowering |
| [0006](0006-symbol-index.md) | Partial | shard、resolution、navigation、rename |
| [0007](0007-diagnostics-completion.md) | Partial | diagnostics、completion、hover |
| [0008](0008-formatter.md) | Current | 安全格式化 |
| [0009](0009-lsp-runtime.md) | Current | LSP transport、生命周期和并发 |
| [0010](0010-zed-integration.md) | Current | Zed extension 和 server 获取 |
| [0011](0011-testing-quality.md) | Current | 测试、fuzz、benchmark、CI gates |
| [0012](0012-generic-engine-eu4-first.md) | Current | 通用 engine 与 EU4-first |
| [0013](0013-embedded-first-party-rules.md) | Superseded | RFC 0014 的兼容指针 |
| [0014](0014-first-party-rule-source.md) | Current | 第一方规则 source/compiler/runtime authority |

## Authority

- 架构、依赖、并发和稳定不变量：[`docs/architecture.md`](../architecture.md)。
- workspace、overlay 和 Vanilla cache：RFC 0003。
- 协议边界、capability 和 LSP 生命周期：RFC 0009。
- 第一方规则 source、编译、artifact 和禁止外部输入：RFC 0014。
- 测试和 CI 当前实际门禁：RFC 0011。

如果实现与 RFC 不一致，应先修正代码或将 RFC 标为 `Partial`，不能用历史注释掩盖差异。
