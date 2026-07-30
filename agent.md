# ParadoxCode Agent Guide

本文件是参与 ParadoxCode 的代理和开发者的工作约定。目标是让每次改动都能沿着既定架构推进，并留下可验证、可维护、可追溯的结果。

## 0. 持久项目授权与产品方向

项目所有者已授权代理持续负责 ParadoxCode 的技术执行，包括规划、架构判断、实现、重构、测试、性能验证和文档维护。除破坏性操作、外部发布、凭据/付费操作以及会实质改变产品方向的决定外，代理应自行作出合理技术判断并继续推进，不需要逐项等待确认。

默认协作规则：

- 保护用户已有和未提交的修改，不擅自清理、覆盖或回退；
- 采用小步、可审查、可回滚的改动，不进行无法验证的一次性大重写；
- 行为保持型重构与新功能分开交付；
- 自动化验证未通过时不得把阶段标记为完成；
- 遇到普通实现细节自行决策，遇到破坏性操作、外部发布或重大产品分歧再请求用户决定；
- 每个阶段记录结果、设计变化、验证、剩余风险和下一步；
- 文档状态必须反映真实端到端能力，不以已有骨架或单元测试代替产品闭环。

产品方向固定为“通用 `pdx-lsp` 引擎，EU4-first”：

- 当前版本只要求把 EU4 支持做完整、可靠和可发布；
- workspace、snapshot、索引、分析查询、LSP runtime、CLI 和发布设施应保持游戏无关；
- EU4 的路径、scope、command、symbol 和特殊语义应集中在 EU4 profile/模块中；
- 暂不承诺其他游戏的交付时间，也不为尚不存在的第二个实现设计复杂插件 ABI 或大量推测性 trait；
- 优先用规则数据和 profile 表达游戏差异，只有在第二个真实游戏证明数据不足时再提炼行为接口；
- 新增其他游戏时，应能复用核心引擎，而不需要重写 workspace、索引、LSP 和并发模型。

## 1. 先理解项目

开始任何实现前，按以下顺序阅读：

1. `docs/README.md`：当前决策、文档索引和文档状态；
2. `docs/architecture.md`：数据流、crate 依赖、并发、身份和错误恢复；
3. `docs/mvp.md`：MVP 成功定义、非目标、阶段和退出条件；
4. 与当前任务直接相关的 RFC；
5. `docs/rfc/0015-first-party-rule-source.md`：第一方规则源码、编译和禁止外部规则输入的边界；
6. `plan.md`：当前执行顺序、阶段状态和风险。

不要把 `reference/` 当作生产代码或可直接复制的测试 corpus。它只用于研究。

## 2. 代理的默认职责

你不是只负责“把代码写出来”，而是负责在现有设计边界内完成一个可验证的垂直切片：

- 先确认任务属于哪个 Phase、哪个 crate 和哪个 RFC；
- 先检查现状和已有未提交改动，再决定编辑范围；
- 以最小增量实现，避免提前引入没有性能证据的框架；
- 同步添加能证明行为的测试或 fixture；
- 如果实现改变了架构约束、公开 API、规则 schema、CLI 或配置，更新相关文档；
- 最后运行与改动相称的验证，并明确报告未运行的检查和剩余风险。

没有实现代码时，先建立最小骨架和验证路径，不要直接堆叠完整语言服务功能。

## 3. 权威性和设计变更

设计冲突时按以下优先级处理：

1. 用户当前明确要求；
2. 已接受的 RFC 和 `docs/architecture.md`；
3. `plan.md` 的阶段约束；
4. 尚未接受的提案、调研笔记和实现便利性。

如果必须改变已接受的边界：

- 先写清问题、备选方案、影响和迁移成本；
- 修改对应 RFC 的状态、决策和验收条件；
- 再修改实现计划和代码；
- 在交付说明中指出这是设计变更，而不是普通重构。

不要为了让测试通过而悄悄放宽规则、吞掉未知输入、绕过 source root resolution 或把业务逻辑塞进 LSP 层。

## 4. 架构边界

必须维持以下依赖方向：

```text
pdx-text
  -> pdx-syntax -> pdx-hir -> pdx-workspace -> pdx-analysis -> pdx-lsp
  -> pdx-format
pdx-game -> pdx-game-eu4
pdx-rules -> pdx-rulec
pdx-rules + pdx-game-eu4 -> pdx-hir / pdx-workspace / pdx-analysis
```

通用规则 runtime 已迁入 `pdx-rules`，EU4 bootstrap/profile 已迁入 `pdx-game-eu4`，迁移期 `pdx-eu4` re-export 已删除。后续继续隔离 analysis/HIR 中的 EU4 特有语义，避免一次性重写。

各层职责：

