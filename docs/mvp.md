# EU4 MVP 实施计划

> 2026-07-20 重新审计：当前代码是 EU4 alpha 功能原型，不是已发布的 v0.1。Phase 2–3 的基础实现可继续作为基线；Phase 4–6A 的功能代码存在，但增量数据流、持久化 Vanilla cache、依赖配置、formatter LSP 接入、后台取消、自动安装和跨平台发布退出条件尚未全部满足。后续先完成 RFC 0013 和发布前架构修复，再进入 Phase 6B。

## MVP 成功定义

用户在 Zed 中打开一个 EU4 Mod workspace 后，可以获得：

- PdxScript 与 EU4 localisation 基础高亮
- syntax、unknown key、unknown symbol、scope diagnostics
- key、effect、trigger、symbol、localisation completion
- definition、references、hover
- document symbol、workspace symbol
- 语义安全的 rename
- 保守、幂等的全文格式化
- 未保存文档、当前 Mod、有序依赖 Mod和 Vanilla 的优先级解析
- 对权威 EU4 规则数据库声明的全部可支持文件类别提供与格式相称的分析
- 使用 `pdx-cwt v0.1` 一次性导入 EU4 CWT corpus，生成项目自有 SQLite `eu4.pdxrules` 与 `rule_hash`
- 首次建立并持久化本地 Vanilla 索引，之后仅支持用户手动刷新

Event、Scripted Effect、Scripted Trigger 和 Localisation Key 仍是必须通过端到端测试的基准类型，但不再是 symbol 范围上限。数据库中的 type、enum、variable、alias、localisation 与 filepath descriptor 能够表达的 definition/reference 都进入统一索引。

## 非目标

- 当前版本实现其他游戏 profile、动态插件 ABI 或多游戏 workspace
- VS Code extension
- Semantic Tokens
- Code Action / Quick Fix
- Inlay Hint、Code Lens、Document Link
- 存档或二进制格式
- 对贴图、音频、字体等资产内容做语义解析；它们只作为路径索引目标
- 完整模拟 EU4 运行时
- 历史 EU4 版本或版本条件规则
- `pdx-cwt` 的 CRUD、历史、diff 或 rollback；这些属于后续版本
- 运行时读取、下载或重新导入 CWT
- 自动监控或自动刷新 Vanilla 索引

## Phase 0：工程和设计基线

交付：

- 初始化 Git 与 Rust workspace
- 建立 `pdx-*` crate、grammar、generated rules、editor、docs、tests、fuzz 目录
- 配置 format、clippy、test、dependency audit 和基础 CI
- 接受 RFC 0001–0012
- 验证 Zed 从 monorepo grammar 目录构建的可行性
- 验证 Zed 下载/查找多平台 `pdx-ls` 的发布路径

退出条件：

- 空 crate 图可以构建且无环。
- 核心 crate 不依赖 Zed 或具体 EU4 Rust 类型。
- workspace 中不存在无 `pdx-` 前缀的项目 Cargo package；binary 为用户 CLI `pdx`、server `pdx-ls` 和维护者工具 `pdx-cwt`。
- grammar 和 server distribution spike 都有书面结论。

## Phase 1：Zed 与 Tree-sitter

交付：

- `tree-sitter-pdx-script`
- `tree-sitter-pdx-eu4-localisation`
- grammar corpus tests
- Zed `highlights.scm`、`brackets.scm`、`indents.scm`、`outline.scm`
- Zed dev extension
- 推荐的 EU4 workspace file type glob 配置

必须覆盖的 PdxScript 语法：

- property、裸 value、嵌套 block、mixed block
- 八种 operator
- quoted/unquoted scalar
- line comment
- header block，例如 `rgb { ... }`
- EU4 conditional parameter block
- 重复 key
- 不完整 string/block/operator 的错误恢复

退出条件：

- corpus 全部通过。
- 任意 corpus 的单字符删除不会导致 parser panic。
- 示例 Mod 在 Zed 中能稳定识别并高亮。

## Phase 2：最小 Language Server

状态：`completed`（2026-07-18；stdio runtime、文档同步和内存 JSON-RPC 集成测试完成）

交付：

- `initialize`, `initialized`, `shutdown`, `exit`
- `didOpen`, incremental `didChange`, `didClose`
- open document version tracking
- URI、UTF-8 byte、UTF-16 position 转换
- Zed 启动 `pdx-ls`
- 内存 transport integration tests

退出条件：

- 乱序 document version 不覆盖新版本。
- `didClose` 后恢复磁盘 candidate。
- emoji、CJK、组合字符位置测试通过。

## Phase 3：Typed CST、Syntax Diagnostics 与 Formatter

状态：`completed`（2026-07-18）；纯 Rust typed CST、stable syntax error mapping、保守 formatter、
独立 fuzz targets 和编辑后/full parse 等价性已实现并通过验证。运行时不依赖 Tree-sitter C；
Tree-sitter grammar 仅保留给 Zed 编辑器侧高亮和 grammar corpus。

交付：

