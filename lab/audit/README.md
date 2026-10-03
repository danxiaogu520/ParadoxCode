# 语义审计

所有审计实现和回归测试已迁入 Rust [tools](../../crates/tools/README.md#diagnostics-and-semantic-evidence)。
从仓库根目录运行 `cargo run --locked -p tools -- audit --help`。

| 命令 | 职责 |
| --- | --- |
| `audit diagnose` | Mod 诊断报告 |
| `audit sweep` | 全量语料诊断、查询和阶段测量 |
| `audit baseline` | 冻结诊断、符号统计、11 个 golden 位置和代表性前缀 |
| `audit errors` | 每条 error 的诊断身份、源文本、安装/DLC 资源和人工决定 |
| `audit diff` | 完整诊断与 SQLite 定义/引用差异 |
| `audit completions` | 导出 LSP 截断前的完整 IDE 候选，并核对记录的有界集合 |

报告由实际 LSP 提供规则身份，不再生成或读取独立的规则 manifest。旧报告字段仍可读取，
并与实际服务器和缓存核对。重复诊断的 ID 与原 Python 实现保持一致，旧人工决定可继续核对。
不完整报告、过期决定、总数不一致和冻结语料变化都会失败；未解释的差异保持待审。

回归检查使用 `cargo run --locked -p tools -- gates audit`；真实服务器协议检查使用
`cargo run --locked -p tools -- lsp test`。详细参数、示例和拒绝条件见 tools 使用指南。
原始语料、安装路径和报告按[本地验收约定](../../CONTRIBUTING.md#local-vanilla-acceptance)保留在本机。
