# RFC 0008：安全格式化

- 状态：Accepted
- MVP：EU4 v0.1

> 实现修订（2026-07-29）：formatter 采用固定、不可配置的规范布局。Script block
> 根据 CST 内容递归选择单行或展开布局，多行 quoted script 递归格式化，block header
> comment、LF、末尾换行和零布局空行均有唯一输出。LSP 的 `tabSize`/`insertSpaces`
> 仍按协议接收但不影响结果。恢复语法、CSV 和不支持格式返回空 edits；range
> formatting 仍属于非目标。crate/LSP 回归、工作区测试和 formatter fuzz smoke 已通过。

## 目标

提供规范化优先、幂等、注释安全的全文格式化。除已识别 quoted script 的内部布局外，
格式化不改变 property 顺序、operator、引号或 scalar spelling。

## 非目标

- AST 重写或 lint autofix
- key 排序
- 等号列对齐
- 自动添加/删除引号
- 数字、日期、颜色正规化
- 用户可配置的格式风格
- range formatting
- localisation 文本内容重排

## 输入

formatter 读取：

- 原始 source text
- typed CST
- token ranges 与 trivia

不读取 HIR 或 workspace index，因此同一语法文本在任何游戏上下文中格式化结果一致。

## 固定规范风格

```text
indent_style = tabs
tab_width = 4 display columns
line_width = 120 display columns
line_ending = LF
final_newline = exactly one for non-empty files
space_around_operator = true
block_open = same_line
layout_blank_lines = 0
```

LSP 必须接收标准 `FormattingOptions`，但 formatter 忽略 `tabSize` 和 `insertSpaces`。
不可拆分的普通 scalar、comment 和 opaque quoted scalar 可以超过 120 列。

## Script block

- 空 block 的唯一形式是 `{ }`。
- 只含 scalar 的 block 永远保持单行，花括号内侧各有一个空格。
- 只有一个 direct property 的 block 在其递归内容可单行且整行不超过 120 个显示列时
  保持单行。
- 两个及以上 direct property、mixed item、comment 或必须展开的后代会使 block 展开。
- 单行判定从最内层向外进行；嵌套本身不强制展开。
- 展开 block 的 `{` 与所属 property/header 同行，item 各占一行，`}` 独占一行。
- 无 comment 的 parameter block 强制单行且 delimiter 内侧无额外空格，不受行宽限制；
  comment 使 parameter block 展开。
- 120 列使用 Unicode 显示宽度，tab stop 固定为 4。

formatter 删除所有布局空行。它不拆分普通 scalar、comment 或 opaque quoted scalar。

## 注释

- 行尾 comment 保持与前一个语义 item 同行，间隔恰好一个空格。
- 独立 comment 保持在相邻 items 之间的相对位置。
- 连续 comment block 不拆分。
- formatter 不尝试把 comment 解析成文档注释或规则元数据。
- block 内第一个 leading comment 成为 block header comment，移动到 opening `{` 后并间隔
  一个空格；后续 comments 正常缩进。
- comment-only block 展开，closing delimiter 不与 comment 同行。

## 算法

使用 token/trivia 流，而不是从 HIR pretty-print：

1. 验证 CST 是否可安全格式化。
2. 从 typed CST 构造递归布局并从内向外决定 block 的单行资格。
3. 顺序输出 token 与 comment；除 quoted script 外不从语义模型重新生成 token。
4. 再次 parse 输出，验证结构、token/comment 顺序和 quoted-script 内部 token 顺序。
5. 第二次格式化必须无变化。
6. 对原始与规范文本的 token/gap 生成最小、互不重叠的 edits；quoted-script edit
   必须限制在原 quoted token 范围内。

## ERROR node 策略

MVP 若存在跨越 property/block 边界的 ERROR 或 missing delimiter，则拒绝全文格式化并返回明确原因。局部无害 error 的安全判定容易出错，推迟到有充分 corpus 后。

拒绝格式化不是 LSP error；返回空 edits，并可记录用户可见日志。未来可以提供 code action 解释原因。

## Quoted script

Script 文件中原本含换行的 quoted string 是 quoted-script candidate。formatter 解码其
quoted payload；只有 payload 能完整解析且至少含一个 property、header block 或 parameter
block 时，才递归使用本 RFC 的布局规则。单 property payload 可以收为单行：

```pdx
first_limit = "has_disaster = example_disaster"
```

需要展开时，opening quote 留在 property 行，payload 增加一级 tab，closing quote 独占一行：

```pdx
first_effect = "
	first_property = yes
	second_property = no
"
```

无法完整解释为脚本，或只含空白、comment 或 bare scalar 的 candidate 退化为 opaque
scalar，内部 bytes 保持不变。Localisation quoted value 永远 opaque。opaque scalar 内部
的空行和 CRLF 不属于布局 trivia，不做修改。

## Localisation

EU4 localisation formatter 独立实现，MVP 只处理：

- 语言头和 entry 的基础缩进
- 删除所有布局空行
- 行尾空白

绝不修改引号内部内容、颜色/格式标记、`$KEY$` 或转义。若 localisation parser 存在错误，返回空 edits。

## CSV 与资源

MVP 不对 CSV 做全文格式化，因为 delimiter、空列、quoting 和行顺序可能具有类别特定语义；CSV 仍可获得 parser、索引和诊断。二进制/媒体资源不提供 formatting。

## 安全属性测试

对每个合法 corpus 验证：

1. `format(format(x)) == format(x)`
2. 除已识别 quoted script 外，format 前后非 trivia token 序列完全相同
3. comment 文本和顺序完全相同
4. quoted script 内部 parse 后的 token/comment 顺序完全相同
5. parse 后无新增 ERROR
6. UTF-8 有效性保持

Fuzz target 对随机可解析输入验证以上属性并限制输出增长比例，防止病态输入造成内存放大。
