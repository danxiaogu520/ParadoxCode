# RFC 0004：EU4 规则数据库与 Runtime Schema

- 状态：Accepted
- MVP：EU4 v0.1

> 2026-07-20 amendment：EU4 是当前唯一交付规则包，但通用 artifact/runtime 类型将按 [RFC 0013](0013-generic-engine-eu4-first.md) 迁入 `pdx-rules`，EU4 特有内容进入 EU4 profile。本 RFC 继续定义 `eu4.pdxrules` 的具体语义与发布要求。
>
> 2026-07-21 amendment：artifact 仍是开发期权威数据，但外部 runtime 文件与扩展携带规则的分发方式已由 [RFC 0014](0014-embedded-first-party-rules.md) 取代。

## 目标

EU4 的文件分类、types、aliases、enums、commands、scopes、symbols、documentation、reference semantics 与覆盖策略全部存入项目自有的 `eu4.pdxrules`。核心分析只消费 EU4 schema；不提供游戏选择器、通用游戏适配接口或其他游戏适配。EU4 语法本身由 `pdx-syntax` 直接实现。

EU4 profile 当前只服务项目选定的 EU4 规则基线。规则数据库可以由项目维护者继续修订；每次逻辑修订产生新的 `rule_hash`。未来其他游戏或版本的策略不由本 RFC 定义。

## 目录

```text
rules/
  eu4.pdxrules          committed SQLite authority
  manifest.json         schema、rule_hash、provenance、统计
  tests/
    fixtures/

editors/zed/
  ...
  bundled-rules/        发布打包时包含 eu4.pdxrules
```

项目不提交权威 `.cwt` source tree。`reference/cwtools-eu4-config` 是本地调研与一次性 bootstrap 输入，不参与正常构建或发布。

`rules/` 是仓库内的 source-of-truth artifact 位置，不是独立分发渠道。面向用户的规则分发只通过编辑器扩展 release 完成。

## 权威链路

```text
one-time EU4 CWT bootstrap corpus
                 |
                 v
         pdx-cwt v0.1 import
                 |
                 v
     self-owned SQLite eu4.pdxrules
                 |
                 +---- canonical logical view ----> rule_hash
                 |
                 v
       read-only runtime RuleSet + Eu4Profile
```

导入成功以后，SQLite 数据库是唯一权威。CWTools 行为兼容和 CWT source revision 不再是后续维护的约束。维护者未来通过 `pdx-cwt` CRUD 直接修订数据库；MVP 只实现 import。

## SQLite 逻辑模型

物理 schema 可以规范化为多张表，但至少表达：

```text
Eu4Rules
  metadata (fixed EU4 target)
  interned_names
  file_categories / file_matchers / parser_kinds
  type_descriptors / subtype_descriptors
  alias_groups / rules / alternatives
  key_matchers / value_matchers / cardinalities
  enums / complex_enum_descriptors
  scopes / scope_groups / scope_transitions / links
  commands / effects / triggers
  symbol_descriptors / reference_descriptors
  resolution_policies
  localisation_descriptors
  variable / value-set descriptors
  filepath / asset descriptors
  documentation
  import_provenance
```

SQLite 行号、rowid、页布局、索引布局、VACUUM 结果和写入时间都不是逻辑身份。表使用稳定 logical id 或显式 primary key；不能依赖隐式 rowid 作为跨版本 ID。

schema 12 的 metadata 必须包含 `game_id = "eu4"`、`schema_version` 和 `rule_hash`。`game_id` 进入 canonical hash；缺失或与所选 `Eu4Profile` 不一致时，server 在启动阶段拒绝 artifact，不能降级后继续提供带错误语义的服务。

## KeyMatcher

未知 key、补全和命令解析共享同一组 matcher：

```text
KeyMatcher
  ExactKey(name_id)
  AnyScalarKey
  TypeKey(type_id)
  EnumKey(enum_id)
  NumericKey(range)
  AliasRef(alias_id)
  OpaqueKey / IgnoreKey（仅在规则明确声明时）
```

`AnyScalarKey` 只接受当前规则位置的任意 scalar key，不会把整个父 context 标记为 open。多个 matcher 是 alternatives；只有全部不接受时才产生 unknown-key diagnostic。

## 静态规则与动态成员

数据库保存查询描述，不保存工作区符号快照。例如：

