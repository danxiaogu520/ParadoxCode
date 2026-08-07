# `pdx-text`

## 模块职责

`pdx-text` 提供与编辑器协议无关的文本、位置、范围和逻辑路径原语，是当前依赖图的底层 crate。
它处理 UTF-8 字节偏移、UTF-16 编辑器位置、行索引以及 workspace 相对逻辑路径。
`Cargo.toml` 当前没有运行时依赖；本模块不携带 EU4 语义。

## 主要公开类型与入口

- `TextSize`：`u32` 类型的 UTF-8 字节偏移。
- `TextRange`：半开字节范围；通过 `new`、`empty`、`start`、`end`、`len`、`is_empty` 操作。
- `Position`、`PositionRange`：零基 UTF-16 编辑器位置及其半开范围。
- `LineIndex::new(text)`：从完整源文本建立行起点索引。
- `LineIndex::offset`、`position`、`position_range`：在字节偏移和 UTF-16 位置之间转换；无效边界返回 `None`。
- `LineIndex::line_count`：返回索引中的行数。
- `LogicalPath::parse`、`as_str`：规范化并校验逻辑路径；`LogicalPath::new` 是兼容构造器。
- `LogicalPathError`：报告路径越出根目录或包含 NUL 的输入。

## 输入、输出与数据流

1. 调用方把源文本 `&str` 交给 `LineIndex::new`。
2. 编辑器位置经 `offset` 转为 UTF-8 字节偏移；语法节点的 `TextRange` 可经 `position_range` 转回 UTF-16 范围。
3. 文件系统相对路径经 `LogicalPath::parse` 统一为 `/` 分隔、无前导 `/` 的逻辑路径。
4. 转换只返回原语或 `Option`/`Result`，不会读取文件、修改文本或解析语法。

## 明确不负责的边界

- 不实现 LSP/编辑器协议，也不定义 workspace、overlay 或快照状态。
- 不解析 Paradox Script/Localisation，不生成 CST/HIR，不提供 formatter。
- 不识别 EU4 文件类别、scope、command、symbol 或规则数据库。
- 不执行磁盘扫描；`LogicalPath` 只是路径值，不等于已验证的文件系统路径。

## 当前限制

- `TextSize` 是 `u32`，单个文本和范围受 `u32` 字节偏移上限约束。
- `LineIndex` 依赖调用方继续提供与索引对应的完整文本；当前没有增量更新 API。
- `LineIndex::offset` 拒绝落在 UTF-8 code point 中间的 UTF-16 位置。
- 面向不可信输入应使用 `LogicalPath::parse`；`LogicalPath::new` 遇到非法路径会保留兼容性规范化结果。

## 验证命令

```text
cargo test -p pdx-text
```
