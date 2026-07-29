# RFC 0005：HIR 与 Scope 系统

- 状态：Accepted
- MVP：EU4 v0.1

## 实现状态（2026-07-25）

增量实现已经落地：`HirFile` 按源顺序保留 property path、key/value 精确 range、直接 scalar、非 key bare value、localisation entry、parser recovery `UnknownConstruct`、带 polarity 的 `ParameterConditional`、按顶层 scripted definition 隔离的 parameter definition/reference，以及 profile-aware definition/reference；同一份不可变 HIR 由 `FileState`/overlay snapshot 缓存，并被 workspace shard 和 analysis 查询共享。semantic root context、semantic parent path、初始 `ScopeState` 与静态 exact command 的嵌套 transition 现在在 lowering 时生成按 range 排序的 `ScopeFact`，包括 `skip_root` type 选中实际语义根后的后代；多个 rule alternative 只要 child context、push scope 和 register replacement 完全等价，也共享同一缓存 transition。signature 冲突时，HIR 仅在直接子 key 能把其他 transition 静态证明为不可能时继续 lowering；type/dynamic key、空 block 或仍有多个可能 signature 时不作猜测。diagnostics 与已有直接子项的 nested completion traversal 通过 HIR 的 logarithmic exact-range 查询消费缓存 fact；completion 因而不会把已由 `days`/`modifier` 消歧的 block 重新按规则顺序选错。若 HIR 因 workspace-dependent type key 无法静态选择，analysis 会用当前 workspace index 再过滤一次，只在剩余规则共享唯一 transition signature 时递归；未解析或仍冲突时只验证可确定的 structural container，不回退到规则顺序。显式切换到同名 child context 时，缓存 parent path 能保留“重置 path”和“沿用 context 追加 path”的区别；transparent wrapper 的 fallback path 也不增加伪 parent segment。空 block 没有子 fact 时，completion 保留每个静态可能 destination 自己的 context、parent path 与 scope，去重后合并候选；diagnostics 仍不凭空确定 transition，无法静态判断时保留 `Unknown`。

key/value completion context 还会优先使用 HIR 的精确 key/scalar range，并把 range end 视为仍在对应 token 上；仅对未成形 recovery 输入使用逐行 `=` 启发式。因此同一行的前一个 property 不会把后一个 property key 误判成 value position。对 `modifier = { factor = ... <trigger> }` 这类结构 parent fields 与 child context 共存的 block，diagnostics 将明确匹配结构规则的子项分区校验，其余子项进入 child context；completion 同时合并两组规则并保留各自实际 parent path。analysis alternative 评分只接受唯一最高分，同分时不再按 source/id 顺序任取一个分支。HIR scope set 转入当前仍为单值的 analysis `ScopeContext` 时，只有唯一候选会具体化；多个候选降级为 `any`，不会任取第一个制造假的确定性。

依赖 workspace member 的 type matcher transition 已在 analysis diagnostics 回退中完成；声明型 dynamic key 仍按任意非空键保守处理。不能由直接子 key 唯一消歧的冲突 signature 不会再随机选择，但跨 alternative 汇总更精确的共同 diagnostics 尚未完成。ROOT/THIS/repeated FROM/PREV register intrinsic，以及 `replace_scope` 中可由当前 scope 和 exact scope-link 静态求值的单段/多段表达式已经在 HIR/analysis 共用语义下落地；每段都以前一段的目标 scope 校验下一段，任一段 unresolved 时保持 `Unknown`/`any`。唯一解析的 scripted effect/trigger invocation 已按其 definition HIR owner 精确验证并补全参数；无法唯一解析 owner 时仍保守使用兼容的 workspace 动态 member fallback。因此当前完成的是静态 scope/intrinsic、recovery 与 parameter lowering/局部导航/唯一调用解析切片，不能标记为完整 scope evaluator。

## 问题

CST 只能回答文本结构，无法回答一个 key 是字段、effect、trigger、symbol definition 还是 scope link。语义还依赖 logical path、父 context、通用 `RuleSet`、所选 `GameProfile` 和当前 workspace snapshot。

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

