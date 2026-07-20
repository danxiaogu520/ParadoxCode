# ParadoxCode 实施计划

> 本文件是 ParadoxCode 从设计基线走向可工作的 EU4 语言工具链的执行计划。
> 具体设计以 `docs/` 下的 RFC 为准；如果实现需要改变已接受的边界，先更新对应 RFC，再更新本计划。

> 2026-07-20 scope amendment：项目调整为通用 `pdx-lsp` 引擎、EU4-first。EU4 仍是当前唯一交付目标，其他游戏优先级低且没有版本承诺；通用边界和迁移原则见 `docs/rfc/0013-generic-engine-eu4-first.md`。

## 1. 项目目标

ParadoxCode 为 Europa Universalis IV（EU4）Mod 提供面向 Zed 的语言工具链，首个可交付版本为 `v0.1.0`。用户在打开一个 EU4 Mod workspace 后，应能获得：

- PdxScript、EU4 localisation 和受支持 CSV 的语法识别与高亮基础；
- syntax、unknown key、unknown symbol、scope 等诊断；
- key、effect、trigger、symbol、localisation 的补全、悬停、定义和引用查询；
- document symbol、workspace symbol 和安全的语义 rename；
- 保守、幂等、保留注释和非 trivia token 的全文格式化；
- 未保存文档、当前 Mod、有序依赖 Mod、Vanilla 的正确覆盖解析；
- 由自有 SQLite `eu4.pdxrules` 驱动的 EU4 文件分类和语义分析；
- 首次建立、持久化，并仅通过用户显式操作刷新的 Vanilla 索引缓存。

核心分析不依赖编辑器。Zed extension 是薄客户端，`pdx-ls` 只负责 LSP 生命周期和协议转换，语言功能集中在 `pdx-analysis`。

## 2. 当前基线

当前工作区已经实现 Phase 0–6A 的主要 EU4 功能原型，但 2026-07-20 重新审计确认它仍是 alpha：架构文档描述的 HIR/cache/snapshot/取消模型和普通用户发布链路尚未完全落地。后续先完成发布前修复阶段，再进入 Phase 6B：

- `docs/architecture.md`：总体架构、数据流、crate 依赖和并发模型；
- `docs/mvp.md`：MVP 成功定义、Phase 0–7 计划和质量预算；
- `docs/rfc/0001`–`docs/rfc/0012`：系统边界、语法、VFS、规则数据库、HIR、索引、语言功能、格式化、LSP、Zed、测试和 CWT 导入约束；
- `reference/`：只读的 CWTools、EU4 Config、Jomini 调研 checkout，不是构建依赖；
- Phase 0–6A 已建立 Rust workspace、EU4 parser、CSV/localisation facade、Zed syntax assets、JSON-RPC/LSP runtime、规则数据库、workspace overlay/index、规则驱动语言功能和安全 rename；这些能力是迁移基线，不代表 v0.1 已完成发布。

因此第一步不是实现语言功能，而是建立能够持续验证这些设计的最小工程骨架。

## 3. 范围和明确的非目标

### 本次范围

- 游戏：只支持 EU4 的最新/最终版本；
- 客户端：先支持 Zed；
- 服务：`pdx-ls`；
- CLI：用户入口为 `pdx`，规则维护工具为 `pdx-cwt`；
- 规则：一次性从固定的 EU4 CWT corpus 导入自有 `eu4.pdxrules`，运行时不读取 CWT；
- 文件：规则数据库声明的所有可支持文本文件类别，按类别选择 PdxScript、localisation、CSV 或资源路径索引；
- 发布：完成 Phase 1–6A 退出条件后发布 `v0.1.0`。

### 不在 MVP

- 当前版本实现其他游戏 profile、动态插件 ABI 或多游戏 workspace；
- VS Code extension；
- Semantic Tokens、Code Action、Quick Fix、Inlay Hint、Code Lens、Document Link；
- 存档、二进制和媒体内容的语义解析；
- EU4 运行时模拟；
- 历史版本规则矩阵；
- `pdx-cwt` 的 CRUD、历史、diff、rollback、持续同步和 CWT 导出；
- 自动监控或自动刷新 Vanilla 索引。

## 4. 目标结构

目标仓库布局如下，具体目录可随实现调整，但依赖方向不能反转：

```text
crates/
  pdx-text/
  pdx-syntax/
  pdx-rules/
  pdx-game-eu4/
  pdx-eu4/             # temporary compatibility facade
  pdx-cwt/
  pdx-hir/
  pdx-workspace/
  pdx-analysis/
  pdx-format/
  pdx-lsp/
  pdx-cli/
grammars/
editors/zed/
rules/
tests/
fuzz/
docs/
reference/
```