| 层 | 允许负责的事情 | 不应负责的事情 |
| --- | --- | --- |
| `pdx-text` | offset、line index、UTF-8/UTF-16、URI/path 基础 | EU4 规则、workspace 状态 |
| `pdx-syntax` | Paradox script、localisation、CSV 的 loss-aware CST、增量 parse、syntax error | 游戏规则数据库、磁盘扫描、LSP 类型 |
| `pdx-rules` | 通用规则 schema、canonical view、`rule_hash`、只读 runtime API | 具体游戏名称表、外部规则 parser、LSP、动态 Mod symbol |
| `pdx-game` | 数据驱动安装标志、跨平台发现、最小验证、用户级本机配置 | 具体游戏名称、语义规则、workspace 索引、编辑器 API |
| `pdx-game-eu4` | EU4 profile、路径、scope、command、symbol 和特殊语义 | LSP、workspace 可变状态、编辑器 API |
| `pdx-rulec` | 严格读取第一方 JSON 规则源码、校验并生成 artifact/manifest | CWT 输入、runtime 依赖、网络同步、用户规则覆盖 |
| `pdx-hir` | 基于 typed CST、RuleSet 和游戏 profile 的 lowering、scope | 编辑器 API、磁盘 I/O |
| `pdx-workspace` | VFS、overlay、source roots、parse/HIR cache、index shards、snapshot | LSP protocol types |
| `pdx-analysis` | 面向 snapshot 的 diagnostics/completion/hover/navigation/rename 查询 | 直接读磁盘、editor client |
| `pdx-format` | 安全 formatter 和 edit 生成 | 语义修复、破坏性重写 |
| `pdx-lsp` | 生命周期、capability、协议转换、取消、publish diagnostics | EU4 名称表、业务查询、规则解释 |
| `editors/zed` | language metadata、queries、server 获取/校验/启动、配置传递 | symbol 提取、scope 推导、EU4 规则实现或规则 artifact 分发 |

`AnalysisHost` 是可变状态的拥有者；请求读取不可变 `AnalysisSnapshot`，查询期间不持有 host 锁。后台结果提交前必须校验文档版本或 snapshot 身份。

## 5. 实现不变量

### 数据和身份

- source 优先级固定为：未保存 overlay > 当前 Mod > 有序依赖 Mod > Vanilla；
- `SourceRootId`、`SourceFileId`、`DocumentId`、`SymbolId` 使用稳定身份；
- 不用绝对路径字符串作为跨请求 symbol 身份；
- 不用 CST node pointer 作为跨请求身份；
- 每个文件独立生成和替换 index shard；
- 被覆盖 definition 可以保留用于解释，但不能成为活动跳转目标；
- Vanilla cache 只在首次配置或用户显式刷新时建立/更新，不因 rule hash 或文件变化自动刷新。

### 错误恢复

- syntax error 不阻止局部 CST 产生；
- lowering 遇到未知节点生成 `UnknownConstruct`，不 panic；
- 未知 scope 保留为 `Unknown`，避免级联错误；
- rule compiler 遇到未知字段、重复身份、无效 cardinality/severity 或 artifact round-trip 差异必须失败；
- formatter 遇到不安全 `ERROR` node 返回无编辑和明确原因。

### 规则数据库

- `eu4.pdxrules` 是开发期唯一权威规则 artifact，发布时嵌入官方 `pdx`/`pdx-ls`；
- 官方 runtime 不接受外部规则路径、下载、搜索或用户覆盖；
- runtime 不读取、下载或导入任何外部规则源；
- `.cwt` 文件不得作为规则编译、测试、运行或更新输入；
- `rules/eu4/*.json` 是唯一权威源，`eu4.pdxrules` 与 manifest 是生成物；
- `rule_hash` hash 的是规范化逻辑内容，不是 SQLite 文件 bytes；
- hash 不受 rowid、插入顺序、页布局、index、VACUUM、时间戳和 import log 影响；
- 动态 scripted effect、trigger、building 等成员来自 `WorkspaceIndex`，不得硬编码进核心 crate 或 extension；
- 导入必须保留 source order、重复 key 和 alternative identity，不能先转换成普通 map。

## 6. 推荐工作流

### 开始前

1. `rg --files` 查找相关实现、fixture、脚本和文档；
2. 查看 `git status`；如果工作区不是有效 Git checkout，不要自行初始化或清理仓库状态；
3. 阅读当前任务关联的 RFC 和现有测试；
4. 写出本次改动的最小范围、预期不变量和验证命令；
5. 若发现已有改动，保留其内容，只编辑任务所需的区域。

Git checkout 使用仓库版本化的 `.githooks/pre-commit` 作为本地质量门禁。首次 clone 后运行
`bash scripts/install-git-hooks.sh`；代理发现 `core.hooksPath` 未指向 `.githooks` 时应自行
安装。正常提交直接执行 `git commit`，让 hook 调用 `scripts/check-quality-gates.sh`，无需
在提交前手工重复整套复杂命令。只有诊断某一失败时才单独运行 `core`、`grammars`、`zed`
或 `release` 分组；CI 继续负责 Windows、MSRV 和依赖策略等环境专属门禁。

### 实现中

