# RFC 0007：诊断与补全策略

- 状态：Partial

## 输入与阶段

`pdx-analysis` 只从一个 `AnalysisSnapshot` 查询，不直接读磁盘。当前支持的 parsed input 是 Script 和 Localisation；`SyntaxOnly`/opaque 文件没有 `ParsedSource`，因此没有当前 analysis 的 syntax 或 semantic diagnostics。

对可解析文档，analysis 先收集 parser errors，再运行适用的 semantic checks 和 symbol resolution。Script 的 structural checks 只在存在匹配的 semantic root/context 时执行；未覆盖的 context 不伪造 unknown-key 诊断。

## DiagnosticCode

当前公开的诊断类别为：

```text
Syntax
UnknownKey
UnknownSymbol
AmbiguousSymbol
UnknownScope
InvalidValue
Cardinality
WrongScope
MacroExpansionCycle
MacroExpansionLimit
```

`UnknownKey` 和表示分析未完整执行的 `MacroExpansionLimit` 默认是 Warning；其余类别默认是 Error。规则的 `severity` 可覆盖适用 semantic rule 的部分结果。parser 内部错误不会被包装成规则错误。宏展开后的普通语义错误保留原 code；只有递归环和预算/诊断限流分别使用 `MacroExpansionCycle`、`MacroExpansionLimit`。

### 规则匹配

在当前 context/path 中，analysis 依次使用规则的 `Exact`、`Type`、`Enum`、`AnyScalar` 和 `Dynamic` key matcher。只有没有任何 matcher 接受时才报告 `UnknownKey`；`AnyScalar` 只开放所在 container，不会开放整个父树。

已确认的 key 若没有适用 scope 会产生 `WrongScope`；当前 scope unknown 或仍有可行 scope 时不强行报错。value matcher 失败产生 `InvalidValue`，required/cardinality 违反产生 `Cardinality`。只有 HIR 已将 scalar 解释为指定 kind reference 时，缺失或同优先级多 candidate 才分别产生 `UnknownSymbol` 或 `AmbiguousSymbol`。

同一 `code`、severity、range 和 message 的诊断会去重，并按 source range 和 code 稳定排序。

### Scripted macro expansion

定义侧只有经 HIR 确认为当前 owner 参数的 substitution/key token 才延后 binding-dependent `InvalidValue`/`UnknownKey`；普通脚本字面 `$X$` 仍按规则诊断。仍含 owner-local 参数的嵌套宏调用也延后展开，待外层真实调用把参数具体化后再递归。调用先按正常 source priority 解析唯一 active definition，再直接读取该 definition shard summary 的 `MacroTemplate`；因此 live workspace 与 cache-only Vanilla 走同一分析路径。analysis 在调用点绑定 scalar 参数、确定 conditional、递归展开嵌套宏，并使用 descriptor body context 与调用者 scope 运行同一 semantic container validator。定义侧 cached scope facts 不参与展开校验。静态 signature 已报告 required 缺参时不再重复进入展开；展开期仍负责活动 conditional 分支产生的动态缺参。

参数生成 token 的错误映射到参数 value range，固定模板内容映射到宏调用名；standalone bare parameter 绑定到 quoted scalar 时，payload 会投影成 source-mapped property tree 后再进入展开体。消息带 expansion owner，原 `UnknownKey`、`InvalidValue`、`WrongScope`、`Cardinality` 等 code 不被包装。递归使用精确定义 identity 栈并报告完整 cycle chain。一次 diagnostics 查询共享深度、expanded-node、token-byte 工作预算并持续检查 cancellation；每次调用的展开诊断也有固定上限。缺模板、歧义或损坏 owner 保守退回 signature/OpenWorld；cache schema 5 持久化完整的规范化模板，因此正常 cache-only Vanilla 定义不会因缺 HIR 而降级。first-party rule 不得按具体 scripted effect/trigger 名称补写 quoted 参数。

### Quoted Script containers

只有匹配 `RuleShape::QuotedScript` 的 quoted scalar 才会下钻；普通字符串和仅匹配 `Leaf` 的
scalar 保持 opaque。analysis 使用 parser 的容错 decoded parse 和可组合 source map，把内层
syntax/semantic diagnostics 精确映回原文件，并用规则的 child context、scope transition 和
同一个 semantic container validator 递归检查。`AnyScalar` 是 container-local fallback：存在具体 key matcher 时不参与 transition；同一 quoted scalar 存在显式 `QuotedScript` shape 时，它优先于普通 `Leaf` transition。diagnostics、completion、hover 和 navigation 共用 query-local budget：嵌套深度 32、单 payload 1 MiB、累计 quoted-token 输入 1 MiB、最多 50,000 个 secondary CST 节点，并持续检查 cancellation。
单行、多行和暂时有 syntax error 的 payload 使用同一语义，不复用 formatter 的安全启发式。
主文档 quoted token 使用 O(1) 线性 offset 映射；只有已经进入 secondary container 的嵌套
token 才物化组合 offset 表。每层 secondary CST 中的 quoted token 都保留该层原始 spelling，
下一次下钻只解码一层，不能预先重新编码。没有 first-party rule 或 workspace macro template
语义的自定义 engine helper 保持 opaque。

