# ParadoxCode 当前架构

- 状态：Current
- 范围：EU4 v0.1；核心 workspace、index、analysis、LSP 和发布设施保持游戏无关
- 规则 authority：[`docs/rfc/0013-first-party-rule-source.md`](rfc/0013-first-party-rule-source.md)

本文只描述当前代码中的边界、数据流和不变量。功能决策见对应 RFC；实现状态不以历史日期或提交记录维护。

## 产品边界

ParadoxCode 是一个通用 PDX Mod 语言引擎，当前唯一完整支持和发布的 profile 是 EU4。EU4 的路径、文件分类、scope、command、symbol 和特殊 lowering 位于 `pdx-game::eu4` 与第一方规则 source 中；通用引擎不根据编辑器名称、binary 名称或硬编码 EU4 名称表启用语义。

当前客户端是 Zed。Zed extension 只负责语言 metadata、Tree-sitter query、server 获取/校验/启动和配置转发；语义分析、symbol、scope、规则解释和 Vanilla indexing 位于 Rust core/server。

## 数据流

```text
Editor buffer / Current Mod / Dependency Mods / Vanilla cache
                           |
                           v
                source text + LineIndex
                           |
                           v
              Script or Localisation CST
                           |
                           v
 profile/rule-aware HIR lowering + ordered macro templates
                           |
                           v
                 FileIndexShard
                           |
                           v
                 immutable snapshot
                           |
                           v
 diagnostics / completion / hover / navigation / rename
       (query-local macro binding and bounded expansion)
                           |
                           v
                       LSP adapter
                           |
                           v
                           Zed
```

规则数据流：

```text
rules/eu4/*.json
       |
       +--> pdx-rules::rulec / pdx-bake validation
       |
       +--> embedded JSON source in pdx-game::eu4
                              |
                              v
                    user-local SQLite artifact
                              |
                              v
                     read-only RuleSet + rule_hash
```

## Crate 边界

```text
pdx-text
pdx-parser  -> pdx-text
pdx-rules   -> pdx-text
pdx-game    -> pdx-rules
pdx-bake    -> pdx-rules
pdx-engine  -> pdx-text + pdx-parser + pdx-rules
pdx-analysis -> pdx-engine + pdx-rules
pdx-lsp     -> pdx-engine + pdx-analysis + pdx-parser + pdx-rules + pdx-game
```

| Package/module | 当前职责 | 不负责 |
|---|---|---|
| `pdx-text` | range、offset、line index、UTF-8/UTF-16、URI 和 `LogicalPath` | workspace、游戏规则 |
| `pdx-parser` | Script/Localisation loss-aware CST、syntax error、edit reparse、formatter | 规则、磁盘扫描、LSP |
| `pdx-rules` | source compiler、SQLite schema、canonical hash、只读 `RuleSet`、matcher | LSP、外部规则语言、动态 workspace members |
| `pdx-game` | 安装发现、用户配置，以及 `eu4` profile、catalog、embedded source、cache provider | 通用 workspace/index |
| `pdx-engine` | VFS、source roots、overlay、parse/HIR state、per-file shard、snapshot | LSP protocol 类型 |
| `pdx-analysis` | 基于 snapshot 的 diagnostics、completion、hover、navigation、symbols、rename | 直接读磁盘、editor client |
| `pdx-lsp` | JSON-RPC/LSP 生命周期、capabilities、协议转换、取消、结果发布 | 规则解释、feature 算法 |
| `editors/zed` | Zed metadata、queries、server 获取/启动、配置转发 | symbol 提取、scope、规则实现 |

`pdx-bake` 是 `pdx-rules` package 中的维护者 binary，不是独立核心 crate。`editors/zed` 和 `fuzz` 是独立 Cargo package/workspace，不属于核心 workspace 依赖图。

## Crate 内部布局

核心 crate 的 `lib.rs` 现在主要承担 facade、模块声明和稳定的公开 re-export；实现按职责分布在内部子模块中。这个布局不改变 crate 依赖方向，也不把 game-specific 语义移入 generic engine 或 LSP。

