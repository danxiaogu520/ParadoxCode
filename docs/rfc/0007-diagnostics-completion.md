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
- localisation 文件 key 补全：`complete()` 对 `FileFormat::Localisation` 非 value 位置返回
  workspace + 打开文件中的 `localisation` kind definitions；value 位置只补 `localisation`
  符号（不混入其他 workspace 定义或标量）。

## Completion item

内部 item 包含 label、kind、detail、documentation、replacement range、insert text、sort score、deprecated。replacement range 必须来自 token/range 计算，不由 Zed 扩展修正。

MVP snippet 保守使用：block command 可插入 `name = {\n    $0\n}`；客户端不支持 snippet 时回退为纯文本。

## 当前实现状态

- **detail 文案**：左值 rule-backed key 显示语义类别裸名——`effect`/`trigger` 命令写
  `effect`/`trigger`，其他 context 写裸 context 名（`modifier_rule`），type/enum/dynamic
  成员写裸集合名（`country_tag`/`government_reform`/`custom_attribute`），scripted 参数写
  `parameter`；右值写值类型裸名（`bool`/`int`/`float`/`scope`/`scope link`/`localisation`/
  集合名）。无 "semantic rule xxx"/"PDX xxx" 套话。
- **无兜底**：语义上下文不可用时返回空列表（syntax-only/identity-only 模式补全为空）。
  `known_keys` 仅保留给 hover 的未知文本判断；scripted effect/trigger 调用在语义路径经
  `KeyMatcher::Type` 补全，不受影响。
- **snippet 能力协商**：`client_snippet_support` 在 initialize 时按
  `textDocument.completion.completionItem.snippetSupport` 捕获（异步路径经
  `PreparedInitialize` 提交）；不支持的客户端收到纯文本（`$0`/`$N` 占位符被剥除，空行清理）。
- **rule-backed key insert text**：`add_semantic_key_items` 按 rule shape 生成
  insert text——新 key 的 Leaf/ValueClause 补 `key = `（光标落在值位），Node 补
  `key = {\n    $0\n}`（snippet 空块骨架）；替换已有 `key = value` 的 key 时只替换
  key 拼写，保留现有赋值。
- **scripted definition snippet**：语义路径的 `Type("scripted_effect")`/`Type("scripted_trigger")`
  候选把其 HIR owner 参数展开为顺序 tab stop（`param = $1`），`$0` 落在块内空行；参数无法
  唯一解析时回退固定骨架。
- **`completionItem/resolve`**：server 声明 `resolveProvider`，rule-backed item 携带
  `data = "rule:<id>"`；resolve 请求经 `pdx_analysis::completion_resolve` 按 rule id 补
  documentation，其余 item 原样返回。
- **fuzzy 匹配**：候选过滤为大小写不敏感 prefix 或 substring；substring 命中加
  `FUZZY_MATCH_PENALTY` 降权，prefix 命中保持原序。
- **scope 表达式**：`ValueMatcher::Scope` 位置补全基础 scope 名、intrinsic（`root`/`this`/
  `from`/`prev`，按解析出的实际 scope 过滤，unknown 时保留），以及从当前 scope 可达的
  scope link 关键字；单跳不满足期望 scope 时再补一跳链（上限 `SCOPE_CHAIN_LIMIT`）。
- **deprecated**：`SemanticRule.deprecated` 进入规范哈希与 SQLite 列；completion item
  透传 `deprecated` 并加 `DEPRECATED_SORT_PENALTY` 降权。

## 不完整输入

completion 不要求完整 HIR。若 cursor 位于 ERROR node：

1. 使用最近完整父 context。
2. 读取左侧 token 判断 key/value。
3. 无法确定 scope 时用 Unknown；scope unknown 时不隐藏候选。
4. 无法确定任何 semantic context 时返回空列表（不再退回 syntax-level candidates）。

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
