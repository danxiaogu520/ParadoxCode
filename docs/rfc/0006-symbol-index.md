# RFC 0006：Symbol 与 Reference Index

- 状态：Partial

## 当前数据结构

当前索引不提供独立的跨请求 symbol、definition 或 reference ID。定义和引用由文件、range、kind/name 以及 analysis 的 `Location` 表示；`SourceFileId` 只标识 host 生命周期内的物理 source file，`DocumentId` 标识 editor URI。

```text
Definition
  kind, name, file_id, range, active

Reference
  kind, name, file_id, range

FileIndexShard
  file_id
  definitions: Vec<Definition>
  references: Vec<Reference>
  syntax_error_count
```

HIR 的 `HirDefinition` 另外保留 `selection_range`；analysis 将其转换为 document/workspace `Symbol`。普通索引定义的 selection range 会从对应文件的 HIR 查回，Vanilla cache 则使用保存的 UTF-16 位置。

## WorkspaceIndex

`WorkspaceIndex` 当前包含：

- `SourceFileId -> FileIndexShard`；
- `(kind, normalized_name) -> DefinitionPointer` 查找桶；
- 规则声明的 case-sensitive kind 集合；
- 无源码文件（例如 Vanilla）使用的 UTF-16 `PositionRange`。

references 保存在各 shard 中，并由 analysis 遍历；当前没有独立的 `(kind, name) -> references` 公共查找表。定义只在 HIR/profile 确认语义角色后进入 shard，不通过纯文本搜索制造 reference。

名称默认使用 ASCII lowercase；若对应 `SymbolDescriptor` 标记 `case_sensitive`，则保留原拼写。不能把所有名称统一做 Unicode lowercase。

## Shard replacement

刷新或单文件变化先生成新的 `FileIndexShard`，再替换同一 `SourceFileId` 的旧 shard。索引会移除旧定义对相关 kind/name bucket 的贡献、插入新贡献，并只重新解析受影响的 bucket；查询不会观察到半更新状态。完全相同的 kind/name/file/range 定义在 shard 提交前去重。

## Resolution

`ReplaceBySymbol` 只激活最高 source priority 的定义；同一最高 priority 的多个不同位置都保持 active，analysis 返回 ambiguous。`Merge` 和 `Unique` 在 index 中保留候选；analysis 在候选不唯一时同样返回 ambiguous，不依赖 map 遍历顺序。被覆盖定义保留在 index 中并标记 `active = false`，可用于说明来源，但默认 navigation 只使用 active resolution。

优先级固定为 `Vanilla < Dependency < Current Mod < Overlay`。overlay 文档在 analysis 的 semantic candidate 中拥有最高优先级，并隐藏同一路径的 backing disk candidate；它不把未保存文本伪装成另一个 source root。

## 查询与编辑

- `definition` 仅对唯一 resolution 返回 selection location；unresolved/ambiguous 返回空结果。
- `references` 先确定唯一 target，再返回解析到同一 location 的引用，可按参数包含 declaration。
- `document_symbols` 使用当前文档的 HIR；`workspace_symbols` 使用 active definitions，并支持 prefix/substring/fuzzy 排序。
- `hover` 可显示 definition 的 kind、logical path、root 和 shadowed/ambiguous 信息。
- rename 先检查唯一 target、合法名称、冲突和可写性，再生成 `WorkspaceEditPlan`；只产生 Current Mod 或 open overlay 的 edits，不直接写磁盘。

local parameter 是 scripted definition 内的局部事实，definition/references/rename 不经过 workspace 全局 symbol bucket。

## 持久化与限制

Current Mod 和 Dependency 的 shard 当前驻留内存；Vanilla 例外是用户本地 `VanillaIndexCache`，保存 shard、导航位置和有界 localisation preview，但不保存源码、CST 或 HIR。cache 的重建规则见 RFC 0003。

当前索引没有跨服务器稳定的 symbol identity，也没有完整的 reference 倒排索引；动态拼接、未被 HIR/rule 确认的文本不会成为确定 reference。更完整的语义依赖仍由 `pdx-analysis` 在 snapshot 上计算。
