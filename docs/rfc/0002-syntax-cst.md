# RFC 0002：语法、CST 与编辑更新

- 状态：Accepted
- MVP：EU4 v0.1

## 目标

在用户持续输入且文件暂时不合法时，仍提供稳定结构、精确 range、注释保留和安全更新。

## 文本前端

MVP 保留一个 EU4 Tree-sitter grammar 资产，供 Zed 编辑器侧使用：

- `tree-sitter-eu4`

EU4 本地化文件由通用 YAML LSP 处理，本项目不提供编辑器端语法高亮。可复用的 Script 和 localisation frontend 属于 syntax 层；具体文件分类由游戏 profile 负责。

规则维护不复用 Script 或任何外部规则语言。`pdx-rulec` 只解析 RFC 0015 定义的严格第一方 JSON source；`.cwt` 不是项目支持的语言或输入。

## Script CST

建议命名节点：

```text
document
item
property
key
operator
value
block
bare_value
quoted_string
header_block
parameter_block
parameter_condition
comment
ERROR
```

语义形状：

```text
Property := Key Operator Value
Value    := Scalar | QuotedString | Block | HeaderBlock
Block    := "{" Item* "}"
Item     := Property | Value | ParameterBlock
```

`Block` 不在 syntax 层分类为 object 或 array，因为 Clausewitz 允许 mixed container。重复 key 合法地保留为多个 property。

## Operator

grammar 从第一版支持：

```text
=  <  <=  >  >=  !=  ==  ?=
```

operator 的语义是否合法由规则层判断。例如同一个 key 可能只允许 `=`，也可能允许 comparison。

## Scalar 分类

lexer 只区分 quoted 与 unquoted。以下分类延迟到规则感知阶段：

- bool
- integer / decimal
- date
- enum
- symbol
- localisation key
- scope expression
- dynamic value

这样可以保留 `1.2.3`、`yes`、`1444.11.11` 等上下文相关文本的原始含义。

## 特殊 EU4 语法

grammar corpus 必须包含：

- header block：`rgb { 1 2 3 }`
- conditional parameter：`[[name] ... ]`
- undefined parameter：`[[!name] ... ]`
- `$PARAM$` scalar 内参数引用
- `@variable` 与包含 `:`、`.`、`|` 的 bare scalar
- 注释紧邻 token
- escaped quote
- UTF-8 BOM
- 未闭合 block/string/parameter block

## ParsedFile

```text
ParsedFile
  source: Arc<SourceText>
  cst: CstNode
  line_index: LineIndex
  syntax_errors: Vec<SyntaxError>
  revision: FileRevision
```

typed CST node 只保存 parser 生成的 kind/range/children 和对 `ParsedFile` source 的受控引用，不复制字符串。

## Trivia

source text 是唯一真相。空白不要求成为命名节点，但 formatter 通过相邻 token byte range 读取 trivia。注释使用命名节点，以便高亮、格式化和关联文档。

禁止从只包含命名节点的 AST 重新打印整个文件。

## 编辑更新

每个 LSP change 当前执行受 revision 保护的 Rust full reparse：

1. 使用旧 `LineIndex` 将 UTF-16 range 转成 byte range。
2. 更新 source text。
3. 使用 Rust parser 重新构建 typed CST、tokens 和 syntax errors。
4. 重建 `LineIndex`。
5. 生成新的 file revision。

编辑更新必须与相同文本的直接 full parse 产生相同可观察结果。未来可以在不改变这一契约的
前提下加入纯 Rust 增量 parser；本项目不把 Tree-sitter C runtime 作为核心依赖。

如果 change range 无效或版本断裂，拒绝该变化并请求/等待 full text resync，不能猜测 offset。

## 错误恢复

- 每个 Rust parser recovery/error node 产生 syntax diagnostic candidate。
- 相邻错误合并，避免一次未闭合 `{` 产生数十条消息。
- HIR lowering 跳过无可识别边界的错误节点，但保留可识别的同级节点。
- completion 可根据 cursor 周围 token 和父节点恢复 context。

## Jomini 的角色

Jomini 作为 grammar 行为和 fuzz case 的参考，不作为 CST dependency。原因是编辑器需要 comments、trivia、source ranges 和 error recovery，而 Jomini tape 主要优化完整输入的高吞吐读取。

未来 CLI 如需批量读取大型已完成文件，可以单独评估 Jomini，不改变 LSP syntax model。

## 资源文件

图片、音频、字体和其他二进制资源不建立 CST。Workspace 只记录 logical path、source root、文件 metadata 和引用目标；格式内容解析不属于 MVP。
