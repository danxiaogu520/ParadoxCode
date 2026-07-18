# RFC 0005：HIR 与 Scope 系统

- 状态：Accepted
- MVP：EU4 v0.1

## 问题

CST 只能回答文本结构，无法回答一个 key 是字段、effect、trigger、symbol definition 还是 scope link。语义还依赖 logical path、父 context、Eu4Rules 和当前 workspace snapshot。

## HIR 原则

- HIR 是按文件、可丢弃并可重建的派生数据。
- HIR 节点始终保留 CST source range。
- lowering 不修改 CST，也不生成规范化源码。
- 未识别结构保留为 unknown，而不是删除。
- HIR 不表示为通用 JSON map，重复 key 和顺序必须保留。

## HIR 结构

```text
HirFile
  items: Vec<HirItem>
  scope_facts: Vec<ScopeFact>
  definitions: Vec<LocalDefinition>
  references: Vec<LocalReference>

HirItem
  Property
  Block
  Definition
  Invocation
  Reference
  ScopeTransition
  ParameterConditional
  LocalisationEntry
  UnknownConstruct
```

一个 CST property 可以在 HIR 中同时贡献多个 fact。例如：

```text
title = example.1.t
```

可以产生 `Property` 和 `Reference<Localisation>`。`example_effect = { ... }` 在 scripted effects 顶层产生 `Definition<ScriptedEffect>`，其 block 内部仍继续 lower 为 effect context。

## LoweringContext

```text
logical_path
file_category
  rule_hash
rule_database
parent_semantic_context
scope_state
```

lowering 只能通过以上显式输入获取语义，不得读取全局 workspace。跨文件解析在 index/analysis 阶段进行。

## Scope 身份

`ScopeId` 是 Eu4Rules 内 intern id。MVP EU4 至少包含实际规则所需的 `country`、`province` 等 scope；不为了完整列表提前创建无规则支持的 scope。

```text
ScopeValue
  Known(ScopeSet)
  Unknown
  Invalid
```

- `Known` 可以包含多个可能 scope。
- `Unknown` 表示信息不足，不应直接产生 wrong-scope error。
- `Invalid` 表示已证明不兼容，可产生诊断。

## ScopeState

```text
ScopeState
  root: ScopeValue
  current: Vec<ScopeValue>
  from: Vec<ScopeValue>
```

当前 scope 是 `current` 栈顶；空栈回退到 root。状态是持久值，进入 block 时派生新状态，离开 block 不修改父状态。

## Intrinsic

通用 evaluator 支持：

- `THIS`：保持 current
- `ROOT`：push root
- `FROM` / repeated FROM：读取 from stack
- `PREV`：pop current
- logical wrappers：保持 scope

Eu4Rules 将具体 spelling 映射到 intrinsic。大小写比较使用 EU4 rule policy。

## Link chain

对 `owner.capital` 一类链：

1. 从 current `ScopeValue` 开始。
2. 对每段查询 scope link rule。
3. 验证 `from` 与当前可能 scope 的交集。
4. 产生目标 `ScopeValue`。
5. 任何段 unresolved 时，后续状态为 `Unknown`，但保留已知的 reference fact。

链中存在部分有效可能性时可以继续分析，并产生低严重度的 partial-scope 信息；MVP 可先不报告 partial，只在完全不相交时报告错误。

## Command scope validation

effect/trigger 规则的 `allowed_scopes` 与当前 `Known(ScopeSet)` 比较：

- 有交集：有效。
- 完全无交集：wrong scope。
- current unknown：不报告 wrong scope。
- command unresolved：报告 unknown command，不再派生 scope error。

这条顺序用于抑制级联诊断。

## Block context

command 可以声明：

- `body_context`：子 block 是 effect、trigger 或特定结构 context。
- `scope_transition`：进入子 block 前改变 scope。
- `push_from`：是否将旧 current 加入 from stack。
- `replace_root/current/from`：少量需要完整替换的规则。

这些都是 Eu4Rules 的有类型操作，不是用户配置中的任意代码。

## Parameters

scripted effect/trigger definition 中扫描 `$NAME$` 和 conditional parameter 可建立 parameter definitions/uses。MVP 将参数限制在定义 block 内的局部 symbol，不加入 workspace 全局 symbol search。

调用处参数验证可后置；parser 和 HIR 必须从 MVP 起保留相关节点，以免未来破坏语法模型。

## 缓存

HIR cache key：

```text
SourceFileId + FileRevision + Eu4RuleHash + FileCategory
```

活动 Eu4Rules 变化会使内存 HIR cache 失效；单文件文本变化只使该文件失效。Vanilla 持久缓存不因 `rule_hash` 自动重建，这是明确的产品决策，用户可手动刷新。
