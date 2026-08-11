# `pdx-engine`

## 模块职责

`pdx-engine` 是规则感知但不绑定编辑器协议的语义引擎边界，负责 source roots、文件读取与资源限制、Script/Localisation 的 parse/HIR 状态、逐文件索引 shard、overlay resolution，以及面向查询的不可变 snapshot。`src/hir/` 提供 `HirFile`、scope state、property、definition/reference 和 scripted parameter lowering，公开入口为 `hir::lower`、`lower_shared`、`lower_with_profile`、`lower_shared_with_profile`；`src/vanilla_cache/` 提供本地只读 Vanilla index artifact。

## 内部布局

`src/lib.rs` 是稳定 facade。workspace model、index、扫描、parse/lower pipeline、host 和 snapshot 分别位于
`model.rs`、`index.rs`、`scan.rs`、`pipeline.rs`、`host.rs` 和 `snapshot.rs`。

HIR 目录按 `model.rs`、`collector.rs`、`parameters.rs`、`scope.rs`、`semantics.rs` 拆分，
`hir/mod.rs` 保持 `pdx_engine::hir::*` 的公开入口。Vanilla cache 目录将 cache facade、SQLite
read/write、preview、稳定 scalar codec 和 macro template codec 分到 `mod.rs`、`read.rs`、`write.rs`、`preview.rs`、`codec.rs`、`template_codec.rs`。
测试按 index、documents、scan、Vanilla 和 HIR lowering 分组，避免测试组织重新聚合到 facade。

## 核心公开类型与入口

- `SourceRootId`、`SourceFileId`、`DocumentId` 是运行期稳定身份；`SourceRoot` 携带 `id`、`kind`、`path`、`order`、`writable`，`SourceRootKind` 为 `Vanilla`、`Dependency`、`CurrentMod`。
- `AnalysisHost::empty/new/with_profile` 创建可变状态 owner；`apply_change` 配置 root/workspace，`refresh_source_roots*` 扫描并原子替换文件状态，`apply_disk_file_changes*` 做 Current Mod/Dependency 的定向更新。
- `AnalysisSnapshot` 由 `AnalysisHost::snapshot` 产生，提供 `revision`、rules/profile、roots、documents、`source_files`、`file_state`、`index`、`scan_report`、`resolve` 和 Vanilla localisation preview。
- `FileState` 保留一个文件版本的 source、`ParsedSource`、可选 HIR 与 `FileIndexShard`；`FileIndexShard` 原子携带 definitions、references、scripted macro signature/template summary 和 syntax error count。
- `WorkspaceIndex` 由 shards 建立，支持 `definitions`、`active_definition`、`references`、位置缓存以及 `replace_shard/remove_shard`。shadowed definitions 会保留，但只有 active definition 用于解析。
- `VanillaIndexCache::from_snapshot/load/load_cancellable/save` 操作 schema `5` 的 SQLite cache；`metadata`、`source_root`、`source_files`、`index`、`localisation_previews` 是只读访问器。cache 持久化 source-independent macro template IR，不保存源码、CST 或完整 HIR。

## 数据流与并发边界

```text
SourceRoot -> scan/profile classification -> FileState
          -> FileIndexShard -> WorkspaceIndex
Overlay + disk candidates -> AnalysisSnapshot -> pdx-analysis queries
```

`AnalysisHost` 是可变状态 owner；分析请求只读取克隆的 `AnalysisSnapshot`，不应在查询期间修改 host。扫描和 cache 加载接受共享的 `WorkspaceScanToken`，可在发现、读取、建 index、SQLite 查询期间协作取消。overlay 可先由 `stage_open_document/stage_document_text` 写入未解析文本，worker 在 snapshot 上调用 `prepare_document`，再由 `commit_prepared_document` 以精确的 URI、version、text、path 检查后提交；过期结果会被拒绝。

同一 logical path 的优先级当前为 overlay `20000`、Current Mod `10000 + order`、Dependency `1000 + order`、Vanilla `0`。`AnalysisSnapshot::resolve` 保留低优先级候选；overlay 存在时只激活 overlay，规则 policy 为 `Merge` 时可同时激活磁盘候选，否则激活最高优先级候选。

## 明确不负责的边界

本 crate 不处理 JSON-RPC/LSP 生命周期、LSP URI/协议 DTO 或编辑器 capability；这些由 `pdx-lsp` 转换。它也不做游戏安装发现、用户配置或外部规则输入；游戏差异来自调用方传入的 `RuleSet` 与 `GameProfile`。`DocumentId` 只保存客户端 URI 字符串，URI 到文件系统路径的协议转换不在这里完成。

## 当前限制

- `ParsedSource` 当前只有 Script/Localisation 的 `Text` 变体；规则分类为 opaque asset 的文件会参与路径/overlay resolution，但不读取 source、不产生 parse/HIR。
- root 扫描默认限制为深度 64、100,000 个文件、单文件 16 MiB，并跳过 symbolic link；profile whitelist、非法路径、文件身份碰撞和取消都会显式报告。
- Vanilla cache 必须从唯一的 `Vanilla` root、保留 ID `0` 的专用 snapshot 建立；cache 不保存 Vanilla source text，只保存 metadata、shards（含有界 macro template IR）、UTF-16 位置和以 240 字符为截断阈值、可能追加省略号的 localisation preview。
- `install_vanilla_cache` 校验 game/root/whitelist/identity collision，但刻意不要求 `rule_hash` 相同；当前 cache API 本身不会因 hash 不同自动重建，启动时的重建策略由 `pdx-lsp` 负责。

## 验证命令

```text
cargo test -p pdx-engine
cargo test -p pdx-engine --doc
```
