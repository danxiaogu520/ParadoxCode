# 规则系统重构：设计方案

> 状态：**阶段 1–5 已完成本次范围内的实现与本地验收**（2026-10-03）。阶段 0 于 2026-09-29 合入 main；阶段 1–5 在长期分支 `feat/rules-v2`。89 条待核验资源项按用户决定排除；PR CI 仍是合并门禁。完整落地状态见 §8.1，冻结版本和验收边界见 `docs/phase5-validation.md`。
> 前提：项目处于 0.x，**允许破坏性修改，不考虑历史兼容**；规则源格式**继续使用 JSON**。
> 重构前问题的统计口径：2026-09-29，`rules/eu4`（`source_format_version` 10）与 `crates/*`。第 1 节保留当时的问题和代码位置，当前入口见文末。
> 当前新规则源统一命名为 `rules/eu4`；历史新源命令的目录路径也使用现名，实际结果仍以各轮冻结身份为准。重构前同名旧树已整体替换。

## 0. 目标

让规则本身成为一个**自洽的小型类型语言**：作者写面向人的源语言（可嵌套、可复用），
编译器（`rulec`）做语义检查并降级为**树形 runtime IR**；消费方沿 IR 树行走，不再在运行时从扁平行重建结构。

2026-10-02 的范围简化删除字段 `when`/`unless`、结构推导 subtype、枚举属性列和通用 trait 元数据；
显式 subtype、脚本控制流和 Callable 文本条件继续使用。上述删除属于方案变更，不列为待补实现。

非目标（第一版不做）：mod 叠加规则（`extends`/`patch`）、多游戏、按游戏版本（`since`/`until`）分叉。
只保证 IR 中 `game_id` 的位置，不为这些特性做任何语义设计（决策 D4）。

## 1. 现状问题

### 1.1 冗余与遗留数据

| 问题 | 证据 |
|---|---|
| `catalog/records`（4.9MB，12,962 行）是死数据 | 唯一语义消费点 `crates/ide/src/hover/symbol.rs:226` 的 `known_keys`，只读 `record.fields.keys()`，把 `key`/`line`/`shape`/`source_file`/`directives` 等元字段名当成“已知脚本键”（隐性 bug）。其余引用：`canonical.rs:77`（hash）、`runtime.rs:152`（排序）、`rulec.rs:1752`（断言 12,962）、`crates/engine/examples/mem_probe.rs` |
| 600 条 subtype 未被执行 | `SemanticRule` 无 subtype 字段；subtype 只存在于 records |
| on_action 规则 226 倍重复且丢失区分度 | `semantic/contexts/on-action.json` 1131 条中 1130 条是同 5 条规则 ×226；`event.country`/`event.province` 被并入同一 `root:on_action`，任何 on_action 都接受两种事件 |
| 语义完全重复的规则 | 忽略 id/溯源/文档后共 1,227 条 |
| 溯源字段失真 | 8,294 条 `source_file` 为已不存在的 `semantic-rules.json`；`line` 是旧行号；id 前缀与 context 不一致 |
| `symbol-descriptors` 2,657 条无信息量 | 全部 `replace-by-symbol` + 大小写不敏感，混入 `!cost`、`effect:<continent>` 等垃圾名。消费方：`resolution.rs:1049/1245/1306`、`navigation.rs:681` 查策略（查不到时默认值也是 replace，等于常量）；`hover/symbol.rs:236` 与 `semantic_tokens.rs:108` 把 `kind_id` 当“已知键”（与 records 同类 bug）。真实的覆盖语义硬编码在 `crates/game/src/eu4/mod.rs:620` 的 `RESOLVED_SYMBOL_KINDS`（控制 shadow 警告） |
| 常量/双重编码字段 | `strict_min` 全为 true；`deprecated: true` 为 0 条；必填同时有 `required`（仅 2 条 true）与 `min_occurs: 1`（4,375 条）；任意标量有 `"any_scalar"` 与 `{"any_scalar": null}` 两种写法；float 边界混用数字与字符串（`"min": "0"`）；`push_scope` 出现 `Unit`/`unit`；`replace_scope` 顺序不稳定；大量显式 `null`/`[]` 默认值 |
| 零散疑点 | `profile/lexicon.json:32` `"ruler_personality": "ancestor_personalities"` 疑似笔误；`is_triggered_only`/`hidden`/`mean_time_to_happen` 在 `type:event` 与 `root:event` 各一份；README 统计过期（写 8525/121/2667，实为 8463/124/2657）；类型描述 `path` 带 CWT 遗留前缀 `game/` |

### 1.2 表达力的结构性限制

1. **扁平行 + `parent_path` 字符串路径**：无法复用子结构（on_action 重复的根因）；路径段只能是字符串，于是有 324 处魔法段（`<religion>` ×163、`<government_mechanic_power>` ×31、`<mission>` ×14、`enum[country_tags]`…）。运行时 `crates/ide/src/semantic.rs:93` 的 `context_rule_view` 每个 context 都要把扁平行重新分桶（`parent_literal`/`parent_dynamic_by_len`/alternative 分组），本质是在运行时重建一棵树。
2. **无条件/变体机制**：没有 subtype、`when`、互斥/依赖字段；只能靠多行 alternative + 语义模糊的 `alternative_id`（3,989 行有值，其中 2,858 行等于自身 id）。
3. **类型表达式割裂**：`KeyMatcher`（8 种）与 `ValueMatcher`（16 种）重叠但不可组合；无 union；template 不能用于值；`TypedPrefix` 仅 `trigger_value:` 一个特例。
4. **作用域模型混杂**：`profile/scopes.json::scope_names`（32 个）同时包含作用域类型、寄存器、链接；链接只有 `scope_member_aliases` 一张表（7 条），没有“从哪来”；而 540 条 trigger/effect 行又用 `allowed_scopes`+`push_scope` 把链接重复写了一遍（如 `controller`：`allowed=[province]`，`push=country`）。
5. **符号定义与引用有 5 个入口**：`types/descriptors`、`catalog/symbol-descriptors`、`profile/symbols.json` 中的 `definitions`（45）/`value_definitions`（19）/`container_value_definitions`（3）/`conditional_definitions`（1）/`token_definitions`（2），以及与结构脱节的全局 `references`（18 条，“任何位置的 `event = X` 都是事件引用”）。
6. **文件根结构有约 10 种机制**：`path`/`path_file`/`type_per_file`/`name_from_file`/`skip_root_paths`/`type_key_filter`/`starts_with`/`root_entries`/`body_context`、`types/root-keys.json`、`types/root-scopes.json`、`profile/semantics.json::root_entry_specs`、`profile/scopes.json::root_scopes`、`entry_wrapper_reroutes` 启发式（`crates/rules/src/model.rs`）。

### 1.3 数据/代码边界不清

控制流与结构语义硬编码在 Rust：

| 硬编码 | 位置 |
|---|---|
| `if`/`else_if`/`else` 链与 `limit` 守卫 | `crates/ide/src/lints.rs:37,130,179-191`；`diagnostics.rs:3121,3130`；`crates/hir/src/model.rs:625-632` |
| `NOT` 多条件语义 | `lints.rs:32,63,72` |
| 值作键的分支容器 `random`/`random_list`/`trigger_switch` | `diagnostics.rs:3025` |
| `limit`/`trigger`/`mtth`/`mean_time_to_happen` 切换上下文 | `dynamic_rules.rs:1868` |
| 仅显示的块 `tooltip` | `diagnostics.rs:3016` |
| 寄存器 `this/root/prev/from/fromfrom…` 与 `event_target:` 前缀 | `dynamic_rules.rs:1856-1858` |

同时 profile 又有不完整的 `control_flow_keys`（13）与 `transparent_scope_wrappers`（3）。

### 1.4 迁移规模

规则 crate 之外，对现有模型的直接依赖：`parent_path` 298 处、`KeyMatcher::` 144 处、`ValueMatcher::` 206 处、`child_context` 60 处、`alternative_id` 26 处、`type_descriptors` 31 处。
热点：`ide/semantic.rs`(49)、`ide/diagnostics.rs`(37)、`ide/completion/context.rs`(36)、`hir/scope.rs`(28)、`ide/resolution.rs`(22)、`ide/completion/candidates.rs`(21)、`hir/semantics.rs`(17)。

### 1.5 应保留的优点

严格输入（`deny_unknown_fields`、manifest 显式列文件、canonical hash、无外部规则依赖）；封闭的 matcher 枚举；已有的声明式种子：template key、bindings、`semantic_context_inheritance`、`TypeRootScope`（ROOT/THIS/FROM）。

## 2. 语言设计

### 2.1 顶层结构与目录组织

一个规则文件可以包含以下任意段；编译器把所有文件的同名段合并到同一名字空间，重名即报错：

```
files     → 路径选择 → 解析器 / 根 schema
schemas   → 结构：fields / patterns / items / include / def / 参数
mixins    → 纯结构字段包（编译期展开）
types     → 符号名字空间：resolution / subtypes / open / builtin / impl
traits    → 名称标记；实现数据由 types.*.impl 给出
enums     → 字面成员列表
scopes    → 作用域类型 / 寄存器 / 链接 / 兼容
```

与旧草案相比，**没有顶层 `intrinsics` 段**：控制流原语是字段属性（2.9），避免“键名全局生效”；**没有单独的 `tables`**：enum 只保存字面成员列表（2.6）。

目录按游戏领域组织（决策 D3）：

```
rules/eu4/
  manifest.json          # 列出全部文件；game_id、target_game_version
  game.json              # 非语言部分：install、filesystem 扫描、hover_cards、fallback_keys
  core/
    scopes.json          # scopes 段
    control-flow.json    # 控制流 mixin（if 链、AND/OR/NOT、random_list…）
    traits.json          # 内置 trait
    trigger.json effect.json modifier.json
  events.json            # files + schemas + types + enums(on_actions)，一个领域一处
  decisions.json  missions.json  history/…  common/…  interface/…  map/…
```

