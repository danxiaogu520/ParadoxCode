# RFC 0009：LSP Runtime

- 状态：Accepted
- MVP：EU4 v0.1

> 实现进度（2026-07-20）：stdio reader 与 workspace event loop 已分离；initialize 的 source-root scan 在候选 host worker 中运行，目录/读取/parse/lower/index 全链路可取消且仅在成功后提交；编辑先 stage 最新文本/版本，parse/lower 在 snapshot worker 准备，并通过版本、文本、路径三重提交门拒绝旧结果；依赖语义的请求按消息顺序等待最新 parse。semantic diagnostics 使用 200ms debounce 与版本门，普通语言请求也在 snapshot worker 执行；`$/cancelRequest` 与过期 diagnostics 使用共享的 editor-neutral token，在 workspace semantic 合并、CWT 递归和主要结果遍历中协作式中止。当前声明能力覆盖的标准 params、initialize result/capabilities、diagnostics 与语言功能 response 已迁入 `lsp-types`，JSON-RPC framing 继续保持轻量自有实现。类型化 `initializationOptions`、项目 TOML、Current Mod、有序只读 Dependency roots 和持久化只读 Vanilla cache 已接入；watched-file 定向更新待完成。

## 边界

`pdx-lsp` 是协议 adapter，不拥有 parser、规则或 feature 算法。它负责：

- stdio JSON-RPC transport
- initialize/shutdown 生命周期
- client capability negotiation
- document version 与 cancellation
- URI、position、TextEdit 转换
- workspace/config 事件转成 `WorkspaceChange`
- publish diagnostics

## Transport

MVP 只支持 stdio，stdout 专用于协议。日志写 stderr 或 LSP logging channel，并默认不记录完整用户源码。

优先选用提供 protocol connection 与 types 的低层 Rust 库，避免 analysis API 被 async service trait 绑定。具体依赖版本在 Phase 0 spike 后锁定。

Server 必须通过进程参数获得规则文件：

```text
pdx-ls --rules <extension-install>/eu4.pdxrules
```

`pdx-ls` 不内嵌、下载、更新或搜索规则。启动时校验 SQLite schema 和 `rule_hash`；失败则保留 syntax-only 能力并报告 workspace error。

## Server 状态

```text
Uninitialized
Initialized
ShuttingDown
Exited
```

- initialize 前除 initialize/exit 外的请求返回 protocol error。
- shutdown 后停止接受语言请求，但等待 exit。
- exit without shutdown 使用非零退出码。
- transport EOF 时安全停止 worker。

## Initialize options

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

`projectConfig` 和其余路径的相对路径均以 client 打开的 workspace root 为基准；inline 字段逐字段覆盖 TOML。`dependencies` 按从低到高优先级解释，ID 大小写不敏感地唯一并产生稳定 root identity；目录必须存在且 root 之间不得相同或嵌套。Current Mod 可写，Dependency 与 Vanilla cache 只读。项目固定为 EU4，不接受 game id、game version 或 DLC source roots。规则路径由 process argument 提供，不在项目配置中 pin `rule_hash`。缺失或无效 Vanilla cache 通过 `window/showMessage` 警告并降级，不阻止 Current Mod/Dependency 启动；旧 `rule_hash` cache 仍加载并提示手动刷新。

客户端未提供配置时，server 仍提供 syntax features，并发布 workspace configuration warning；不得猜测 Steam 安装路径后静默扫描。Vanilla cache 缺失时不自动扫描游戏目录。

## 文档同步

声明 incremental sync，并实现：

- `didOpen`：创建 overlay，记录 version 和完整文本。
- `didChange`：按顺序应用 changes，拒绝陈旧 version。
- `didClose`：移除 overlay，恢复 backing disk candidate；新建未保存文件则从 workspace 移除。
- `didSave`：MVP 可接收并触发磁盘 metadata refresh，但不能假定一定发送。

内部所有 range 是 UTF-8 byte offset。只在 LSP 边界使用 `LineIndex` 转换 UTF-16 line/character。

## 请求与优先级

高优先级：

- completion
- hover
- definition
- prepare rename

普通优先级：

- references
- document symbol
- formatting
- rename

后台：

- workspace scan
- semantic diagnostics
- workspace symbol 大查询

每个请求捕获 snapshot 和 cancellation token。后台任务不得阻塞 event loop 应用 didChange。

## Capabilities

Phase 2 只声明同步能力。对应 feature 实现通过测试后逐步声明：

- completion provider
- hover provider
- definition provider
- references provider
- document/workspace symbol provider
- rename provider，含 prepare
- document formatting provider

未实现能力不能提前声明。Semantic Tokens 和 Code Action 在 v0.2 前不声明。

## Diagnostics

MVP 使用 push diagnostics：

- syntax diagnostics 低延迟发布。
- semantic diagnostics debounce 且可取消。
- 每次发布完整替换该 URI 的旧结果。
- close 后发布空 diagnostics，除非 client 生命周期不需要。
- worker 返回时校验 file revision，过期结果丢弃。
- 默认只发布当前 Mod与其未保存 overlay 的 diagnostics；Vanilla/Dependency 只参与索引和查询。

Workspace 中未打开文件的全量错误由 `pdx check` 提供；Workspace Diagnostics 是后续能力。

## 文件变化

Phase 4 接收 Current Mod和 Dependency 的 client watched-file notification，并保留 server-side scan fallback。来自 watcher 的磁盘变化不能覆盖打开文档 overlay，只更新 backing candidate。Vanilla 不注册持续 watcher；只有显式刷新操作才重建其缓存。

## Panic 隔离

- 单请求 panic 不应破坏协议输出；在 worker 边界捕获并记录内部错误。
- parser/analysis 对用户输入不得 panic，这是 fuzz/测试门禁。
- 发生内部错误时返回标准 internal error，不发布伪装成脚本错误的 diagnostic。

## 集成测试

使用内存 connection 驱动完整消息序列，至少覆盖：

- initialize -> open -> changes -> feature -> close -> shutdown -> exit
- UTF-16 position
- cancellation
- stale diagnostics
- invalid request before initialize
- change version disorder
- client 不支持 snippet/related information 时的 capability fallback
