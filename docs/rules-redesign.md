# 规则系统重构：设计方案

> 状态：**设计已定稿；阶段 0（清理）已完成**（2026-09-29，分支 `refactor/rules-drop-records`），阶段 1 起待实施。第 3 节列出全部已定决策；第 8 节只剩实施期间凭数据收口的细节，不阻塞开工。
> 前提：项目处于 0.x，**允许破坏性修改，不考虑历史兼容**；规则源格式**继续使用 JSON**。
> 统计口径：2026-09-29，`rules/eu4`（`source_format_version` 10）与 `crates/*`。

## 0. 目标

让规则本身成为一个**自洽的小型类型语言**：作者写面向人的源语言（可嵌套、可复用、可条件化），
编译器（`rulec`）做语义检查并降级为**树形 runtime IR**；消费方沿 IR 树行走，不再在运行时从扁平行重建结构。

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
schemas   → 结构：fields / patterns / items / include / when / def / 参数
mixins    → 纯结构字段包（编译期展开）
types     → 符号名字空间：resolution / subtypes / open / builtin / impl
traits    → 能力 + 绑定 + 约束（编译期展开）
enums     → 枚举，可带属性列（承载 on_actions 等）
scopes    → 作用域类型 / 寄存器 / 链接 / 兼容
```

与旧草案相比，**没有顶层 `intrinsics` 段**：控制流原语是字段属性（2.9），避免“键名全局生效”；**没有单独的 `tables`**：enum 可带列（2.6），一个概念。

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
          | "impl" name                  (* ref<impl ModifierSource> *)
          | param ;
literal   = "'" { char | "{" expr "}" } "'" ;   (* 无洞即常量；有洞即模板 *)
param     = "$" name [ "." name ] ;       (* 参数化 schema 的形参；$key.<列> 见 2.6 *)
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
| `ref<event>` `ref<event.country>` `ref<impl ModifierSource>` | 符号引用；可限定 subtype 或 trait | `Type`、`Dynamic`、`lexicon.member_kind_aliases` |
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
| `when` / `unless` | subtype 条件，见 2.5 | 无 |
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

约束：形参只能出现在类型表达式的 `arg` 位置；只允许一层参数（schema 形参不能再作为另一参数化 schema 的实参以外的用途）；每个参数化 schema 的实例数上限 64，超出即编译错误（防爆炸）。

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

`files` 条目：`path`（目录前缀，去掉 `game/` 遗留前缀）、`ext`、`file`（精确文件名，取代 `path_file`）、`strict`（不递归子目录）、`parser`（`script`/`localisation`/`asset`/`syntax-only`，默认 `script`）、`resolution`（`replace-by-path`/`merge`/`replace-directory`，默认 `replace-by-path`）、`root`（根 schema 名；或一个带 `def` 的字段规格，表示整个文件就是一个实例）。

`def` 规格：
- 简写：值或键位置写 `def<T>`，实例名取该标量本身。
- 完整：`{ "type": "T[.subtype]", "name": ... }`，`name` 取 `"key"`（默认）/ `"field:<字段名>"` / `"file"`（文件名去扩展名），可附 `"strip_prefix"`/`"strip_suffix"`（取代 `name_strip_prefix/suffix` 与 `lexicon.member_name_suffixes`）。
- 在 def 位置写 subtype（`event.country`）即给实例打上该 subtype；这是 subtype 的第一种来源（2.5）。

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

### 2.5 条件与 subtype

subtype 有两种来源，均在 `types` 中声明：

```jsonc
"types": { "event": {
  "subtypes": {
    "country":   {},                                         // 来源一：由 def 位置赋予（events_file）
    "province":  {},
    "triggered": { "when": { "is_triggered_only": "'yes'" } } // 来源二：由实例体中的标量字段判定
  }
}}
```

`when` 是“字段 → 类型表达式”的合取；值写 `null` 表示“该字段不存在”（取代 `conditional_definitions` 的 `absent_field`）。字段规格用 `when`/`unless` 引用 subtype：

```jsonc
"event_body": { "fields": {
  "is_triggered_only":   { "value": "bool" },
  "mean_time_to_happen": { "body": "mtth", "unless": "triggered" },
  "trigger":             { "body": "trigger" }
}}
```

**求值顺序（消除循环依赖）**：
1. 先确定 def 赋予的 subtype；
2. 再对实例体的**直接子标量字段**求值所有 `when` 谓词；被任何 `when` 读取的字段本身**不得带** `when`/`unless`（编译期检查）；
3. 最后用得到的 subtype 集合校验整个实例体。多个 subtype 可同时成立（`country` + `triggered`）。

`ref<event.triggered>` 只接受满足该 subtype 的实例。

### 2.6 Enums：可带属性列

```jsonc
"enums": {
  "dlc_event_pictures": ["...", "..."],                        // 简写：无列
  "on_actions": {
    "columns": { "scope": "scope_type", "from": "scope_type?" },
    "rows": {
      "on_startup":                     { "scope": "country" },
      "on_province_religion_converted": { "scope": "province" }
    }
  }
},
"schemas": {
  "on_actions_file": { "map": { "key": "enum<on_actions>", "body": "on_action_body<$key.scope>" } }
}
```

`$key` 是 map/pattern 中**被匹配到的键**的隐式绑定；键为带列 enum 时可读其列。编译器按列值把 enum 行分组，为每组生成一个 pattern（键 matcher 为该组行子集，值为单态化后的 `on_action_body<country>` 等），runtime 仍然只有普通 pattern。

on_action 从 1,130 行变为一个参数化 schema + 一张 enum，并恢复国家/省份事件的区分。
`profile.enum_extra_members` 直接并入 rows；`engine_set_flags` 见 2.7 的 `builtin`。

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

**trait = 能力与约束**（属于 Types）。第一版内置集合固定为 4 个，新增 trait 需要改 Rust（runtime 要理解它的语义）：

```jsonc
"traits": {
  "Localised":      { "params": { "name": "$", "desc": null },
                      "bindings": { "name": { "loc": "{name}", "required": true }, "desc": { "loc": "{desc}" } } },
  "HasIcon":        { "params": { "sprite": "GFX_$" }, "bindings": { "icon": { "sprite": "{sprite}" } } },
  "ModifierSource": { "requires": { "include": "modifier_block" } },
  "Callable":       { "params": { "body": "schema" }, "capabilities": ["replacement", "condition", "dynamic_key"] }
},
"types": {
  "decision":        { "impl": { "Localised": { "name": "$_title", "desc": "$_desc" } } },
  "building":        { "impl": { "Localised": { "name": "building_$" }, "HasIcon": {}, "ModifierSource": {} } },
  "scripted_effect": { "impl": { "Callable": { "body": "effect" } }, "resolution": "replace" }
}
```

- trait 参数里的 `$` 是**实例名占位符**，与类型表达式的 `$形参` 不在同一语法中（trait 参数不是类型表达式）。
- `impl` 可写在 subtype 内，只对该 subtype 生效（取代 binding 的 `subtype`/`condition`）。
- `Localised`/`HasIcon` 取代 `bindings/localisation.json` 与 `bindings/sprite.json`；`Callable` 取代 `dynamic_definition` 与 `token_definitions`（`$param$` 参数由 Callable 统一处理）；`ModifierSource` 取代 profile `semantic_context_inheritance` 中 22 条 `type:X → [modifier]`，并使 `ref<impl ModifierSource>` 可用。

**判定规则**：只有满足以下之一才定义为 trait，否则用 mixin——(a) 引擎/IDE 会统一处理它（hover、本地化检查、调用参数推导）；(b) 它出现在类型约束中。
**防过度设计约束**：无 trait 继承链；同一类型对同一 trait 只能 impl 一次；全部编译期展开成扁平数据，runtime 无动态派发。

### 2.8 Scopes

```jsonc
"scopes": {
  "types":     ["country", "province", "trade_node", "unit", "monarch", "heir", "consort",
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
  "compat": [{ "actual": "trade_node", "expected": "province" }]
}
```

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

`link` 键匹配时，编译器已知链接的 `from`/`to`，runtime 据此检查当前作用域并 push 目标作用域。与链接同名的标量 trigger（如 `controller = ROOT`）照常写在 `fields` 中：按 2.3 的查找规则，标量形态命中精确字段，块形态落到 `link` pattern。现有 540 条带 `push_scope` 的 trigger/effect 行里，纯链接的那部分由转换脚本折叠进 `scopes.links`。

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
| `weighted` / `chance` | 值作键的加权分支 / 概率块 | `diagnostics.rs:3025` 的 `random`/`random_list` |
| `switch` | 分支键是“`on` 字段所指 trigger 的合法值”：runtime 读取 `on_trigger` 的值，到 trigger schema 中查该键的标量字段，用其值 matcher 校验各分支键；分支体为 `self`。此行为完全由 kind 实现，不需要额外的表达式语法 | `diagnostics.rs:3025` 的 `trigger_switch` |
| `transparent` | 作用域透明包装 | `transparent_scope_wrappers` |
| `display_only` | 只影响提示，不执行 | `diagnostics.rs:3016` |

`trigger`/`mtth`/`mean_time_to_happen` 切换上下文**不是**控制流，只是普通字段（`"body": "trigger"`、`"body": "mtth"`）；`dynamic_rules.rs:1868` 的硬编码随树形 IR 自然消失。`control_flow_keys` 由带 `control` 的字段推导。

### 2.10 语言基础设施

- **编译期语义检查**：未定义/未引用的 schema、type、enum、mixin；include 冲突；不可达重载；参数化实例数超限；subtype `when` 依赖违规；作用域链接 `from` 不匹配；trait 未满足 `requires`；同一 trait 重复 impl。
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
| D8 | enum 与 table 合一（可带列）；控制流为字段属性而非顶层段；作用域效应只有对象形式；字段 `card` 默认 `0..1`（**该默认已被 D14 推翻：`card` 改为必填显式**） | 技术决定 |
| D9 | 删除全局 `references` 表：引用只来自 schema 中的 `ref<>`。未被 schema 覆盖的位置不再产生引用——这是有意的行为变化，由基线中的引用计数对比兜底 | 技术决定 |
| D10 | 参数化 schema 编译期单态化；runtime 无泛型、无动态派发 | 技术决定 |
| D11 | 第一版内置 trait 固定为 Localised / HasIcon / ModifierSource / Callable | 技术决定 |
| D12 | profile 拆分：非语言部分（install、filesystem 扫描、hover_cards、fallback_keys）移入 `game.json` 且不参与语言语义；其余全部并入语言（第 4 节表） | 技术决定 |
| D13 | **职责分离：机制闭集、策略全量。** `engine`/`hir`/`ide`/`pdc`/`parser` 只实现机制（匹配、作用域、单态化、索引、诊断框架）；一切游戏策略（键、形状、作用域、名字、目录）只能来自规则数据。程序**不按规则目录结构读规则**（全盘读取，D16）；规则目录只服务人工维护 | 用户决策 |
| D14 | **规则显隐（默认值哲学）。** 高频设默认、低频强制显式；默认值**只许出现在收紧语义一侧**（放宽型默认必须挂基线）；默认准入门槛为单值占比 ≥2/3；一切默认可机械展开（`fmt --expanded` / hover）。据此 **`card` 无默认、强制显式**（数据：`1` 占 50%、`0..1` 占 35%、可重复型 13%，无多数派，且它驱动「缺必填键 / 重复键」两类诊断），`FileRule.resolution` 默认翻转为 `merge` | 用户决策 |
| D15 | **避免重复的边界。** 复用（mixin / 参数化 schema / enum 列 / trait / `self`）必须满足三次法则（≥3 个真实站点才抽象）；深度上限为 `include` 一层、参数一层、禁止传递链；复用不得破坏溯源 | 用户决策（配套） |
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

1. `card` 去 `serde(default)` + 转换器始终输出 card + `rules/eu4-v2` 重生成 + 规范 §3.1 默认列与论证按 50/35/13 重写（D14）。
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
| `profile/symbols.json` | `definitions`/`container_value_definitions` → def 位置；`value_definitions` → `def<>`；`references` → `ref<>`；`conditional_definitions` → subtype `when`；`token_definitions` → `Callable` |
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
    pub subtype_gates: Box<[SubtypeGate]>,                 // 本 schema 作为某类型实例体时的 when 谓词
}

pub struct Field {
    pub key: MatcherId,
    pub value: FieldValue,               // Scalar(MatcherId) | Block(SchemaId) | SelfBlock | Quoted(SchemaId)
    pub card: Card,                      // (min, Option<max>)
    pub scope: Option<ScopeEffect>,
    pub def: Option<DefSpec>,
    pub gate: Option<SubtypeCond>,       // when / unless
    pub control: Option<ControlKind>,
    pub doc: Option<Symbol>,
    pub severity: Severity,
    pub deprecated: bool,
}
```

`Matcher` 是 2.2 表达式的降级结果：`Scalar | Literal | Template | Int | Float | Bool | Date | Loc | Path | Ref{type, subtype, trait} | Def{..} | Enum{id, rows: Option<BitSet>} | Scope | Link | Quoted | Opaque | Union(Box<[MatcherId]>)`。

### 5.2 查询 API

消费方持有 `SchemaId` 而不是 `(context, parent_path)`：

```rust
impl RulesIr {
    fn root_schema(&self, path: &LogicalPath) -> Option<SchemaId>;
    fn lookup(&self, schema: SchemaId, key: &str, shape: Shape) -> Candidates<'_>; // 精确哈希 + patterns
    fn child(&self, field: FieldId, current: SchemaId) -> Option<SchemaId>;        // 解开 SelfBlock
    fn fields(&self, schema: SchemaId, subtypes: &SubtypeSet) -> impl Iterator<Item = FieldId>; // 补全
    fn subtypes_of(&self, schema: SchemaId, body: &impl ScalarFields) -> SubtypeSet;
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

### 阶段 1：语言前端（长期分支 `feat/rules-v2`）

- `docs/rules-language.md` 规范；`source` 源类型 + 生成的 JSON Schema；类型表达式解析器（附解析错误的定位测试）；编译器语义检查（2.10 列表，逐条有单测）。本阶段不接 runtime。

### 阶段 2：转换脚本 — **自动部分已完成**（2026-09-30）

> 实施备注：
> - 工具为 `crates/tools` 下的 `rules-migrate`（`cargo run -p tools --bin rules-migrate`，切换后删除），读 `rules/eu4`（旧 manifest），写 `rules/eu4-v2/`（新源暂存区，阶段 5 切换时改名顶替）与 `docs/rules-migrate-report.md`（覆盖率 + 人工清单）。可重复运行、输出确定（重复运行逐字节一致，实测）。
> - 验收：`rulec check rules/eu4-v2` **0 error**（329 条 `UnusedDefinition` warning 属迁移期正常）；行数对平：8,463 = 1,227 去重 + 6,907 进字段 + 149 进 items + 127 折叠进 `scopes.links` + 10 折叠为寄存器位移 + 8 进 on_action 折叠 + 35 孤儿行（进人工清单）。
> - on_action 折叠按设计产出 `on_actions` enum（`scope` 列）+ `on_action_body<S>`，但**丢弃了 `from` 列**：（scope, from）组合共 65 个，会打爆 §10.1 检查 4 的 64 实例上限；`starts_with`（`on_harmonized_*`）落为一条模板 pattern，body 参数待人工定夺。
> - `ModifierSource` trait 暂不带 `requires: {include: "modifier_block"}`：`semantic_context_inheritance` 的 type:X→modifier 是"实例体自带 modifier 字段"而非"含 `modifier` 子块"，两种形态并存，requires 形态留人工收口（连同 trait impl 一起）。
> - 修了两处 phase 1 的实现缺陷：`compile::check_instantiation_cap` 的迭代计数原为逐轮累加、域 ≥3 必然打到上限，改为不动点重算；`source` 源类型补 `Serialize`（转换器序列化输出用），JSON Schema 工件随之重新生成（schemars 现在能写出 `default` 值）。
> - 人工清单按类别落在 `docs/rules-migrate-report.md`：38 个魔法段位置的 def/ref 判定、`strip_prefix` 模板、typed-prefix 算子过滤、`when` 谓词、trait impl（含子类型折叠）、文件类目扩展名/排除前缀、孤儿结构位置等。
> - D14（`card` 无默认、`FileRule.resolution` 默认翻转等显隐裁决）落地后需重跑 `rules-migrate` 重生成 `rules/eu4-v2`——转换器已确定性，重跑无额外成本；这是 §3.2 缺口 1–4 的一部分。

- **自动部分**：`parent_path` 扁平行 → 嵌套 schema；alternative → 重载/union；matcher → 表达式字符串；去重（1,227 条）；on_action 折叠为 enum + 参数化 schema；纯链接行折叠进 `scopes.links`；profile 各表按第 4 节搬迁；`member_kind_aliases` 归一。
- **人工部分**：324 处魔法段的真实结构、subtype 的 `when`、trait impl、`control` mixin、mixin 抽取。
- 自动部分必须**可重复运行、结果确定**：人工精修开始前如果 main 上的 `rules/eu4` 有改动，重跑即可；人工精修开始后冻结 main 上的 `rules/eu4`（如有紧急修改，在两边手工同步）。
- 验收：输出通过 `rulec check`；报告自动覆盖率（按行数）与人工清单。

### 阶段 3：IR 与查询 API

- 实现第 5 节 IR、编译降级与查询 API；以 events + on_action + decisions 为样板写 IR 级单测（def 收集、subtype 判定、单态化、`link` pattern）。

### 阶段 4：消费方改造

- 按 5.3 的顺序改造 hir → ide；每完成一个模块就跑 golden，允许 golden 变化，但每个变化须在提交说明中归类为“预期（新语言更精确）”或“已修复的回归”。

### 阶段 5：切换

- 删除旧 `model`/`rulec`/`SemanticRule` 与全部旧规则文件；`source_format_version` 升为 11。
- **退出标准**（全部满足才合入 main）：
  1. golden 全部通过，所有更新过的 golden 逐条有归类说明；
  2. sweep 对比基线：无未解释的新增 error；每类型定义数一致或有解释；每类型引用数一致或有解释（D9 的兜底）；
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

## 附：关键代码位置

- 规则模型/编译/运行时：`crates/rules/src/{model,matcher,profile,rulec,runtime,canonical}.rs`
- 嵌入与 EU4 profile：`crates/game/src/eu4/mod.rs`
- 规则源：`rules/eu4/`（`manifest.json` 列出全部文件）
- 主要消费方：`crates/hir/src/{scope,semantics,model,collector}.rs`，`crates/ide/src/{semantic,resolution,navigation,diagnostics,lints,dynamic_rules,dynamic_contracts,modifier_scope,localisation,semantic_tokens}.rs`，`crates/ide/src/completion/`，`crates/ide/src/hover/`
- 扫描：`scripts/sweep.mjs`（本地，不进 CI）
