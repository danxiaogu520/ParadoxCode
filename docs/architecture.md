# ParadoxCode 总体架构

> 本文件描述当前架构边界和已落地的数据流。

## 目标

ParadoxCode 提供通用 PDX Mod 语言引擎。EU4 是当前唯一有完整交付承诺的游戏 profile；首个客户端是 Zed，核心分析独立于编辑器。未来编辑器和低优先级游戏 profile 复用同一套 workspace、index、analysis 和 LSP runtime。

Vanilla setup 只为生成持久 cache 保留 source text、index shard、UTF-16 位置和本地化预览；完整 CST/HIR 在每个文件完成 shard 后即释放，不跨 setup 阶段继续占用 host 内存。

MVP 不以少数语义类别为边界；目录发现由选定 `GameProfile` 的 source-root 白名单控制，EU4 profile 遵循 CWTools 的 `scriptFolders`，并只把 `.txt`、`.gfx`、`.yml` 文件交给规则分类和索引。Script 与 localisation 使用独立 parser；其他扩展、媒体、字体、音频、贴图等资源不进入 Vanilla/工作区磁盘索引。显式打开的文档仍可按规则分类以提供编辑器级语法能力。省份 ID 等脚本语义从 `positions.txt` 等可解析的权威游戏文件提取。存档和二进制数据不做内容解析。

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

Developer-maintained rules/eu4 source tree
                  |
                  +--> strict pdx-bake validation/build (developer/release)
                  |
                  v
       embedded first-party JSON source bundle
                  |
                  v
    pdx-ls first-party artifact cache provider
                  |
                  v
       user-local validated SQLite artifact
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
- 下载、校验、缓存并启动与平台匹配的官方 `pdx-ls`；第一方 JSON source bundle 由 server 内嵌，SQLite runtime artifact 在用户本机按需生成
- 首次启动时允许 server 在后台执行一次快速 Vanilla 发现；复杂选择和深度扫描交给 `pdx setup vanilla`
- 只为 EU4 Script 注册编辑器语言；localisation 由 server 在 workspace 扫描中静默解析，不注册独立 Zed 语言

不得在扩展内实现 symbol 提取、scope 推导或诊断。Tree-sitter grammar/query 只属于这一层；
`pdx-parser` 核心运行时使用纯 Rust parser，不链接 Tree-sitter C。

### LSP 层

`pdx-lsp` 只负责协议生命周期、capability negotiation、URI/position 转换、请求取消和 diagnostics 发布。当前声明能力覆盖的标准输入、initialize result/capabilities、diagnostics 与语言功能响应均使用 `lsp-types` DTO；轻量 JSON-RPC framing、worker 调度和 editor-neutral analysis API 不绑定 async service framework。所有功能算法仍只调用 `pdx-analysis`。

### 分析层

`pdx-analysis` 是纯查询门面。每个请求读取 `AnalysisSnapshot`，不直接读磁盘，也不持有 editor client。

### 工作区层

`pdx-engine` 维护 VFS、open document overlay、source roots、parse/HIR cache 和 index shards。可变状态只存在于 `AnalysisHost`；请求使用不可变 snapshot。

LSP initialize 将 client 打开的 root 与类型化 `initializationOptions` 解析成 source roots；可选 `.pdx/project.toml` 描述 Current Mod 和从低到高排列的 Dependency Mods，inline 字段可覆盖 TOML。相对路径以打开的 worktree 为基准，目录会 canonicalize 并拒绝重叠。这属于 adapter 配置解析；优先级、只读属性和索引仍由 editor-neutral workspace 模型执行。

规则 artifact 是本机版本化 SQLite cache。官方 server 内嵌严格第一方 JSON source bundle，启动时先解析并计算 canonical `rule_hash`；用户 cache 的 schema、game identity 和 hash 匹配时只读加载，否则在临时文件中重新编译、round-trip 校验后原子安装。规则 cache 不接受外部 source path、用户覆盖或旧 hash fallback。Vanilla cache 仍只持久化 source-file location metadata（包括 definition/reference 的 UTF-16 导航位置）、semantic shards，以及本地化 definition 的有限长度派生 Hover 预览；不持久化源码、CST 或 HIR。这样即使 Vanilla 源目录当前不可读，definition/reference 仍可返回可用的编辑器范围，本地化 Hover 仍可显示短文本。`pdx index vanilla` 保留为显式低层建库入口；`pdx setup vanilla` 负责发现、验证、建库和用户级配置。首次 LSP 启动在没有项目覆盖和历史尝试记录时只后台执行一次快速发现；唯一候选完整建库后通过 event loop 原子安装，零候选或多候选记录结果并提示用户手动深度扫描。正常启动先完成 Current Mod/Dependency 的初始化并回复 `initialize`，再在可取消后台 worker 中只读加载 Vanilla cache，完成后由 event loop 原子合并；在合并完成前，依赖 Vanilla 的 snapshot 查询排队，避免读取半成品索引。后续 workspace refresh 会跳过 Vanilla root。Vanilla cache 的 `game_id` 必须匹配，`rule_hash` 不一致时按 RFC 0003 重建 Vanilla index。

### 规则层

`pdx-bake` 和 `pdx-ls` 共享 `pdx-rules::rulec` 的严格第一方 JSON source 编译核心。它校验稳定身份、cardinality、severity、type descriptor、type-instance localisation binding（含 subtype condition）和 artifact round-trip；`pdx-bake` 面向开发/发布生成临时 artifact，官方 `pdx-ls` 则在用户本地 cache 中按需生成 SQLite。项目不提供 CWT importer、fallback 或用户规则同步入口。

