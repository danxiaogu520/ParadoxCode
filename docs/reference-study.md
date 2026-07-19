# CWTools、EU4 Config 与 Jomini 调研

## 调研基线

参考代码以浅克隆形式保存在 `reference/`，只用于架构研究：

| 项目 | 本地目录 | 调研提交 |
|---|---|---|
| cwtools/cwtools | `reference/cwtools` | `b377453dee803f9258be92cfc49896d09039702d` |
| cwtools/cwtools-eu4-config | `reference/cwtools-eu4-config` | `a85622d368bbb7afca938ed70fdd5eda44aec769` |
| rakaly/jomini | `reference/jomini` | `6039568fe0813d3a894d786ed96c29a9a1578e17` |

三者分工不同：CWTools 是规则驱动的 Paradox 脚本分析库；EU4 Config 是 CWTools 消费的游戏规则语料；Jomini 是高性能 Clausewitz 文本/二进制读取库。ParadoxCode 只复刻 EU4 所需的可观察规则行为，不复制多游戏架构，也不把上述项目作为运行时依赖。

`reference/cwtools` 是 CWTools 引擎源码和测试材料，不是完整 EU4 规则源。EU4 规则语料来自独立上游 [`cwtools/cwtools-eu4-config`](https://github.com/cwtools/cwtools-eu4-config)。本次浅克隆固定在 `master` 的 `a85622d368bbb7afca938ed70fdd5eda44aec769`（2026-04-15），上游声明为 MIT License。这一提交是一次性 bootstrap 导入和调研 baseline，不是项目的浮动构建依赖。

本次研究和 EU4 profile 只服务项目选定的 EU4 规则基线，不建立历史版本矩阵。导入 provenance 记录上游 commit、输入 hash 和许可证；后续人工维护改变 `rule_hash`。其他游戏 profile 的版本策略不由本研究决定。

## EU4 CWT 真实语料结论

### 1. Source set 是一个多入口目录，不是单个 schema 文件

固定提交包含 73 个 `.cwt` 文件，共 32,687 行。语义分散在三类入口：

- `common/`、`events/`、`history/`、`map/` 等目录中的 file type 和结构规则。
- 根目录的 `effects.cwt`、`triggers.cwt`、`scope_links.cwt`、`scopes.cwt`、`enums.cwt`、`modifiers.cwt` 和 `localisation.cwt` 等全局 catalog。
- `folders.cwt` 这类特殊文件：内容是裸的目录路径列表，不是常规 `key = value` block。

因此一次性 importer 必须从指定根目录按规范路径发现全部 `.cwt`，同时按文件角色选择 parser entry point。上游还包含 `.txt` 和 `on_actions.csv` 等非 CWT 材料；它们不会因为位于同一目录就被隐式导入。导入完成后不在项目中保留这套原生文本规则。

### 2. Corpus 确认了需要保留重复和多形 rule

当前 corpus 中可观察到 155 次 `type[...]`、600 次 `subtype[...]`、2,931 次 `alias[...]` 和 777 次 `enum[...]` bracket form。这些数字是语法出现次数，不是去重后的 runtime 对象数。

同一 alias 或字段可以多次声明不同 RHS 形状；例如 scripted effect 既可为 scalar 也可为带参 block，`multiply_variable` 有多个同名 alternative。一个普通 map 会覆盖早先声明，所以 typed AST 和 Canonical Rule IR 必须保留 source order、重复 key 以及 alternative identity。

### 3. Type 识别由路径和结构共同决定

真实 `type[...]` 声明使用 `path`、`path_file`、`path_strict`、`type_per_file`、`name_field`、`skip_root_key` 和 `starts_with` 等 metadata。`skip_root_key` 既可以是 scalar，也可以是多段路径 block；多个 type 还可共享同一 physical file，再由根 key 或 name field 区分。

这意味着 `file_matchers` 不能只表示为 glob。数据库需要表达 directory/file 限制、strictness、per-file definition、根路径跳过和 name extraction，并保留冲突时的匹配优先级。

### 4. Directive 兼容必须以 corpus inventory 为准

`##` 语义注释中大量出现 `cardinality`、`scope`、`replace_scope`、`type_key_filter`、`push_scope` 和 `severity`；`###` 文档注释有 2,793 行。除了标准 `## key = value` 形式，corpus 还存在 `## required`、`## optional`、`## cardinality 0..1` 等无等号的 legacy spelling，以及纯说明性的 `## primary`。

importer 不能把所有 `##` 都假定为同一个 assignment grammar，也不能把无法解析的形式静默降级为普通注释。一次性导入先生成 raw directive inventory，再将本次 corpus 的每种 spelling 分类为 semantic directive、文档标记、明确的 non-semantic comment 或 import error。

### 5. Bootstrap 输入不等于长期权威

corpus 含有 TODO、WIP 说明、重复声明和历史 spelling。它适合提供初始规则覆盖，但不适合作为 ParadoxCode 永久兼容的 authoring format。导入时必须暴露未解决的 alternative、无效 reference 或 metadata 冲突；导入成功后，提交的 SQLite 数据库成为唯一权威，维护者可以直接修订规范化规则。

### 6. CWTools 使用 matcher，而不是统一的“动态上下文”

CWTools 将 CWT field 编译为 `SpecificField`、`ScalarField`、`TypeField`、`AliasField` 等 tagged union，再组成 node/leaf/value-clause rule。校验时会：

1. 按 alias 名称收集并按活动 subtype 展开候选规则。
2. 把 exact key 放入字典，把 scalar/type/enum 等通用 matcher 放入候选数组。
3. 对每个 CST node/leaf 先取 exact candidates，再执行通用 matcher。
4. 若任一候选完整校验成功则接受；没有 matcher 接受时才产生 unexpected-property diagnostic。

对应实现位置：

- `CWTools/Rules/RulesTypes.fs`：`NewField` 与 `RuleType` tagged union。
- `CWTools/Rules/RulesParser.fs`：`processKey` 将 `scalar`、`<type>`、`alias_name[...]` 等转换为 field matcher。
- `CWTools/Rules/RulesWrapper.fs`：按名称收集 alias groups 和 type rules。
- `CWTools/Rules/RuleValidationService.fs`：展开 active subtype/alias，建立 exact-key dictionary 与 generic candidate arrays，并在无候选时产生 unexpected property。
- `CWTools/Rules/FieldValidators.fs`：`ScalarField` 恒匹配，`SpecificField` 比较 key，`TypeField` 查询 type member set。
- `CWTools/Game/RulesManager.fs`：从 workspace entities 建立 type maps，并迭代到 type member 数量稳定。

EU4 生产 CWT 确实使用：

- `scalar = scalar` 作为任意 LHS key，例如 `interface/sprites.cwt`。
- `<subject_type> = { ... }`、`<building> = bool` 等工作区类型驱动 key。
- `alias_name[effect]`、`alias_name[trigger]` 等 alias 集合。
- `alias[effect:<scripted_effect>]`，将工作区扫描得到的 scripted effect names 纳入 effect key 集合。

EU4 corpus 的直接例子分别位于 `interface/sprites.cwt`、`map/tiles.cwt`、`history/history_consolidated.cwt`、`events/events.cwt`、`effects.cwt` 和 `common/scripted_triggers_and_effects.cwt`。

因此 ParadoxCode 数据库应保存精确 matcher：`ExactKey`、`AnyScalarKey`、`TypeKey`、`EnumKey`、`NumericKey`、`AliasRef` 与必要的 opaque/ignore matcher。`TypeKey` 的成员不写进规则数据库，而由 Vanilla、依赖 Mod和当前 Mod 的 `WorkspaceIndex` 在运行时提供。

## 从 CWTools 获得的启示

### 1. 规则必须是一等数据

CWTools 的 `RulesTypes.fs` 将规则区分为 node、leaf、value clause、type、alias，并在 options 中记录 cardinality、required scopes、description、severity、reference details 等信息。这证明仅有 `key -> value type` 不足以描述 Paradox 脚本。

ParadoxCode 采用以下结论：

- Eu4Rules 必须能表达结构规则、值规则、symbol 类型、cardinality 和 scope 要求。
- 补全、诊断、hover 与 reference extraction 必须共享同一份编译后的规则库。
- CWTools `.cwt` 只是一轮 bootstrap 输入；`pdx-cwt v0.1` 将其导入自有 SQLite 数据库，之后数据库成为唯一权威。

### 2. 路径是语义输入

CWTools 的 `FileManager` 和 rule path options 都把 logical path 作为类型识别依据；`ResourceManager` 同时保存 physical path、logical path 和 overwrite 状态。

ParadoxCode 采用以下结论：

- `events/a.txt` 是语义身份的一部分，不能只根据文件内容分析。
- physical path、logical path、source root 必须是不同类型。
- 被覆盖资源仍需保留在 workspace 中，用于展示来源与解释覆盖关系，但不能参与活动定义解析。

### 3. Scope 是上下文栈而不是单个枚举

CWTools 使用包含 `Root`、`From` 和当前 `Scopes` 的 `ScopeContext`，并显式处理 `THIS`、`ROOT`、`FROM`、`PREV` 以及点链式 scope link。

ParadoxCode 采用以下结论：

- MVP scope state 至少需要 root、current stack、from stack。
- scope transition 与 command validation 必须使用同一个状态机。
- `Any/Unknown` 是容错分析所需状态，不能用错误 scope 代替未知 scope。

### 4. 每个文件应独立计算和替换

CWTools 为 entity 维护延迟 `ComputedData`，更新文件时替换资源并重算派生信息。这支持了按文件增量更新的方向。

ParadoxCode 采用更明确的 file shard：每个文件产生独立的 definition、reference、diagnostic input 和 scope fact 集合。更新文件时原子替换 shard，而不是修改多个全局可变集合。

### 5. Localisation 必须单独解析

CWTools 有独立的 YAML localisation parser 和大量 BOM、语言头、引号、注释相关测试。这印证 EU4 localisation 不是普通 PdxScript，也不应该直接交给通用 YAML parser。

## 不照搬 CWTools 的部分

- 不建立可切换的游戏分支 manager；项目核心直接固定为 EU4 专用规则。
- 不使用按字符串排序 source scope 来决定覆盖顺序；优先级由显式 `SourceRootOrder` 决定。
- 不依赖进程级可变 scope manager 或 string manager；分析 snapshot 必须可隔离测试。
- 不让运行时 analysis 解释 CWT comment directives；`pdx-cwt v0.1` 只需覆盖本次 EU4 corpus 使用的 construct，并在导入时转换成显式字段。
- 不让 LSP feature 直接访问 mutable resource manager；feature 只读取不可变 snapshot。
- 不把 parse tree 过早转换成只剩 key/value 的 entity tree。

## 从 Jomini 获得的启示

### 1. Clausewitz block 不是纯 object 或纯 array

Jomini 的 `TextToken::MixedContainer` 表明同一个 `{ ... }` 可以同时包含 property 和裸 value。ParadoxCode 的 grammar 不应强制 block 只能是 object 或 array。

### 2. EU4 语法包含常被忽略的构造

Jomini 的 lexer/tape 覆盖了：

- `=`, `<`, `<=`, `>`, `>=`, `!=`, `==`, `?=`
- quoted 与 unquoted scalar
- `rgb { ... }`、`hsv { ... }` 一类 header block
- 条件参数块 `[[parameter] ... ]` 与 `[[!parameter] ... ]`
- 重复 key
- UTF-8 BOM
- 注释紧邻 token
- 字符串转义

这些构造应进入 ParadoxCode grammar corpus 和 fuzz seeds。不能把日期、布尔、数字在 lexer 阶段固定分类，因为同一文本在不同规则上下文中可能有不同含义。

### 3. 线性表示适合高吞吐读取

Jomini 的 tape 使用成对 begin/end index 和借用 scalar，避免构建重量级对象树。ParadoxCode 的 file index 和 HIR 可以借鉴“紧凑、按文件、ID 引用”的思想，避免为每个语法节点分配复杂对象。

### 4. Fuzz 应覆盖全部公共读取路径

Jomini 不仅 fuzz parser，还遍历 DOM、字段分组、scalar conversion 和反序列化。ParadoxCode 的 fuzz 也不应只验证 parser 不 panic，还应覆盖 typed CST 遍历、增量 edit、formatter 和 lowering。

## 不直接使用 Jomini 作为编辑器 CST 的原因

Jomini 的主要目标是快速读取存档和游戏数据，而 Language Server 需要：

- 每个 token 的精确 source range
- 注释和空白 trivia
- 输入不完整时仍产生可导航结构
- 按编辑增量更新
- 错误节点与恢复边界
- 安全地产生最小 TextEdit

Jomini 的 README 明确指出其 tape writer 不保留注释。其 tape 还会规范化输入并跳过部分“ghost object”。因此它非常适合作为语法行为参考和未来批处理 CLI 的可选技术，但不作为 `pdx-syntax` 的主 CST。

## 对 ParadoxCode 的最终影响

1. `pdx-syntax` 的 Rust parser 负责 loss-aware、error-tolerant CST；Tree-sitter 只服务 Zed 编辑器侧 grammar/highlighting。
2. CST block 允许 property 与裸 value 混合。
3. `pdx-cwt v0.1` 一次性导入 CWTools 的 type/alias/cardinality/path/scope/reference 等模型；SQLite `eu4.pdxrules` 随后成为唯一权威规则源。
4. Workspace 使用 logical path、显式 source root 和按文件 shard。
5. HIR 保留 definition/reference/invocation/scope transition，而不是 JSON 对象。
6. MVP parser corpus 从一开始覆盖 Jomini 暴露的 EU4 特殊语法。
7. CWTools 实现不成为运行时依赖；`.cwt` 兼容范围冻结为本次 bootstrap corpus，而不是永久跟随上游。
8. 导入 provenance 锁定 config revision、输入逻辑 hash 和许可证，不记录或选择 EU4 版本。
9. CWT 导入前先生成 construct/directive inventory，corpus 中的 legacy spelling 必须显式导入或显式报错。
10. 规则 matcher 与动态 workspace type members 分离；`rule_hash` 只描述规范化规则逻辑内容。
