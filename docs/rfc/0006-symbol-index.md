# RFC 0006：Symbol 与 Reference Index

- 状态：Accepted
- MVP：EU4 v0.1

## Symbol kinds

MVP 不固定 symbol kind 白名单。`SymbolKindId` 和 descriptor 由 Eu4Rules 产生，包括导入后的 type definitions、enums、variables、aliases、localisation、filepath/assets 以及可调用 effect/trigger definitions。

Event、ScriptedEffect、ScriptedTrigger 和 Localisation 是强制端到端基准，但不在核心中形成 Rust enum。显示名称、definition pattern、case policy 和按类别定义的 resolution policy 都来自数据库。

## Definition

```text
Definition
  id: DefinitionId
  kind: SymbolKindId
  name: InternedName
  original_name: TextRange
  full_range: TextRange
  source_file: SourceFileId
  logical_path: LogicalPath
  source_root: SourceRootId
  visibility: Active | Shadowed
  detail: DefinitionDetail
```

`original_name` 指向可 rename 的精确 token；`full_range` 用于 document symbol 和 peek UI。

## Reference

```text
Reference
  id: ReferenceId
  kind: SymbolKindId
  name: InternedName
  range: TextRange
  source_file: SourceFileId
  role: Read | Call | LocalisationUse
  resolution: Resolved | Ambiguous | Unresolved
```

index 不使用纯文本搜索制造 reference。只有 HIR 根据规则确认语义角色后才产生 reference。

## File shard

```text
FileIndexShard
  definitions
  references
  document_symbols
  name_dependencies
```

WorkspaceIndex 建立：

- `(kind, normalized_name) -> definitions`
- `(kind, normalized_name) -> references`
- `source_file -> shard`
- prefix/fuzzy workspace symbol search structure

同一个文件更新时先构建新 shard，成功后一次性替换。查询不得观察到半更新状态。

实现不得用“清空并重建整个 lookup map”伪装成 shard replacement。替换或删除一个 shard 时，只移除该 `SourceFileId` 在旧 definition/reference buckets 中的贡献、插入新贡献，并重新解析受影响的 `(kind, normalized_name)` buckets。未受影响的 buckets 不参与排序和 resolution。最高优先级并列时保留多个 active candidates，使查询返回 ambiguous；不得按容器或遍历顺序任取一个 winner。

## 名称规范化

每个 symbol descriptor 可声明 case policy。index key 存 normalized name，同时保留原始文本。不能统一对全部 symbol 做 Unicode lowercase；EU4 规则的显式策略由 fixture 验证。

## Definition resolution

查询 `(kind, name)`：

1. 读取所有 definition candidates。
2. 排除被文件覆盖策略隐藏的 candidates。
3. 应用 symbol kind resolution policy。
4. 返回 resolved、ambiguous 或 unresolved。

不得依赖 hash map iteration order。若最高优先级仍有多个且 Eu4Rules 未定义 tie-break，则必须 ambiguous。

## Go to Definition

- cursor 在 reference 上：返回 resolution 的 active definition。
- cursor 在 definition name 上：返回自身。
- ambiguous：返回所有合法位置（若客户端支持）或不跳转并在 hover 解释；不任意选择。
- shadowed definition：允许用户从该定义位置查看自身，但其引用解析仍指向 active definition。

## Find References

先确定 cursor 对应的 active definition identity，再读取同 kind/name references 并过滤 resolution。默认结果：

- 包含当前 Mod 和打开文档中的 resolved references。
- 可以显示 Vanilla/Dependency 的只读 references。
- `includeDeclaration` 按 LSP 参数决定。
- ambiguous/unresolved references 不算作确定引用，可在未来提供单独 UI。

## Rename

Rename 是 index 上的事务计划：

1. prepare 阶段确认 cursor 对应唯一、可写 definition。
2. 校验新名称格式。
3. 检查同 kind 新名称在目标优先级是否冲突。
4. 收集解析到该 definition 的可写 references。
5. 只收集当前 Mod及其未保存 overlay 中解析到该 definition 的 references；Vanilla/Dependency 永不进入 edit。
6. 生成按 URI 分组、range 逆序的 edits。
7. 不直接写文件，由 LSP client 应用 WorkspaceEdit。

MVP 不 rename：

- ambiguous symbol
- Vanilla/Dependency Mod definition
- 仅通过动态拼接形成的可能引用
- comment 或普通字符串中的文本匹配

## Workspace Symbol

只返回 definitions，默认 active 优先。结果包括 kind、name、container/logical path 和 location。查询使用前缀加轻量 fuzzy scoring；MVP 无需复杂全文搜索依赖。

## 索引持久化

当前 Mod和依赖 Mod索引在 MVP 中可只驻留内存。Vanilla 是明确例外：首次配置时生成本地持久缓存，之后由 LSP 启动后台加载；可读且 schema/game identity 有效的 cache 若记录 `rule_hash` 与当前内嵌 JSON source 编译出的规则不一致，则从 cache metadata 的 Vanilla 源目录以当前规则重建，并以 SQLite transaction 保存，提交后安装新 cache。重建成功发送 INFO；扫描、重建或事务保存失败则回退安装已加载的旧 cache 并发送 WARNING（含失败原因和两个 hash）。缓存除索引和导航位置外，可保存本地化 definition 的有限长度派生 Hover 预览，但不保存源码/CST/HIR。缺失、损坏或 schema/game identity 不兼容时 server 报告不可读并降级，不能静默扫描游戏目录；文件内容或 fingerprint 变化不自动刷新，显式用户刷新仍可重建。