```text
pdx-parser/src/format/
  mod.rs, common.rs, script.rs, localisation.rs, equivalence.rs, tests.rs
pdx-parser/src/quoted_script.rs
  quoted payload decode/encode, recovery parse, composable UTF-8 source map

pdx-rules/src/
  model.rs, matcher.rs, profile.rs, runtime.rs, canonical.rs, sqlite.rs, rulec.rs, tests.rs

pdx-engine/src/
  model.rs, index.rs, scan.rs, pipeline.rs, host.rs, snapshot.rs
  hir/{mod.rs, model.rs, collector.rs, parameters.rs, scope.rs, semantics.rs, templates.rs, tests.rs}
  vanilla_cache/{mod.rs, read.rs, write.rs, codec.rs, preview.rs}

pdx-analysis/src/
  types.rs, support.rs, semantic.rs, macro_expansion.rs, quoted_script.rs, resolution.rs, diagnostics.rs, hover.rs,
  navigation.rs
  completion/{mod.rs, context.rs, candidates.rs, macro_constraints.rs, support.rs}
  tests/{mod.rs, support.rs, diagnostics.rs, completion.rs, semantic.rs, scope.rs, hover.rs,
         navigation.rs, rename.rs}

pdx-lsp/src/
  initialize.rs, workspace.rs, vanilla.rs, requests.rs, protocol.rs, text.rs, transport.rs, uri.rs
  server/{event_loop.rs, workers.rs, document_events.rs}
  tests/{support.rs, transport_lifecycle.rs, workspace_vanilla.rs, request_adapter.rs, freshness.rs}
```

公共 API 仍由各 crate facade 导出，例如 `pdx_engine::hir::*`、`pdx_parser::format::*` 和
`pdx_lsp::*` 的既有路径保持稳定。测试也按功能域拆开，真实 JSON-RPC、workspace、cache、HIR
和 analysis 行为仍在对应 crate 内验证。

## Workspace 与 source roots

`AnalysisHost` 持有可变 workspace；`AnalysisSnapshot` 是查询用的不可变视图。当前 source root 类型为 `Vanilla`、`Dependency` 和 `CurrentMod`，未保存文本以 overlay candidate 覆盖其 backing file：

```text
Vanilla < ordered Dependency Mods < Current Mod < Open Document Overlay
```

同一 logical path 的低优先级候选保留为 shadowed，resolution 由 file category 和 symbol policy 决定。overlay 不创建新的 Mod root，并继承 backing path 的写权限。

LSP/CLI 可通过 `.pdx/project.toml` 或 typed initialization options 配置 Current Mod、从低到高排列的 Dependency Mods 和 Vanilla cache。相对路径以打开的 workspace root 为基准；root 重叠、越界和重复 dependency ID 被拒绝。

扫描先受 `GameProfile.scan_roots` 白名单限制，再应用 profile 的 scan extension。当前 EU4 全量扫描扩展为 `.txt`、`.gfx`、`.yml`；规则 catalog 仍可为显式打开文档提供更细的分类。`Script` 和 `Localisation` 进入 parser/HIR，`Asset` 只登记路径，`SyntaxOnly` 不创建 `ParsedSource` 或 HIR。CSV 当前属于 opaque/syntax-only，不提供 CSV parser、CSV HIR 或列级语义。

扫描限制包括最大递归深度 64、所有 roots 最多 100,000 个普通文件、单个分类源文件最多 16 MiB，以及最多保留 256 条详细问题。EU4 profile 支持安全的 Windows-1252 legacy text 转换；不可读、控制字符异常或其他二进制内容按可恢复问题跳过。

## Rules 与 Vanilla cache

`rules/eu4/` 的 JSON 是唯一规则 authority。source format 当前为 `7`，runtime SQLite schema 当前为 `18`。官方 `pdx`/`pdx-ls` 内嵌 JSON source，启动时计算 canonical `rule_hash`，只读加载匹配的用户本地 SQLite artifact；缺失、损坏、schema、`game_id` 或 hash 不匹配时临时编译、round-trip 校验后替换 cache。未通过校验的 artifact 不进入 runtime；正式 server 不接受 `--rules`、外部规则路径、CWT 或用户规则覆盖。

