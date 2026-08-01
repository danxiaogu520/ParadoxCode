# RFC 0007：诊断与补全策略

- 状态：Accepted
- MVP：EU4 v0.1

## 原则

诊断由自有 `Eu4Rules` 驱动。unknown key 必须遵循实际 matcher，不能用笼统的 closed/open/dynamic context 标签近似。

## 诊断阶段

按顺序执行：

1. Syntax
2. File classification / EU4 rule availability
3. Structural rule matching
4. Value and operator validation
5. Symbol resolution
6. Scope validation

后续阶段依赖前序结果。若 command 未解析，不再针对它报告 scope 或 argument shape 派生错误。

## Syntax diagnostics

来源：Rust parser recovery/error nodes 和 localisation parser errors。

规则：

- 一条根因错误尽量只产生一条 diagnostic。
- 未闭合 delimiter 指向 opening token，并在可能时附 expected closing token。
- syntax message 不暴露 parser 内部 node 名。
- syntax diagnostics 可在 didChange 后立即发布。

## Unknown key

当前语义节点暴露一组 `KeyMatcher`：

- `ExactKey(name)`
- `AnyScalarKey`
- `TypeKey(type_id)`
- `EnumKey(enum_id)`
- `NumericKey(range)`
- `AliasRef(alias_id)`
- 明确的 opaque/ignore matcher

校验先尝试 exact candidate，再尝试通用 matcher。`AliasRef` 展开 Eu4Rules 中的 alias alternatives；`TypeKey` 查询 `WorkspaceIndex`，`EnumKey` 查询数据库静态 enum 与 workspace 动态 enum members。只有所有 matcher 都不接受该 key 时才报告 unknown key。`AnyScalarKey` 只开放它所在的节点，不影响父节点、兄弟节点或整个文件。

## Unknown symbol

只有 HIR 确认 value 是某 kind reference 才报告。结果：

- 无 candidate：unknown symbol。
- 多个同优先级 candidate：ambiguous symbol。
- candidate 仅存在于 shadowed 文件：说明被覆盖来源。

诊断附稳定 code 和 data，供未来 Code Action 使用。

## Scope error

仅在 command resolved 且当前 scope 与 allowed scopes 完全不相交时报告。message 包含：

- command
- 当前可能 scope
- 允许 scope
- 导致当前 scope 的最近 transition（若可用）

## Severity

默认：

- syntax error：Error
- unknown symbol：Error
- ambiguous symbol：Error
- wrong scope：Error
- unknown key：Warning，直到 EU4 rule 覆盖率达到发布门槛
- deprecated rule：Warning
- rule database unavailable/corrupt：Information（server 降级 syntax-only）

Eu4Rules 可以为规则指定 severity，但不能将 parser crash 等内部错误伪装成用户代码错误。

## Diagnostic identity 与去重

内部 diagnostic：

```text
code, primary range, message key, related ranges, severity, source
```

同 code/range/root cause 合并。消息格式化在 analysis 边界完成；LSP 层只转换。

## 发布策略

- Syntax：立即计算。
- 当前 Mod和未保存文件的 semantic diagnostics：编辑后约 200 ms debounce。
- 受索引变化影响的其他打开文件：后台、可取消。
- Vanilla 与依赖 Mod：参与索引和语义查询，但默认不向编辑器发布 diagnostics。
- 当前 Mod未打开文件：MVP 不主动 push，CLI check 可全量运行。
- 新 revision 到达后丢弃旧 revision diagnostics。

## Completion context

completion 先确定 cursor role：

```text
PropertyKey
PropertyValue
BlockItem
ScopeChainSegment
LocalisationKeyReference
ParameterName
UnknownRecovery
```

来源：typed CST、cursor 附近 token、semantic context、scope state、Eu4Rules 和 WorkspaceIndex。

## Completion 类型

### Key / command

- 当前 closed context 中的字段
- effect/trigger context 中的内建 commands
- 当前 scope 合法的 scripted effect/trigger
- scope links 与 intrinsic

排序时 scope 合法项优先；scope unknown 时不隐藏候选。

### Value

- bool、enum、有效 operator
- 指定 kind symbols
- localisation keys
- scope expressions
- number/date snippet 或 placeholder

### Localisation

- 在被规则标为 localisation 的 value 位置补全 key。
- localisation 文件 entry key 的定义补全不作为 MVP 必需项。

## Completion item

内部 item 包含 label、kind、detail、documentation、replacement range、insert text、sort score、deprecated。replacement range 必须来自 token/range 计算，不由 Zed 扩展修正。

MVP snippet 保守使用：block command 可插入 `name = {\n    $0\n}`；客户端不支持 snippet 时回退为纯文本。

## 不完整输入

completion 不要求完整 HIR。若 cursor 位于 ERROR node：

1. 使用最近完整父 context。
2. 读取左侧 token 判断 key/value。
3. 无法确定 scope 时用 Unknown，不返回空列表。
4. 只在无法确定任何 semantic context 时退回 syntax-level candidates。

## Hover

hover 组合：

- rule documentation
- command kind 和 value shape
- allowed/current scopes
- symbol definition kind、logical path、source root
- override/ambiguity 信息

不得读取或展示整个 Vanilla 文件内容。

当前实现约束：

- 普通未知文本、注释和未确认语义角色的 scalar 不返回 hover，避免制造无意义的 tooltip；
- semantic property hover 展示 context、parent path、shape、value matcher、operator、cardinality、scope 和 scope transition；
- scalar value hover 展示所属 property、实际值和 accepted/does not match 校验结果；
- 同一个 property 仍有多个可行 rule alternative 时，必须稳定地展示全部候选，不能按遍历顺序选一个；
- symbol hover 展示 resolved/unresolved/ambiguous 状态、source root、logical path 和 shadowed definitions；symbol 查询使用当前文件和 symbol bucket 的定向路径，不构造完整 workspace semantic 列表；
- localisation symbol 只展示当前解析到的单条短文本；Vanilla 可从 cache 的有限长度派生预览展示该文本，但不展示完整源文件或 Vanilla 文件内容；
- scripted definition 的局部参数 hover 展示 owner、inferred optional/required arity、语法形式和当前 owner 内的 occurrence 数量；
- semantic rule provenance 以 `source_file:line` 形式展示，便于维护第一方规则源。
