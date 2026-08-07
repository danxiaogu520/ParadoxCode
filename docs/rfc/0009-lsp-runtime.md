# RFC 0009：LSP Runtime

- 状态：Current
- 适用版本：EU4 v0.1

## 职责

`pdx-lsp` 是协议 adapter，不实现 parser、规则解释或分析算法。它负责 stdio JSON-RPC
transport、initialize/shutdown/exit 生命周期、client capability negotiation、文档版本、
取消、URI/position/TextEdit 转换、workspace/config 事件转换，以及 diagnostics 发布。
分析请求统一调用 `pdx-analysis` 的 immutable snapshot 查询。

## 进程与规则入口

正式 `pdx-ls` 进程无规则参数。它接受 `--version`/`-V`，其他 process argument（包括
`--rules`）均返回错误；不存在外部规则文件、规则路径或规则下载入口。启动 composition
root 从 `pdx-game::eu4` 取得内嵌第一方 JSON source，使用用户本地 SQLite artifact，用户路径
不可用时才使用不持久化的临时 artifact。规则 authority 与 cache 校验见 [RFC 0014](0014-first-party-rule-source.md)。

`initialize` 的当前 options 只描述 workspace：

```json
{
  "projectConfig": ".pdx/project.toml",
  "modDirectory": "mod",
  "dependencies": [
    { "id": "dependency-id", "path": "dependencies/dependency-id" }
  ],
  "vanillaIndexCache": ".pdx/cache/vanilla.pdxindex"
}
```

相对路径以 client workspace root 为基准；inline 字段覆盖 TOML。Dependency 按给定顺序形成
只读 roots，Current Mod 可写；空路径、重复 ID、root 重叠或嵌套会报 `INVALID_PARAMS`。
没有配置时使用打开的 workspace 作为 Current Mod，不猜测路径来替代配置。

initialize 的 Current Mod/Dependency scan 在 worker 中完成并在成功后提交。显式 Vanilla cache
在 initialize response 后后台加载；规则 hash 不匹配时可按 cache 记录的 Vanilla source 重建，
失败则以 warning 继续使用已加载的旧 cache。没有显式 cache 时，官方入口可按用户配置执行
一次后台 EU4 Vanilla discovery/index；进度通过 `window/workDoneProgress` 或提示消息报告。

## 生命周期与传输

状态为 `Uninitialized`、`Initializing`、`Initialized`、`ShuttingDown`、`Exited`。initialize
前只接受 initialize、exit 和取消；shutdown 后只等待 exit；未先 shutdown 的 exit 使用非零
退出结果。MVP 只支持 stdio，stdout 专用于协议。framing 限制 header 总长度 8 KiB、单条
JSON-RPC message 32 MiB，并拒绝重复 `Content-Length`；workspace/overlay 单文件上限为
16 MiB，结果列表也有固定上限。

文档同步使用 incremental sync：`didOpen` 创建 overlay，`didChange` 按顺序应用 changes 并
拒绝陈旧 version，`didClose` 移除 overlay，`didSave` 可触发磁盘 metadata refresh。内部 range
使用 UTF-8 byte offset，只在 LSP 边界经 `LineIndex` 转换为 UTF-16 line/character。

## 已声明并实现的能力

initialize result 当前声明并由 dispatch 实现：

- completion（含 `completionItem/resolve`）
- hover
- definition
- references
- document symbol 与 workspace symbol
- `prepareRename` 与 rename
- document formatting

同时声明 incremental text sync。Semantic Tokens、Code Action 和 Workspace Diagnostics 当前
不声明；range formatting 也不作为 LSP capability 提供。

## 并发、诊断与文件变化

event loop 是 mutable `AnalysisHost` 的唯一提交者；语言请求在捕获单一 `AnalysisSnapshot` 后
进入 worker，不持有 host lock。编辑先提交最新文本和 version，再异步 parse/lower；结果必须同时
匹配当前 document version、文本和路径，否则丢弃。普通请求、parse、workspace scan 和诊断都
支持协作式取消；`$/cancelRequest` 会取消对应任务。

syntax/semantic diagnostics 使用 push publish；semantic diagnostics 默认 debounce 200 ms，
提交前检查版本，过期结果不发布。单次 publish 最多 1000 项，completion 和 workspace symbol
结果也有上限。Current Mod/overlay 的诊断会更新，Dependency 和 Vanilla 主要用于索引与查询。

当 client 支持动态 watched-file registration 时，server 为 Current Mod 和 Dependency 注册
create/change/delete watcher；磁盘变化不会覆盖打开文档的 overlay。Vanilla 不注册持续 watcher，
其 cache 只在显式刷新或规则 hash 不匹配的启动流程中处理。

## 当前限制

- transport 只有 stdio；没有 TCP 或其他长连接服务端。
- 当前正式组合入口固定为 EU4 profile，不接受运行时游戏切换或外部规则输入。
