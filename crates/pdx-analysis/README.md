# `pdx-analysis`

## 模块职责

`pdx-analysis` 是 editor-neutral 的查询 facade：从 `pdx-engine::AnalysisSnapshot` 读取已解析的 document、HIR、`WorkspaceIndex`、rules/profile，生成诊断、补全、悬停、导航、符号和安全重命名 DTO。它不拥有可变 workspace，也不直接读磁盘；`pdx-lsp` 只负责把这些 DTO 转成协议值。

## 内部布局

`src/lib.rs` 只保留模块声明、稳定的公开 re-export 和 crate facade。实现按查询职责拆分：

- `types.rs`：公共 DTO、diagnostic/completion/hover/rename 类型和取消 token；
- `support.rs`：解析输入、CST/HIR 输入适配和共享文本辅助；
- `semantic.rs`：规则上下文、scope transition、semantic matcher 和诊断共享逻辑；
- `resolution.rs`：workspace semantic 收集、候选和 symbol resolution；
- `diagnostics.rs`、`completion/`、`hover.rs`、`navigation.rs`：各类 editor-neutral query；
- `tests/`：按 diagnostics、completion、semantic/scope、hover、navigation、rename 分组的行为测试。

`completion/` 内部再分为 context、candidate generation 和通用 completion support；拆分只改变文件组织，不改变 `pdx_analysis::*` 的调用路径。

## 核心公开类型与入口

- `CancellationToken` 是可 clone 的共享取消标记；`Cancelled` 是查询提前停止的 marker。
- 诊断 DTO 为 `DiagnosticCode`、`Diagnostic`；code 当前包括 `Syntax`、`UnknownKey`、`UnknownSymbol`、`AmbiguousSymbol`、`UnknownScope`、`InvalidValue`、`Cardinality`、`WrongScope`，并由 `as_str/severity` 提供稳定 wire code 和严重度。
- 导航基础 DTO 为 `Location`、`Symbol`、`ReferenceInfo`；`WorkspaceSymbol` 是 `Symbol` 的类型别名。`Location` 可指 open `DocumentId`、indexed `SourceFileId` 或 logical path，不携带 LSP URI。
- 补全 DTO 为 `CompletionItem`、`CompletionKind`、`CompletionResult`；item 包含 replacement range、insert text、sort score、deprecated 和可选 `resolve_data`。`completion_resolve` 可按 rule token 补回 documentation。
- Hover DTO 是 `Hover { contents, range }`；重命名 DTO 为 `PrepareRenameResult`、`WorkspaceTextEdit`、`WorkspaceEditPlan`，拒绝原因由 `RenameError`/`RenameFailure` 表示。
- 基础入口是 `analyze`、`analyze_document`、`analyze_source_file`、`diagnostics`、`complete`/`completion`、`hover`、`definition`、`references`、`document_symbols`、`workspace_symbols`、`prepare_rename`、`rename`。

## Snapshot queries 与取消

每个 query 接受 `&AnalysisSnapshot`，位置使用 `pdx-text::TextSize` 的 UTF-8 byte offset。诊断、补全、hover、definition、references、prepare-rename、rename、document-symbol 和 workspace-symbol 都有对应的 `_with_cancellation` 入口，返回 `Result<DTO, Cancelled>`；查询会在遍历 workspace/index/rule data 时检查 token。无后缀的便利函数使用新建 token，适合不会主动取消的调用方。

```text
AnalysisSnapshot
  -> document/source-file input + ParsedFile/HirFile
  -> semantic rules/profile + WorkspaceIndex
  -> editor-neutral DTO
  -> pdx-lsp protocol conversion
```

`analyze` 汇总当前所有 open overlay 的 diagnostics；`analyze_document` 可分析 document candidate，`analyze_source_file` 面向 indexed disk file。`CompletionResult`、`FileAnalysis`、`WorkspaceEditPlan` 都记录生成它们的 snapshot `revision`，供上层做 freshness 判断。

## 查询语义

- completion 同时覆盖 semantic key/value、localisation key/value 和 workspace symbol；`CompletionKind` 区分 `Key`、`Value`、`Symbol`、`Localisation`。
- hover 可展示 rule/property/value 文档、symbol 信息、localisation preview 和 scripted-definition parameter 信息；不会要求读取另一文件的完整 source。
- definition 对 unresolved 或 ambiguous symbol 返回空结果而不随机选择；references 可由 `include_declaration` 控制定義是否包含。
- rename 先 prepare，再检查单一 PDX identifier、唯一解析、可写位置和命名冲突；edit 只针对 open overlay 或可写 Current Mod 位置，并按安全应用顺序返回。

## 明确不负责的边界与当前限制

本 crate 不解析 JSON-RPC、不声明 capability、不做 UTF-16 `Position`/URI 转换、不处理 client version event，也不执行 game install discovery。查询只认识 snapshot 已有的文本/HIR/index；不存在或不支持的输入通常返回 `None`、空 vector 或空 completion，而不是访问磁盘补齐数据。批量 push diagnostics 明确排除 disk-only 文件，disk 文件仍可参与导航和 workspace-symbol；未唯一解析的目标不会产生导航或 rename edit。

## 验证命令

```text
cargo test -p pdx-analysis
cargo test -p pdx-analysis --doc
```
