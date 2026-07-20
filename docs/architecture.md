# ParadoxCode 总体架构

> 2026-07-20 状态说明：通用 `pdx-rules`、EU4 profile、per-file cache、廉价 snapshot 和共享结构 HIR facts 已落地；rule/profile-aware scope lowering、后台取消和发布闭环仍未达到本文约束。迁移计划见 [RFC 0013](rfc/0013-generic-engine-eu4-first.md)。

## 目标

ParadoxCode 提供通用 PDX Mod 语言引擎。EU4 是当前唯一有完整交付承诺的游戏 profile；首个客户端是 Zed，核心分析独立于编辑器。未来编辑器和低优先级游戏 profile 复用同一套 workspace、index、analysis 和 LSP runtime。

MVP 不以少数目录为边界。凡是权威 EU4 规则数据库能够分类的文件类别，都进入与其格式相称的分析范围。PdxScript、localisation 与受支持 CSV 使用独立 parser；媒体、字体、音频、贴图等资源只作为 filepath reference 目标被索引。存档和二进制数据不做内容解析。

当前 EU4 profile 服务项目选定的 EU4 规则基线。规则数据库可以继续由项目维护者修订，每次逻辑修订产生新的 `rule_hash`。通用引擎不假定所有游戏都只有一个版本；版本策略属于各游戏 profile 和规则 artifact metadata。

## 数据流

```text
Editor buffer / current mod / dependency mods / Vanilla cache
                  |
                  v
        Source Text + Line Index
                  |
                  v
       Rust format-specific loss-aware CST
                  |
                  v
   profile path- and rule-aware HIR lowering <---- Immutable RuleSet + GameProfile
                  |
                  v
       Per-file semantic index shard
                  |
                  v
       Immutable workspace snapshot
                  |
                  v
 diagnostics / completion / hover / navigation / rename
                  |
                  v
              LSP adapter
                  |
                  v
                 Zed

One-time CWTools EU4 .cwt bootstrap input
                  |
                  v
          pdx-cwt importer v0.1
                  |
                  v
      self-owned SQLite eu4.pdxrules
                  |
                  v
 canonical logical hashing -> rule_hash
```

## 核心边界

### 编辑器层

Zed extension 只负责：

- 文件识别与 Tree-sitter queries
- 获取或查找 `pdx-ls`
- 启动 Language Server
- 传递 workspace 配置
- 从扩展安装包解析 `eu4.pdxrules`，使用 `pdx-ls --rules <path>` 显式传入
- 首次配置时建立 Vanilla 本地索引缓存，并提供显式“刷新 Vanilla 索引”操作

不得在扩展内实现 symbol 提取、scope 推导或诊断。Tree-sitter grammar/query 只属于这一层；
`pdx-syntax` 核心运行时使用纯 Rust parser，不链接 Tree-sitter C。

### LSP 层

`pdx-lsp` 只负责协议生命周期、capability negotiation、URI/position 转换、请求取消和 diagnostics 发布。所有语言功能调用 `pdx-analysis`。

### 分析层

`pdx-analysis` 是纯查询门面。每个请求读取 `AnalysisSnapshot`，不直接读磁盘，也不持有 editor client。

### 工作区层

`pdx-workspace` 维护 VFS、open document overlay、source roots、parse/HIR cache 和 index shards。可变状态只存在于 `AnalysisHost`；请求使用不可变 snapshot。

### 规则层

`pdx-cwt v0.1` 只负责一次性读取 CWTools 建模的 EU4 `.cwt` 配置并导入自有 SQLite 规则数据库。导入完成后，`.cwt` 不再是项目的权威规则源，也不进入正常构建、发布或运行时。未来其他游戏可以拥有自己的 importer/profile，但不属于 EU4 v0.1。

`pdx-rules` 定义通用 SQLite artifact schema、规范化逻辑视图、`rule_hash` 算法、只读加载与运行时查询 API；`pdx-game-eu4` 提供 EU4 profile 与 bootstrap catalog。`pdx-eu4` 现在只保留临时 re-export facade，待调用方和文档迁移完成后删除。运行时只读加载并冻结 `RuleSet`，schema 12 将 `game_id` 纳入 metadata 与 canonical hash；EU4 组合入口在启动服务前校验 profile 身份。

数据库保存 `TypeKey("scripted_effect")`、`AliasRef("effect")` 等 EU4 静态 matcher；实际 scripted effect、building 等成员来自 `WorkspaceIndex`。EU4 command、scope 和目录规则直接属于核心 EU4 实现，不设计其他游戏的替换层。

## Crate 依赖方向