### 2.2 类型表达式（key 与 value 共用）

写法为**字符串迷你语法**（决策 D2），编译器解析为统一的 `Matcher`，取代 `KeyMatcher`/`ValueMatcher`。
解析错误报告“源文件 + JSON pointer + 表达式内列号”。

```ebnf
expr      = alt { "|" alt } ;
alt       = prim [ range ] | ctor "<" arg ">" | "path" [ "<" name ">" ] | literal | param ;
prim      = "scalar" | "int" | "float" | "bool" | "date" | "loc" | "link" | "opaque" ;
range     = "[" [ number ] ".." [ number ] "]" ;
ctor      = "ref" | "def" | "enum" | "scope" | "quoted" ;
arg       = name [ "." name ]            (* ref<event.country>：类型.subtype *)
          | name "strip_prefix" name     (* ref<estate strip_prefix estate_>：去掉词缀 *)
          | param ;
literal   = "'" { char | "{" expr "}" } "'" ;   (* 无洞即常量；有洞即模板 *)
param     = "$" name ;                    (* 参数化 schema 的形参 *)
name      = ident ;
```

| 表达式 | 含义 | 取代的旧 matcher |
|---|---|---|
| `scalar` | 任意标量 | `AnyScalar` |
| `'yes'` | 常量 | `Exact` |
| `'monthly_{ref<government_mechanic_power>}'` | 模板 | `Template`、`TypedPrefix`（`'trigger_value:{ref<scripted_trigger>}'`） |
| `int[1..10]` `float[0..]` `bool` `date` | 标量类型；边界一律为数字 | `Int`/`Float`/`Bool`/`Date` |
| `loc` | 本地化键 | `Localisation` |
| `path` `path<gfx>` | 文件路径；`<texture>` 等为路径类别 | `Filepath`/`TexturePath` |
| `ref<event>` `ref<event.country>` | 符号引用；可限定 subtype | `Type`、`Dynamic`、`lexicon.member_kind_aliases` |
| `def<country_flag>` | 在此处**定义**一个符号 | `DynamicSet`、`profile.value_definitions` |
| `enum<country_tags>` | 枚举成员 | `Enum` |
| `scope<country>` `scope<any>` | 作用域表达式（寄存器、链接、tag…） | `Scope` |
| `link` | 仅用于 key：任何作用域链接/寄存器/前缀链接（2.8） | trigger/effect 中逐条手写的链接行 |
| `quoted<trigger>` | 引号字符串，内容按 schema 解析 | `RuleShape::QuotedScript`、`quoted_script_definition_keys` |
| `opaque` | 不检查的文本 | `Opaque` |
| `a \| b` | union，按书写顺序尝试 | 多行 alternative |

语义约定：键与符号名一律大小写不敏感（与现状一致）；union 的各分支必须同为标量或同为块（形态不同的重载用 2.3 的字段数组）。

### 2.3 Schemas：结构

schema 是一个**块**的描述：

```jsonc
"schemas": {
  "event_body": {
    "include": [],                                   // mixin 名列表，编译期展开（2.7）
    "fields": {                                      // 精确键，哈希查找
      "id":      { "value": "scalar", "card": "1" },
      "title":   { "value": "loc" },
      "picture": { "value": "ref<sprite> | enum<dlc_event_pictures>", "card": "0..*" },
      "desc":    [ { "value": "loc", "card": "0..*" },               // 数组 = 形态重载
                   { "body": "conditional_desc", "card": "0..*" } ],
      "option":  { "body": "event_option", "card": "0..*" }
    },
    "patterns": [                                    // 非精确键，按序尝试
      { "key": "ref<scripted_trigger>", "value": "scalar" }
    ],
    "items": null,                                   // 块内裸值的类型（列表块）
    "open": false                                    // 未声明的键是否允许；默认 false
  }
}
```

**字段规格**（fields 的值或 patterns 的元素）：

| 键 | 含义 | 默认 |
|---|---|---|
| `key` | 仅 patterns：键的类型表达式 | — |
| `value` / `body` / `list` / `map` | 四选一：标量类型表达式 / 块 schema 名（或 `"self"`）/ 裸值列表块，值为元素的类型表达式 / 同构映射块 `{ "key": expr, "value": expr }` 或 `{ "key": expr, "body": schema }` | — |
| `card` | `"1"`、`"0..1"`、`"1..*"`、`"0..*"`、`"2..5"` | `"0..1"`（现有 8,463 行中 7,355 行 `max=1`，1,013 行无上限） |
| `scope` | 作用域效应，见 2.8 | 无 |
| `def` | 此位置定义一个符号实例，见 2.4 | 无 |
| `control` | 控制流原语，见 2.9 | 无 |
| `doc` / `severity` / `deprecated` | 文档 / 诊断等级 / 弃用 | 空 / error / false |

**`value` 与 `body` 严格区分**：`value`（以及 map 的 `"value"`）永远是类型表达式，`body` 永远是 schema 名，二者不会互相猜测。
**schema 级简写**：整个 schema 写成 `{ "map": {...} }` 或 `{ "list": expr }`，分别等价于只有一个 pattern / 只有 `items` 的 schema。
**查找与形态分派**：先在 `fields` 中按键查，并按形态（标量 / 块 / 引号脚本）过滤重载；精确字段没有形态匹配的重载时，才落到 `patterns` 按序尝试。同形态重载按书写顺序匹配；编译器检查不可达重载（后一个被前一个完全覆盖）。
**块体 `"body": "self"`**：沿用当前 schema（用于作用域切换块与控制流块，2.8、2.9）。

**参数化 schema**：名字带形参列表，实参在引用处给出；编译期**单态化**展开，runtime 无泛型：

```jsonc
"on_action_body<S>": { "fields": {
  "events":        { "list": "ref<event.$S>" },
  "random_events": { "map": { "key": "int", "value": "ref<event.$S> | '0'" } }
}}
```

约束：形参只能出现在类型表达式或另一 schema 的实参位置；只允许一层参数（schema 形参不能再作为另一参数化 schema 的实参以外的用途）；每个参数化 schema 的实例数上限 64，超出即编译错误（防爆炸）。

### 2.4 Files 与 def：根结构合入 schema

`files` 取代 `catalog/file-categories.json` 与类型描述中的所有路径字段；文件本身就是一个 schema，符号实例在 schema 的某些位置用 `def` 声明：

```jsonc
"files": {
  "events":           { "path": "events", "ext": "txt", "root": "events_file" },
  "scripted_effects": { "path": "common/scripted_effects", "ext": "txt", "root": "scripted_effects_file" },
  "event_pictures":   { "path": "interface", "ext": "gfx", "root": "gfx_file" },
  "localisation":     { "path": "localisation", "ext": "yml", "parser": "localisation" }
},
"schemas": {
  "events_file": { "fields": {
    "namespace":      { "value": "scalar", "card": "0..*" },
    "country_event":  { "def": { "type": "event.country",  "name": "field:id" },
                        "scope": { "set": { "root": "country",  "this": "country" } },
                        "body": "event_body", "card": "0..*" },
    "province_event": { "def": { "type": "event.province", "name": "field:id" },
                        "scope": { "set": { "root": "province", "this": "province" } },
                        "body": "event_body", "card": "0..*" }
  }},
  "decisions_file": { "fields": {
    "country_decisions": { "map": { "key": "def<decision>", "body": "decision_body" } }
  }},
  "scripted_effects_file": { "map": { "key": "def<scripted_effect>", "body": "effect" } }
}
```

`files` 条目：`path`（目录前缀，去掉 `game/` 遗留前缀）、`ext`、`file`（精确文件名，取代 `path_file`）、`strict`（不递归子目录）、`parser`（`script`/`localisation`/`asset`/`syntax-only`，默认 `script`）、`resolution`（`replace-by-path`/`merge`，默认 `replace-by-path`）、`root`（根 schema 名；或一个带 `def` 的字段规格，表示整个文件就是一个实例）。

`def` 规格：
- 简写：值或键位置写 `def<T>`，实例名取该标量本身。
- 完整：`{ "type": "T[.subtype]", "name": ... }`，`name` 取 `"key"`（默认）/ `"field:<字段名>"` / `"file"`（文件名去扩展名），可附 `"strip_prefix"`/`"strip_suffix"`（取代 `name_strip_prefix/suffix` 与 `lexicon.member_name_suffixes`）。
- 在 def 位置写 subtype（`event.country`）即给实例打上该 subtype；subtype 只由定义位置赋予（2.5）。

旧机制映射：

| 旧机制 | 新表达 |
|---|---|
| `skip_root_paths` | 文件 schema 中显式写出外层包装块 |
| `type_key_filter`（排除 potential/slot…） | 精确字段优先于 patterns，剩余键才落到 `def` 所在的 pattern |
| `starts_with` | def 的键写模板：`"key": "'mission_{scalar}'"` |
| `type_per_file` / `name_from_file` | 文件根即 def：`"root": { "def": { "type": "T", "name": "file" }, "body": "..." }` |
| `root_entries` / `body_context` | 由 def 位置的 `body` 直接给出 |
| `root-keys.json` / `root-scopes.json` / profile `root_scopes` | def 位置上的键与 `scope.set` |
| `root_entry_specs.insertion` | 由值类型推导补全插入形式（`def<country_tag>` → 引号赋值，`enum<…>` → 裸值，块 → 块） |
| `profile.symbols.definitions`（45）/ `container_value_definitions`（3） | def 位置（后者为 `"name": "field:name"`） |
| `profile.symbols.value_definitions`（19，`set_country_flag`） | 值位置 `"value": "def<country_flag>"` |
| `profile.symbols.references`（18） | 值位置 `ref<…>`；**不再有全局引用表**（见 3 的 D9） |
| `entry_wrapper_reroutes` 启发式 | 删除，由 schema 显式描述 |

