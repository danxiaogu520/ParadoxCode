# RFC 0005：HIR 与 Scope 系统

- 状态：Partial

## 当前 HIR

`pdx-engine::hir` 把 `ParsedFile` 转换成按 source order 保存的文件级派生事实。lowering 的输入是显式的 parsed syntax、可选 `LogicalPath`、`RuleSet` 和 `GameProfile`；不会读取全局 workspace。

```text
HirFile
  syntax: Arc<ParsedFile>
  properties
  localisation_entries
  bare_values
  definitions
  references
  scope_facts
  unknown_constructs
  parameter_conditionals
  parameter_definitions
  parameter_references
```

`HirProperty` 保留 key/value/full range、重复 key、路径和直接 scalar。Localisation frontend 产生 `HirLocalisationEntry`。profile/rule 解释可额外产生 `HirDefinition`、`HirReference` 和 semantic reference origin；这些事实仍带精确 `TextRange`，不携带独立的跨请求 symbol ID。

CST recovery 节点不会静默丢失，而是进入 `HirUnknownConstruct`。HIR 不把重复 key 折叠成 map，也不规范化或改写源码。

## Scope 数据

当前粗粒度文件 scope `Scope` 只有 `Unknown` 和 `Root`。更细的 lowering 状态使用字符串 spelling：

```text
ScopeValue = Known(Vec<String>) | Unknown | Invalid
ScopeState
  root: ScopeValue
  current: Vec<ScopeValue>
  from: Vec<ScopeValue>
  previous: Vec<ScopeValue>
```

`Known` 可以包含多个可能 scope；`Unknown` 表示信息不足，`Invalid` 表示规则已经证明不兼容。`ScopeFact` 按精确 key range 保存 semantic context、parent path 和进入该 root 时的 `ScopeState`，并支持按 range 查找。

## 当前 lowering 行为

profile-aware lowering 会根据规则和 logical path 选择 semantic root/context，并生成静态可确定的 nested transition。规则可以改变 child context、push scope 或替换 `root/current/from/previous` 中的 register。当前 evaluator 对 `THIS`、`ROOT`、`FROM`、重复 `FROM`、`PREV` 及 profile 映射的逻辑 wrapper 保留 register 状态；只有完整的 register token 才会被识别。

scope link chain 按段计算：上一段的可能目标作为下一段的输入；任一段无法解析时保留 `Unknown`，不任取一个候选。type matcher 的 dynamic transition 可以先在 HIR 中保留候选，analysis 随后用 `WorkspaceIndex` 确认 workspace member；未确认的 type key 不作为确定 transition。

analysis 只有在当前 scope 与规则 allowed scopes 无交集时报告 wrong-scope；scope unknown 或存在多个可能值时保持保守。相同 transition signature 的 alternatives 可以合并；冲突或无法由直接子项消歧时不猜测规则顺序。

## Parameters

在 scripted definition block 内，`$NAME$` substitution 和 `[[NAME] ... ]`/`[[!NAME] ... ]` conditional 会生成局部 parameter definition/reference。每个 occurrence 保留 name range、语法形式和 owner definition range；同名参数不会跨 definition block 合并。document symbols、hover 和 rename 可直接消费这些局部 facts。lowering/index 阶段还会按首次出现归纳每个 scripted definition 的紧凑调用签名：存在未受 conditional 保护的 substitution/key/text use 时参数为 required；所有实际取值 use 都位于 conditional 内、或参数只出现在 conditional 中时为 optional。当前 `required` 是无条件存在性的保守投影，不表达“提供参数 A 时参数 B 才必填”的条件依赖。该摘要不携带 CST 指针或源码，可随 index shard 保存。

## Cache 与生命周期

HIR 是 per-file、可丢弃的派生数据，和同一 `FileState` 的 parsed source、revision、index shard 一起缓存。文本 revision、active `RuleSet` 或 profile 变化会使对应文件状态重建；Vanilla 持久 cache 不保存 HIR。

## 当前限制

当前不是完整的通用 scope evaluator：复杂或冲突的 alternatives、跨分支 partial-scope 结果和无法确认的动态成员不会被强行具体化。analysis 对多个候选通常显示为 `any`，而非任取一个。`FileAnalysis.scope` 仍是保守的粗粒度值；细节只通过 HIR `ScopeFact` 和 analysis 内部状态参与查询。