依赖主链：

```text
pdx-text
  -> pdx-syntax -> pdx-hir -> pdx-workspace -> pdx-analysis -> pdx-lsp
  -> pdx-format                                      -> pdx-cli
pdx-rules -> pdx-hir / pdx-workspace / pdx-analysis / pdx-lsp
pdx-rules + pdx-game-eu4 -> pdx-cwt
```

关键对象包括 `AnalysisHost`、`AnalysisSnapshot`、`SourceRoot`、`SourceFile`、`OpenDocumentOverlay`、`ParsedFile`、`HirFile`、`FileIndexShard`、`WorkspaceIndex`、`RuleSet`、`Eu4Profile` 和 `VanillaIndexCache`。跨请求身份使用稳定 ID，不使用绝对路径字符串或 CST node pointer。

## 5. 分阶段路线

所有阶段都应以小的、可审查的增量交付。阶段完成后满足退出条件，才进入下一阶段；阶段中的实现顺序可以在不改变边界的前提下调整。

### Phase 0：工程骨架和设计基线

状态：`completed`（2026-07-16）

工作项：

- [x] 初始化 Rust workspace、基础目录和最小 `pdx-*` crate；
- [x] 创建 `pdx`、`pdx-ls`、`pdx-cwt` 的 CLI/server 占位入口；
- [x] 配置 rustfmt、clippy、测试、文档检查、依赖许可证/安全审计和基础 CI；
- [x] 建立 grammar、fixture、fuzz、Zed extension 的最小目录；
- [x] 验证 Zed 从 monorepo grammar 目录构建的可行性，并记录 local `file://` 与发布镜像方案；
- [x] 验证多平台 `pdx-ls` 发布、查找和下载路径，并记录 artifact matrix；
- [x] 将 RFC 0001–0012 的设计约束转成 crate-level 文档和最小 API 骨架。

退出条件：空 crate 图可构建且无环；核心 crate 不依赖 Zed；EU4 语法和规则边界在核心 crate 内固定；所有 workspace package 使用 `pdx-` 前缀；grammar 和 server distribution spike 有书面结论。

说明：当前目录不是 Git checkout，且 `agent.md` 禁止代理自行初始化或清理仓库状态，因此 Git 仓库初始化由宿主环境负责；这不影响 Cargo workspace、CI 和 Phase 1 的实现入口。

依赖：无。

### Phase 1：Zed 与 Tree-sitter grammar

状态：`completed`（2026-07-17；源码与自动化质量门禁完成）

工作项：

- [x] 实现 `tree-sitter-pdx-script`；
- [x] 实现 `tree-sitter-pdx-eu4-localisation`；
- [x] 为受支持 CSV 确定独立 Rust parser facade，并提供 editor-only CSV grammar；
- [x] 添加 grammar corpus、错误恢复和单字符删除测试；
- [x] 添加 Zed 的 highlights、brackets、indents、outline queries；
- [x] 建立 Zed dev extension、language metadata 和文件识别策略。

PdxScript 至少覆盖 property、裸 value、嵌套/混合 block、八种 operator、quoted/unquoted scalar、注释、header block、conditional parameter block、重复 key 和不完整 string/block/operator。

退出条件：corpus 全部通过；任意 corpus 单字符删除不导致 parser panic；Zed manifest、metadata 和 query 编译检查通过；extension 源码不包含 effect/trigger 名称表或 scope 规则。当前环境没有可自动控制的 Zed GUI，因此示例 Mod 的最终识别/高亮仍应在发布前按编辑器手册执行一次宿主环境 smoke test。

依赖：Phase 0。

### Phase 2：最小 Language Server

状态：`completed`（2026-07-18；stdio runtime、文档同步与内存 transport 集成测试完成）

工作项：

- [x] 实现 `initialize`、`initialized`、`shutdown`、`exit`；
- [x] 实现 `didOpen`、增量 `didChange`、`didClose`；
- [x] 跟踪文档版本和 open document overlay；
- [x] 完成 URI、路径、UTF-8 byte、UTF-16 position 转换；
- [x] 让 `pdx-ls` 通过 stdio 启动并完成最小握手；
- [x] 用内存 transport 编写真实 JSON-RPC/LSP integration tests。

退出条件：乱序版本不会覆盖新版本；`didClose` 恢复磁盘 candidate；emoji、CJK、组合字符位置测试通过；请求在未 initialize 时得到规范错误；取消和 stale result 不会污染当前状态。当前实现已通过内存 transport 集成测试；真实 Zed 安装 smoke test 仍属于宿主环境发布检查。