## 发布与新鲜度

`pdx-analysis::analyze` 默认只汇总 open overlay 的 push diagnostics；磁盘文件仍可通过单文件分析查询，并参与索引和 navigation。LSP 在编辑后约 200ms debounce 运行可取消的诊断任务。

诊断任务携带文档版本。新版本到达时旧任务会被取消或其结果被丢弃，只有仍对应当前版本的结果才发布 `textDocument/publishDiagnostics`。查询层和 LSP 层都使用协作式 cancellation。

## Completion

补全先依据 cursor 附近的 CST/HIR range 判断 key/value 位置，再读取 semantic context、scope、`RuleSet` 和 `WorkspaceIndex`：

- Script key 位置提供当前 context/path 的 rule-backed keys、type/enum/dynamic members 和可达 scope 相关项；
- Script value 位置提供 bool、数值/date、enum/type、scope、localisation 等适用 matcher 的候选；
- Localisation entry 的 key 位置只提供 `localisation` definitions；value 位置也只提供 `localisation` candidates；语言 header 位置返回空列表；
- 语义 context 无法确定时返回空列表，不退回不受规则约束的 syntax-level 候选。

每个 `CompletionItem` 携带 label、kind、detail、documentation、replacement range、insert text、排序分数和 `deprecated` 标记。rule-backed item 可携带 `resolve_data = rule:<id>`；`completionItem/resolve` 再从当前 `RuleSet` 读取 documentation。结果按排序分数和大小写不敏感 label 排序并去重。

scope completion 使用当前已知 scope、profile 提供的 intrinsic/scope 名称和可达 scope link；scope unknown 时保留候选而不是假定一个具体 scope。嵌套 block 的 context 选择优先消费 HIR 的 cached scope facts，无法唯一选择时保持保守。

scripted macro 参数约束除 scalar `ValueMatcher` 外，也跟踪 standalone bare `Target` 所在的 semantic container。参数在调用点写成 quoted scalar 时，completion 以推导出的 context/path/scope 下钻；嵌套宏、conditional、环和预算沿用宏约束会话。quoted 深度上的 completion insertion 会逐层转义 `"`/`\\`。

scripted effect/trigger 补全从 active workspace/Vanilla definition 的紧凑 signature 生成调用：零参数使用 `name = yes`；所有参数化宏都使用 named block，并只预填 required 参数。参数数量不隐式创造 positional scalar shorthand。block 调用会把缺失的 required 参数诊断为 Error，把 last-wins 的重复参数诊断为 Warning；参数名通过 owner-qualified signature 校验。无法唯一解析 definition/signature 时，参数域保持 open-world：不报告 unknown parameter、不回退到静态参数 enum，并提供保守的空 block 补全。

scripted definition body 内输入 `$` 时，completion 只读取包含光标的宏 owner 的 HIR 参数定义，返回 `$NAME$` token，并按键位/值位设置 replacement range 与 detail；不会跨 owner 泄漏候选。

scripted macro 调用块的参数键继续由 owner-qualified signature domain 提供。参数 value 位置在活动 definition 的 shard template 可用时执行查询内符号约束收集：目标参数以符号值沿 conditional 和 named nested-macro forwarding 传播，各 use-site 的适用 `ValueMatcher` 复用普通 value candidate 生成语义，并对多个 use-site 的候选取交集。冲突约束返回空候选；复合 token、键位 substitution、环或预算中止时不猜测体内约束，退回 signature/OpenWorld。cache-only Vanilla 与 live workspace 使用同一路径。

quoted Script 补全从 decoded CST 判断内层 key/value 位置，虚拟 property range 通过 source map
落在主文档坐标。候选仍走普通 semantic completion；写回主文档前按 quoted 嵌套深度逐层编码
`"` 和 `\\`，避免 completion edit 提前闭合外层字符串。hover 使用同一语义上下文；规则或
profile 确认的引用也会投影到普通 definition/references/rename 流程，但 secondary CST 不进入
持久索引。

## Hover 与当前限制

hover 只对已确认的 symbol、semantic property/value、规则 documentation、局部 parameter 或 Vanilla cache 的 localisation preview 生成；普通未知 scalar 和 comment 不制造 tooltip。definition/reference 的来源、priority、shadowed/ambiguous 状态可由 analysis 组合到 hover。已解析 scripted macro 的 symbol hover 还显示规范调用形式以及 required/optional 参数。

semantic property/value hover 默认面向脚本作者，而不是规则 compiler 调试器：单一语义显示 documentation、值 matcher、必要的有效 scope、实际 scope transition 和有意义的 cardinality；多语义只显示紧凑的 matcher 摘要，并在候选语义共享时合并 documentation。等价的 source-derived rule rows 不重复渲染。context、parent path、shape、operator、scope registers 和 source provenance 属于内部分析数据，不进入默认 hover；scope 不匹配时只显示面向用户的有效 scope 提示。

当前限制是：CSV/其他 opaque resource 没有 parser 级诊断或补全；scope evaluator 仍是部分实现；未被规则和 HIR 确认的动态文本不会产生确定的 symbol 诊断。未打开的 Current Mod 文件不主动 push diagnostics，但可通过 CLI/直接 analysis 查询处理。
