# PDX Block 与共享语义上下文实证调研

- 状态：Research
- 日期：2026-07-30
- 范围：EU4-first 语义规则建模

本文记录一次针对真实大型 EU4 Mod 的只读研究，以及它对 ParadoxCode property、Block、
Effect、Trigger、Modifier 和 scope 建模的启示。本文不是已接受 RFC，不改变当前运行时
行为或规则 source schema；后续若采纳结论，应先修订相关 RFC 和迁移计划。

## 研究问题

EU4 不同文件类别拥有差异很大的外层结构，但其中大量位置使用相同的 Effect 或 Trigger
语句集合。需要确定：

- Effect、Trigger 是否应作为跨文件复用的一等语义概念；
- Block 应建模为互斥的 Effect/Trigger/Struct 类型，还是结构与语义上下文的组合；
- `owner` 等 scope link 如何在 Effect 和 Trigger 中复用；
- Modifier、event option、on_action 和 AI modifier 等混合形态是否会推翻简单模型；
- 一词多义与无法消歧时应保留什么信息。

## 样本与边界

研究样本为用户提供的本机 Steam Workshop EU4 Mod，Workshop item 为 `3047072888`，
descriptor 声明支持 EU4 `1.37.5.0`。只读取 Script 文件和目录元数据，没有执行样本中的
二进制、Lua 或其他程序，也没有把 Mod 文件或源码片段复制进仓库、fixture 或测试
corpus。

样本规模：

| 项目 | 数量 |
| --- | ---: |
| 全部文件 | 11,569 |
| `.txt` 文件 | 7,430 |
| `common` 文件 | 2,715 |
| `history` 文件 | 3,770 |
| `events` 文件 | 468 |
| `missions` 文件 | 244 |
| `decisions` 文件 | 180 |
| `common/scripted_effects` 文件 | 106 |
| `common/scripted_triggers` 文件 | 37 |

本次采用目录统计、文本检索和代表性嵌套结构抽样，不把词法出现次数当作精确 AST
统计。注释、重复的 Vanilla 派生内容和不完整脚本可能影响文本检索数量，因此本文只把
它们用于发现模式，不作为规则完整性的证明。

## 观察

### 外层 Schema 不同，Effect/Trigger 语言相同

Event、Mission、Decision、Triggered Modifier 和其他 `common` 类型拥有不同的固定字段，
但它们的 `effect`、`immediate`、`hidden_effect` 等位置复用同一套 Effect 语句；
`trigger`、`potential`、`allow`、部分 highlight 条件等位置复用同一套 Trigger 语句。

Scripted Effect 定义的根 Block 本身使用 Effect 语义，Scripted Trigger 定义的根 Block
本身使用 Trigger 语义。工作区中新定义的 scripted effect/trigger 也应分别进入这两个
共享成员集合，而不是复制到每一种文件 Schema。

仓库当前第一方规则已经反映这一事实：`semantic-rules.json` 中约有 1,863 条 `effect`
context 规则和 1,854 条 `trigger` context 规则，多种 root context 再通过
`child_context` 进入它们。

### Block 经常同时包含固定字段和共享语句

真实样本否定了简单的互斥分类：

```text
EffectBlock | TriggerBlock | StructBlock
```

典型反例：

- Event option 有 `name`、`trigger`、`ai_chance` 等固定字段，同时直接接受 Effect；
- on_action 有 `events` 等专用字段，同时直接接受 Effect；
- AI modifier 有 `factor` 等固定字段，同时直接接受 Trigger；
- effect context 中的 `if` 有 `limit` 等控制字段，同时直接接受 Effect；
- trigger context 中的 `if` 有控制字段，同时直接接受 Trigger。

因此，所谓 mixed block 的常见本质不是一种新的封闭 Block 类型，而是：

```text
固定字段集合 + 一个共享 body context
```

### Scope link 改变 scope，但通常保留 body context

`owner` 在 Scripted Effect 和 Scripted Trigger 中都大量出现。它的共同语义是从当前
province 等 scope 导向 owner country；差异来自外层语义上下文：

```text
Effect × Province  --owner--> Effect × Country
Trigger × Province --owner--> Trigger × Country
```

因此 `owner` 不应分别硬编码为 `owner_effect` 和 `owner_trigger`。它应声明 scope
transition，并继承调用点的 body context。相反，`limit` 明确进入 Trigger context，
不能继承外层 Effect context。

### Modifier 不是单一 Block 类型

样本中至少存在以下不同概念：

- static/event modifier 定义：直接接受共享 Modifier 成员；
- AI modifier clause：固定 `factor` 等字段，加 Trigger body context；
- effect 参数中的 modifier reference：标量 symbol reference；
- triggered modifier 定义：固定 `potential`/`trigger` 字段，加 Modifier 成员。

因此 `ModifierBlock` 不适合作为通用封闭类型。应分别使用 Modifier semantic context、
具体 Block Schema 和 modifier symbol kind。

### Map、List 和参数结构仍需要独立值形态

并非所有 Block 都承载 Effect/Trigger/Modifier：