结果：Types 子系统只剩纯符号语义；“去哪收集实例、哪里是引用”全由结构回答；一个类型可有多个 def 位置。

### 2.5 显式 subtype 与统一展示

subtype 只用于由定义位置明确赋予的符号分类，例如 `def` 的
`event.country` 与对应的 `ref<event.country>`。subtype 声明为空对象。
不再依据实例字段推导 subtype，不支持字段 `when`/`unless` 或 subtype 内的 `impl`。

schema 字段统一提供，继续执行类型、形态、作用域与数量检查。
本地化和图标 binding 合并至类型级 `impl`，所有实例均可展示；原条件必需
binding 改为可选展示项，类型原有的必需 binding 保持原要求。
原先引用结构推导 subtype 的表达式放宽为基础类型引用。
字段数量组合继续由 `forms` 表达，脚本 `if`/`else` 等由 `control` 表达。
源格式版本升为 13，解析器拒绝已删除的条件语法和未使用的泛化声明。

### 2.6 Enums：字面成员列表

```jsonc
"enums": {
  "on_actions_country": ["on_startup"],
  "on_actions_province": ["on_province_religion_converted"]
},
"schemas": {
  "on_actions_file": { "patterns": [
    { "key": "enum<on_actions_country>", "body": "on_action_body<country>", "card": "0..*" },
    { "key": "enum<on_actions_province>", "body": "on_action_body<province>", "card": "0..*" }
  ] }
}
```

不同作用域由明确分组的 enum 和 pattern 表达，共用 `on_action_body<S>`。
不再保留属性列、行子集或 `$key` 隐式绑定，参数只来自 schema 显式形参。
`profile.enum_extra_members` 直接并入成员列表；`engine_set_flags` 见 2.7 的 `builtin`。

### 2.7 Types、mixin 与 trait

**types** 字段：

| 字段 | 含义 | 取代 |
|---|---|---|
| `resolution` | `replace`：游戏加载时按名覆盖，同名定义产生 shadow 警告 / `independent`：结构性重复键（每个单位文件里的 `maneuver`），各自独立，不产生 shadow 警告。两者的导航与重命名行为相同（都与现状的 `replace-by-symbol` 一致） | `symbol-descriptors`、`RESOLVED_SYMBOL_KINDS` |
| `subtypes` | 见 2.5 | records 中未执行的 subtype |
| `open` | 开放世界：任意名字都可能存在（`event_target`、`saved_name`…），`ref` 不报未定义 | `open_world_value_kinds` |
| `builtin` | 引擎预置的成员 | `engine_set_flags`、`hardcoded_*` 类 |
| `impl` | 实现的 trait 及其参数 | 见下 |

默认 `resolution` 为 `independent`；只有 `RESOLVED_SYMBOL_KINDS` 中的 13 个类型写 `replace`。
`closed_dynamic_kinds` 即“不 `open` 且有 `def<>` 位置”的默认情形，无需声明。

**mixin = 结构复用**（属于 Schemas，编译期展开，运行时不存在）：

```jsonc
"mixins": {
  "gated":       { "fields": { "potential": { "body": "trigger" }, "allow": { "body": "trigger" } } },
  "ai_weighted": { "fields": { "ai_will_do": { "body": "modifier_rule" } } },
  "modifier_block": { "fields": { "modifier": { "body": "modifier" } } }
},
"schemas": { "decision_body": { "include": ["gated", "ai_weighted"], "fields": { "effect": { "body": "effect" } } } }
```

include 冲突（两个 mixin 或 mixin 与本体声明同一键）为编译错误，除非本体显式覆盖（本体优先，且必须标 `"override": true`）。

**trait = 消费方识别的名称标记**（属于 Types）。声明只写空对象，实际绑定或 body 由类型的 `impl` 给出，不保留通用 params、bindings、requires 或 capability 标签。第一版内置集合固定为 4 个，新增 trait 需要改 Rust（runtime 要理解它的语义）：

```jsonc
"traits": {
  "Localised":      {},                                   // binding 集合由 impl 给出
  "HasIcon":        {},                                   // binding 集合由 impl 给出
  "ModifierSource": {},
  "Callable":       {}
},
"types": {
  "decision": { "impl": { "Localised": {
    "name": { "loc": "$_title", "required": true },
    "desc": { "loc": "$_desc" }
  } } },
  "idea_group": {
    "impl": { "Localised": { "name": { "loc": "$", "required": true },
                             "bonus": { "loc": "$_bonus", "required": true },
                             "start": { "loc": "$_start" } } }
  },
  "building":        { "impl": { "Localised": { "name": { "loc": "building_$", "required": true } },
                                 "HasIcon": { "icon": { "sprite": "GFX_$", "required": true } },
                                 "ModifierSource": {} } },
  "scripted_effect": { "impl": { "Callable": { "body": "effect" } }, "resolution": "replace" }
}
```

- `Localised`/`HasIcon` 的 **impl 逐条枚举 binding**：实参名即 binding 名（hover 行标签与稳定身份的一部分），实参值给出 `loc`/`sprite` 模板与 `required`。binding 集合是**每类型的数据**，故 trait 本身不声明 binding。D19 变更说明：① 语料用例 188 条 binding、96 个类型、38 个 binding 名；② IR 退化形态——单态化时展开为每类型的 `(name, 模板, required)` 扁平行，runtime 无动态派发；③ 对 64 实例上限无影响（trait impl 不是参数化 schema 实例）；④ 规范 diff 见 `docs/rules-language.md` §7.3。
- binding 模板里的 `$` 是**实例名占位符**，与类型表达式的 `$形参` 不在同一语法中（binding 模板不是类型表达式）。
- `impl` 只在类型级声明，对该类型的所有实例生效。
- `Localised`/`HasIcon` 取代 `bindings/localisation.json` 与 `bindings/sprite.json`；`Callable` 取代 `dynamic_definition` 与 `token_definitions`（`$param$` 参数由 Callable 统一处理），调用语义由其 trait 身份和 body 实参决定；`ModifierSource` 取代 profile `semantic_context_inheritance` 中 22 条 `type:X → [modifier]`，消费方直接查询实现该 trait 的类型以执行 modifier 诊断。引用只支持具体类型及显式 subtype。

**判定规则**：只有引擎/IDE 会统一处理它（hover、本地化检查、调用参数推导）才定义为 trait，否则用 mixin。
**防过度设计约束**：无 trait 继承链；同一作用域对同一 trait 只能 impl 一次、同一 binding 名只能贡献一次；全部编译期展开成扁平数据，runtime 无动态派发。

### 2.8 Scopes

```jsonc
"scopes": {
  "types":     ["country", "province", "unit", "monarch", "heir", "consort",
                "mercenary_company", "rebel_faction", "religion", "culture", "advisor", "leader",
                "trade_company", "global", "none"],
  "registers": { "root": {}, "this": {}, "prev": { "chain": true }, "from": { "chain": true } },
  "links": {
    "owner":      { "from": ["province", "unit"], "to": "country" },
    "controller": { "from": ["province"], "to": "country" },
    "capital":    { "from": ["country"], "to": "province" },
    "emperor":    { "from": ["any"], "to": "country" },
    "event_target:{ref<event_target>}":        { "from": ["any"], "to": "any" },
    "global_event_target:{ref<global_event_target>}": { "from": ["any"], "to": "any" }
  },
  "compat": []
}
```

- EU4 的贸易节点执行作用域统一为 `province`，包括迭代器、节点名称块和贸易政策寄存器。`trade_node` 仅保留为节点定义与 `ref<trade_node>` 的符号类型，不作为 scope 类型或别名。
- `any` 是保留字，表示任意作用域，不出现在 `types` 中。
- `registers.chain` 表示可重复拼接（`prev_prev`、`fromfrom`…），取代 `dynamic_rules.rs:1856` 的硬编码列表与 `scope_names` 中手写的 `prev_prev`。
- 链接键可以是模板，取代 `dynamic_scope_prefixes`；`dynamic_value_prefixes` 同理以值模板表达（`'variable:{ref<variable>}'`）。
- `scope_completions` 由 registers + types 推导，不再手写。

规则上的作用域效应统一为一个字段，**只有对象形式**（不设字符串简写，消除 `"scope": "country"` 的歧义）：

```jsonc
"scope": { "in": ["country"], "push": "province", "set": { "root": "country", "this": "country", "from": "any" } }
```

`in` 取代 `allowed_scopes`（缺省 = any），`push` 取代 `push_scope`，`set` 取代 `replace_scope` 与 `TypeRootScope`。

trigger/effect 中的作用域切换块不再逐条手写，由一个 pattern 统一描述：

```jsonc
"trigger": { "patterns": [ { "key": "link", "body": "self" } ] }
```

`link` 键匹配时，编译器已知链接的 `from`/`to`，runtime 据此检查当前作用域并 push 目标作用域。与链接同名的标量 trigger（如 `controller = ROOT`）照常写在 `fields` 中：按 2.3 的查找规则，标量形态命中精确字段，块形态落到 `link` pattern。只将 trigger/effect 语义一致的纯链接折叠进 `scopes.links`；只属于一个上下文的块保留为带 `body`、`scope.in`、`scope.push` 的显式字段。用户已确认 EU4 的 `any_*` 仅用于 trigger/limit，effect 使用 `every_*` 或 `random_*`，不能通过全局 `link` 扩大其可用上下文。

### 2.9 控制流：字段上的 `control` 属性

