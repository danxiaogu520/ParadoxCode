# ParadoxCode 设计文档

ParadoxCode 是通用 PDX Mod 语言工具引擎。当前唯一有交付承诺和完整规则覆盖目标的游戏是 Europa Universalis IV（EU4）；其他游戏未来可以通过 profile 接入，但优先级低，当前不纳入版本计划。

当前仓库处于 alpha：EU4 主要语言功能、增量 per-file HIR、共享 snapshot、后台取消和 formatter LSP 已有自动化回归，但完整 Vanilla/依赖配置和普通用户安装发布仍未闭环。

## 当前决策

- 项目名：ParadoxCode
- EU4 脚本核心：PdxScript
- Language Server：`pdx-ls`
- 命令行入口：`pdx`
- 首选编辑器：Zed
- 当前交付游戏：EU4（EU4-first，不排除未来 profile）
- 模块命名：全部 Rust crate、binary 与内部包使用 `pdx-` 前缀
- MVP 文件范围：EU4 规则数据库声明的全部可支持文件类别；PdxScript、localisation 和 CSV 使用独立语法前端，二进制/媒体资源只进入路径索引
- 权威规则来源：提交到项目的自有 SQLite `eu4.pdxrules`；CWTools `.cwt` 仅作为一次性 bootstrap 导入源
- 规则版本：对数据库规范化逻辑内容计算唯一 `rule_hash`，与发布它的扩展版本绑定，与 EU4 版本无关
- 工作区来源：未保存 overlay > 当前 Mod > 有序依赖 Mod > Vanilla；没有 DLC source root
- Vanilla 索引：首次配置时建立本地缓存，此后仅由用户显式刷新，不因 `rule_hash` 或文件变化自动重建

## 文档索引

- [总体架构](architecture.md)
- [RFC 0013：通用 PDX 引擎与 EU4-first](rfc/0013-generic-engine-eu4-first.md)
- [已取代：EU4-only 架构决策](decision-eu4-only.md)
- [EU4 MVP 计划](mvp.md)
- [CWTools、EU4 Config 与 Jomini 调研](reference-study.md)
- [RFC 0001：系统边界与 crate 依赖](rfc/0001-system-boundaries.md)
- [RFC 0002：语法、CST 与增量解析](rfc/0002-syntax-cst.md)
- [RFC 0003：工作区、VFS 与覆盖解析](rfc/0003-workspace-vfs.md)
- [RFC 0004：EU4 规则数据库与 Runtime Schema](rfc/0004-eu4-rules-schema.md)
- [RFC 0005：HIR 与 Scope 系统](rfc/0005-hir-scope.md)
- [RFC 0006：Symbol 与 Reference Index](rfc/0006-symbol-index.md)
- [RFC 0007：诊断与补全策略](rfc/0007-diagnostics-completion.md)
- [RFC 0008：安全格式化](rfc/0008-formatter.md)
- [RFC 0009：LSP Runtime](rfc/0009-lsp-runtime.md)
- [RFC 0010：Zed 集成](rfc/0010-zed-integration.md)
- [RFC 0011：测试、Fuzz 与质量门禁](rfc/0011-testing-quality.md)
- [RFC 0012：CWT 一次性导入与权威规则数据库](rfc/0012-cwt-rule-compiler.md)
- [RFC 0013：通用 PDX 语言引擎与 EU4-first 产品策略](rfc/0013-generic-engine-eu4-first.md)

## 文档状态

RFC 0001–0012 是首轮设计基线；其中 EU4-only 产品约束已由 RFC 0013 取代。现有 Phase 0–6A 状态需要按真实端到端能力重新审计，不能把 crate 骨架、内部单元测试或开发机 smoke 等同于可发布产品。

文档中的示例是 ParadoxCode 自有设计示例，不应直接复制游戏文件或参考项目的测试 corpus。