- weighted/random list 使用数值 key，每个 entry 的值再进入 Effect；
- RGB 等 Block 是有序 bare scalar tuple；
- `add_country_modifier` 一类参数 Block 只有固定字段；
- 部分开放容器以 symbol、enum、数值或动态 value-set 成员作为 key。

这些形态属于右值内容约束，不应被 Effect/Trigger 上下文吞并。

## 推荐领域模型

### Semantic Context

Semantic Context 是可跨文件和 Block Schema 复用的 property signature 集合。EU4 profile
至少需要 `effect`、`trigger` 和 `modifier`，并可继续声明其他 profile-defined context。

```text
SemanticContext
  id
  property signatures
  included shared member groups
```

Effect/Trigger 是稳定、共享的语句命名空间，不是某一种文件类别的私有字段表。Scope
links、逻辑 wrapper 和 workspace scripted command 可以通过规则组进入一个或多个
context。

### Block Schema

Block 在 syntax 层仍只是保留顺序、重复 key、bare value 和 recovery item 的 `{ ... }`
容器。语义层由 Block Schema 描述：

```text
BlockSchema
  fixed fields
  body context policy
  block-level cardinality/ordering constraints
```

body context policy 有三种基本状态：

```text
None                 纯结构、map、tuple 或 opaque Block
Fixed(ContextId)     固定进入 Effect、Trigger、Modifier 等 context
Inherit              继承调用点 context
```

每个 direct property 同时查询固定字段和 body context。若两者都匹配，保留候选并执行
正常的 alternative 消歧，不能隐式规定固定字段或 context 永远优先。

### Property Signature

Property 不使用一个扁平类型枚举，而由正交约束组成：

```text
PropertySignature
  key pattern
  allowed operators
  value type
  semantic roles/facts
  scope contract
  cardinality
```

key pattern 负责匹配 exact、symbol member、enum member、numeric key、dynamic member
或 any scalar。semantic roles 负责表达 field、effect/trigger invocation、definition、
reference、scope link 等事实；一个 property 可以同时产生多个事实。

### Value Type

右值至少区分：

```text
Scalar
  bool / int / decimal / date / exact literal
  enum / symbol / localisation / filepath
  scope expression / dynamic value / opaque scalar

Block
  BlockSchema
  map entry block
  list/tuple block
  opaque block

Union / Optional / Unknown
```

Effect/Trigger Block 是对 Block Schema body context 的友好称呼，不应在通用 runtime
中成为封闭的 EU4-only Rust 枚举。

## 一词多义与歧义

文本 spelling 不是语义身份。同一个 key 可以在 Effect、Trigger 和某个 Struct Schema
中对应不同 property signature。消歧依次使用文件类别、root Schema、parent path、当前
semantic context、operator、右值形态、直接子项、scope 和 workspace symbol。

只淘汰已经证明不可能的候选，不按 source order、hash-map 顺序或任意评分随机选择。
仍然歧义时：

- diagnostics 只使用所有候选共同成立的结论；
- completion 合并所有仍可能 context 的成员；
- hover 展示候选义项；
- definition、references 和 rename 要求去重后的唯一 symbol identity；
- scope transition 不唯一时保留 alternatives/unknown，抑制级联错误。

一个已解析 signature 同时产生 field 和 localisation reference 等多个 fact，不属于歧义。

## 对当前实现的影响

当前 `KeyMatcher`、`ValueMatcher`、`SemanticRule.context`、`parent_path`、
`child_context`、scope transition 和 HIR `ScopeFact` 已提供大部分运行时基础。RFC 0005
也已明确支持结构 parent fields 与 child context 共存。因此推荐渐进演化，不进行
一次性重写：

1. 在第一方 source 概念上明确 `SemanticContext` 与 `BlockSchema`；
2. 将 `child_context: Option<String>` 的领域含义拆成 `None`、`Fixed` 和 `Inherit`；
3. 将 scope transition 与 body context policy 保持正交；
4. 让 source 中的多个文件 Schema 引用共享 context，由 `pdx-bake` 编译成规范化规则；
5. HIR 保留 resolved/ambiguous context 与 scope facts，analysis 查询不重新猜测；
6. 用原创最小 fixture 重现 option、on_action、AI modifier、scope link、weighted list
   等模式，不复制研究样本。

若 source schema、artifact schema、canonical hash 输入或公开 runtime API 因此变化，
必须先修订 RFC 0004、0005、0007、0015 中的相关边界，并提供确定性迁移和分层回归。

## 结论

推荐的核心抽象是：

> 文件类别确定外层 Block Schema；Block Schema 用固定字段描述自身结构，并通过 body
> context 复用 Effect、Trigger、Modifier 等共享语句集合；scope link 独立改变 scope，
> 通常继承调用点 body context。

这既保留了不同 EU4 文件类型的结构差异，也避免复制高度一致的 Effect/Trigger 规则，
并能解释真实大型 Mod 中常见的 option、on_action、modifier clause、`if`、`owner`、
weighted list 和 scripted definition。