控制流原语不再按键名全局生效，而是作为字段规格的 `control` 属性写在 `core/control-flow.json` 的 mixin 中，trigger/effect schema 各自 include：

```jsonc
"mixins": { "effect_control": { "fields": {
  "if":             { "body": "self", "card": "0..*", "control": { "kind": "branch", "guard": "limit", "chain": ["else_if", "else"] } },
  "else_if":        { "body": "self", "card": "0..*", "control": { "kind": "branch_continue", "guard": "limit" } },
  "else":           { "body": "self", "card": "0..*", "control": { "kind": "branch_continue" } },
  "limit":          { "body": "trigger", "control": { "kind": "guard" } },
  "random_list":    { "map": { "key": "int", "body": "self" }, "control": { "kind": "weighted" } },
  "random":         { "body": "random_body", "control": { "kind": "chance" } },       // random_body = self + chance 字段
  "trigger_switch": { "body": "trigger_switch_body", "control": { "kind": "switch", "on": "on_trigger" } },
  "hidden_effect":  { "body": "self", "control": { "kind": "transparent" } },
  "tooltip":        { "body": "self", "control": { "kind": "display_only" } }
}}}
```

`kind` 是 Rust 中的**封闭枚举**，逐一对应 1.3 的硬编码：

| kind | 语义 | 删除的硬编码 |
|---|---|---|
| `branch` / `branch_continue` | if 链：`chain` 列出可跟随的兄弟键；`guard` 为守卫子块 | `lints.rs` if 链、`diagnostics.rs:3130`、`hir/model.rs:625` |
| `guard` | 守卫子块，体在 trigger 上下文 | `diagnostics.rs:3121`、`dynamic_rules.rs:1868` 的 `limit` |
| `logic` | `AND`/`OR`/`NOT`，带 `"op"`；作用域透明；`NOT` 的多条件 lint 由 `op` 驱动 | `lints.rs:32,63,72`、`transparent_scope_wrappers` |
| `constant` | 标量 bool 谓词的结果等于其值；仅声明此原语的字段参与逻辑常量折叠 | `always` 按名字识别的常量 lint |
| `weighted` / `chance` | 值作键的加权分支 / 概率块 | `diagnostics.rs:3025` 的 `random`/`random_list` |
| `switch` | 分支键是“`on` 字段所指 trigger 的合法值”：runtime 读取 `on_trigger` 的值，到 trigger schema 中查该键的标量字段，用其值 matcher 校验各分支键；分支体为 `self`。此行为完全由 kind 实现，不需要额外的表达式语法 | `diagnostics.rs:3025` 的 `trigger_switch` |
| `transparent` | 作用域透明包装 | `transparent_scope_wrappers` |
| `display_only` | 只影响提示，不执行 | `diagnostics.rs:3016` |

`trigger`/`mtth`/`mean_time_to_happen` 切换上下文**不是**控制流，只是普通字段（`"body": "trigger"`、`"body": "mtth"`）；`dynamic_rules.rs:1868` 的硬编码随树形 IR 自然消失。`control_flow_keys` 由带 `control` 的字段推导。

`constant` 的 D19 证据与退化形态见 `rules-language.md` §9：保留普通 scalar bool IR，
只增加字段属性，不创建 schema 实例，不影响 64 实例上限。只有声明此属性的谓词才能折叠，
避免把 `is_capital = yes` 等普通条件误当成常量。

### 2.10 语言基础设施

- **编译期语义检查**：未定义/未引用的 schema、type、enum、mixin；include 冲突；不可达重载；参数化实例数超限；作用域链接 `from` 不匹配。
- **JSON Schema**：由 Rust 源类型生成（schemars），供编辑器补全与校验；类型表达式在 JSON Schema 中只做 `pattern` 粗校验，精确校验靠 `rulec`（接受的代价）。
- **工具**：`rulec fmt`（规范化格式、省略默认值、字段排序）、`rulec check`（只做语义检查，编辑器可调用）。
- **语言规范**：`docs/rules-language.md`，本节内容的正式版。
- **溯源自动化**：编译器用“源文件 + JSON pointer”生成溯源；源中不再手写 `id`/`source_file`/`line`。

## 3. 已定决策

| # | 决策 | 性质 |
|---|---|---|
| D1 | **一次性切换 + 一次性转换脚本**：在长期分支上完成新语言、新 IR 与全部消费方改造，一次切换后删除旧模型；不存在新旧 IR 并存期 | 用户决策 |
| D2 | 类型表达式为**字符串迷你语法**，规范给出 EBNF（2.2） | 用户决策 |
| D3 | 规则目录**按游戏领域**组织，公共部分放 `core/`（2.1） | 用户决策 |
| D4 | mod 叠加 / 多游戏 / 版本分叉**只预留不设计** | 用户决策 |
| D5 | 源格式 JSON；不以 `SemanticRule` 为中间层；`parent_path`、`context` 字符串、`alternative_id` 一次去掉 | 原决策 |
| D6 | 正确性靠 golden 测试 + 原版全量扫描（sweep）的**基线对比**（第 6 节），不做新旧逐行对拍 | 原决策，补充基线 |
| D7 | Schemas / Types / Scopes 三个正交子系统为骨架；root entry 通过 `files` + `def` 合入结构；mixin 与 trait 分离 | 原决策 |
| D8 | enum 只保留字面成员列表（**属性列已于源版本 13 删除**）；控制流为字段属性而非顶层段；作用域效应只有对象形式；字段 `card` 强制显式（原默认 `0..1` 已被 D14 推翻） | 技术决定，后续简化 |
| D9 | 删除全局 `references` 表：引用只来自 schema 中的 `ref<>`。未被 schema 覆盖的位置不再产生引用——这是有意的行为变化，由基线中的引用计数对比兜底 | 技术决定 |
| D10 | 参数化 schema 编译期单态化；runtime 无泛型、无动态派发 | 技术决定 |
| D11 | 第一版内置 trait 固定为 Localised / HasIcon / ModifierSource / Callable | 技术决定 |
| D12 | profile 拆分：非语言部分（install、filesystem 扫描、hover_cards、fallback_keys）移入 `game.json` 且不参与语言语义；其余全部并入语言（第 4 节表） | 技术决定 |
| D13 | **职责分离：机制闭集、策略全量。** `engine`/`hir`/`ide`/`pdc`/`parser` 只实现机制（匹配、作用域、单态化、索引、诊断框架）；一切游戏策略（键、形状、作用域、名字、目录）只能来自规则数据。程序**不按规则目录结构读规则**（全盘读取，D16）；规则目录只服务人工维护 | 用户决策 |
| D14 | **规则显隐（默认值哲学）。** 高频设默认、低频强制显式；默认值**只许出现在收紧语义一侧**（放宽型默认必须挂基线）；默认准入门槛为单值占比 ≥2/3；一切默认可机械展开（`fmt --expanded` / hover）。据此 **`card` 无默认、强制显式**（数据：`1` 占 50%、`0..1` 占 35%、可重复型 13%，无多数派，且它驱动「缺必填键 / 重复键」两类诊断），`FileRule.resolution` 默认翻转为 `merge` | 用户决策 |
| D15 | **避免重复的边界。** 复用（mixin / 参数化 schema / enum / trait / `self`）必须满足三次法则（≥3 个真实站点才抽象）；深度上限为 `include` 一层、参数一层、禁止传递链；复用不得破坏溯源 | 用户决策（配套） |
| D16 | **废除 manifest + 全盘读取。** 目录是维护单元，清单是派生物；全盘扫 → 路径排序合并 → 同名定义报错（确定性由排序保证）。规则 hash 降级为**身份/断言**（基线对拍、bug 报告、CI 漂移），不参与运行时缓存决策；**缓存以构建身份为戳，更新即强制重建**；构建身份预留规则 hash 字段（D4 精神） | 用户决策 |
| D17 | **任务树三层归属。** `crates/game/src/eu4/mission` 拆为：事实→规则（顶层块=树、`required_missions`=边、`slot`/`position`=几何字段）；机制→引擎（引用图组装、环检测、稳定字段序回写）；游戏形态→游戏包，经**能力接口**暴露。引擎与 ide 不得按 `game_id` 分支；第二个结构化视图出现前不抽象 `tree_views` DSL | 用户决策 |
| D18 | **`INSTALL_DESCRIPTOR` 进 `game.json`。** 判据：平台探测是机制（留引擎），游戏识别是数据（进 `game.json` 的 `install` 段，细化 D12） | 代定（用户授权） |
| D19 | **特性准入门槛（硬性变更模板）。** 新增语法 / trait / control kind 必须列出：≥3 处真实语料用例、IR 退化形态、对 64 实例上限的影响、规范 diff。机制闭集（`control.kind`、四个内置 trait）的修改视同规范修改 | 代定（用户授权） |
| D20 | **无猜测、无静默回退。** 规则没说的，引擎不猜；`open` / `opaque` / `builtin` / `fallback_keys` 是显式豁免通道，数量只减不增、目标归零 | 提案（待认可） |

技术决定均可在实施中凭数据推翻，推翻时更新本表。

### 3.1 边界的判定测试与例外