Vanilla index cache schema 当前为 `4`。它保存 cache metadata、source-file metadata、semantic shards、scripted macro 的紧凑调用签名、definition/reference 的 UTF-16 位置和有界的 localisation preview；不保存 Vanilla 源码、CST、HIR 或 macro template。因此 Vanilla macro 保持 signature/OpenWorld 调用分析，不进行体展开。Vanilla 源文件内容或 fingerprint 变化需要显式 refresh；规则 hash mismatch 可在 LSP 启动后台自动重建。缓存文件缺失、损坏或 schema 不兼容（例如旧测试版本产物）时，若自动发现可用，LSP 会从已记录或发现的安装目录重建到同一显式路径，而不是静默失去 Vanilla 符号；发现失败不写入用户配置，且不阻断启动。cache 加载或重建期间，当前 LSP 会延迟受影响的 snapshot 请求，完成后由 event loop 安装完整 cache；失败时保留已加载的旧 cache并报告原因。LSP 启动加载使用 `load_cancellable_for_install`：校验与完整加载一致，但跳过 symbol lookup maps 的派生（安装时合并 shards 后只构建一次）；`install_vanilla_cache` 用单次 `from_shards_with_rules` 构建合并索引（case policy 与 lookup maps 一起派生），`source_file_paths` 用 `HashMap` 键控物理路径。

## 并发与生命周期

- event loop 是 `AnalysisHost` 变化的唯一提交者；查询取得 snapshot 后不持有 host 可变状态。
- initialize scan、parse/lower、单文件变化、semantic diagnostics 和语言请求在 worker 中运行，并支持协作式取消。
- 编辑结果必须匹配当前 document version、文本和路径；过期 diagnostics 不发布。
- 每个文件独立生成并替换 `FileIndexShard`；取消或 workspace-level error 不安装半成品 snapshot。
- semantic diagnostics 默认约 200 ms debounce；completion/hover 等交互查询优先于后台诊断。
- scripted macro 的 diagnostics 展开和值补全约束收集均使用单次查询内的 identity 栈、节点/token-byte/展开深度预算和 cancellation，不持有 `AnalysisHost` 锁，也不使用跨 snapshot 的全局可变缓存。
- `AnalysisSnapshot` 携带按 `(revision, key)` 键控的共享惰性查询缓存（`SnapshotQueryCache`，有界容量、条目不可变）：每个 revision 内 overlay 文档的语义提取、workspace member 判定和 scripted macro 定义解析只计算一次，所有 worker 复用；revision 前进即天然失效，不使用全局状态。
- `RuleSet` 构建期按 `context → exact key` 索引 semantic 规则，并提供上下文内 key 查询；EU4 的 `effect`/`trigger` 上下文各有约 1900 条规则，逐属性匹配只扫描该 key 的 exact 规则与少量非 exact matcher，不再线性扫描整个上下文。
- `AnalysisSnapshot` 维护 `physical path → SourceFileId` 映射（扫描与磁盘变更时增量维护），overlay 归属与路径解析是 O(log n) 查找，不再线性扫描含 Vanilla 的完整文件表。
- 规则标记为 `quoted_script` 的 scalar 使用 parser 提供的容错 secondary Script parse 和可组合 UTF-8 source map；diagnostics、completion、hover 和 navigation/rename 的语义引用收集在查询内下钻，普通 quoted scalar 仍保持 opaque。secondary parse 共用深度、单 payload、累计字节、节点数和 cancellation 预算，不写入主 HIR。
- LSP 当前使用 stdio JSON-RPC，stdout 只输出协议数据。
- 开发诊断器使用有界的 `pdx/workspaceDiagnostics` snapshot request 分批查询 Current Mod 的磁盘
  file state；该请求复用普通 analysis diagnostics、支持取消且不创建 overlay。显式打开但不在
  workspace scan 中的文件仍走标准 `didOpen`/push diagnostics 路径。
- 同一诊断器的 Vanilla-source 模式先通过 `pdx/classifyPaths` 选择 profile 文件，再使用有界的
  `pdx/textDiagnostics` request（每批至多 16 个文件和 16 MiB）分析调用方提供的瞬时文本；查询
  不写入 `AnalysisHost`，符号解析仍读取与 first-party `rule_hash` 匹配的不可变 Vanilla snapshot。

## 当前身份与索引

