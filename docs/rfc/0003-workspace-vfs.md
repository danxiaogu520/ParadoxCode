# RFC 0003：工作区、VFS 与覆盖解析

- 状态：Accepted
- MVP：EU4 v0.1

## 目标

同时分析 Vanilla、依赖 Mod、当前 Mod和未保存文档，并对每个 definition/reference 给出确定的来源解析。EU4 DLC 不形成 source root。

## 基础模型

```text
SourceRoot
  id
  kind: Vanilla | Dependency | CurrentMod
  physical_root
  order
  writable

SourceFile
  id
  root_id
  physical_path
  logical_path
  file_category

OpenDocumentOverlay
  uri
  backing_source_file
  version
  text
```

未保存文档不是新的 Mod source root，但它的文本 candidate 在当前 snapshot 中拥有最高有效优先级。它仍保留 backing source root 的 ownership/writability，用于 rename 和 edit 权限；新建未保存文件归入当前 Mod root。

## Source root 顺序

有效内容优先级从低到高：

```text
Vanilla < Dependency Mods（配置顺序）< Current Mod < Open Document Overlay
```

打开文档 overlay 覆盖自己的 backing candidate。

顺序必须显式保存在配置中，不能依赖目录名、绝对路径或遍历顺序。依赖 Mod 列表后出现者优先还是前出现者优先必须在配置 schema 中固定并在 UI 文档中说明；建议列表从低到高。

## 路径规范化

`LogicalPath` 使用 `/`、移除 `.` segment、拒绝逃逸 root 的 `..`。比较策略由 Eu4Rules metadata 指定，EU4 MVP 对 ASCII 路径大小写不敏感，但保留原始 spelling 用于显示。

不得通过 filesystem canonicalize 作为逻辑身份，因为符号链接、未存在的新文件和 Windows 大小写会导致不稳定结果。

## 文件分类

文件 matcher 完全来自 Eu4Rules，包括 bootstrap CWT 中 `type[...]` 的 `path`、`path_strict`、`path_file`、`path_extension`、`type_per_file` 等规范化约束。Event、scripted effect、scripted trigger 和 localisation 是强制回归类别，但不是分类白名单。

Eu4Rules 为每个 logical path 返回：

```text
Parsed(category, language, candidate types)
Localisation(category, language)
StructuredText(category, CsvDialect)
AssetOnly(filepath categories)
SyntaxOnly(language)
Unsupported(reason)
```

PdxScript 可支持的脚本扩展（例如 `.txt`、`.gui`、`.gfx`、`.sfx`、`.asset`、`.map`）是否进入语义分析，由数据库 path/type matcher 决定。`.yml` 与受支持 `.csv` 使用各自 parser。贴图、音频、字体等文件只进入 asset filepath index。

没有数据库 semantic rule 但语法可解析的文档仍获得高亮、syntax diagnostics 和 formatting，标记为 `SyntaxOnly`，不能伪造 unknown-key 诊断。分类不相信 editor language id。

## 覆盖解析

覆盖分两步：

### 文件候选解析

按 normalized logical path 分组，根据 file category 的 `FileResolutionPolicy` 选择：

- `ReplaceByRelativePath`：只激活最高 root 的同路径文件。
- `Merge`：保留所有适用文件，由 symbol policy 决定定义关系。
- `ReplaceDirectory`：由 Mod descriptor 的 `replace_path` 使低层目录资源失活。

`replace_path` 属于 Mod 元数据，不代表存在 DLC source root。无法确定的策略保留全部候选并产生歧义信息，不能静默使用遍历顺序。

### Symbol 解析

活动文件中的同 kind、同 normalized name definition 按 `SymbolResolutionPolicy` 处理：

- `ReplaceBySymbol`
- `Merge`
- `Unique`

所有 symbol kind 的具体策略由 Eu4Rules 声明。解析结果为 `Resolved`、`Ambiguous` 或 `Unresolved`，不能任意选择第一项。

## 保留非活动资源

workspace index 保留被覆盖文件及其 definitions，但带 `Visibility::Shadowed`。用途：

- hover 显示覆盖来源
- diagnostics 解释为何 definition 不活动
- 调试 load order

普通 definition/reference 查询默认只使用 active view。

## 项目与本机配置

`.pdx/project.toml` 保存可提交的 EU4 项目身份：当前 Mod 相对根和从低到高排列的依赖 Mod 标识符。它不保存 game 字段、绝对机器路径，也不 pin `rule_hash`。

```toml
current_mod = "."
dependencies = ["shared-foundation", "content-extension"] # low -> high
```

用户级或编辑器本地配置负责把 Vanilla 和依赖标识符解析成绝对路径。这些本机设置不得成为 parser 或 HIR 的隐式全局输入。

## Vanilla 索引缓存

首次配置扩展时，用户选择 Vanilla 目录并建立本地持久化索引。之后：

- 正常启动直接查询缓存，不持续扫描 Vanilla。
- 只有用户在扩展设置/命令中显式触发“刷新 Vanilla 索引”才重建。
- 扩展升级和 `rule_hash` 变化不自动删除、迁移或重建缓存。
- cache metadata 记录创建时间、源目录指纹和当时的 `rule_hash`，只用于可观察性与手动决策。
- 缓存位于用户本机，不提交、不打包、不再分发 Vanilla 内容。

## 按文件增量更新

每个 `SourceFile` 拥有：

```text
ParsedFileCache
HirFileCache
FileIndexShard
```

更新算法：

1. 应用 disk 或 overlay text change。
2. 重建该文件 parse/HIR/shard。
3. 在新 snapshot 中原子替换旧 shard。
4. 若 logical path、definition names 或 visibility 变化，只重算相关 resolution buckets。
5. 重新诊断依赖这些 buckets 的打开文件。

## Snapshot

snapshot 包含 revision 一致的 VFS view、Eu4Rules 和 WorkspaceIndex。长查询必须可取消。旧 snapshot 可以完成正在进行的只读请求，但结果提交前 LSP 层可以按 document version 丢弃过期 diagnostics。

## 配置错误

- 当前 Mod root 缺失：workspace-level error。
- Vanilla root/cache 缺失：降级为不含 Vanilla 的分析并发出 warning。
- source roots 重叠：error，除非显式允许。
- 同一 physical file 被两个 root 收录：只保留显式优先 root并报告配置问题。