| # | 判定测试 | 例外 / 备注 |
|---|---|---|
| D13 | 实现第二个游戏（或测试内假想游戏）时 Rust 侧 diff 为零；grep 门：`engine`/`hir`/`ide`/`pdc`/`parser` 除 fixture 外零游戏专名 | 机制闭集（`control.kind`、四个内置 trait）有意留 Rust，是特性不是违例；`crates/game` 定性为游戏数据包 |
| D14 | 每个可省字段的默认值都有多数派数据（逐字段清单以 `docs/rules-language.md` 各表的 Default 列为唯一权威）；必填缺失在解析层即拒绝（无 `serde(default)` + `deny_unknown_fields`），语义层再查 | `scope.in`（`any` 59%）是唯一弱多数，靠 sweep 基线兜底；`doc` 是内容字段不参与裁决 |
| D15 | 每个抽象提案必须列出 ≥3 个具体站点 | 局部重复优于跨文件链 |
| D16 | 同一输入两次全盘读取 → 同合并结果、同 hash；目录内增删无关文件不影响产物 | `game.json` 是包配置不是 manifest（保留文件名） |
| D17 | 引擎/ide 无 `game_id` 分支；移除 `mission` 模块后引擎仍可编译 | 游戏形态的布局算法（EMT 箭头、网格语义）暂留游戏包 |
| D18 | `game.json` 含 `install` 段；引擎侧无 EU4 可执行文件名字面量 | — |
| D19 | 变更说明的四个字段齐全 | 机制闭集修改走规范流程 |
| D20 | `opaque` / `open` / fallback 计数报表只减不增 | 临时豁免须在迁移/覆盖率报告登记 |

### 3.2 本轮边界引出的落地缺口

1. `card` 去 `serde(default)` + 转换器始终输出 card + `rules/eu4` 重生成 + 规范 §3.1 默认列与论证按 50/35/13 重写（D14）。
2. `FileRule.resolution` 默认翻转为 `merge`（现默认只命中 6%，全表唯一打不中多数的默认值）（D14）。
3. `FileRule.root` 对 `script` parser 的必填检查（规范已要求，`compile` 未实现）（D14 强制力）。
4. `MapSpec.value|body`、`BindingSpec.loc|sprite` 的「至少一个」检查（D14 强制力）。
5. 烘焙硬门：`compile::check` 成为 bake 的强制前置——源有 error 即拒绝产出嵌入产物（D14「拒绝烘焙」的保证），已写入阶段 5 退出标准第 6 条。
6. card 语法 lint：`0..0` 警告（这是禁用不是基数）、`N..N` 提示定长元组（改用 `list` + card 元数）、同名键不同 card 提示核对（D14 配套）。

## 4. 现有数据的去向

| 现有 | 去向 |
|---|---|
| `catalog/records/*` | 删除（第 0 阶段） |
| `catalog/symbol-descriptors.json` | 删除（第 0 阶段）；覆盖语义进 `types.*.resolution` |
| `catalog/file-categories.json` | `files` |
| `semantic/contexts/{trigger,effect,modifier}.json` | `core/{trigger,effect,modifier}.json` 的 schemas |
| `semantic/contexts/on-action.json` | `events.json`：`on_actions` enum + `on_action_body<S>` |
| `semantic/contexts/special.json`、`semantic/definitions/**` | 各领域文件的 schemas |
| `types/descriptors/*` | 各领域文件的 `files` + `types` + def 位置 |
| `types/root-keys.json`、`types/root-scopes.json` | def 位置的键与 `scope.set` |
| `values/enums/*` | 各领域文件的 `enums` |
| `bindings/localisation.json`、`bindings/sprite.json` | `Localised` / `HasIcon` impl |
| `profile/install.json`、`profile/filesystem.json`、`profile/cards.json` | `game.json` |
| `profile/lexicon.json` | `fallback_keys` → `game.json`；`member_kind_aliases` 删除（转换脚本一次性把别名归一为类型名）；`member_name_suffixes` → def 的 `strip_suffix`；`enum_extra_members` → enum rows |
| `profile/scopes.json` | `scopes`（`scope_member_aliases` → links；`root_scopes` → def 的 `scope.set`；`scope_completions` 推导） |
| `profile/semantics.json` | `root_entry_specs` 推导；`transparent_scope_wrappers`/`control_flow_keys` → `control`；`semantic_context_inheritance` → include + `ModifierSource`；`quoted_script_definition_keys` → `quoted<…>` |
| `profile/symbols.json` | `definitions`/`container_value_definitions` → def 位置；`value_definitions` → `def<>`；`references` → `ref<>`；`conditional_definitions` 的引用放宽为基础类型；`token_definitions` → `Callable` |
| `profile/dynamic.json` | `dynamic_scope_prefixes` → 模板链接；`dynamic_value_prefixes` → 值模板；`open_world_value_kinds` → `open`；`closed_dynamic_kinds` → 默认；`engine_set_flags` → `builtin` |
| `crates/game/src/eu4/mod.rs::RESOLVED_SYMBOL_KINDS` | `resolution: "replace"` |

## 5. Runtime IR

### 5.1 形态

编译产物是一棵以 id 互相引用的 arena，全部在编译期完成展开、单态化与索引构建：

```rust
pub struct RulesIr {
    pub game_id: Symbol,
    pub files: Vec<FileRule>,            // 路径 matcher → parser / resolution / root: SchemaId
    pub schemas: Vec<Schema>,            // SchemaId(u32)
    pub fields: Vec<Field>,              // FieldId(u32)
    pub matchers: Vec<Matcher>,          // MatcherId(u32)，驻留去重
    pub types: Vec<TypeInfo>,            // TypeId(u32)：resolution / subtypes / open / builtin / traits
    pub enums: Vec<EnumInfo>,
    pub scopes: ScopeModel,              // types / registers / links（含模板链接）/ compat
    pub strings: Interner,
    pub provenance: Vec<Provenance>,     // FieldId → (源文件, JSON pointer)
    pub game: GameConfig,                // game.json：非语言部分
}

pub struct Schema {
    pub exact: FxHashMap<Symbol, SmallVec<[FieldId; 1]>>, // 小写键 → 重载
    pub patterns: Box<[FieldId]>,                          // 按序尝试
    pub items: Option<MatcherId>,
    pub open: bool,
}

pub struct Field {
    pub key: MatcherId,
    pub value: FieldValue,               // Scalar(MatcherId) | Block(SchemaId) | SelfBlock | Quoted(SchemaId)
    pub card: Card,                      // (min, Option<max>)
    pub scope: Option<ScopeEffect>,
    pub def: Option<DefSpec>,
    pub control: Option<ControlKind>,
    pub doc: Option<Symbol>,
    pub severity: Severity,
    pub deprecated: bool,
}
```

`Matcher` 是 2.2 表达式的降级结果：`Scalar | Literal | Template | Int | Float | Bool | Date | Loc | Path | Ref{type, subtype} | Def{..} | Enum{id} | Scope | Link | Quoted | Opaque | Union(Box<[MatcherId]>)`。

**已实现**（阶段 3，`crates/rules/src/ir.rs` + `lower.rs`）。实际形态与本节草图的逐条差异（traits arena、`FileRule.root` 三态、`fields` 返回 `Vec`）见 §6 阶段 3 的实施备注。

### 5.2 查询 API

消费方持有 `SchemaId` 而不是 `(context, parent_path)`：

```rust
impl RulesIr {
    fn root_schema(&self, path: &LogicalPath) -> Option<SchemaId>;
    fn lookup(&self, schema: SchemaId, key: &str, shape: Shape) -> Candidates<'_>; // 精确哈希 + patterns
    fn child(&self, field: FieldId, current: SchemaId) -> Option<SchemaId>;        // 解开 SelfBlock
    fn fields(&self, schema: SchemaId) -> impl Iterator<Item = FieldId>; // 补全
}
```

HIR 在降级时为每个块节点记录其 `SchemaId`（以及实例的 `SubtypeSet` 与入口作用域），ide 的诊断、补全、hover、解析都从 HIR 读取，不再各自重新推导上下文。`semantic.rs:93` 的 `context_rule_view` 及其分桶索引整体删除——它们变成编译期的 `Schema.exact`/`patterns`。

### 5.3 影响面与改造顺序

1. `crates/rules`：新增 `source`（serde 源类型 + JSON Schema）、`expr`（类型表达式解析）、`compile`（检查、展开、单态化、索引）、`ir`；删除 `model` 中的 `SemanticRule`/`RuleRecord`/`TypeDescriptor`、`entry_wrapper_reroutes`、`profile` 中已并入语言的部分。
2. `crates/hir`：`scope.rs`、`semantics.rs`、`collector.rs`、`model.rs` 改为沿 `SchemaId` 行走；def 收集改由 `Field.def` 驱动；删除 `model.rs:625` 的 if 链判断。
3. `crates/ide`：`semantic.rs` → `diagnostics.rs` → `completion/`（`context.rs`、`candidates.rs`、`dynamic_constraints.rs`）→ `resolution.rs`/`navigation.rs`/`hover/`/`semantic_tokens.rs` → `dynamic_rules.rs`/`dynamic_contracts.rs`/`modifier_scope.rs`/`localisation.rs` → `lints.rs`；逐个删除 1.3 的硬编码。
4. `crates/game/src/eu4/mod.rs`：删除 `RESOLVED_SYMBOL_KINDS` 与内嵌的旧模型构造；`crates/engine/examples/mem_probe.rs`、`crates/pdc/src/requests.rs` 跟进。

## 6. 迁移计划

### 阶段 0：清理（直接进 main，每项一个 PR）— **已完成**（2026-09-29）

> 实施备注：
> - 第 4 项里 `deprecated` 只清数据层（55 处显式 `deprecated: false` 随显式默认值一并删除），**字段保留**——补全排序与诊断弃用标记由它驱动且有测试覆盖，5.1 的新 IR 也保留该属性（凭数据推翻了"删字段"的字面读法）。
> - `required` 则**整字段删除**：`min_occurs` 成为唯一必填编码（原两行 `required: true` 本就带 `min_occurs: 1`）；随其退役的还有 hover 的 "- required" 行、补全"缺失必填前置"排序档（`rule_required_missing`）及其专属简写测试，`pdc/ruleSearch` 不再输出 `required` 键。这些面相将来由 `card` 重新提供。
> - 第 5 项落在 `editors/vscode/scripts/baseline.mjs`（与 sweep.mjs 并列，`npm run baseline`），输出进 gitignore 的 `performance-results/baselines/`；实跑需要与 checkout 规则匹配的服务端二进制（脚本强制校验三方 rule_hash）。

