# RFC 0010：Zed 集成

- 状态：Accepted
- MVP：EU4 v0.1

> 2026-07-21 amendment：扩展携带规则和 `--rules` 启动参数已由
> [RFC 0013](0013-embedded-first-party-rules.md) 取代。扩展只下载、校验、缓存并启动内嵌
> 第一方规则的官方 server。

## 原则

Zed extension 是薄客户端。它只包含语言 metadata、Tree-sitter queries、server 安装/启动代码和配置映射，不携带、实现或解释任何 EU4 semantic rule。

## Extension 结构

```text
editors/zed/
  extension.toml
  Cargo.toml
  src/lib.rs
  languages/
    eu4/
      config.toml
      highlights.scm
      brackets.scm
      indents.scm
      outline.scm
```

extension 将主脚本 grammar 注册为用户可见的 `Europa Universalis IV`（机器标识 `eu4`），
并注册一个 `pdx-ls`。extension 不为 localisation 或 CSV 注册编辑器语言；server 仍会在
workspace 扫描中静默解析 localisation，以支持跨文件索引和 Script 中的 localisation 引用。

## 文件识别冲突

Script 使用 `.txt`、`.gui`、`.gfx` 等多个共享扩展，EU4 localisation 使用 `.yml`，都与常见语言冲突。Zed language `path_suffixes` 不支持 glob，因此不能在 extension metadata 中安全地全局抢占这些扩展。

MVP 策略：

1. 不全局声明宽泛 `.txt` 关联，或只提供明确需用户确认的关联。
2. `pdx init --editor zed` 从 EU4 `RuleSet` 的完整 file matcher catalog 生成项目级 `.zed/settings.json` fragment，使用 `file_types` glob 将全部可支持 EU4 文件关联到相应语言。
3. 用户可以手动选择语言。
4. `pdx-ls` 始终根据 logical path 再分类，绝不信任 language id 决定语义。

生成设置的缩略示意：

```json
{
  "file_types": {
    "Europa Universalis IV": [
      "common/**/*.txt",
      "events/**/*.txt",
      "decisions/**/*.txt",
      "missions/**/*.txt",
      "history/**/*.txt",
      "interface/**/*.gui",
      "interface/**/*.gfx"
    ]
  }
}
```

实际列表不得由这段示例维护，而由 EU4 `RuleSet` 生成；其中包括全部数据库 path/type matcher 能够映射到 Zed glob 的类别。无法无损映射的 matcher 必须出现在生成报告中，并提供手动 language selection fallback。最终 key/name 以 Zed 实际 API spike 为准。

## Grammar 分发

Zed grammar registration 引用 repository 与固定 revision。当前 monorepo 把 grammar 放在子目录，Phase 0 必须验证 Zed 构建工具是否支持该布局。

若不支持，采用 CI split mirror：

- `grammars/tree-sitter-eu4` 发布到独立只读镜像仓库。
- source of truth 仍在 ParadoxCode monorepo。
- extension pin 镜像 revision（Zed manifest 的 `rev`），不追踪 branch。

禁止手工维护两份 grammar。

## Language Server 获取

开发模式：

1. 首先读取显式配置的 executable path。
2. 其次在 worktree/PATH 中查找 `pdx-ls`。

发布模式：

- 根据平台与架构下载 GitHub Release artifact。
- 校验版本、文件名与 checksum。
- 缓存在 extension work directory。
- 下载/解压失败返回可操作错误。

Zed 0.7 adapter 已实现上述顺序：它查询与 extension `CARGO_PKG_VERSION` 完全相同的 tag，
先读取命名 sidecar，再对 archive bytes 做 SHA-256；校验通过后只接受单个精确命名的
`pdx-ls`/`pdx-ls.exe`。tar/zip 中的额外成员、目录、data descriptor、加密或未知压缩
方式都会失败，tar header checksum/USTAR 标识和 ZIP local/central metadata/CRC 也必须
一致；checksum sidecar、压缩 archive 与解压后 executable 还有独立硬上限。tar reader
在 executable payload 上限之外只额外允许一个 Python USTAR record 的容器开销，并再次
按 header size 校验 payload，避免容器开销误伤边界合法产物或反向放宽 executable 上限。
HTTP body 通过 Zed streaming API 逐 chunk 检查并在扩容前拒绝越界，避免异常 asset 在
下载、校验或解压阶段耗尽 extension WASM 内存。失败通过 language-server
installation status 返回可操作错误。native 单测锁定 sidecar、受限 reader 与 extractor，
并直接把 Python packager 生成的 tar.gz/zip 交给 Rust extractor 做跨实现契约回归；CI
还逐项核对 Rust 五平台映射与 canonical `server-distribution.json`。CI 另行编译实际
`wasm32-wasip1` extension，并对 native test targets 执行严格 clippy。