```text
pdx-text
pdx-syntax    -> pdx-text
pdx-rules     -> pdx-text
pdx-game-eu4  -> pdx-rules + pdx-text + pdx-syntax
pdx-cwt       -> pdx-text + pdx-rules + pdx-game-eu4
pdx-hir       -> pdx-text + pdx-syntax + pdx-rules
pdx-workspace -> pdx-text + pdx-syntax + pdx-rules + pdx-hir
pdx-analysis  -> pdx-workspace + pdx-hir + pdx-rules
pdx-format    -> pdx-text + pdx-syntax
pdx-lsp       -> pdx-analysis + pdx-format
pdx-cli       -> pdx-lsp + selected runtime crates
```

实际约束：

- `pdx-text` 不依赖其他 workspace crate。
- `pdx-syntax` 实现可复用 PDX 文本前端，但不依赖游戏规则数据库、workspace 或 LSP。
- `pdx-rules` 定义 SQLite schema、hash 与只读 runtime view，不依赖具体游戏名称表、CWT parser 或 LSP。
- `pdx-game-eu4` 保存 EU4 profile 和无法由通用规则数据表达的 EU4 特殊语义。
- `pdx-cwt` 依赖 `pdx-text`、`pdx-rules` 与 `pdx-game-eu4`，MVP 只提供 CWT 到 SQLite 数据库的一次性导入工具；它不是 runtime dependency。
- `pdx-hir` 通过稳定的 typed CST API lowering；当前结构 facts 已被 workspace shard 与 analysis 查询共享，`RuleSet`/显式 profile 驱动的 typed semantic facts 和 scope lowering 仍待补齐。
- `pdx-workspace` 不依赖 LSP 类型。
- `pdx-analysis` 不依赖任何 editor API。
- `pdx-lsp` 是唯一允许依赖 LSP protocol types 的核心 crate。
- `editors/zed` 不属于 Rust workspace 的核心依赖图。

## 核心对象

```text
AnalysisHost                mutable owner, applies changes
AnalysisSnapshot            immutable query view
SourceRoot                  vanilla/dependency/current mod
SourceFile                  physical candidate in a root
OpenDocumentOverlay         unsaved text replacing one candidate
ParsedFile                  source text + tree + line index
HirFile                     contextual semantic nodes
FileIndexShard              definitions + references + facts
WorkspaceIndex              merged immutable index
RuleSet                 loaded immutable rules for one game profile
RuleHash                canonical logical rule content hash
GameProfile             game identity and profile-specific semantics
VanillaIndexCache           local persistent Vanilla shard cache; manual refresh only
```

## 并发模型

- LSP event loop 顺序应用文档版本和配置变化。
- parse 与单文件 HIR 更新可在 worker 中执行，但结果提交时必须校验文档版本。
- 查询获取 snapshot 后不持有 host 锁。
- workspace scan 和 semantic diagnostics 可取消。
- completion/hover 优先于后台全量诊断。

MVP 不立即引入通用增量计算框架。先使用清晰的按文件 cache key 和 shard replacement；只有性能数据证明需要时再评估 query framework。

当前 alpha 的同步 transport、深拷贝 snapshot 和 query-time workspace 重解析不满足以上并发模型，属于发布前必须消除的实现缺口，而不是允许长期保留的简化。

## 稳定身份

- `SourceRootId`：一个加载来源。
- `SourceFileId`：一个物理候选文件，生命周期内稳定。
- `LogicalPath`：相对 EU4 根的规范路径。
- `DocumentId`：打开文档 URI 对应的编辑器身份。
- `SymbolId`：由 `SymbolKind + normalized name + defining SourceFileId + local discriminator` 组成。

禁止把绝对路径字符串或 CST node pointer 当作跨请求稳定身份。

## 错误恢复原则

- syntax error 不阻止产生局部 CST。
- lowering 遇到未知节点产生 `UnknownConstruct`，不 panic。
- scope 未知时保留 `Unknown`，避免级联错误。
- CWT bootstrap 导入遇到 corpus 中未支持的构造时失败；导入后数据库自身的 schema/invariant 错误阻止发布。
- formatter 在 CST 不安全时返回无编辑和明确原因。

## 安全与版权

- 不执行 Mod 或 CWT 配置中的任意代码；CWT 只由受限 importer 解释。
- 限制扫描文件大小和最大嵌套深度相关资源消耗。
- CWT bootstrap 输入只存在于本地 `reference/` 调研工作树，不作为项目规则源或发布内容；导入 provenance 记录上游 commit 与许可证，但不保存原生 CWT 文本。
- 不提交或再分发 Vanilla 游戏文件；Vanilla 缓存只存在于用户本机。