- typed CST facade
- localisation 与受支持 EU4 CSV 的独立 typed parser facade
- 编辑更新与 full reparse 的等价性
- syntax error mapping
- 保留 trivia 的全文 formatter
- parser、edit-update、formatter fuzz targets

退出条件：

- formatter 对合法 corpus 幂等。
- 格式化不丢失或移动注释到不同语义节点。
- 含不安全 `ERROR` node 的文档不生成破坏性 edit。

## Phase 4：CWT 导入、EU4 权威规则数据库与 Workspace Index

状态：`implemented, acceptance reopened`（2026-07-20 重新审计）；规则导入、SQLite runtime、root/overlay 和基础 shard 已实现，但 LSP 未接入 dependency/Vanilla 配置，Vanilla cache 未持久化，单文件更新链路尚未真正替换 HIR/index shard。

交付：

- `pdx-cwt v0.1` importer 和本次 EU4 corpus construct inventory
- `pdx-cwt` 的最小原创 CWT importer fixtures；不提交权威 CWT source tree
- 一次性转换 CWT type/alias/enum/value/cardinality/path/scope/reference/documentation/directive metadata
- 生成 SQLite `eu4.pdxrules`、规范化 manifest 与唯一 `rule_hash`，并提交 artifact
- 只读加载 SQLite 后冻结为内存 `Eu4Rules`
- 由数据库 path/type descriptor 生成完整的 EU4 file category catalog
- vanilla/dependency/current mod source roots
- open document overlay
- 按 file/symbol category 配置的 `ReplaceBySymbol`、`ReplaceByRelativePath`、`Merge`、`Unique` resolution
- EU4 数据库驱动的 type/enum/variable/localisation/filepath definition/reference shards
- Vanilla 首次索引缓存和显式手动刷新入口
- Event、Scripted Effect、Scripted Trigger、Localisation 的强制回归场景

退出条件：

- 来源候选顺序符合 overlay > current mod > ordered dependencies > Vanilla。
- file change 只替换自己的 shard。
- 被覆盖 definition 可被解释但不作为活动跳转目标。
- 本次 bootstrap corpus 中不存在被静默忽略的 CWT 构造。
- 对相同数据库逻辑内容计算相同 `rule_hash`，不受 SQLite 页布局、VACUUM 或写入顺序影响。
- 每个数据库声明的可支持文件类别至少有 classification/parse fixture；主要语义类别有 definition/reference fixture。
- 更新扩展携带新的规则和 `rule_hash`；项目配置不 pin `rule_hash`。

## Phase 5：语言功能

状态：`implemented, acceptance reopened`（2026-07-20 重新审计）；主要查询已实现并有回归，但 query-time workspace 重解析和深拷贝 snapshot 尚未满足性能与增量边界。

交付：

- syntax、unknown key、unknown symbol、ambiguous symbol、unknown scope diagnostics
- key/command、value、localisation、symbol completion
- hover
- definition、references、document/workspace symbol
- 不完整输入、未解析/歧义 symbol、未知 scope 的 unit/golden 回归，以及真实内存 JSON-RPC integration

退出条件：

- 所有 handler 都委托给 `pdx-analysis`。
- incomplete CST 中 completion 仍有结果。
- unresolved/ambiguous symbol 不产生随机跳转。
- scope unknown 不产生级联 scope errors。
- Eu4Rules 声明的所有可支持文本类别都至少获得 syntax diagnostics；具有结构/语义 descriptor 的类别获得对应 semantic features。
- Vanilla 与依赖 Mod参与解析、索引和查询，但默认不向编辑器发布其 diagnostics；当前 Mod和未保存文件正常发布。

## Phase 6A：Rename 与 v0.1

状态：`implemented, not released`（2026-07-20 重新审计）；rename 内部链路和开发机 smoke 已通过，但自动获取 server、rules artifact 打包、formatter capability、跨平台 release 和干净 clone 安装尚未闭环。

交付：

- [x] prepare rename
- [x] resolved definition/reference rename
- [x] name validation 和 conflict detection
- [x] WorkspaceEdit
- [x] 只修改当前 Mod和属于当前 Mod 的未保存 overlay

退出条件：

- 不修改 Vanilla 或依赖 Mod；定义位于只读来源时直接拒绝 rename。
- ambiguous reference 拒绝 rename。
- rename 后重新分析无新增 unresolved reference。

完成重新打开的 Phase 4–6A 验收条件后发布 `v0.1.0`。

## Phase 6B 与 Phase 7

`v0.2` 再实现 Semantic Tokens、Code Action 与 Quick Fix。之后再评估其他编辑器客户端。任何新功能都排在增量架构和可安装 EU4 v0.1 之后。

## 建议质量预算

这些是基准目标，不是通过删除诊断换取的硬承诺：

- 单文件 Rust full parse：典型文件 P95 小于 20 ms；后续纯 Rust 增量优化必须保持同一可观察结果
- completion：已完成初始索引时 P95 小于 100 ms
- hover/definition：P95 小于 50 ms
- semantic diagnostics：编辑后 200 ms debounce，可取消
- 单文件更新：不得触发无关文件重新 parse

性能门槛应在真实可再分发 fixture 建立后再校准。
