# RFC 0008：安全格式化

- 状态：Current
- 适用版本：EU4 v0.1
- 实现入口：`pdx-parser::format::format`；LSP 通过 `textDocument/formatting` 调用

## 目标与边界

formatter 对已成功解析的 Script 或 localisation 文本生成规范化的全文 edits。它只读取原始
文本、typed CST、token range 和 trivia，不读取 HIR、规则库或 workspace index。因此相同的
语法文本不会因游戏语义上下文不同而产生不同布局。

`FormatResult` 包含按 UTF-8 byte range 表示的非重叠 `TextEdit`，以及可选的
`FormatSkipReason`。LSP 只把 edits 转换为协议的 UTF-16 range；`FormattingOptions` 会被接收，
但 `tabSize` 和 `insertSpaces` 不改变 formatter 的固定结果。

## 当前规范风格

- 使用 tab 缩进；tab 的显示宽度为 4 列。
- 使用 LF；非空文件恰好保留一个末尾换行，空文件保持为空。
- 删除布局空行；等号和其他 operator 两侧各保留一个空格。
- 使用 120 个 Unicode 显示列作为可展开的普通行宽。
- 保留 property 顺序、operator、引号、scalar spelling 和普通 token 文本。

Script block 的布局从内向外决定：空 block 输出 `{ }`；只含 scalar 的 block 保持单行；
单个可递归收缩且不超过行宽的 property block 保持单行；多个 property、mixed item、comment
或无法单行化的后代展开为每项一行。普通 scalar、comment 和 opaque quoted scalar 不会被
拆行；无 comment 的 parameter block 保持紧凑形式，含 comment 时展开。

行尾 comment 与前一语义 item 同行并间隔一个空格；独立 comment 的相对顺序保持不变。block
开头的 leading comment 会成为 opening delimiter 后的 header comment，后续 comment 按层级
缩进。

多行 quoted string 只有在解码后能完整解析为含 property、header block 或 parameter block
的 Script 时才作为 quoted script 递归格式化；否则整个 quoted value 保持 opaque。递归深度
受实现上限约束，普通 localisation value 永远不解释其内部内容。

Localisation 只规范语言头、entry 的基础空格、布局空行和行尾空白；引号内部文本、颜色/格式
标记、`$KEY$` 和转义保持不变。

## 安全校验

1. 输入 CST 存在任意 parser error 时返回空 edits 和 `UnsafeSyntax`。
2. 生成规范文本后重新 parse，检查错误、CST 形状、token kind/数量及 comment 顺序。
3. 已识别 quoted script 递归比较内部结构；普通 token 文本不得被改写。
4. 第二次格式化必须无 edits；否则返回 `SafetyValidationFailed`。
5. 通过校验后才生成最小、按源顺序排列且互不重叠的 edits。

## 当前限制

- 只提供全文 `textDocument/formatting`，没有 range formatting。
- 风格不可配置；不做 key 排序、列对齐、引号推断、数值/日期/颜色正规化或 lint autofix。
- CSV、二进制和媒体资源没有 formatter；未被 Script/localisation parser 识别的文档返回空
  edits。