所选游戏 profile 将具体 spelling 映射到 intrinsic。大小写比较使用该 profile 的 rule policy。

当前 evaluator 在读取表达式和写入 `replace_scope` register 时都只把完整重复 token 识别为 register intrinsic，例如 `FROM`、`FROMFROM`、`PREV`、`PREVPREV`；`previous_owner`、`from_owner` 等普通 identifier 不得因前缀相同而误读为 register。`replace_scope = { from = owner }` 会用当前已知 scope 查询 exact effect/trigger scope-link，并把唯一目标写入 FROM；目标冲突或当前 scope 未知时写入 `Unknown`。

## Link chain

对 `owner.capital_scope` 一类链：

1. 从 current `ScopeValue` 开始。
2. 对每段查询 scope link rule。
3. 验证 `from` 与当前可能 scope 的交集。
4. 产生目标 `ScopeValue`。
5. 任何段 unresolved 时，后续状态为 `Unknown`，但保留已知的 reference fact。

当前 HIR evaluator 会让已知 scope 集合中仍兼容的分支继续到下一段；任一段没有可用目标则降级为 `Unknown`。旧 analysis fallback 只在每段得到唯一目标时继续。partial-scope 信息诊断仍未实现。

每段 exact link 查询通过冻结 `RuleSet` 的 case-insensitive exact-key index 取得候选，不再线性扫描完整 semantic rule 表；root selection、diagnostics 与 completion 的 container rule 查询同样使用 context index。派生索引不参与规则 artifact 的 canonical hash。

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

这些都是 `RuleSet`/游戏 profile 的有类型操作，不是用户配置中的任意代码。

## Parameters

scripted effect/trigger definition 中扫描 `$NAME$` 和 conditional parameter 建立 parameter definitions/uses。lowering 为每个 occurrence 保留 exact name range、syntax kind 与所属顶层 definition range；同名参数按 profile 的大小写策略在单个 block 内以首次 occurrence 作为定义锚点，在不同 definition block 中分别推断。analysis 的 definition/references/hover/rename 直接解析这些局部 facts，不经过 workspace 全局 symbol resolution，因而不会跨 block 串线；rename 只替换 exact name range，保留 `$`、`[[`、`!` 等语法，并沿用 Current Mod 可写边界与局部重名冲突检查。

Parameter definitions/references 按 occurrence range 排序；HIR 提供 position lookup 和 owner-range iterator，analysis 不为每次 hover/rename/invocation validation 全表扫描参数 facts。fuzz target 同时校验 range containment、排序与 reference 不重叠不变量。

Document symbol 查询把每个 inferred parameter 作为 `parameter` symbol 返回，full range 保留首次 occurrence，selection range 精确覆盖名称；这些局部 symbol 不进入 workspace symbol 查询，避免不同 scripted definition 的同名参数污染全局结果。

workspace 仍将局部参数 facts 投影为调用参数动态 enum member，作为 unresolved/ambiguous invocation 的保守兼容路径。对于唯一 active scripted effect/trigger，analysis 通过 index definition 的 file/range 定位 `FileState` HIR owner，只接受并补全该 owner 的参数；另一个 definition 的同名空间不再交叉污染。打开 overlay 中的唯一 definition 优先参与此解析，覆盖的磁盘 candidate 不会被误用。

调用处参数验证可后置；parser 和 HIR 必须从 MVP 起保留相关节点，以免未来破坏语法模型。

## 缓存

HIR cache key：

```text
SourceFileId + FileRevision + GameId + RuleHash + FileCategory
```

活动 `RuleSet` 或游戏 profile 变化会使内存 HIR cache 失效；单文件文本变化只使该文件失效。Vanilla 持久缓存不因 `rule_hash` 自动重建，这是明确的产品决策，用户可手动刷新。

结构 property 仍以保留重复 key 的 source-order flat vector 暴露；scope lowering 以一次线性 stack pass 建立直接子项邻接表，再沿子边递归，避免对每个父节点重新扫描全文件。生成的 `ScopeFact` 携带 context、parent path 和 persistent registers，并按 range 排序；analysis 用 exact-range logarithmic lookup。