- 每次改动保持可编译或尽量保持局部可验证；
- 公开 API、diagnostic code、symbol kind、schema rule id 使用稳定命名；
- 用户输入路径、文件内容和配置错误必须显式返回，禁止用 `unwrap`/`expect` 逃逸；
- `unsafe` 默认禁止；若依赖确实需要，封装在最小边界并写出 safety contract；
- 后台任务要可取消，或有明确的资源和时间上限；
- 不为低优先级游戏实现推测性功能或复杂插件系统；只保留已经被核心引擎与 EU4 profile 边界证明有用的扩展点；
- EU4 名称表和特殊规则属于 EU4 profile/规则包，不放进通用 LSP 层或 Zed extension。

### 完成前

- 添加或更新 unit、golden、integration、corpus、property/fuzz 测试中的适用层级；
- 对 parser/formatter 使用原创 fixture，避免复制 Vanilla 或参考仓库 corpus；
- 对规则/compiler 运行 source schema、foreign key、stable ID、canonical hash 和 round-trip 校验；
- 对 LSP 改动运行真实 JSON-RPC transport 测试；
- 对 Zed 改动运行 manifest/build 或可行的 smoke test；
- 更新 `plan.md` 阶段状态，或在交付报告中说明为什么暂不更新；
- 汇报验证结果、未运行的检查和剩余风险。

## 7. 测试策略

按改动层级选择测试，不要只测试最内部的函数：

- `pdx-text`：offset、line endings、UTF-16、URI/path；
- `pdx-syntax`：typed CST、错误提取、增量编辑、错误恢复；
- `pdx-rules`：schema、只读加载、foreign key、hash 稳定性和 runtime invariants；
- `pdx-rulec`：严格 source schema、stable identity、invariant、deterministic hash、artifact round-trip；
- `pdx-hir`：scope transition、unknown context、typed lowering；
- `pdx-workspace`：root order、overlay、覆盖解析、shard replacement、snapshot；
- `pdx-analysis`：diagnostics、completion、definition、references、hover、rename；
- `pdx-format`：trivia safety、token preservation、idempotence；
- `pdx-lsp`：真实 JSON-RPC、capability fallback、版本乱序、取消、stale diagnostics；
- Zed：manifest、Wasm/build、文件识别和 server 启动 smoke test。

MVP fuzz 至少覆盖 Script/localisation parse、incremental edit 等价性、typed CST walk、HIR lowering、formatter、line index、第一方规则源码解析和 EU4 CSV parser。发现的 crash 修复后必须进入 regression corpus。

## 8. 版权、安全和数据边界

- 不提交或再分发 Vanilla EU4 文件、用户本地 Vanilla cache 或外部规则语料；
- `reference/` 只用于研究，不进入正常构建和 runtime；
- 规则 artifact 只包含已确认可再分发的数据和必要 provenance；
- manifest 记录 source format、目标游戏版本、artifact schema、canonical hash 和 artifact checksum；
- Mod/规则配置永不作为任意代码执行；
- 扫描限制文件大小、嵌套深度、路径逃逸和资源消耗；
- compiler 输入路径固定、可复现、稳定排序，并拒绝重复 logical identity；
- atomic publish 前先完成完整 validation，禁止留下半写数据库。

## 9. 交付报告格式

完成任务时，报告应简要包含：

1. 结果：实现了什么，涉及哪些文件；
2. 设计：是否遵循现有 RFC，是否有设计变更；
3. 验证：运行了哪些命令，结果如何；
4. 未完成：哪些检查未运行，原因是什么；
5. 风险：已知限制、后续建议和是否影响下一 Phase。

如果任务被阻塞，先完成所有不依赖外部输入的检查，并说明阻塞点、已尝试的替代路径和需要的最小决策。不要用静默降级掩盖缺失规则、缺失依赖或不确定的解析结果。

## 10. 常用文档入口

- [当前执行计划](plan.md)
- [设计文档索引](docs/README.md)
- [总体架构](docs/architecture.md)
- [EU4 v0.1 MVP 验收基线](docs/mvp.md)
- [系统边界与 crate 依赖](docs/rfc/0001-system-boundaries.md)
- [语法、CST 与增量解析](docs/rfc/0002-syntax-cst.md)
- [Workspace/VFS](docs/rfc/0003-workspace-vfs.md)
- [EU4 规则 artifact](docs/rfc/0004-eu4-rules-schema.md)
- [HIR 与 Scope](docs/rfc/0005-hir-scope.md)
- [Symbol/Reference Index](docs/rfc/0006-symbol-index.md)
- [诊断与补全](docs/rfc/0007-diagnostics-completion.md)
- [安全格式化](docs/rfc/0008-formatter.md)
- [LSP Runtime](docs/rfc/0009-lsp-runtime.md)
- [Zed 集成](docs/rfc/0010-zed-integration.md)
- [测试与质量门禁](docs/rfc/0011-testing-quality.md)
- [第一方规则源码与编译器](docs/rfc/0015-first-party-rule-source.md)
- [通用 PDX 引擎与 EU4-first](docs/rfc/0013-generic-engine-eu4-first.md)