依赖：Phase 0、Phase 1 的语言识别 spike。

### Phase 3：Typed CST、syntax diagnostics 与 formatter

状态：`completed`（2026-07-18；纯 Rust typed CST facade、syntax error mapping、保守 formatter、fuzz targets 和编辑后/full parse 等价性已通过验证）

工作项：

- [x] 在 syntax crate 上建立 typed CST facade；
- [x] 实现 localisation 和受支持 CSV 的独立 typed parser facade；
- [x] 接入 revision-safe 编辑更新，并验证与 full reparse 的可观察结果一致；
- [x] 将 syntax error 映射到稳定 diagnostic；
- [x] 实现保留 trivia 的保守全文 formatter；
- [x] 添加 parser、增量编辑、formatter fuzz target；

编辑更新使用纯 Rust parser 重新构建 CST；结果同时与 full reparse 的 typed CST、token 序列和 diagnostics 做等价性验证。Tree-sitter 仅留在 Zed grammar 资产，不进入核心运行时。

退出条件：合法 corpus 格式化幂等；不丢失或跨语义节点移动注释；含不安全 `ERROR` node 的文档不生成破坏性 edit；所有输出 range 位于 source 范围内。

依赖：Phase 1、Phase 2 的文本同步基础。

### Phase 4：CWT 导入、规则数据库和 Workspace Index

状态：`implemented, acceptance reopened`（2026-07-20）；SQLite runtime、原创 CWT importer、root/overlay resolver、file shards 和主要语义回归已实现，但 dependency/Vanilla LSP 配置、持久化 cache 和真实单文件更新链路仍需完成。

这是 MVP 中最大的一阶段，应拆成“规则 schema/runtime”和“importer”两个可独立审查的序列。

规则数据库：

- [x] 定义 SQLite schema、稳定 logical identity、schema version 和 runtime read-only loader；
- [x] 实现 canonical logical projection 和版本化 `rule_hash`；
- [x] 生成 `rules/eu4.pdxrules` 与 manifest/report；
- [x] 为 EU4 path/type descriptor 生成 file category catalog；
- [x] 校验 foreign key、stable ID、matcher/reference、schema 和 hash invariants。

CWT importer：

- [x] 按规范路径发现并排序显式输入；
- [x] 保留重复规则、source order、alternative identity、node/leaf/value clause 形状；
- [x] 关联 `##` directive 和 `###` documentation；
- [x] 导入 type、subtype、alias、enum、scope、link、effect、trigger、modifier、localisation、folder/path metadata；
- [x] 未知 construct 保留为 normalized `cwt_nodes`，不得静默丢弃；
- [x] 使用单事务写入、临时文件和 atomic replace；
- [x] 输出 import report、provenance、输入 hash 和 logical row counts。

Workspace/index：

- [x] 实现 vanilla、dependency、current mod source roots；
- [x] 实现 open document overlay；
- [x] 按 file/symbol category 实现 `ReplaceBySymbol`、`ReplaceByRelativePath`、`Merge`、`Unique` resolution seam；
- [x] 实现 EU4 type/enum/variable/localisation/filepath definition/reference shard seam；
- [x] 实现 Vanilla 首次索引缓存和显式手动刷新入口；
- [x] 添加 Event、Scripted Effect、Scripted Trigger、Localisation 强制回归场景。

退出条件：已验证来源顺序为 overlay > current mod > ordered dependencies > Vanilla；单文件变化只替换自身 shard；被覆盖 definition 可解释但不是活动跳转目标；73 文件 bootstrap corpus 无被静默忽略的构造；相同逻辑数据库内容产生相同 `rule_hash`；文件分类、解析和 Event/Scripted Effect/Scripted Trigger/Localisation definition fixture 已通过。

依赖：Phase 3；CWT 调研基线见 `docs/reference-study.md` 和 RFC 0012。

### Phase 5：语言功能

状态：`completed`（2026-07-20 重新验收）；查询功能和 LSP integration 已实现，query-time 全 workspace 重解析、深拷贝 snapshot，以及 event-loop 上的 workspace scan、parse/lower、diagnostics 和语言查询已消除；initialize scan 与 analysis 内部协作式取消均有回归。

工作项：

- [x] 实现 syntax、unknown key、unknown symbol、ambiguous symbol、scope diagnostics；
- [x] 实现 key/command、value、localisation、symbol completion；
- [x] 实现 hover；
- [x] 实现 definition、references、document symbol、workspace symbol；
- [x] 将所有 handler 委托给 `pdx-analysis`，LSP 层只做协议转换；
- [x] 为 incomplete CST、unresolved symbol、ambiguous symbol、unknown scope 添加回归测试；
- [x] 默认只向编辑器发布当前 Mod 和未保存文件 diagnostics。

