# RFC 0012：CWT 一次性导入与权威规则数据库

- 状态：Accepted
- MVP：EU4 v0.1

## 决策

`pdx-cwt` 是 ParadoxCode 的规则数据库管理程序。长期方向是对自有 SQLite 规则数据库提供 CRUD、验证、hash、diff 和历史能力；`v0.1` 只实现一项功能：把指定的 CWTools EU4 `.cwt` corpus 一次性导入 `eu4.pdxrules`。

导入成功并人工验收以后：

- `eu4.pdxrules` 成为唯一权威。
- 项目不保存、构建或发布原生 CWT 规则文本。
- 不要求继续兼容未来 CWTools/CWT 变化。
- 后续规则修订直接作用于数据库，并产生新的 `rule_hash`。
- LSP runtime 永远不依赖 `pdx-cwt`。

当前 EU4 parity baseline 使用 73 个固定输入，artifact schema 为 11，importer 为
`phase12-cwt-starts-with-1`，canonical `rule_hash` 为
`446f21f2c08d8d802c8769df34259f880bb63467726592d3f95ee1cea7b71484`（schema 12，包含 `game_id = "eu4"`）。该 baseline 已覆盖
CWTools 的 duplicate/alternative、leaf-value/value-clause、cardinality、scope transition、
dynamic value、type path/skip-root 和 subtype `starts_with` 语义。

## MVP 边界

`pdx-cwt v0.1` 必须提供：

```text
pdx-cwt import
  discover a supplied CWT directory
  parse the EU4 bootstrap corpus
  inventory constructs/directives
  lower into ParadoxCode's normalized schema
  validate cross references and invariants
  write SQLite in one transaction
  compute canonical logical rule_hash
  emit manifest/import report
```

`v0.1` 明确不提供：

- interactive query UI
- create/update/delete commands
- rule history、diff、rollback
- CWT export 或 round trip
- 持续同步上游
- 其他游戏导入

## 模块职责

```text
pdx-cwt
  import command
  CWT source discovery
  bootstrap parser and typed import model
  directive decoder
  corpus inventory
  normalization into pdx-eu4 write model
  import validation and report

pdx-eu4
  SQLite schema and migrations
  stable logical IDs
  write transaction API used by importer
  canonical logical content projection
  rule_hash implementation
  read-only runtime loader and query API
```

`pdx-analysis`、`pdx-workspace`、`pdx-lsp` 和 `pdx-ls` 不依赖 `pdx-cwt`。Importer 自己拥有 CWT parsing 模块；它可以借鉴 `pdx-syntax`，但不应迫使 runtime grammar 公开 CWT-specific API。

## Bootstrap 输入

调研输入位于本地：

| Source | Revision |
|---|---|
| `reference/cwtools` | `b377453dee803f9258be92cfc49896d09039702d` |
| `reference/cwtools-eu4-config` | `a85622d368bbb7afca938ed70fdd5eda44aec769` |

`reference/cwtools` 用于理解语义，`reference/cwtools-eu4-config` 是本次 EU4 数据导入 corpus。两者都不成为正常构建输入或发布资产。

Importer 接收显式 source directory，不读取浮动网络 branch。Import report 记录：

- upstream identity/revision
- 输入文件规范路径与 content hash
- 许可证和 attribution
- importer/schema version
- construct/directive inventory
- warnings/errors 与人工决策

报告不嵌入原始 CWT 文本。ParadoxCode 只服务 EU4 最新/最终版本，因此不记录 `target_game_version`，也不生成版本条件规则。

## Source discovery

Importer 按规范相对路径发现 `.cwt` 并排序。`folders.cwt` 等特殊入口按角色解析。同目录的 `.txt`、`.csv` 和其他材料不会被隐式导入；若本次导入确实需要某个辅助输入，必须由命令参数或 import manifest 显式列出其角色和 hash。

拒绝：

- 路径逃逸或无法解码的输入
- 重复 logical input identity
- 不稳定的文件遍历顺序
- 解析失败却继续写入
- 未分类的 corpus construct/directive

## CWT import model

Importer 必须保留足够信息完成一次准确转换：

- bracketed key：`type[event]`、`alias[effect:x]`、`enum[...]`
- node、leaf、leaf-value、value-clause 形状
- quoted/unquoted scalar、operator、source order 和重复规则
- `##` directive 与后续 rule 的关联
- `###` documentation 与目标 rule/type/alias 的关联
- source file/range，仅用于 import error 与 provenance

不能先转换为普通 key/value map，因为它会丢失 alternatives、重复 key 和 rule shape。

## 本次 corpus 的兼容范围

兼容范围由固定的 EU4 bootstrap corpus 决定，不由“所有可能 CWT”决定。至少导入该 corpus 实际使用的：

### Declarations

- types、subtypes、aliases、single aliases
- enums、complex enums、value sets
- scopes、scope groups、links
- effects、triggers、modifiers、localisation、folders
- file/path/type metadata

### Rule/matcher shapes

- node、leaf、leaf value、value clause、subtype、alternative
- exact/scalar/type/enum/numeric/scope/localisation/filepath/icon matcher
- alias match left/name、single alias、alias references
- variable/value set/get matcher
- key/value quote and operator constraints

### Directive metadata

- cardinality and strict minimum
- required/push/replace scope
- severity
- comparison/operator constraints
- type/reference hints
- localisation/modifier metadata
- documentation、display name、abbreviation 等展示字段
- corpus 中无等号 legacy spelling