安装完成时 extension 另存解压后 executable 的 SHA-256；命中 version+target cache 时先
校验文件类型、非零/大小上限与该 digest。旧版本遗留的无 checksum cache、截断文件或被
修改的 executable 都会被移除并重新下载，不会永久陷入重复启动坏 binary。文件类型
检查不跟随 symlink，安装目录也必须是实际目录；写入前会清理精确命名的临时文件，因此
预置 cache/temp symlink 不能把下载内容写出 extension work directory。

extension 不自行构建 Rust server，也不运行 package manager 安装脚本。

仓库内的 `pdx release package` 子命令按上述 target matrix 生成只含一个 executable
的 deterministic `.tar.gz`/`.zip` 与相邻 `{archive}.sha256`；`cargo test` 用原创 fixture
同时验证 Linux/macOS archive contract、Windows archive contract、executable mode、checksum
和重复打包字节稳定性。packager、完整矩阵 verifier 和测试通过共用 `crates/pdx-lsp/src/release.rs`
读取 `server-distribution.json`，不各自复制 target/filename table；checksum/archive/executable
大小上限也在该契约中声明，并由 Rust producer/verifier 与 policy
共同锁定。tag workflow 在五种原生
runner 上构建并校验 server 版本，汇总后
必须由完整矩阵 verifier 接受才创建 immutable GitHub Release。发布资产上的 clean-machine
extension 下载/启动仍是发布阻塞项。

第一方规则内嵌于 server release。extension 启动命令必须为：

```text
<resolved-pdx-ls>
```

开发模式也不能覆盖规则。内嵌规则损坏属于 server build/release defect。

## 配置传递

Zed settings 映射为 EU4 initialize options：

- `.pdx/project.toml` 路径
- Vanilla 本地索引缓存路径
- dependency mods（标识符和显式顺序，由本机设置解析路径）
- current mod directory

server 是配置含义的最终解释者。extension 只做 JSON 传递和必要路径发现。

## Vanilla 首次索引与刷新

扩展安装后的首次 EU4 workspace 设置中，如果本地 Vanilla cache 不存在，extension 引导用户选择 Vanilla 目录，并调用核心 CLI/server indexing entry point 建立缓存。扩展设置提供显式“刷新 Vanilla 索引”操作。

核心入口建议为：

```text
pdx index vanilla \
  --source <selected-eu4-directory> \
  --output <extension-local-cache>
```

首次建立和后续手动刷新使用同一入口；extension 只负责触发和展示结果。

- 正常启动不扫描或监控 Vanilla。
- extension 更新或 `rule_hash` 变化不自动刷新。
- cache metadata 中旧 `rule_hash` 只用于展示，不阻止加载。
- extension 不包含 indexing 逻辑，只发起核心命令并传递路径。

## Tree-sitter queries

Phase 1 提供：

- 基础 key/operator/string/number-like/comment captures
- block bracket matching
- block indentation
- event 和 scripted definition 的 outline（只做 syntax 近似）

高亮不得依赖完整 EU4 command 列表。语义区分 effect/trigger/symbol 留给后续 Semantic Tokens；MVP 的 Tree-sitter 高亮保持通用。

## 验收

- Install Dev Extension 成功。
- 打开配置过的 EU4 workspace 时只启动一个 `pdx-ls`。
- 所有注册的文本 language 都能发送 didOpen/change/close。
- server path 配置和 PATH fallback 可测试。
- extension 启动命令不含可替换第一方规则的参数。
- 首次 Vanilla cache 建立和手动刷新有发布前 smoke checklist。
- extension 源码中搜索不到 effect/trigger 名称表或 scope 规则。