退出条件：不完整输入仍有可用 completion；unresolved/ambiguous symbol 不产生随机跳转；unknown scope 不产生级联 scope errors；所有可支持文本类别至少有 syntax diagnostics；具有 descriptor 的类别获得相应 semantic features。

依赖：Phase 4。

### Phase 6A：Rename 与 v0.1 发布

状态：`implemented, not released`（2026-07-20）；prepare rename、safe WorkspaceEdit 和只读来源保护已实现，自动安装、规则包获取、formatter LSP、跨平台 release 与干净 clone smoke 尚未完成。

工作项：

- [x] 实现 prepare rename；
- [x] 只对已解析且无冲突的 definition/reference 生成 WorkspaceEdit；
- [x] 校验名称、冲突和修改范围；
- [x] 只修改当前 Mod 和属于当前 Mod 的未保存 overlay；
- [x] 拒绝修改 Vanilla、依赖 Mod 或定义位于只读来源的 symbol；
- [x] 完成发布构建、Zed 安装/启动 smoke test 和用户文档。

退出条件：rename 后重新分析不产生新增 unresolved reference；ambiguous reference 直接拒绝；Phase 1–6A 全部退出条件通过；无已知 parser/formatter crash；规则数据库、manifest、`rule_hash` 和 extension asset 一致；支持平台完成 Zed smoke test。

依赖：Phase 5。

重新满足所有退出条件后发布 `v0.1.0`。

### Phase R：通用引擎边界与发布前架构修复

状态：`in progress`（2026-07-20）

按可独立验证的小切片执行：

1. 接受 RFC 0013，拆分通用规则 runtime 与 EU4 profile，迁移期间保留兼容 re-export（`pdx-rules`、`pdx-game-eu4`、schema 12 `game_id` 校验与 `pdx-eu4` facade 已建立；data-only profile 已显式贯穿 CLI/LSP/host/snapshot，workspace symbol/reference 与 analysis scope/key/member fallback 已迁移；`pdx-eu4` facade 删除和 syntax 历史 API 重命名待后续独立切片）；
2. 实现真实 per-file HIR/FileState，overlay 变化只更新一个文件（FileState、磁盘复用、overlay 按版本 parse/lower cache，以及供 workspace shard/analysis 共享的 property/localisation/scalar 与 profile-aware definition/reference HIR facts 已完成；scope/CWT typed lowering 待完成）；
3. 将 snapshot 改为共享不可变状态，查询创建 snapshot 时不深拷贝 workspace 文本和索引（已完成）；
4. 删除 analysis query-time 全 workspace 重解析，查询只读取当前 HIR 与 WorkspaceIndex（已完成）；
5. 增加 index bulk build 和真正的单 shard 增量 replacement（已完成）；
6. 修复稳定 SourceFileId、symlink 顺序、文件大小/深度/数量限制和错误隔离（已完成）；
7. 将 LSP transport 迁移到类型化协议层，增加 worker、debounce、版本门和在途取消（已完成：stdio reader 分离，initialize 候选 host scan worker，prepared-document parse worker/三重提交门，semantic diagnostics 200ms debounce，snapshot request worker，共享 cancellation token 与 analysis 内部 checkpoint；workspace scan 覆盖目录/读取/parse/lower/index 检查点并有取消原子性回归；`lsp-types` 接管当前声明能力覆盖的标准 params、initialize result/capabilities、diagnostics 和语言功能 response，轻量 JSON-RPC framing 有意保留）；
8. 接入 formatting、dependency roots、Vanilla cache 持久化和文件变化更新；
9. 建立大型 synthetic workspace benchmark 与“编辑一个文件只 parse/lower 一次”计数测试（已完成：默认 2,000 个原创 EU4 event 文件，覆盖 cold/unchanged/单磁盘变化/单 overlay 编辑；线程局部测试计数器证明 overlay 编辑 parse/lower 各一次且不重建磁盘 `FileState`）；
10. 完成 Zed 自动获取、多平台 release、checksum 和干净 clone 端到端安装测试。

Phase R 完成前不开始 Semantic Tokens、Quick Fix、其他游戏 profile 或新的编辑器客户端。

### Phase 6B：v0.2 与后续

状态：`future`

- Semantic Tokens；
- Code Action 与 Quick Fix；
- 其他经过实际用户需求验证的编辑器能力。

### Phase 7：VS Code 薄客户端

