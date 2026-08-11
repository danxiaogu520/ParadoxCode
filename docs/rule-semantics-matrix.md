# 规则声明与运行时语义矩阵

- 状态：Current audit reference
- 规则 authority：[`RFC 0013`](rfc/0013-first-party-rule-source.md)

本文只记录当前 `rules/eu4/*.json` 与代码消费者的关系，不是第二份规则 source。规则 source、artifact 和 profile 改变时，应同步更新本文的消费状态。

## Authority 与生成物

```text
rules/eu4/*.json
      |
      +--> pdx-rules::rulec / pdx-bake
      |
      +--> embedded source in pdx-game::eu4
                             |
                             v
                   user-local SQLite RuleSet
```

SQLite artifact 只保存 source 的编译结果；`rule_hash` 取 canonical logical content，不取 SQLite 文件 bytes。动态 workspace members 不写入 artifact，也不改变规则 hash。

## 当前字段消费

| Source | 当前含义 | 主要消费者 | 状态 |
|---|---|---|---|
| `manifest.json` | source format、game、目标版本 | source compiler、artifact loader | 已校验 |
| `catalog.file_categories` | path matcher、parser kind、file resolution | `pdx-rules`、`pdx-engine` | 已接入 |
| `catalog.symbol_descriptors` | symbol kind、resolution、case policy | `WorkspaceIndex`、analysis | resolution 已接入；case-sensitive 端到端仍为 Partial |
| `catalog.records` | 规范化 catalog/key 数据 | canonical hash、known-key 查询 | 已保存和部分消费，不等同完整语义规则 |
| `semantic-rules.key` | exact/type/enum/dynamic/any-scalar matcher | HIR、diagnostics、completion | 已接入 |
| `semantic-rules.value` | scalar、bool、number/date、type/enum、scope、localisation、dynamic 等 value matcher | HIR、analysis | 已接入；具体 source 覆盖取决于规则记录 |
| `semantic-rules.operator` | operator 约束 | HIR、analysis、completion | 已接入 |
| `semantic-rules.shape` | node、leaf、leaf-value、value-clause、quoted-script 形态 | HIR、analysis | 已接入；quoted-script diagnostics/completion/hover/navigation/rename 下钻 |
| `context` / `parent_path` | semantic context 和结构路径 | HIR、analysis context lookup | 已接入，无法消歧时保守保留候选 |
| `type-descriptors.scripted_macro` | scripted effect/trigger 的 body context、启用状态、usage 元数据和动态成员边界 | HIR、WorkspaceIndex、analysis、Vanilla cache shard | lookup/body context、owner-local template、调用侧 scalar/quoted-script binding、conditional、递归/预算、caller-scope semantic validation，以及参数 value/standalone-container use-site 约束补全已接入；具体宏及参数语义只由 workspace 派生，schema 5 Vanilla cache 持久化同一规范化 template，不用 first-party path rule 硬编码 helper |
| `child_context` | 子 block context | HIR scope facts、analysis traversal | 已接入 |
| `allowed_scopes` | scope 兼容性 | HIR、diagnostics | 已接入，未知 scope 不级联报错 |
| `push_scope` / `replace_scope` | scope register transition | HIR、analysis | 已接入 |
| `min_occurs` / `max_occurs` / `strict_min` | cardinality | diagnostics、completion、hover | 已接入 |
| `required` / `deprecated` | completion/文档元数据和规则属性 | analysis、artifact hash | 字段已接入，source 覆盖有限 |
| `alternative_id` | alternative 分组和消歧 | HIR、analysis | 已接入，不能任意选第一项 |
| `documentation` / provenance | 规则说明和来源 | hover、completion、diagnostics | 已接入；结构化 related information 仍有限 |
| `enum-values.json` | 静态 enum 成员 | validation、completion | 已接入，并与 profile/workspace members 合并 |
| `type-root-keys.json` / `type-root-scopes.json` | root context 和初始 scope | HIR | 已接入 |
| `type-descriptors.json` | type instance 路径、name/type selector | engine、HIR、localisation | 已接入；动态成员来自 workspace |
| `localisation-bindings.json` | type instance 到 localisation key 的模板和条件 | HIR、analysis | 已接入，支持 required/optional/subtype/explicit-field |

## EU4 profile 提供的行为

以下行为需要 `pdx-game::eu4` 或通用 engine/analysis 的 profile-aware 代码，不应被误写成 JSON 单独完成：

- EU4 scope 名称、scope completion 和 scope compatibility；
- `country_event`、`province_event` 等 root fallback；
- `AND`、`OR`、`NOT` 等逻辑 wrapper 的解释；
- member kind alias、fallback key 和 localisation shorthand；
- scripted effect/trigger 参数的局部 definition/reference；
- WorkspaceIndex 中实际存在的 dynamic members。

规则 source 描述静态 matcher、context、文档和 transition 数据；profile 解释算法，workspace 提供当前动态事实。

## 当前限制

- `case_sensitive` 尚未在 index、resolution、completion 和 rename 全链路形成一致行为。
- CSV 仍是 syntax-only/opaque，没有 CSV parser 或列级语义消费。
- `required`、`deprecated` 等字段已进入模型，但当前 source 不覆盖所有可能语义样本。
- `catalog.records` 不是完整的 definition/reference/value rule 执行层。
- scope/control-flow 和动态 type member 的最终确认仍可能返回多候选或 `Unknown`。

规则修改必须通过 source schema/invariant、canonical hash、SQLite round-trip、manifest 和受影响的 HIR/analysis/LSP 测试；不得直接编辑 SQLite artifact。