当前稳定身份包括 `SourceRootId`、`SourceFileId`、`DocumentId`、`LogicalPath`。`Definition` 和 `Reference` 仍通过 kind/name、file id 和 source range 表示；项目尚未实现跨 server 的 `SymbolId`、`DefinitionId` 或 `ReferenceId`。macro 递归检测只使用 snapshot/query 内精确的 overlay document/version/range 或 source file/revision/range identity，不把 kind/name 单独当作定义身份。

`FileIndexShard` 保存 definitions、references、scripted macro summary 和 syntax error count。`WorkspaceIndex` 维护 per-file shards、definition lookup buckets、case-sensitive kind 集合和无源码文件使用的 UTF-16 position ranges。被覆盖定义保留为 inactive/shadowed，普通 navigation 只使用 active resolution。scripted effect/trigger 的成员由实际 workspace/Vanilla index 动态提供；宏参数的 occurrence 仍只保留在所属 HIR owner 内，不进入全局 symbol bucket。每个可降低的宏定义在 shard 中保存参数 signature 和 source-independent `MacroTemplate` IR，后者只含有序 property/bare value/conditional、token fragment 与 source range，不含源码或 CST identity。

HIR 与 shard summary 使用同一种按源码顺序排列的 `MacroTemplate`。调用侧仅绑定 scalar token，duplicate parameter 仍诊断但按 last-wins 展开；conditional 按参数是否 supplied 确定激活。展开体使用 descriptor 的 body context 和调用点 scope 重新执行现有 semantic validator，不读取定义侧缓存的 `ScopeFact`。参数派生内容的诊断映射到调用参数值，固定模板内容映射到调用名；普通 semantic code 保留，只有 cycle/limit 使用专门 code。定义体内仍依赖当前 owner 参数的嵌套调用会延后到外层调用具体化后再递归，避免把 `$X$` 当作最终实参。

宏调用参数的 value completion 使用同一模板和查询预算做符号化约束收集：当前参数绑定为 `Target`，其他已提供 scalar 保留具体值，未知或 block 参数保持 `Unknown`；conditional 按 supplied/absent 选择，目标经 named block 转发到嵌套宏时继续追踪。每个目标 use-site 的 `ValueMatcher` 先生成候选，再对多个 use-site 取交集；作为 standalone bare item 注入 container 的目标参数会推导 quoted Script 的 context/path/scope，调用点可直接在引号内补全。宏诊断展开也会把这种 quoted argument 投影为虚拟 property tree 并精确回映。活动定义来自 Current Mod、Dependency 或 cache-only Vanilla 时均消费其 shard template；缺模板、环、预算耗尽或复合 token 才保守退回 signature/OpenWorld。first-party path rule 不描述具体 scripted effect/trigger 的参数语义，动态宏及其模板必须从 workspace source/cache index 派生。

## 错误与安全不变量

- syntax error 不阻止产生可遍历的局部 CST。
- HIR 对未知结构保留 `UnknownConstruct`，未知 scope 保留 `Unknown`，不 panic。
- formatter 对不安全 CST 返回零 edits 和明确 skip reason。
- 规则 compiler 对未知字段、重复 stable identity、非法 invariant、round-trip/hash 不一致失败。
- 不执行 Mod、规则或外部脚本中的任意代码。
- 不提交或再分发 Vanilla 文件和用户本地 Vanilla cache。
- 用户路径、文件大小、嵌套深度、消息大小和结果数量均有边界。

## 当前限制

- parser 的 edit 更新仍是 full reparse，不是纯 Rust incremental parser。
- CSV 没有 parser、formatter、HIR 或列级语义诊断。
- HIR scope evaluator、动态成员确认和部分 profile 语义仍是保守的 Partial 实现。
- 没有跨进程稳定 symbol identity，也没有完整 reference 倒排索引。
- Vanilla cache 不保存 macro template，HeaderBlock、property-value conditional 或含 syntax error 的宏 owner 当前也不会生成可展开模板；这些调用保守退回 signature/OpenWorld 行为。
- quoted Script 的 navigation 只收集 profile/rule 已确认的 reference、value definition、localisation 和 scripted-macro 语义；任意动态拼接文本仍保持 opaque，也不会写入持久 index shard。
- 只有 EU4 profile 有 v0.1 交付承诺；没有运行时游戏切换或第三方 plugin ABI。
