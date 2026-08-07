# `pdx-lsp`

## 模块职责

`pdx-lsp` 是 JSON-RPC/LSP 适配层：负责 `Content-Length` framing、server lifecycle、document version/event、取消与后台 worker 编排、URI/UTF-16 position 转换，以及把 `pdx-analysis` DTO 序列化为协议结果。语义规则、HIR、symbol resolution 和 workspace index 仍属于 `pdx-rules`、`pdx-engine`、`pdx-analysis`。

## 内部布局

`src/lib.rs` 是 facade，稳定导出 server、URI 和 EU4 composition-root 入口。协议相关实现分布如下：

- `initialize.rs`、`workspace.rs`、`vanilla.rs`：初始化、source-root 配置和 Vanilla setup；
- `requests.rs`：`SnapshotRequestContext` 及 analysis DTO 到 LSP 结果的适配；
- `protocol.rs`、`text.rs`、`uri.rs`、`transport.rs`：错误/DTO、UTF-16 text change、URI 和 framing；
- `server.rs`：`LspServer` 公共状态和构造；`server/` 下的 `event_loop.rs`、`workers.rs`、
  `document_events.rs` 分别负责 event loop、后台任务和 document/message event；
- `tests/`：transport/lifecycle、workspace/Vanilla、request adapter 和 freshness 的真实 JSON-RPC 测试。

这些模块保持 `pdx_lsp::*` 的既有公开路径，不在 LSP 层复制 semantic rule 或 analysis 算法。

## 核心公开类型与入口

- `LspServer` 由 `try_new`（空规则、适合协议测试）或 `try_new_with_rules(InitializeOptions, RuleSet, GameProfile)` 创建；后者先校验 profile 的 `game_id`。
- `InitializeOptions` 当前是空 struct，不接受规则路径或 user override；`AutoVanillaConfiguration` 只携带 `GameInstallDescriptor` 与 `UserPaths`。
- `with_auto_vanilla` 启用一次用户级 Vanilla discovery；`state`、`options`、`snapshot`、`commit_diagnostics`、`diagnostics` 是可观察/测试入口。
- `run_stdio`、`run_stdio_with_profile`、`run_stdio_with_profile_and_auto_vanilla` 启动 stdio；`run_transport<R: Read + Send, W: Write>` 让测试直接驱动同一 JSON-RPC framing。错误统一为 `LspError`。
- URI 边界由 `uri_to_path`、`path_to_uri` 和 `UriError` 提供；只接受本地 `file://`（或 `localhost` authority），位置转换依赖 `LineIndex`。

## stdio 生命周期与并发边界

状态依次为 `Uninitialized -> Initializing -> Initialized -> ShuttingDown -> Exited`。`initialize` 在 worker 中解析 workspace roots、配置/扫描 source roots，并返回 capability；成功后接受 document events。`shutdown` 进入 draining，客户端随后必须发送 `exit`；未 shutdown 就 exit 返回 `ExitWithoutShutdown`，输入提前结束也不是 clean exit。

主 event loop 是 workspace host 的唯一 owner；reader、parse、diagnostic、snapshot request、disk-change 和 Vanilla worker 通过 channel 回传结果。worker 只使用 immutable snapshot 或 host clone；parse/diagnostics 提交前会核对当前 document version，过期结果丢弃，snapshot request 则固定读取捕获的 immutable snapshot。`$/cancelRequest` 会取消 initialize、analysis request、parse、diagnostics 或 source/cache work；诊断默认 debounce 200 ms。

## capabilities 与 protocol conversion

initialize 声明 incremental `didOpen/didChange/didClose` sync、completion（触发字符 `=`、空格、`:`，支持 resolve）、hover、definition、references、prepare rename/rename、document symbols、formatting 和 workspace symbols。若 client 支持 dynamic watched-file registration，server 会为 Current Mod/Dependency 注册 `workspace/didChangeWatchedFiles`；Vanilla 不走 watched-file 更新。

`SnapshotRequestContext` 映射 `textDocument/completion`、`completionItem/resolve`、`hover`、`definition`、`references`、`prepareRename`、`rename`、`documentSymbol`、`formatting`、`workspace/symbol`。`typed_params/typed_value` 用 serde 做参数校验；`TextRange` 与 LSP UTF-16 `Position` 通过 `LineIndex` 双向转换，`Location` 再映射为 document URI、物理 file URI 或 workspace-root path URI。无 snippet support 时会去掉 `$0/$1` 占位符。

当前协议结果上限为 512 个 completion、256 个 workspace symbols、1,000 个 published diagnostics；截断会通过 `isIncomplete` 或附加诊断说明。LSP 端只做转换和安全 freshness 检查，不重新解释 semantic rule。

## rule/profile composition 与 CLI binaries

lib 重新导出 EU4 composition root 的 `INSTALL_DESCRIPTOR`、`first_party_rules`、`first_party_rules_cached`、`first_party_rules_ephemeral`、`profile`。`src/bin/pdx-ls.rs` 使用 `profile()`；能解析 `UserPaths` 时从用户 rules cache 加载并启用 auto Vanilla，否则使用 ephemeral first-party rules。正式 server 不从 initialization options 接受外部规则。

`src/bin/pdx.rs` 调用 `cli::execute_pdx`。当前命令为 `pdx --version`、`index vanilla --source ... --output ...`、`setup vanilla`、`check policy|zed|release|grammar-fuzz|all`、`release package|verify` 和 `dev prepare-manifest`；`CliError::exit_code` 将 usage 错误与运行时错误分为 2/1。`check` 子模块公开 `CheckResult`/`CheckOutcome` 与四类检查函数；`release` 子模块公开 `ServerArtifact`、`ArchiveKind`、`ServerLimits`、contract load、package、verify 入口。

## 明确不负责的边界与当前限制

本 crate 不实现 parser/HIR、EU4 name table 或 analysis policy，也不允许用户规则路径、网络规则下载或规则 override。传输层只实现本地 `file://` URI；`pdx-ls` 只有 stdio binary，socket/编辑器集成由调用方提供 stream 或 extension。Vanilla cache 的 hash mismatch 重建、磁盘扫描、安装发现和用户配置都通过 composition root/worker 协作，不能把 LSP capability 当作这些数据的 authority。

## 验证命令

```text
cargo test -p pdx-lsp
cargo run -p pdx-lsp --bin pdx -- --version
cargo run -p pdx-lsp --bin pdx-ls -- --version
```
