# RFC 0010：Zed 集成

- 状态：Accepted
- MVP：EU4 v0.1

## 原则

Zed extension 是薄客户端。它可以包含语言 metadata、Tree-sitter queries、server 安装/启动代码、配置映射，以及作为独立 asset 打包的 `eu4.pdxrules`，但不实现或解释任何 EU4 semantic rule。

## Extension 结构

```text
editors/zed/
  extension.toml
  Cargo.toml
  src/lib.rs
  languages/
    pdx-script/
      config.toml
      highlights.scm
      brackets.scm
      indents.scm
      outline.scm
    pdx-eu4-localisation/
      config.toml
      highlights.scm
      outline.scm
    pdx-eu4-csv/
      config.toml
  bundled-rules/
    eu4.pdxrules
    manifest.json
```

extension 注册 PdxScript/localisation grammar、CSV 文件关联和一个 `pdx-ls`。所有文本语言连接同一个 server；CSV 由 server 的独立 parser 分析，不伪装成 PdxScript CST。

## 文件识别冲突

PdxScript 使用 `.txt`、`.gui`、`.gfx` 等多个共享扩展，EU4 localisation 使用 `.yml`，都与常见语言冲突。Zed language `path_suffixes` 不支持 glob，因此不能在 extension metadata 中安全地全局抢占这些扩展。

MVP 策略：

1. 不全局声明宽泛 `.txt` 关联，或只提供明确需用户确认的关联。
2. localisation 使用 `l_<language>:` 首行模式作为辅助，但它不能识别缺少/错误语言头的文件。
3. `pdx init --editor zed` 从 Eu4Rules 的完整 file matcher catalog 生成项目级 `.zed/settings.json` fragment，使用 `file_types` glob 将全部可支持 EU4 文件关联到相应语言。
4. 用户可以手动选择语言。
5. `pdx-ls` 始终根据 logical path 再分类，绝不信任 language id 决定语义。

生成设置的缩略示意：

```json
{
  "file_types": {
    "PdxScript": [
      "common/**/*.txt",
      "events/**/*.txt",
      "decisions/**/*.txt",
      "missions/**/*.txt",
      "history/**/*.txt",
      "interface/**/*.gui",
      "interface/**/*.gfx"
    ],
    "EU4 Localisation": ["localisation/**/*.yml"]
  }
}
```

实际列表不得由这段示例维护，而由 EU4 `Eu4Rules` 生成；其中包括全部数据库 path/type matcher 能够映射到 Zed glob 的类别。无法无损映射的 matcher 必须出现在生成报告中，并提供手动 language selection fallback。最终 key/name 以 Zed 实际 API spike 为准。

## Grammar 分发

Zed grammar registration 引用 repository 与固定 revision。当前 monorepo 把 grammar 放在子目录，Phase 0 必须验证 Zed 构建工具是否支持该布局。

若不支持，采用 CI split mirror：

- `grammars/tree-sitter-pdx-script` 发布到独立只读镜像仓库。
- `grammars/tree-sitter-pdx-eu4-localisation` 同理。
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

extension 不自行构建 Rust server，也不运行 package manager 安装脚本。

规则文件不随 server release 下载。它由 extension release 自身携带，扩展升级自然携带新规则和 `rule_hash`。extension 启动命令必须为：

```text
<resolved-pdx-ls> --rules <extension-path>/bundled-rules/eu4.pdxrules
```

开发模式可以显式覆盖规则路径，但仍使用相同 CLI 参数。规则损坏或缺失时 extension 报告可操作错误，不从网络回退。

## 配置传递

Zed settings 映射为 EU4 initialize options：

- `.pdx/project.toml` 路径
- Vanilla 本地索引缓存路径
- dependency mods（标识符和显式顺序，由本机设置解析路径）
- current mod directory
- development-only rule path override

server 是配置含义的最终解释者。extension 只做 JSON 传递和必要路径发现。

## Vanilla 首次索引与刷新

扩展安装后的首次 EU4 workspace 设置中，如果本地 Vanilla cache 不存在，extension 引导用户选择 Vanilla 目录，并调用核心 CLI/server indexing entry point 建立缓存。扩展设置提供显式“刷新 Vanilla 索引”操作。

核心入口建议为：

```text
pdx index vanilla \
  --rules <extension-path>/bundled-rules/eu4.pdxrules \
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
- extension 启动参数指向自己打包的规则数据库。
- 首次 Vanilla cache 建立和手动刷新有发布前 smoke checklist。
- extension 源码中搜索不到 effect/trigger 名称表或 scope 规则。