未知 construct 不能静默忽略。解决方式只能是实现 importer mapping，或把该输入明确标为 non-semantic 且在 import report 中人工批准。最终 release database 不能包含未解析的 raw CWT fragment 作为运行时 fallback。

## 规范化 matcher

CWTools 的 `SpecificField`、`ScalarField`、`TypeField`、`AliasField` 等形式导入为 ParadoxCode 自有 matcher：

```text
ExactKey
AnyScalarKey
TypeKey
EnumKey
NumericKey
AliasRef
OpaqueKey / IgnoreKey
```

例如 `alias[effect:<scripted_effect>]` 保存为 alias `effect` 中的 `TypeKey(scripted_effect)` alternative。Importer 不把当时 Vanilla/Mod 中实际 scripted effect 名称复制进规则数据库；这些成员始终由 runtime `WorkspaceIndex` 提供。

## Import pipeline

```text
1. discover_sources
2. hash_and_inventory_inputs
3. parse_cwt_documents
4. attach_directives_and_documentation
5. lower_to_normalized_write_model
6. resolve_static alias/type/enum/scope references
7. validate matcher/cardinality/path/resolution invariants
8. create temporary SQLite database
9. write all rows in one transaction
10. validate foreign keys and logical projections
11. compute rule_hash
12. atomically publish eu4.pdxrules + manifest + report
```

任何 error 都阻止发布。不得留下半写数据库；最终路径只通过 atomic replace 接收已验证文件。

## Stable logical identity

数据库对象使用可读的 stable identity 或由它确定性派生的 ID，例如：

```text
type/event
alias/effect/add_prestige
enum/event_types/country_event
file_category/history_province
```

SQLite `rowid`、插入顺序和进程内地址不是 identity。同 logical identity 的多个合法 alternative 使用显式 alternative ordinal/stable sub-id；非法重复在导入时报告。

## rule_hash

`rule_hash` 不是 SQLite 文件 bytes hash，也不是 CWT source hash。它是规范化逻辑数据库状态的 hash：

```text
canonical semantic rows
  sorted by table identity and stable primary key
  encoded with fixed field order and scalar encoding
  hashed with a versioned cryptographic algorithm
```

包含所有影响 runtime 语义、documentation、分类和 resolution 的字段；排除页布局、数据库索引、时间戳、VACUUM 状态、import 日志和 source range。Import provenance 自身不改变规则语义，因此不进入 `rule_hash`，但 manifest 单独保存其 hash。

Importer 输出：

```text
rule_hash
hash_algorithm_version
artifact_schema_version
logical row counts
database file checksum（仅用于传输完整性，不作为规则版本）
```

## SQLite 输出

输出文件名固定为 `eu4.pdxrules`。MVP 可以使用普通 SQLite 文件加只读 runtime 打开模式。发布前执行：

- `foreign_key_check`
- schema/version check
- stable-id uniqueness check
- dangling matcher/reference check
- canonical `rule_hash` recomputation
- runtime loader smoke test

数据库提交到 ParadoxCode，并在扩展构建时复制进扩展 release。项目不提交原始 CWT source；数据库不能依赖 source text 才能解释某条规则。

## CLI

MVP 只承诺：

```text
pdx-cwt import \
  --source <reference/cwtools-eu4-config> \
  --output <rules/eu4.pdxrules> \
  --manifest <rules/manifest.json> \
  --report <temporary-or-reviewed-path>
```

成功时打印 `rule_hash`。重复导入相同 corpus 应产生相同规范化逻辑内容和 `rule_hash`；不要求 SQLite 文件 byte-for-byte 相同。

## Runtime 和分发

`pdx-ls` 只接受显式规则路径：

```text
pdx-ls --rules <extension-install>/eu4.pdxrules
```

Server 验证 schema、逻辑 hash 和 manifest/header 后只读加载。它不搜索 CWT、不调用 importer、不下载规则，也不根据项目配置选择历史 hash。

规则由扩展完全拥有并与扩展版本绑定。更新扩展即更新规则与 `rule_hash`；`.pdx/project.toml` 不记录 hash。旧规则只随旧扩展版本存在。

## 测试

MVP 测试包括：

1. 最小原创 CWT strings 覆盖 importer construct/directive。
2. 在本地 bootstrap corpus 上执行一次完整 import acceptance。
3. SQLite schema/invariant corruption tests。
4. logical hash 对插入顺序、索引、VACUUM 和物理重建保持稳定。
5. runtime loader 对生成数据库 smoke test。
6. matcher differential fixtures，验证 exact、scalar、type 和 alias 的预期行为。

正常 CI 以提交的 `eu4.pdxrules` 为权威，验证 schema、`rule_hash` 和 runtime 行为，不要求重新获取或保存 CWT corpus。

## 安全与许可证

- Importer 不执行 CWT 或 Mod 代码。
- 限制输入大小、嵌套深度、总规则数和 SQLite transaction 大小。
- manifest 保留上游 identity、revision、许可证和 attribution。
- 不把 Vanilla 游戏文件或由其建立的用户本地缓存提交/打包。

## 拒绝的方案

- 永久以 CWT 为权威 source：不符合一次导入后自行维护的目标。
- 运行时加载 CWT：扩大启动成本和兼容面。
- 对 SQLite 文件 bytes 做规则版本：物理布局变化会制造虚假版本。
- 把规则嵌入 `pdx-ls`：破坏扩展拥有规则和编辑器独立 server 的边界。
- 把 workspace type members 固化进规则数据库：会让每个 Mod 改变规则版本。
