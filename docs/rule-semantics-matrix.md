# 规则声明与运行时语义矩阵

本文记录 `rules/eu4/*.json`、生成的 `eu4.pdxrules`、通用 engine/analysis 和 EU4
profile 之间的语义责任。它是当前实现的审计基线；不是第二份规则源，也不替代
`docs/rfc/0014-first-party-rule-source.md`。

## 基线

当前第一方源和生成物的规模为：

- 116 个文件分类；
- 2674 个 symbol descriptor；
- 13810 个 catalog record；
- 8535 条 semantic rule；
- 161 个 semantic context。
- 97 个 type localisation；
- 191 条 type-instance localisation mapping（153 required、23 optional、1 explicit-field）。

规则源能够成功编译、SQLite round-trip，并由嵌入式 artifact 加载。当前测试证明了
主要静态 matcher、cardinality、scope transition、动态 workspace member 和 LSP 查询
路径能够工作；下表进一步区分“已执行”和“仅被保存/间接使用”。

## 字段消费矩阵

| 规则源 | 声明的语义 | 运行时消费者 | 当前状态 |
| --- | --- | --- | --- |
| `manifest.json` | source format、game、目标版本 | `pdx-bake`、artifact loader | 已闭环校验 |
| `catalog.file_categories` | 路径分类、parser、文件覆盖策略 | `pdx-rules`、`pdx-engine` workspace/VFS | `script`、`localisation`、`asset`、`syntax-only` 已接入；当前数据只使用 `merge` 和 `replace-by-relative-path` |
| `catalog.symbol_descriptors` | symbol kind、symbol resolution、大小写策略 | `WorkspaceIndex`、analysis resolution | resolution 已接入；当前实现统一按 ASCII 小写索引，`case_sensitive` 尚未形成真正行为差异 |
| `catalog.records` | 规范化 catalog 字段、来源顺序 | canonical hash、`known_keys()`（hover 未知文本判断） | 主要作为已知 key/审计数据；没有作为完整 definition/reference/value 规则执行；completion 不再消费 known_keys（语义上下文不可用时返回空列表） |
| `semantic-rules.key` | exact/type/enum/dynamic/any-scalar key matcher | HIR transition、unknown-key、completion | 已接入 |
| `semantic-rules.value` | exact/bool/int/float/type/enum/scope/localisation/dynamic value matcher | HIR/analysis diagnostics、value completion | 已接入；`filepath`/`opaque` 是 runtime 类型，但当前源没有实际样本 |
| `semantic-rules.operator` | 操作符约束，例如 `=` | semantic property matching、completion | 已接入 |
| `semantic-rules.shape` | node、leaf、leaf-value、value-clause | HIR lowering、analysis validation/completion | 已接入 |
| `semantic-rules.context`/`parent_path` | semantic context 和结构路径 | HIR/analysis context lookup | 已接入；无法唯一确定 transition 时采取保守策略 |
| `semantic-rules.child_context` | 子 block 的 semantic context | HIR scope facts、analysis traversal | 已接入 |
| `semantic-rules.allowed_scopes` | 当前 Game Scope 限制 | HIR/analysis scope filtering | 已接入；未知 scope 在 HIR 阶段保守保留 |
| `semantic-rules.push_scope` | 进入子 block 时的静态 scope | HIR、analysis scope context | 已接入 |
| `semantic-rules.replace_scope` | ROOT/THIS/FROM/PREV 等 scope register 更新 | HIR、analysis scope context | 已接入；scope expression 链接采用静态唯一目标解析 |
| `semantic-rules.min_occurs`/`max_occurs` | cardinality | analysis diagnostics、hover | 已接入 |
| `semantic-rules.strict_min` | 最小 cardinality 的严格程度 | minimum cardinality severity | 已接入 |
| `semantic-rules.required` | required 标记 | completion 排序、hover、文档展示 | 字段已保留，但当前源没有 `required=true`；尚未形成独立缺失字段诊断语义 |
| `semantic-rules.deprecated` | deprecated 标记 | completion item `deprecated` 标志与排序降权 | 字段与 SQLite 列/规范哈希已接入（`#[serde(default)]`，默认 `false`）；当前源没有 `deprecated=true` 样本 |
| `semantic-rules.alternative_id` | alias/alternative 分组 | alternative selection、cardinality、transition | 已接入；不确定时拒绝任意选择 |
| `semantic-rules.documentation` | 规则说明 | hover、completion documentation | 已接入 |
| `semantic-rules.source_file`/`line` | 规则 provenance | runtime model、artifact、semantic diagnostics、hover | 已接入 semantic diagnostics message 和 hover 文本；尚未提供独立结构化 related information |
| `enum-values.json` | 静态 enum 成员 | enum validation、completion | 已接入，并与 workspace member/profile extra member 合并 |
| `type-root-keys.json` | type root selector | HIR root context selection | 已接入 |
| `type-root-scopes.json` | type root 初始 scope | HIR initial scope | 已接入 |
| `type-descriptors.json` | 路径、文件、root skip、name/type selector | engine type member extraction、HIR root selection | 已接入；动态成员仍来自 WorkspaceIndex |
| `localisation-bindings.json` | type 实例到 loc key 的模板、required/optional、subtype、subtype condition、explicit field | HIR derived localisation references、analysis resolution/diagnostics | 已接入；required 模板生成缺失 key 检查，optional 仅作为完整映射保留，explicit field 由 semantic localisation 规则执行 |

