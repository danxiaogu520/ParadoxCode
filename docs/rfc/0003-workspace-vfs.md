# RFC 0003：工作区、VFS 与覆盖解析

- 状态：Current

## 当前对象

`pdx-engine` 以 `AnalysisHost` 保存可变 workspace，以 `AnalysisSnapshot` 提供不可变查询视图。主要对象为：

```text
SourceRoot       id, kind, path, order, writable
SourceFile       SourceFileId, root_id, physical_path, logical_path, category_id, resolution
DocumentSnapshot DocumentId, version, text, LineIndex, source, path, ParsedSource/HIR
FileState        file revision 的 source、ParsedSource、HIR、FileIndexShard
```

`SourceRootKind` 当前只有 `Vanilla`、`Dependency` 和 `CurrentMod`。`DocumentSource::Overlay` 表示未保存编辑文本；关闭文档后可恢复为 `Disk` candidate。overlay 不是新的 Mod root，并沿用其 backing path 的写权限。

## Root 优先级

内容优先级由 root kind 和显式 `order` 计算，低到高固定为：

```text
Vanilla < Dependency < Current Mod < Overlay
```

当前实现使用 Vanilla priority `0`、Dependency 的 `1000 + order`、Current Mod 的 `10000 + order`，overlay 使用 `20000`。同一 logical path 的低优先级 candidate 保留为 shadowed；overlay 存在时覆盖其 backing disk candidate。不可依赖目录名、绝对路径或 filesystem 遍历顺序决定优先级。

文件级策略来自 `FileCategory`：`ReplaceByRelativePath` 激活最高 candidate，`Merge` 保留适用 candidates，`ReplaceDirectory` 表示目录级替换。symbol 级冲突由规则中的 `SymbolResolutionPolicy` 处理，见 RFC 0006。

## 发现与分类

source-root 扫描先受 `GameProfile::scan_roots` 白名单限制，再应用可选的 `scan_extensions`。逻辑路径使用 `/`、拒绝 root 外的 `..`，并由 `RuleSet::classify` 选择最具体的匹配 category。

category 的 parser 只有：

- `Script`：解析为 Script CST/HIR；
- `Localisation`：解析为 Localisation CST/HIR；
- `Asset`：只登记 path，不读取为文本；
- `SyntaxOnly`：当前 engine 不创建 `ParsedSource` 或 HIR。

因此 CSV 当前是 `SyntaxOnly`/opaque resource，不提供 CSV 列解析或语义索引。显式打开文档可按规则或受支持扩展选择 Script/Localisation，但这不会增加 CSV parser。

默认扫描边界为递归深度 64、最多检查 100000 个普通文件、单文件最多 16 MiB、最多保留 256 条详细问题。EU4 profile 可在 UTF-8 失败时安全转换 Windows-1252；含非文本控制字符的结果跳过。单文件读取问题可恢复，root 不可读、超出全局限制或 ID 冲突会使刷新失败并保留旧 snapshot。

## 原子刷新与增量变化

`refresh_source_roots_*` 先发现、读取、parse/lower、构造 file states 和 shards，再一次性安装 `source_files`、`file_states`、`WorkspaceIndex` 与 scan report。`WorkspaceScanToken` 在发现、读取、解析和索引阶段协作取消；取消或 workspace-level error 不提交部分结果。

`apply_disk_file_changes_cancellable` 只处理 Current Mod 和 Dependency 的定向变化，按文件替换 state/shard；打开 overlay 保持有效。已安装的 Vanilla root 不注册这条磁盘更新路径，也不会在普通 workspace refresh 中重新扫描。

## Vanilla cache

`VanillaIndexCache` 当前 schema 为 `4`。cache 保存 `game_id`、`rule_hash`、source identity/fingerprint、文件元数据、semantic shards、scripted macro 的参数签名、definition/reference 的 UTF-16 位置以及有界的 localisation Hover preview；不保存 Vanilla 源码、CST 或 HIR。macro signature 只包含定义 kind/name/range 以及按首次出现排序的参数名和 required 标记，并且必须对应同一 shard 中的实际 definition。

LSP 后台加载可读 cache 时，若 metadata 的 `rule_hash` 与当前规则不同，会从 cache 记录的 Vanilla source root 自动重建，完成 SQLite transaction 后再安装；重建失败则保留并安装旧 cache，同时报告原因。cache 缺失、损坏或 game/schema 不兼容时不隐式扫描目录，继续提供不含 Vanilla 的分析。

Vanilla 源文件内容或 `source_fingerprint` 变化本身不会触发自动刷新。只有显式的 Vanilla refresh/setup 操作才重新读取源目录；规则 hash mismatch 是唯一的自动重建条件。

## 当前限制

workspace 只对当前代码支持的 file category 建立文本语义；opaque 资源只有路径级信息。Vanilla cache 是用户本地数据，不含源文本，也不能替代当前规则或成为规则 authority。
