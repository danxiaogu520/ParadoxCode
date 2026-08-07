# ParadoxCode 当前架构

- 状态：Current
- 范围：EU4 v0.1；核心 workspace、index、analysis、LSP 和发布设施保持游戏无关
- 规则 authority：[`docs/rfc/0014-first-party-rule-source.md`](rfc/0014-first-party-rule-source.md)

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
      profile/rule-aware HIR lowering
                           |
                           v
                 FileIndexShard
                           |
                           v
                 immutable snapshot
                           |
                           v
      diagnostics / completion / hover / navigation / rename
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

pdx-rules/src/
  model.rs, matcher.rs, profile.rs, runtime.rs, canonical.rs, sqlite.rs, rulec.rs, tests.rs

pdx-engine/src/
  model.rs, index.rs, scan.rs, pipeline.rs, host.rs, snapshot.rs
  hir/{mod.rs, model.rs, collector.rs, parameters.rs, scope.rs, semantics.rs, tests.rs}
  vanilla_cache/{mod.rs, read.rs, write.rs, codec.rs, preview.rs}

pdx-analysis/src/
  types.rs, support.rs, semantic.rs, resolution.rs, diagnostics.rs, hover.rs, navigation.rs
  completion/{mod.rs, context.rs, candidates.rs, support.rs}
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

`rules/eu4/` 的 JSON 是唯一规则 authority。source format 当前为 `5`，runtime SQLite schema 当前为 `16`。官方 `pdx`/`pdx-ls` 内嵌 JSON source，启动时计算 canonical `rule_hash`，只读加载匹配的用户本地 SQLite artifact；缺失、损坏、schema、`game_id` 或 hash 不匹配时临时编译、round-trip 校验后替换 cache。未通过校验的 artifact 不进入 runtime；正式 server 不接受 `--rules`、外部规则路径、CWT 或用户规则覆盖。

Vanilla index cache schema 当前为 `3`。它保存 cache metadata、source-file metadata、semantic shards、definition/reference 的 UTF-16 位置和有界的 localisation preview；不保存 Vanilla 源码、CST 或 HIR。Vanilla 源文件内容或 fingerprint 变化需要显式 refresh；规则 hash mismatch 可在 LSP 启动后台自动重建。cache 加载或重建期间，当前 LSP 会延迟受影响的 snapshot 请求，完成后由 event loop 安装完整 cache；失败时保留已加载的旧 cache并报告原因。

## 并发与生命周期

- event loop 是 `AnalysisHost` 变化的唯一提交者；查询取得 snapshot 后不持有 host 可变状态。
- initialize scan、parse/lower、单文件变化、semantic diagnostics 和语言请求在 worker 中运行，并支持协作式取消。
- 编辑结果必须匹配当前 document version、文本和路径；过期 diagnostics 不发布。
- 每个文件独立生成并替换 `FileIndexShard`；取消或 workspace-level error 不安装半成品 snapshot。
- semantic diagnostics 默认约 200 ms debounce；completion/hover 等交互查询优先于后台诊断。
- LSP 当前使用 stdio JSON-RPC，stdout 只输出协议数据。

## 当前身份与索引

当前稳定身份包括 `SourceRootId`、`SourceFileId`、`DocumentId`、`LogicalPath`。`Definition` 和 `Reference` 仍通过 kind/name、file id 和 source range 表示；项目尚未实现跨 server 的 `SymbolId`、`DefinitionId` 或 `ReferenceId`。

`FileIndexShard` 保存 definitions、references 和 syntax error count。`WorkspaceIndex` 维护 per-file shards、definition lookup buckets、case-sensitive kind 集合和无源码文件使用的 UTF-16 position ranges。被覆盖定义保留为 inactive/shadowed，普通 navigation 只使用 active resolution。

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
- 只有 EU4 profile 有 v0.1 交付承诺；没有运行时游戏切换或第三方 plugin ABI。
