# 本地实验入口

性能测量、语义审计和真实 LSP 测试统一由 Rust [tools](../crates/tools/README.md) 提供。
本目录保留实验用途的导航；实现和测试位于 `crates/tools/`。

| 工作 | 入口 |
| --- | --- |
| 基准、A/B、查询延迟、内存和画像 | [性能测量](perf/README.md)：`tools perf` |
| 逐条诊断、资源证据、语义差异、完整补全 | [语义审计](audit/README.md)：`tools audit` |
| 真实 LSP 端到端验证 | `tools lsp test` |

源码语料留在 `data/`；运行产物留在忽略的 `target/`。本地实验属于
[开发者反馈](../CONTRIBUTING.md#validation)，需要保持实际被测程序、语料和报告身份一致。