1. **删除 `catalog/records`**：去掉 `RulesModel.records`、`RuleRecord`，以及 `canonical.rs:77`、`runtime.rs:152`、`rulec.rs:1752`、`mem_probe.rs` 的相关代码。
2. **删除 `symbol-descriptors`**：`resolution.rs`/`navigation.rs` 的 4 处查询改为常量 `ReplaceBySymbol`（现状所有条目都是这个值，查不到时的默认值也是它，行为等价），并删掉随之成为死代码的 `Merge`/`Unique` 分支与 `SymbolResolutionPolicy`；`RESOLVED_SYMBOL_KINDS` 暂留（新语言中进 `types.*.resolution`）。
3. **修正“已知键”**：`hover/symbol.rs::known_keys` 与 `semantic_tokens.rs:108` 统一为同一个函数，来源 = `fallback_keys` ∪ 所有 `KeyMatcher::Exact` 规则键；不再混入元字段名和描述符名。补回归测试：`line`、`shape`、`source_file` 不是已知键。
4. 清理常量与双重编码字段（`strict_min`、`deprecated`、`required` vs `min_occurs`、`any_scalar` 两种写法、float 边界字符串、`Unit`/`unit`、显式默认值）；修 `lexicon.json:32`；更新 README 统计。
5. **建立基线工具**（本阶段最重要的交付）：在 `scripts/sweep.mjs` 旁增加基线导出，对原版全量输出——每文件的诊断（code、range、message）、每类型的定义数与引用数、golden 文件上若干固定位置的补全候选。基线含授权数据，只存本地、不进仓库（与现有 sweep 约束一致）。

### 阶段 1：语言前端（长期分支 `feat/rules-v2`）— **已完成**（2026-09-30）

- `docs/rules-language.md` 规范；`source` 源类型 + 生成的 JSON Schema；类型表达式解析器（附解析错误的定位测试）；编译器语义检查（2.10 列表，逐条有单测）。本阶段不接 runtime。

### 阶段 2：转换脚本 — **已完成**（自动部分 2026-09-30，人工收口 2026-10-01）

> 实施备注：
> - 历史工具 `rules-migrate` 曾读取旧规则树，生成类型化新源与 `docs/rules-migrate-report.md`；新源现统一位于 `rules/eu4/`。当时重复运行逐字节一致。2026-10-02 已删除转换器及其专属测试，新源直接维护，不再从旧源重新生成。迁移报告保留为源版本 13 的历史转换记录；旧模型和旧源的最终退役已在阶段 5 完成。
> - 验收：`rulec check rules/eu4` **0 error**（329 条 `UnusedDefinition` warning 属迁移期正常）；行数对平：8,463 = 1,227 去重 + 6,907 进字段 + 149 进 items + 127 折叠进 `scopes.links` + 10 折叠为寄存器位移 + 8 进 on_action 折叠 + 35 孤儿行（进人工清单）。
> - 历史输出（现已改为显式分组）：on_action 折叠按当时设计产出 `on_actions` enum（`scope` 列）+ `on_action_body<S>`，但**丢弃了 `from` 列**：（scope, from）组合共 65 个，会打爆 §10.1 检查 4 的 64 实例上限；`starts_with`（`on_harmonized_*`）落为一条模板 pattern，body 参数待人工定夺。
> - 历史设计（requires 已删除）：`ModifierSource` trait 当时未带 `requires: {include: "modifier_block"}`：`semantic_context_inheritance` 的 type:X→modifier 是"实例体自带 modifier 字段"而非"含 `modifier` 子块"，两种形态并存，requires 形态留人工收口（连同 trait impl 一起）。
> - 修了两处 phase 1 的实现缺陷：`compile::check_instantiation_cap` 的迭代计数原为逐轮累加、域 ≥3 必然打到上限，改为不动点重算；`source` 源类型补 `Serialize`（转换器序列化输出用），JSON Schema 工件随之重新生成（schemars 现在能写出 `default` 值）。
> - 人工清单按类别落在 `docs/rules-migrate-report.md`：38 个魔法段位置的 def/ref 判定、`strip_prefix` 模板、typed-prefix 算子过滤、trait impl（所有展示 binding 合并到类型级）、文件类目扩展名/排除前缀、孤儿结构位置等。
> - D14（`card` 无默认、`FileRule.resolution` 默认翻转等显隐裁决）落地时已重跑转换器生成新源；这是 §3.2 缺口 1–4 的历史处理记录。

- **自动部分**：`parent_path` 扁平行 → 嵌套 schema；alternative → 重载/union；matcher → 表达式字符串；去重（1,227 条）；on_action 折叠为 enum + 参数化 schema；纯链接行折叠进 `scopes.links`；profile 各表按第 4 节搬迁；`member_kind_aliases` 归一。
- **人工部分**：324 处魔法段的真实结构、trait impl、`control` mixin、mixin 抽取。
- **人工收口结果**（2026-10-01）：`docs/rules-migrate-report.md` 的人工清单为空，全部条目要么被机械化、要么作为**显式损失**记入覆盖率表：
  - 魔法段 def/ref 判定：非定义上下文（`trigger`/`effect`）的 `{type: X}` 键判为调用位（20 处）；跨目录引用按「上下文的 profile 路径 vs 类型的 profile 路径」判定（5 处转 ref pattern，13 处保持 def map）。
  - `Localised`/`HasIcon` 改为 **impl 逐条枚举 binding**（D19 变更说明见 §2.7），trait 不再声明固定 binding；原条件 binding 合并至类型级 impl，作为可选展示项。
  - `date_field` 伪段与 `key_segment` 对齐、同路径多类型的 file root 合并、`params` 中的 `null` 不再被裁掉、原条件定义的引用放宽至同路径基础类型。
  - `token_definitions` 参数键折叠进 `Callable` 的 dynamic-key 能力（最后一个空 enum 桩消失）；`strip_prefix` 模板在表达式语法中新增 `strip_prefix` 子句（D19 三处用例）。
  - 显式损失（覆盖率表逐条计数）：typed-prefix 算子过滤 3、on-action `from` 列 258、实例名前缀条件的 binding 3、field 源 binding 0、参数键 6。
- 历史自动转换要求**可重复运行、结果确定**，已在人工精修前后验证。转换器退役后，规则行为回归直接读取新源或生产 IR；转换专属的别名归一与旧路径序列化断言随工具删除。
- 当时验收：输出通过 `rulec check`，0 error（273 条迁移期 `UnusedDefinition` warning）、8,463 行对平、人工清单为空、重复运行逐字节一致。转换器退役后的回归覆盖与验证见 `docs/phase5-validation.md`。

### 阶段 3：IR 与查询 API — **已完成**（2026-10-01）

> 实施备注：
> - 代码落在 `crates/rules/src/ir.rs`（arena + 查询 API）与 `crates/rules/src/lower.rs`（降级）。
>   入口是 `lower::lower(sources, GameConfig) -> Result<RulesIr, LowerError>`：先跑 `compile::check`，
>   只要有 error 就拒绝产出 IR（阶段 5 退出标准 6「拒绝烘焙」的落点）。
> - 与 §5.1 草图的差异（阶段 4 消费方必须知道）：
>   1. `RulesIr` 保留 `traits: Vec<TraitInfo>` 名称标记，`Callable` 按身份识别；
>      本地化/图标绑定与 Callable body 只保存在类型 impl，通用 trait 元数据已删除。
>   2. 2026-10-02 简化：删除 `Field.gate`、`Schema.subtype_gates`、字段谓词求值和 subtype trait impl。
>      `RulesIr::fields(schema)` 返回完整的确定性字段列表。显式 subtype 只用于引用分类。
>   5. `FileRule.root` 是三态 `RootRule`：`Schema(id)` / `Instance { def, body }`（整文件一个实例）/
>      `Opaque`（`localisation`、`asset`、`syntax-only` 不建模结构），取代草图里的裸 `SchemaId`。
> - `Symbol` 分两类：**身份**（schema / type / enum / trait / scope / register / 字段键 / 形参 / 类别名）
>   一律 ASCII 小写驻留，因为语言各处大小写不敏感，查询侧不需要再折叠；**文本**（字面量、模板文本、
>   `doc`、binding 模板）原样驻留，因为拼写本身是数据。`Interner` 同时提供两种查找。
> - 两个 arena 都做驻留去重，且都用「结构指纹 + 来源」当键：
>   matcher 按结构指纹去重（float 取位模式）；**field 连 provenance 一起进指纹**，所以同一个 mixin
>   贡献给上百个 schema 时只存一份，而两个 schema 各自手写的同形字段不会被合并（不会丢来源）。
>   语料实测：1,224 schema / 9,673 field / 3,894 matcher（源语言自身 6,080 条 field spec + mixin 展开）；
>   未做 field 驻留前是 93,378 field——mixin「编译期展开」的乘法代价，阶段 5 的 `mem_probe` 依赖这一步。
> - matcher 是 arena 里的一条记录，`Schema.exact: FxHashMap<Symbol, Box<[FieldId]>>` 的键是折叠后的
>   小写键；`lookup` 先给同 shape 的 exact 重载（按书写序），再给 `patterns`（按书写序）。
> - 单态化：语料只有 `on_action_body<S>` 一个参数化 schema。`on_actions_file` 使用
>   country、province、unit、mercenary_company 四个字面 enum 和显式 pattern，直接引用
>   对应的 `on_action_body<scope>`；`on_harmonized` 模板复用 country 实例。
>   不再生成属性列分组或 matcher 行子集。实例键是 `(base name, 实参元组)`，
>   `Schema.arguments` 保留实参，语料断言不存在未绑定形参。
> - `body: "self"` 保持 `FieldValue::SelfBlock`（arena 没有自环），由 `child(field, current)` 解开；
>   字段级 `map` / `list` 生成保留名 `$map` / `$list` 的合成 schema（`$` 不是合法标识符字符，
>   不可能与声明名撞车），schema 级 `{ "map": … }` / `{ "list": … }` 简写就地展开。
> - `files` 按入口名排序后用 `max_by_key(specificity)` 选类目，复刻旧 `Model::classify`（同分取靠后的）；
>   旧语料 124/124 条 `case_sensitive` 为 false、唯一一处 `path_suffix` 已被迁移折成 `path` + `file`，
>   所以 IR 的 `FileMatcher` 固定 `case_sensitive: false` / `path_suffix: None`，`strict` 只作为
>   扫描事实保留、不参与匹配。同分序要在阶段 5 的 sweep 对比里复核。
> - 单测：`lower::tests` 用一份 events + on_action + decisions 的样板覆盖 def 收集（含 `map` 键上的
>   def、`field:id` 名字来源、mixin 字段的 provenance）、显式 subtype 引用分类与所有字段查询、查询 API 的 shape 分派（同键多 shape 重载、pattern 只答自己描述的 shape、
>   键大小写不敏感）、单态化（分组行集、`$S` 代入、`ref | '0'` union）、`link` pattern（`SelfBlock`、
>   scope link 的 `from`/`to`、模板 link 的空洞）；另有 `the_first_party_corpus_lowers` 全量降级
>   `rules/eu4`，断言 137 条 files、on_action 四组行数分布、100 条 scope link、4 个 event subtype、
>   无 `$unbound` 实例（语料不在仓库时自动跳过）。