```text
TypeKey("scripted_effect")
AliasRef("effect")
TypeKey("building")
```

实际成员来自运行时 `WorkspaceIndex`：

```text
overlay > current mod > ordered dependency mods > Vanilla cache
```

因此新增 scripted effect 不修改数据库或 `rule_hash`。Eu4Rules 说明“去哪里查、如何匹配、如何解析冲突”；WorkspaceIndex 提供“当前有哪些名字和定义”。

## 文件覆盖范围

数据库不得只使用四目录白名单。它应分类：

- CWT bootstrap 建模的全部 PdxScript 文件类别。
- EU4 localisation YAML。
- 规则明确声明且 parser 支持的 CSV/特殊文本。
- filepath/icon/file rule 引用的 asset 类别。

Event、Scripted Effect、Scripted Trigger 和 Localisation 是强制 acceptance baseline，而不是 symbol 范围上限。名为 `dlc_metadata` 的可分类目录只是 Mod 文件类别，不创建 DLC source root。

## Symbol 与 reference 描述

Symbol kind 不在 Rust 中固定枚举：

```text
SymbolDescriptor
  kind_id
  definition_pattern
  name_source
  path_constraints
  case_policy
  resolution_policy
  reference_patterns
  localisation_links
```

文件和 symbol category 分别选择明确策略：

- `ReplaceByRelativePath`
- `ReplaceBySymbol`
- `Merge`
- `Unique`

不确定策略时保留全部候选并报告 ambiguous；不能任意选择 hash-map 或扫描顺序中的第一项。

## Runtime 不可变性

`pdx-rules` 以 read-only 模式打开 SQLite，校验 schema、foreign keys、逻辑不变量与 `rule_hash`，然后构建只读 runtime view 并通过 `Arc<RuleSet>` 共享；`pdx-game-eu4` 负责确认这是 EU4 profile 可消费的 artifact。

Runtime API 不暴露 `insert`、`update` 或 `delete`。加载失败时 server 降级为 syntax-only，并发布 workspace-level 配置错误；不能部分加载或从网络寻找替代规则。

## rule_hash

`rule_hash` 对规范化逻辑内容计算，算法必须：

1. 选择所有影响运行时语义、文档、文件分类和 resolution 的字段。
2. 按 stable table id、primary key 和字段顺序 canonicalize。
3. 使用明确的文本编码、NULL 表示、整数编码和 collection ordering。
4. 排除 SQLite 物理布局、时间戳、缓存、索引和非语义 import 日志。
5. 使用固定的加密散列算法并在 manifest 中记录算法版本。

数据库加载后重新计算的 hash 必须与 manifest/header 一致。相同逻辑内容即使重建 SQLite 文件也得到相同 hash；任一语义规则变化都必须改变 hash。

## 分发与版本

- `eu4.pdxrules` 是独立于 `pdx-ls` 的文件。
- 规则完全由编辑器扩展拥有，随扩展 release 打包，不独立发布、下载或更新。
- 扩展版本升级自然携带新的规则和 `rule_hash`。
- `.pdx/project.toml` 不 pin `rule_hash`。
- Zed 使用 `pdx-ls --rules <extension-path>/eu4.pdxrules` 显式传入。
- `pdx-ls` 不内嵌、不下载、不更新规则。
- 需要旧规则时只能使用携带它的旧扩展版本；MVP 不提供 hash registry。

artifact schema 不兼容时拒绝加载。schema version 只描述存储/API 格式，不代表另一个游戏或可切换的游戏版本。

## 未来 CRUD

未来 `pdx-cwt` 可以增加查询、增加、修改、删除、事务、diff、历史和 rollback。所有写操作必须：

- 通过 schema/invariant validation。
- 在事务中原子提交。
- 重新计算并输出唯一 `rule_hash`。
- 不要求还原或维护 CWT 文本。

这些能力不属于 v0.1，不能延迟一次性 importer 和 LSP MVP。

## 非目标

- 在运行时解释 `.cwt`。
- 永久追踪 CWTools 或保持行为兼容。
- 用 YAML/TOML 维护第二份规则真相。
- 从游戏文件自动改写权威规则。
- 把 workspace symbol members 写进 `eu4.pdxrules`。
- 支持历史 EU4 版本或版本条件规则。
- 在规则中执行 Lua/Wasm/Rust plugin。
