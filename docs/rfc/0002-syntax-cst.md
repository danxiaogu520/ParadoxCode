# RFC 0002：语法、CST 与编辑更新

- 状态：Current

## 当前前端

`pdx-parser::FileFormat` 当前只有两项：`Script` 和 `Localisation`。两者都生成保留 source range 的 typed CST；parser 不读取规则数据库，也不把 scalar 预判成游戏语义类型。

CSV 当前只作 `SyntaxOnly`/opaque resource 处理。它不是 `FileFormat`，没有 CSV CST、列级诊断、HIR 或 formatter。`pdx-engine` 对 `ParserKind::SyntaxOnly` 不创建 `ParsedSource`。

## Script CST

当前 `CstKind` 包含：

```text
Document, Property, Key, Operator, Value, Block,
BareValue, QuotedString, HeaderBlock, ParameterBlock,
ParameterCondition, Comment, Bom, Error
```

基本形状为：

```text
Property := Key Operator Value
Value    := scalar | quoted string | Block | HeaderBlock | ParameterBlock
Block    := "{" item* "}"       // mixed container
```

`Block` 保留混合内容，不区分 object/array；重复 key 作为多个 `Property` 按 source order 保留。支持的 operator 为 `=`、`<`、`<=`、`>`、`>=`、`!=`、`==` 和 `?=`。`BareValue` 保留原始文本，数字、日期、enum、symbol 和 scope 等解释留给 HIR/rule 层。

`HeaderBlock` 覆盖 `rgb { 1 2 3 }` 一类形式；`ParameterBlock` 覆盖 `[[name] ... ]` 与 `[[!name] ... ]`。注释和 UTF-8 BOM 作为节点/token 保留，quoted string 支持转义字符。

主 CST 始终把 quoted string 保持为单个 opaque `QuotedString`。对于规则明确声明为
`quoted_script` 的 scalar，parser 另提供容错 secondary Script parse：解码 `\"`/`\\`，保留
parser recovery，并返回 decoded UTF-8 byte boundary 到原 quoted token 的单调 source map。
analysis 可以组合多层 map，把嵌套 quoted Script 的诊断、补全、hover 和 navigation/rename range 映回主文档；该 API 不会
自行判断普通 prose 是否是 Script，也不会改变主 CST 文法。

## Localisation CST

Localisation parser 按行处理：

- `l_english:` 这类语言头是 `LanguageHeader`；
- `key:version "value"` 产生 `LocalisationEntry`、key、version 和字符串节点；
- 无引号值使用 `UnquotedValue`，行尾 `#` 后内容是 comment；
- 缺少 key、version、value 或 closing quote 会生成稳定的 syntax error 和 `Error` 节点；
- BOM、原始 range 和 source text 都保留。

Localisation value 的内容不在 parser 层解释为 localisation reference；该判断由 HIR 和 analysis 完成。

## ParsedFile 与错误恢复

```text
ParsedFile
  format       FileFormat
  source       原始 UTF-8 文本
  root         CstNode
  tokens       source order 的 SyntaxToken
  errors       source order 的 SyntaxError
  revision     解析 revision
```

节点只保存 kind、range 和 children，文本通过 `ParsedFile::text` 从 source 读取。行索引属于 `pdx-text` 和 engine 的 `DocumentSnapshot`，不是 `ParsedFile` 字段。syntax error 不阻止 parser 返回可遍历的局部树；HIR 再将无法识别的结构保留为 `UnknownConstruct`。

## 编辑与格式化

`ParsedFile::apply_edit` 接受 UTF-8 byte range 或 full-document replacement。range 越界、切分无效 UTF-8 或无法应用时返回 `EditError`；成功后当前实现执行 full reparse，并递增 revision。编辑结果必须与同一 source 的直接 `parse` 具有相同的 CST、token 和 error 可观察结果。

`pdx-parser::format::format` 只对无 syntax error 的文件工作。formatter 生成 canonical text 后重新解析，并校验结构等价、token 安全性和幂等性；校验失败返回零 edits 及 `UnsafeSyntax` 或 `SafetyValidationFailed`，不会猜测性改写源码。

## 当前限制

当前没有纯 Rust incremental parser，编辑更新仍是 full reparse；没有 CSV parser 或 CSV formatter；parser 不负责规则覆盖率、跨文件 resolution、scope evaluator 或 LSP 协议转换。