- 实现第 5 节 IR、编译降级与查询 API；以 events + on_action + decisions 为样板写 IR 级单测（def 收集、subtype 判定、单态化、`link` pattern）。

### 阶段 4：消费方改造 — **已完成**（主路径 2026-10-01，验收收口 2026-10-03）

- HIR、IDE 与生产 LSP 入口直接消费 `RulesIr`。转换器于 2026-10-02 退役；阶段 5 已删除旧源目录和旧模型兼容路径，并完成保留行为的夹具迁移与验收。

> 以下记录主路径接入时的局部验证范围。后续全量语料和旧夹具对照发现的 Callable、作用域、
> 诊断和呈现缺口已在阶段五收口；历史局部验证不能代替最终验收。
> 最终证据见 [阶段五本地验收记录](phase5-validation.md)。

> 实施备注：
> - `game::eu4::first_party_ir()` 的嵌入式 bundle 成为生产语义来源。`runtime_rules()` / `RuleSet::from_ir_catalog`
>   只提供文件目录和 `game.json` 的配置桥接，**不生成扁平 SemanticRule**；stdio 入口安装同一个 `Arc<RulesIr>`。
>   `pdc/ruleSearch` 返回 schema/field、card 与真实源文件/JSON pointer；`mem_probe` 统计 IR arena。
> - HIR 沿 `SchemaId` 行走，为块记录 schema、subtypes、入口作用域，为键记录选定 `FieldId`。
>   exact/pattern、形态重载、`self`、列表裸值、def/ref、trait bindings、Callable 参数及引号脚本都由 IR 驱动。
>   register/link 和 `Field.scope` 更新 ROOT/THIS/FROM/PREV；`Field.control` 决定参数的分支可选性。
> - 显式 subtype 的 ref 使用工作区 `SymbolFacts`。磁盘扫描和缓存重建先收集定义，再依据候选索引重降级引用；
>   IDE 的文档、闭合文件和临时文本查询使用 overlay-aware 的完整符号事实，不受补全来源偏好影响。
> - 诊断、补全、hover、导航、localisation previews、semantic tokens 和 scope inlay 改用 HIR facts / IR。
>   控制链、guard、logic、switch、display、cardinality 和 subtype 限制按字段声明执行；Callable 签名来自 HIR。
>   mission 图结构校验与通用符号解析继续复用现有模块。非空 IR 路径不再重建 `(context, parent_path)` 规则树。
> - `RulesIr::fingerprint()` 哈希当前 arena、驻留字符串、溯源和配置，精确键哈希表先排序。
>   host 在安装 immutable Arc 时缓存指纹；分析上下文和磁盘缓存都包含它。
>   SQLite schema 升为 **17**，保存 `ir_hash` 和定义的 subtype 集合；不兼容 IR 的缓存不得降级复用。
> - golden 变化分两类：**预期精化**包括 on_action 的 country/province 引用限制、IR guard/control 校验与真实溯源；
>   缺少 guard 只保留一条控制流提示，已声明键的错误形态报告 `InvalidValue`，不误报 `UnknownKey`。
>   **已修复的迁移回归**包括 alias invocation 的 `min=1` 错转（调用词汇应为可选）、13 个 def pattern
>   错添一层 map、sprite/decision wrapper 指向空 schema、以及 `.gfx` descriptor 被归入 `.txt` 文件规则。
>   修正转换器并确定性重生成规则源；同 card 的重复重载随之合并。pattern 的 max 按实际键计数，
>   branch chain、switch/weighted 的分支体和 ROOT/THIS 值校验都遵循 IR 与当前 HIR 状态。
> - 验收覆盖空旧语义模型下的跨文件导航、空块/缺值/Callable 补全、本地化预览、hover 卡片、引号脚本范围、取消、
>   IR 切换、两遍扫描、缓存刷新、控制链与作用域值；自定义 branch/guard 键的回归确认参数必填性由 IR 决定。
>   新增 `ir_schema_diagnostics` golden。旧模型 fixture 保持原有 golden。
> - 本地验收通过：workspace 的 fmt、all-targets/all-features check 与 test、Clippy（`-D warnings`），
>   Rustdoc（`-D warnings`）及 artifact gate。artifact 的规则源、编译、manifest 可复现性、
>   嵌入产物一致性和 game_id 五项检查全部通过。
>   `rulec check rules/eu4`：88 个源文件，0 errors、268 个 `UnusedDefinition` warnings；
>   转换器重复生成的 90 个 bundle 文件逐字节一致，且与仓库源文件一致。
>   授权原版 sweep、补全候选对比与加载性能退出标准仍属于阶段 5。

### 阶段 5：切换 — **本次范围内已完成**（2026-10-03）

