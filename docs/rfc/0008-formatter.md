# RFC 0008：安全格式化

- 状态：Accepted
- MVP：EU4 v0.1

> 实现进度（2026-07-20）：保守 PdxScript/localisation formatter、幂等与 trivia safety 回归、LSP `documentFormattingProvider`、typed params/edits 和 snapshot worker 路由已完成。客户端 `tabSize`/`insertSpaces` 会映射为 editor-neutral 选项；恢复语法、CSV 和不支持格式返回空 edits。range formatting 仍属于非目标。

## 目标

提供保守、幂等、注释安全的全文格式化。格式化只改变布局，不改变 property 顺序、operator、引号或 scalar spelling。

## 非目标

- AST 重写或 lint autofix
- key 排序
- 等号列对齐
- 自动添加/删除引号
- 数字、日期、颜色正规化
- range formatting
- localisation 文本内容重排

## 输入

formatter 读取：

- 原始 source text
- typed CST
- token ranges 与 trivia
- `FormatOptions`

不读取 HIR 或 workspace index，因此同一语法文本在任何游戏上下文中格式化结果一致。

## 默认风格

```text
indent_style = spaces
indent_width = 4
line_ending = preserve
final_newline = preserve
space_around_operator = true
block_open = same_line
max_blank_lines = 2
```

Zed/LSP 配置可以覆盖 indent width 与 tabs。MVP 不做自动行宽换行，因为 Paradox scalar 与 localisation 内容常包含语义敏感文本。

## 注释

- 行尾 comment 保持与前一个语义 item 同行。
- 独立 comment 保持在相邻 items 之间的相对位置。
- 连续 comment block 不拆分。
- formatter 不尝试把 comment 解析成文档注释或规则元数据。
- 空 block 内 comment 正常缩进但不移动出 block。

## 算法

使用 token/trivia 流，而不是从 HIR pretty-print：

1. 验证 CST 是否可安全格式化。
2. 顺序遍历 leaf tokens 与 comment nodes。
3. 根据 delimiter、item boundary 和 comment 计算目标 whitespace。
4. 只生成 whitespace ranges 的 edits；MVP 允许最终合并为一个 full-document edit，但测试内部变更必须仅涉及 trivia。
5. 再次 parse 输出并验证无新增 syntax error。
6. 第二次格式化必须无变化。

## ERROR node 策略

MVP 若存在跨越 property/block 边界的 ERROR 或 missing delimiter，则拒绝全文格式化并返回明确原因。局部无害 error 的安全判定容易出错，推迟到有充分 corpus 后。

拒绝格式化不是 LSP error；返回空 edits，并可记录用户可见日志。未来可以提供 code action 解释原因。

## Localisation

EU4 localisation formatter 独立实现，MVP 只处理：

- 语言头和 entry 的基础缩进
- entry 之间的空行上限
- 行尾空白

绝不修改引号内部内容、颜色/格式标记、`$KEY$` 或转义。若 localisation parser 存在错误，返回空 edits。

## CSV 与资源

MVP 不对 CSV 做全文格式化，因为 delimiter、空列、quoting 和行顺序可能具有类别特定语义；CSV 仍可获得 parser、索引和诊断。二进制/媒体资源不提供 formatting。

## 安全属性测试

对每个合法 corpus 验证：

1. `format(format(x)) == format(x)`
2. format 前后非 trivia token 序列完全相同
3. comment 文本和顺序完全相同
4. parse 后无新增 ERROR
5. UTF-8 有效性保持

Fuzz target 对随机可解析输入验证以上属性并限制输出增长比例，防止病态输入造成内存放大。