`pdx-rules` 定义通用 source bundle 解码、SQLite artifact schema、规范化逻辑视图、`rule_hash` 算法、只读加载与运行时查询 API，以及不含游戏名称的 data-only `GameProfile` 描述；`pdx-game` 定义数据驱动的安装标志、跨平台发现、用户级本机配置，并在 `eu4` 模块中提供内嵌 JSON source bundle、EU4 profile、安装描述和 bootstrap catalog。迁移期 `pdx-eu4` re-export 已在调用方清零后删除。运行时只读加载并冻结 `RuleSet`，同时建立不参与逻辑哈希的 case-insensitive exact-key/context semantic indices，供 scope-link、nested transition lowering、root selection、diagnostics 和 completion 等热点查询跳过无关规则；schema 15 已将 `game_id` 纳入 metadata 与 canonical hash，并保存带 subtype condition 的 type-instance localisation bindings；schema 16 在此基础上保存 semantic rule 的 deprecated 标志。EU4 组合入口在启动服务前校验 profile 身份，并把 profile 显式传入 LSP/host/snapshot。

当前 workspace shard 的 symbol/reference 路径，以及 analysis 的 root scope、scope spelling/completion/compatibility、fallback key、semantic member alias 与额外 enum member，都由 EU4 profile 数据驱动。workspace/analysis/LSP 通用生产代码不再按 `game_id` 字符串或 EU4 名称白名单隐式启用这些行为；syntax facade 的历史 `Eu4FileFormat`/`parse_eu4*` 名称也已在未发布边界内收敛为 `FileFormat`/`parse*`，具体 localisation/CSV 行为仍由 EU4 profile 的文件分类选择。

数据库保存 `TypeKey("scripted_effect")`、`AliasRef("effect")` 等 EU4 静态 matcher；实际 scripted effect、building 等成员来自 `WorkspaceIndex`。EU4 command、scope 和目录规则属于 EU4 profile 实现，不设计其他游戏的推测性替换层。

## Crate 依赖方向

```text
pdx-text
pdx-parser    -> pdx-text
pdx-rules     -> pdx-text
pdx-game      -> pdx-rules
pdx-bake     -> pdx-rules
pdx-engine -> pdx-text + pdx-parser + pdx-rules
pdx-analysis  -> pdx-engine + pdx-rules
pdx-lsp       -> pdx-analysis + pdx-parser
pdx-lsp       -> engine, analysis, parser, rules, game crates (includes CLI binaries)
```

实际约束：

- `pdx-text` 不依赖其他 workspace crate。
- `pdx-parser` 实现可复用 PDX 文本前端，但不依赖游戏规则数据库、workspace 或 LSP。
- `pdx-rules` 定义 SQLite schema、hash 与只读 runtime view，不依赖具体游戏名称表、外部规则语言或 LSP。
- `pdx-game` 包含安装发现和 EU4 profile（`eu4` 模块）。
- `pdx-bake` 是维护者 CLI；编译核心由 `pdx-rules` 复用，开发/发布检查与 `pdx-ls` 用户 cache 物化使用同一套 source validation 和 artifact round-trip。
- `pdx-engine`（含 HIR lowering 模块）通过稳定的 typed CST API lowering；结构/recovery/conditional-parameter facts 与 `RuleSet`/显式 profile 驱动的 definition/reference facts 已被 workspace shard 和 analysis 查询共享。不依赖 LSP 类型。
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
- 首次 Vanilla 快速发现与索引在可取消后台 worker 中运行；worker 只返回完整 cache，event loop 是安装 cache 和更新当前 snapshot 的唯一所有者。
- 编辑先在 event loop 提交最新文本、版本和 `LineIndex`，parse/单文件 HIR 在 immutable snapshot worker 中准备；结果只有在版本、文本和路径仍完全一致时才能提交。依赖语义的后续请求会有序等待最新 parse，不读取旧文本。
- 查询获取 snapshot 后不持有 host 锁。
- initialize 的 source-root scan 在候选 `AnalysisHost` worker 中运行，目录发现、受限读取、parse/lower、bulk index 和 priority resolution 均可取消，只有完整成功后才由 event loop 原子提交。semantic diagnostics 使用约 200ms debounce，在 immutable snapshot worker 上运行，提交时校验文档版本；新编辑会使旧任务失效。普通语言请求同样捕获单一 snapshot 后进入 worker。`$/cancelRequest` 和过期 diagnostics 通过共享的 editor-neutral `CancellationToken` 在 workspace semantic 合并、semantic rule 递归及主要结果遍历中协作式中止。
- completion/hover 优先于后台全量诊断。

MVP 不立即引入通用增量计算框架。先使用清晰的按文件 cache key 和 shard replacement；只有性能数据证明需要时再评估 query framework。

当前 alpha 已消除深拷贝 snapshot、query-time workspace 重解析，以及 event-loop 上的 workspace scan、parse/lower、semantic diagnostics 和语言查询；initialize scan 与 analysis 查询均支持内部协作式取消。后续并发工作应集中在文件变化的定向更新，而不是重新引入 event-loop 阻塞。

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
- 第一方规则源码出现未知字段、重复身份或无效 invariant 时编译失败；数据库 schema/hash/round-trip 错误阻止发布。
- formatter 在 CST 不安全时返回无编辑和明确原因。

## 安全与版权

- 不执行 Mod 或规则源码中的任意代码；规则编译器只解析严格 JSON 数据。
- 限制扫描文件大小和最大嵌套深度相关资源消耗。
- 不编译、测试或发布任何外部规则输入，包括 `.cwt` 文件；正式 `pdx-ls` 只使用内嵌 first-party JSON source bundle。
- 不提交或再分发 Vanilla 游戏文件；Vanilla 缓存只存在于用户本机。