2026-10-03 的最终清理已删除旧模型、旧编译模块、旧规则源及兼容消费者。
资源项按用户明确决定排除 89 条；其余 Vanilla error、定义/引用差异和固定完整补全集合均已完成审查。
最终工作区回归 976 项通过，全部目标/特性 Clippy、rustdoc 和工程产物检查通过。
三组最终性能配对相对 `96f50c8` 峰值 RSS 中位数 1,146.75 → 1,071.09 MiB，
规则加载 191.5 → 22.8 ms；六项退出标准满足本次范围，阶段五验收结束。
完整证据及首次扫描仍多 1.7 秒的已知风险见 [最终验收记录](phase5-validation.md#退出标准)。
以下引用块保留各轮历史数据，不代表当前仍存在旧源或兼容消费者。

> **本轮模块工作收官，仍有后续退出项（2026-10-02）**：新 IR 的检查、lower 和 bake 在
> `crates/game/build.rs` 执行；运行时解码 arena 并恢复查询索引。
> 当前源版本为 13，字段条件、结构推导 subtype 与 subtype trait 门控已删除，通用 trait 元数据等未使用机制与迁移器已退役。
> `rules/ir-manifest.json` 与嵌入产物、重新编译结果由 artifact gate 核对。
> 检查失败拒绝创建或覆盖产物已有负例。
>
> 2026-10-02 用户结束本轮 Vanilla error 逐条审查，保留 268 个待审查身份，转入补全模块。
> 当前源版本 13 的补全冻结版修复联合值域遗漏作用域过滤；生产 IR IDE 433 项通过。
> 固定 11 个位置的完整候选与 LSP 上限内输出一致，7 组相同、4 组差异已逐项解释，
> 补全退出项在该固定范围通过。此补全轮不执行 Vanilla 全量诊断，不替代剩余语义与引用审查。
>
> 源版本 13 的首轮性能冻结版已优化模糊建议、schema/属性范围查找、作用域模板及缓存生命周期；
> 全工作区 1,031 项、生产 IR IDE 438 项测试、fmt 和全目标 Clippy 通过。
> 相对阶段五开始时旧模型基线（`96f50c8`）的三组安静完整配对诊断中位数 45.8 → 23.0 秒，完整探针 86.3 → 74.2 秒，
> 峰值 RSS 1,065.56 → 999.73 MiB（下降 6.18%）；同规则优化前后均为 11,649 条，
> digest `0xd8432448a0e077f6`。仅规则加载另测三组，194.4 → 22.8 ms。
> 该基线下内存与规则加载退出项通过，诊断延迟劣化已修复。
> 随后以远端 origin/main `e771357` 的源码独立编译，重跑三组相同采样节点对照：
> 诊断 46.1 → 22.9 秒、规则加载 52.4 → 22.9 ms，但峰值 RSS 756.75 → 869.13 MiB，
> 上升 14.85%，该轮 origin/main 内存退出项未通过。首次扫描扣采样等待后为 3.6 → 13.0 秒。
>
> 最新峰值 RSS 与首次扫描专项优化增加索引专用 lowering、按符号事实依赖重放、
> 稳定索引复用与大文件串行/小文件限量并行。全工作区 1,033 项、生产 IR IDE 438 项、
> HIR 46 项、fmt 和全目标全特性 Clippy 通过。九个安静完整进程轮换三个冻结版本：
> 同规则峰值中位数 1,028.39 → 935.45 MiB（下降 9.04%），首次加载并扫描 12.9 → 5.2 秒
> （下降 59.69%）；完整诊断均为 11,649 条、digest `0xd8432448a0e077f6`。
> 同期 origin/main 峰值为 977.06 MiB，当前低 4.26%；仅规则加载 51.7 → 23.1 ms，
> 本轮内存与规则加载中位数退出项通过。首次加载并扫描仍比 main 的 3.5 秒多 1.7 秒。
> RSS 波动较大，完整范围、二进制/源码 SHA 及测量边界见 `docs/phase5-validation.md`。
>
> 最近覆盖各验收面的完整冻结版为简化前的 `ir-bindings-acceptance`，源版本 11、指纹 `3250e7ac…`，SQLite 23。
> 当前 source 13 / SQLite 24 的 Vanilla 语义报告已重新冻结；资源与引用审查状态见下方 D6/9，完整补全和性能证据仍归属于各自冻结版本。
> 常规 Rust 门禁 1,033 项、生产 IR 对照 430 项（含 80 项空旧模型专项）、
> Clippy、Rustdoc、artifact、policy 及最新 release 的 LSP/MCP smoke 均通过。
> 同一份 Vanilla 文本完成 8,670 文件全量审计：8,344 errors，旧版为 8,123；
> 按位置新增 286、移除 65，尚需完成全部诊断和引用差异的语义归类。
> 完整缓存为 587,104 definitions、432,652 references；31 类定义计数依据已重新核对，
> 194 类引用差异继续审查。全部 11 个完整补全集合已导出且前 512 项与 LSP 一致；
> 仍为 7 组相同、4 组差异，与 roundtrip 轮集合相同。
>
> 本轮修复联合值域误建引用、重叠类型导航、提示 payload 缓存摘要重复、
> 脚本函数被精确字段遮蔽、标量参数引用、legacy 改革及本地化绑定断链。
> （历史记录，结构条件已于 2026-10-02 删除。）已知作用域的寄存器值参与重载选择。
> 政府属性接受内置和自定义名称；寄存器 `role`、switch `selector_schema` 已声明化。
> 常规测试继续保留旧模型夹具，生产 IR 差异有独立断言；旧源与兼容路径尚未删除。
>
> 安静环境下三组规则加载中位数 190.9 → 19.2 ms，峰值 RSS 40.95 → 23.23 MiB。
> 该历史冻结版完整内存三组顺序配对均正常完成：峰值 RSS 中位数 1,013.83 → 931.61 MiB
> （下降 8.11%），释放后 RSS 902.27 → 510.55 MiB。诊断阶段中位数 45.8 → 194.5 秒，
> 延迟劣化仍存在；该结论对应同一冻结版本，没有并发编译或全量审计。
> 完整证据、剩余规则问题及复现方法见 [阶段五本地验收记录](phase5-validation.md)。
> 本地语料、缓存、完整报告和冻结二进制均保持忽略。

- 已删除旧 `model`/规则编译模块/`SemanticRule` 与全部旧规则文件；当前 `source_format_version` 为 13。
- **退出标准**（全部满足才合入 main）：
  1. golden 全部通过，所有更新过的 golden 逐条有归类说明；
  2. Vanilla 全量语义审查：每个 error 都有位置、上下文、原因与证据，区分原版问题、规则问题、分析器问题及审计语料缺失；规则和分析器误报修复后重跑。旧版只作定位参考，不要求错误、定义或引用数量完全对齐；定义与引用按实际语义验证（D9 的兜底）；
  3. 补全候选对比：固定位置的候选集合一致或有解释；
  4. `mem_probe` 内存与规则加载时间不劣于切换前；
  5. 1.3 列出的硬编码全部删除（grep 验证）；
  6. `compile::check` 是 bake 的强制前置：源有 error 即拒绝产出嵌入产物（D14 的「拒绝烘焙」保证）。

## 7. 风险

| 风险 | 应对 |
|---|---|
| 消费方改造量大（`parent_path` 298 处等），长期分支漂移 | 阶段 0 在 main 上先做完，缩小 diff；分支定期同步 main；冻结 `rules/eu4` |
| 单态化实例爆炸 | 每个参数化 schema 实例数上限 64，超出编译失败 |
| 删除全局引用表导致引用丢失 | 基线引用计数逐类型对比；缺口用补 schema 解决，而不是恢复全局表 |
| 字符串表达式缺少编辑器支持 | `rulec check` 给出精确定位；JSON Schema 做 pattern 粗校验 |
| 人工精修量超预期 | 阶段 2 先产出自动覆盖率与人工清单，据此重新估期 |

## 8. 实施期间收口的细节（不阻塞开工）

- `desc` 等“标量或块”字段的完整清单，由转换脚本统计 `shape` 冲突得出。
- `open` 类型的 `ref` 是否对“从未 def 过的名字”给出 info 级提示：以 sweep 噪声量决定。

### 8.1 原计划落地盘点（最终状态，2026-10-03）

| 原则 | 当前落地状态 |
| --- | --- |
| D2/3/7/10/11：类型语言、领域组织、正交子系统、单态化和内置 trait | 生产主路径已落地；D4 的非目标未扩张 |
| D1/5：一次切换与删除旧模型 | 已完成。旧源、旧模型、旧编译模块、SemanticRule 及兼容消费者已删除，生产语义统一读取 RulesIr；保留行为夹具已迁移，旧 API 专用夹具按理由退役 |
| D6/9：语义审查和结构声明引用 | 本次范围内通过。最终 8,324 errors 中 7,948 条有冻结资源缺失证据、214 条原版问题、73 条规范性诊断；89 条资源项按用户决定排除，诊断仍正常输出。最终 587,163 个定义及 431,839 条引用的完整多重集合与已审冻结版一致，相对旧版的差异全部归类，未归类位置为零。绑定遗漏已修复，tutorial 的 11 处覆盖限制保留；38 项既有策略保持现状，R09 全面合并为 province。证据见 phase5-validation.md |
| D8/14：声明控制流、显式 card 与默认值展开 | 严格解析、语义检查和 bake 硬门已落地；新增 `rulec fmt`、`--expanded`、只读 `--check` |
| D15：复用边界与溯源 | include/参数层级和溯源已有约束；删除 8 个未使用 mixin，将 28 个不足三处的 mixin 展开到 32 个源使用位置；仅保留直接使用 25–86 次的 4 个共享词汇 mixin |
| D12/18：非语言配置和安装识别数据化 | 安装事实已移入 `game.json.install`，构建生成静态 descriptor；平台探测接口和发现行为保持原契约 |
| D13/17：全部游戏策略声明化和任务树能力接口 | 原计划边界已验收。任务树路径、命名空间、字段和写回顺序来自规则配置；图与写回机制移入引擎，布局留游戏包；生产 IDE/pdc 无 EU4 分支。§1.3 的旧控制流、ScopeContext 和 matcher 消费者已删除，当前消费者读取 IR 声明；D20 的额外归零提案不属于本次退出条件 |
| D16：全盘读源、废除 manifest、构建身份缓存 | 已落实：递归读取 JSON、相对路径排序、同名定义拒绝；源 manifest 删除，包身份移入 game.json；SQLite 24 按构建身份失效，规则/IR 指纹仅作报告身份 |
| D19：特性准入说明 | trait binding、strip_prefix、constant 已有规范与语料说明 |
| D20：无猜测与豁免归零 | 仍是待认可提案；当前 3 个开放 schema、6 个开放类型、83 个 fallback key，未宣称归零 |

低风险收尾中间版本的 IR 指纹为 `fd079aa53a8e61ee46baa0767b18ca4d67bd9e29ec9b72229f69df2f1f54218b`。
随后 D16 使用 SQLite 24 和构建身份失效；当前规则指纹为
`c82f078ed894a5cd6c7416292599e55d87e0a4a6363eb06542cbb901443e6eea`，源格式仍为 13。
用户已撤回重复 custom_attributes 的合并放行，恢复 `0..1`；38 项复核中其他项保持现状；R09 已明确批准将 trade_node 执行作用域全面合并为 province。
此前补全、全量 Vanilla 和性能报告继续属于各自冻结版本；最终冻结版已另行复核既定门禁。
固定收官清单见 [阶段五退出标准](phase5-validation.md#退出标准)：引用差异归类、旧模型及
硬编码删除、完整补全和原性能指标均已通过本次范围；89 条资源项明确排除，不作为本次阻塞。
D20、扩展 tutorial 支持等不自动追加为阶段五退出条件。

## 附：关键代码位置

- 规则源/编译/运行时：`crates/rules/src/{source,compile,lower,ir,bake,catalog,runtime}.rs`，CLI 为 `crates/rules/src/bin/{rulec,bake-ir}.rs`
- 嵌入与 EU4 profile：`crates/game/build.rs`，`crates/game/src/eu4/mod.rs`
- 规则源：`rules/eu4/`（递归发现 JSON）；产物身份：`rules/ir-manifest.json`
- 主要消费方：`crates/hir/src/{ir_lowering,callable,scope,model,collector}.rs`，`crates/ide/src/{ir_queries,ir_semantic,ir_callable,resolution,navigation,diagnostics,dynamic_rules,dynamic_contracts,localisation,semantic_tokens}.rs`，`crates/ide/src/completion/`，`crates/ide/src/hover/`
- 任务树通用机制：`crates/engine/src/structure.rs`；游戏布局：`crates/game/src/mission.rs`
- 扫描：`editors/vscode/scripts/sweep.mjs`（本地，不进 CI）；只读审计工具：`lab/perf/`
