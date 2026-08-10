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
```

`UnknownKey` 默认是 Warning；其余类别默认是 Error。规则的 `severity` 可覆盖适用 semantic rule 的部分结果。parser 内部错误不会被包装成规则错误。

### 规则匹配

在当前 context/path 中，analysis 依次使用规则的 `Exact`、`Type`、`Enum`、`AnyScalar` 和 `Dynamic` key matcher。只有没有任何 matcher 接受时才报告 `UnknownKey`；`AnyScalar` 只开放所在 container，不会开放整个父树。

已确认的 key 若没有适用 scope 会产生 `WrongScope`；当前 scope unknown 或仍有可行 scope 时不强行报错。value matcher 失败产生 `InvalidValue`，required/cardinality 违反产生 `Cardinality`。只有 HIR 已将 scalar 解释为指定 kind reference 时，缺失或同优先级多 candidate 才分别产生 `UnknownSymbol` 或 `AmbiguousSymbol`。

同一 `code`、severity、range 和 message 的诊断会去重，并按 source range 和 code 稳定排序。

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

scripted effect/trigger 补全从 active workspace/Vanilla definition 的紧凑 signature 生成调用：零参数使用 `name = yes`；所有参数化宏都使用 named block，并只预填 required 参数。参数数量不隐式创造 positional scalar shorthand。block 调用会把缺失的 required 参数诊断为 Error，把 last-wins 的重复参数诊断为 Warning；参数名通过 owner-qualified signature 校验。无法唯一解析 definition/signature 时，参数域保持 open-world：不报告 unknown parameter、不回退到静态参数 enum，并提供保守的空 block 补全。

## Hover 与当前限制

hover 只对已确认的 symbol、semantic property/value、规则 documentation、局部 parameter 或 Vanilla cache 的 localisation preview 生成；普通未知 scalar 和 comment 不制造 tooltip。definition/reference 的来源、priority、shadowed/ambiguous 状态可由 analysis 组合到 hover。已解析 scripted macro 的 symbol hover 还显示规范调用形式以及 required/optional 参数。

semantic property/value hover 默认面向脚本作者，而不是规则 compiler 调试器：单一语义显示 documentation、值 matcher、必要的有效 scope、实际 scope transition 和有意义的 cardinality；多语义只显示紧凑的 matcher 摘要，并在候选语义共享时合并 documentation。等价的 source-derived rule rows 不重复渲染。context、parent path、shape、operator、scope registers 和 source provenance 属于内部分析数据，不进入默认 hover；scope 不匹配时只显示面向用户的有效 scope 提示。

当前限制是：CSV/其他 opaque resource 没有 parser 级诊断或补全；scope evaluator 仍是部分实现；未被规则和 HIR 确认的动态文本不会产生确定的 symbol 诊断。未打开的 Current Mod 文件不主动 push diagnostics，但可通过 CLI/直接 analysis 查询处理。