## JSON 之外的 EU4 语义

以下内容目前由 [EU4 profile](../crates/pdx-game/src/eu4.rs) 提供，而不是由 JSON
独立驱动：

- EU4 scope 名称和 scope completion；
- `country_event`/`province_event` 的初始 scope fallback；
- `trade_node` 与 `province` 的 scope compatibility；
- `AND`、`OR`、`NOT` transparent logic wrapper；
- member kind aliases；
- fallback keys；
- scripted effect/trigger 参数 definition 和 reference；
- localisation reference shorthand；
- `legacy_government` 条件 definition；
- 动态 WorkspaceIndex member 的最终内容。

这些内容不应简单复制到 JSON：动态成员属于 workspace 状态，scope/control-flow 的
解释属于 EU4 profile 行为。需要在后续 ADR 中明确哪些是 profile-only，哪些应该迁移为
第一方数据字段。

## 已确认的收敛任务

## 领域模型冲突清单

这些不是简单的“字段尚未消费”，而是 `CONTEXT.md` 的领域定义与当前第一方 artifact
之间的直接冲突，必须在规则源或编译校验层解决：

| 术语 | 领域约束 | 当前 artifact | 处理要求 |
| --- | --- | --- | --- |
| Province Collection Predicate | 裸 `area`、`region`、`continent` 是 scalar trigger，不创建 scope | 原先 `area`/`region` 各有一条 Trigger node；现已删除，compiler 也会拒绝同类新规则；Effect node 仍需由实际游戏语义 fixture 单独确认 | 增加原始规则 fixture，确认 Effect 侧语义后决定是否保留 |
| Logic Wrapper | `AND`、`OR`、`NOT` 是 Trigger-only，并保持 Game Scope | profile 将三者作为通用 transparent wrapper；artifact 中 `OR`/`NOT` 出现在 `root:government_reform` 结构中 | 明确 root context 的实际 Trigger Body，再限制 wrapper 的可用 context |
| Scope Iterator | `every/random` 为 Effect-only，`any/all` 为 Trigger-only，`limit` 只属于 Effect iterator | 当前 semantic rules 同时依赖 context 和 `child_context`，但没有独立的 iterator family/cardinality 模型 | 用 context-sensitive fixture 验证，不以 key 名称白名单替代规则语义 |

### P0：行为与声明一致性

1. 让 symbol descriptor 的 `case_sensitive` 真正影响 index、resolution、completion 和 rename。
2. 明确 `required` 与 `min_occurs` 的关系；避免同一“必填”概念出现两种未定义语义。
3. 为每种实际使用的 file resolution policy 增加 overlay、current mod、dependency、Vanilla fixture。

### P1：权威边界

1. 决定 `catalog.records` 是仅供 key catalog，还是需要升级为可执行语义。
2. 将静态 EU4 aliases/root scope/fallback metadata 与 profile-only 行为分开记录。
3. 把 `source_file`/`line` 接入规则解释或 diagnostics provenance。

## 验收要求

每次规则或语义实现变更必须至少通过：

1. first-party source schema/invariant validation；
2. artifact round-trip、canonical hash 和 manifest 校验；
3. 受影响的 HIR、analysis 和真实 JSON-RPC 测试；
4. embedded artifact server launch；
5. 本矩阵对应字段的正例、反例和动态 workspace fixture。