状态：`future`

为 VS Code 编写复用 `pdx-ls` 的薄客户端。不得把语言分析规则复制进客户端。

## 6. 建议的垂直切片

为了尽早验证端到端链路，第一条可运行切片应围绕少量原创 fixture 完成：

```text
PdxScript fixture
  -> CST
  -> syntax diagnostic
  -> HIR lowering
  -> file index shard
  -> workspace snapshot
  -> pdx-analysis query
  -> LSP response
  -> Zed smoke test
```

建议先覆盖 Event、Scripted Effect、Scripted Trigger 和 Localisation 四类基准对象，再扩展到规则数据库声明的其他类别。它们是回归基准，不是最终 symbol 范围上限。

每个切片应同时提交：实现、最小原创 fixture、unit/golden/integration test、必要的 RFC/README 更新，以及已知限制。

## 7. 质量门禁

每个 pull request 至少应验证：

- `cargo fmt --check`；
- workspace/all-targets clippy，warnings denied；
- unit、integration、doc tests；
- Tree-sitter corpus；
- `eu4.pdxrules` schema/invariant validation；
- manifest `rule_hash` 与数据库 canonical logical content 一致；
- logical hash 对插入顺序、SQLite index、VACUUM 和物理重建保持稳定；
- runtime loader smoke test；
- Zed extension manifest/build check；
- 依赖许可证和 advisory policy；
- fuzz target 编译和短 smoke。

定时任务再运行长 fuzz、性能趋势和跨平台 build。性能目标作为基准和回归信号，不通过牺牲诊断正确性达成。

建议基准：单文件 Rust full parse P95 < 20 ms；后续纯 Rust 增量优化必须保持同一可观察结果；completion P95 < 100 ms、hover/definition P95 < 50 ms、编辑后 semantic diagnostics 约 200 ms debounce 且可取消。

## 8. 关键风险与决策门

| 风险 | 早期信号 | 处理方式 |
| --- | --- | --- |
| CWT corpus 有未建模构造 | import report 出现 unknown/ignored construct | 在发布前实现 mapping 或人工批准 non-semantic；禁止静默跳过 |
| CST/formatter 丢失信息 | 注释、重复 key 或错误节点在 round-trip 中改变 | 保留 trivia 和 source order；不安全时不生成 edit |
| source root 覆盖错误 | 跳转命中被覆盖 definition 或 overlay 被忽略 | 用 resolution policy fixture 固化顺序和 category 行为 |
| 规则 hash 不稳定 | VACUUM/插入顺序改变 `rule_hash` | 只 hash canonical logical projection，加入稳定性测试 |
| LSP 层积累业务逻辑 | handler 直接访问磁盘或 EU4 名称表 | 将查询移入 `pdx-analysis`，LSP 只做生命周期和转换 |
| Zed 文件识别冲突 | 宽泛 `.txt` 关联误伤其他语言 | 由 EU4 规则生成项目级 glob；保留手动选择 fallback |
| 版权/分发污染 | Vanilla 或原生 CWT 出现在提交/发布包 | 只提交自有 fixture、数据库和 provenance；检查 ignore 与 CI |
| 后台任务产生旧结果 | 编辑后 diagnostics 回退 | 版本校验、可取消任务、不可变 snapshot 和 shard replacement |

## 9. 完成定义

只有同时满足以下条件，ParadoxCode 才算完成 `v0.1.0`：

1. Phase 1–6A 的实现和退出条件全部通过；
2. Zed 能安装 extension、启动 `pdx-ls`，并加载 extension 携带的 `eu4.pdxrules`；
3. 当前 Mod、依赖、Vanilla 和未保存 overlay 的解析顺序有自动化回归测试；
4. 主要语言功能均通过真实 LSP JSON-RPC integration test，而不是仅测试内部 handler；
5. `rule_hash`、schema、manifest、runtime loader 和 importer report 均通过校验；
6. 不提交 Vanilla 游戏文件、用户缓存或权威 CWT source tree；
7. 文档中的 initialize options、EU4 rules schema、CLI 和实际实现一致；
8. 已知限制、平台支持和安装方式写入发布文档。

## 10. 文档维护规则

- 架构或边界变化：先改对应 RFC，再改本文件；
- 阶段状态、交付项或退出条件变化：改本文件，并在变更说明中写明原因；
- 新增 crate、CLI、配置项或 artifact：同步更新 `docs/README.md` 和相关 RFC；
- 外部调研结论：记录来源、commit、许可证和是否进入构建链；
- 示例必须是 ParadoxCode 自有原创样例，不直接复制游戏文件或参考仓库 corpus。
