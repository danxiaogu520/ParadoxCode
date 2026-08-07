# `pdx-parser`

## 模块职责

`pdx-parser` 是 Paradox 文本的 loss-aware 语法边界，依赖 `pdx-text` 和 `unicode-width`。
当前入口覆盖两种前端：Paradox key/value Script，以及 EU4 Localisation YAML-like 文本。
解析结果保留源文本、token、带 UTF-8 范围的 CST 和可稳定映射的语法错误。

## 主要公开类型与入口

- `FileFormat::{Script, Localisation}`：选择前端。
- `parse(format, source) -> ParsedFile`：从 `&str` 创建 revision 为 `0` 的解析句柄。
- `ParsedFile::{format, source, root, tokens, text, errors, revision}`：读取前端、原文、CST 根、token、范围文本、错误和 revision。
- `SyntaxEdit::full`、`SyntaxEdit::ranged`：构造整文档或 `TextRange` 替换。
- `ParsedFile::apply_edit`：应用编辑并返回新的解析结果；非法范围返回 `EditError::InvalidRange`。
- `CstKind`、`CstNode`、`SyntaxToken`、`TokenKind`：由 crate 根重新导出的 CST/token 类型。
- `SyntaxError`、`SyntaxErrorKind`：包含 `kind`、`range`、`message`；`code()` 返回稳定诊断码。
- `pdx_parser::format::{format, FormatResult, TextEdit, FormatSkipReason}`：执行固定风格的安全格式化。

## 输入、输出与数据流

1. 调用方用 `FileFormat` 和源文本调用 `parse`。
2. 对应 Script 或 Localisation 前端生成有范围的 `CstNode` 树、源顺序 token 和源顺序 `SyntaxError`。
3. `ParsedFile::text(range)` 从句柄保留的原文切出节点或 token 文本；CST 节点本身只保存 kind、range 和 children。
4. `apply_edit` 生成替换后的文本并重新解析，revision 递增；结果应与对新文本完整 `parse` 的 CST、token 和错误一致。
5. `format` 只在语法安全且校验通过时输出非重叠 `TextEdit`；否则返回零 edits 和明确的 skip reason。

## 明确不负责的边界

- 不解释 EU4 规则、scope、command、symbol 或动态 workspace 成员。
- 不读取磁盘、不扫描 Mod/Vanilla、不管理 overlay/index，也不依赖 LSP 类型。
- 不把语法解析结果直接变成 HIR、诊断查询或编辑器协议响应。
- `FileFormat` 当前不包含 CSV 或其他资源格式；文件分类由上层 profile/rules 选择。

## 当前限制

- `apply_edit` 的公开形状支持编辑，但当前实现对编辑后的完整文本重新解析，并非 subtree reuse。
- formatter 是固定策略：Script 使用 tabs、LF 和递归 block 布局；不提供配置对象。
- 含语法错误的文件仍会产生可遍历 CST，但 formatter 会以 `UnsafeSyntax` 跳过重写。
- 范围统一使用 UTF-8 字节偏移；编辑器侧的 UTF-16 转换由 `pdx-text::LineIndex` 负责。

## 验证命令

```text
cargo test -p pdx-parser
```
