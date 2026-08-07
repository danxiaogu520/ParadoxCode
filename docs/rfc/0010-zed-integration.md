# RFC 0010：Zed 集成

- 状态：Current
- 适用版本：EU4 v0.1

## 已实现的扩展

扩展位于 `editors/zed`，是 Zed 的薄客户端，包含语言 metadata、Tree-sitter grammar/query
引用和 `pdx-ls` 的发现、安装、校验、启动代码。`extension.toml` 注册语言
`Europa Universalis IV`（机器标识由 Zed 生成）和 `pdx-ls` server；语义分析、symbol、scope
和规则解释全部留在 Rust core，扩展不携带规则表。

语言配置使用 `grammars/tree-sitter-eu4` 的固定 revision；当前 query 提供 syntax-level 的
highlight、bracket、indent 和 outline，不把 EU4 command/scope 名称表编入扩展。
`.txt` 不被全局抢占，`gui`/`gfx` 由语言配置声明。

## 当前静态推荐设置

仓库内 `editors/zed/recommended-settings.json` 是手工维护的静态 fragment，当前内容为：

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

它用于 EU4 Mod workspace 的文件关联，不是规则编译产物，也不会按 `RuleSet` 自动生成。项目
当前没有 `pdx init --editor zed`；无法由该 fragment 覆盖的文件仍可在 Zed 中手动选择语言。

## server 获取、校验与启动

`language_server_command` 按以下顺序选择 executable：

1. Zed `lsp.pdx-ls.binary.path` 的显式路径；
2. worktree `PATH` 中的 `pdx-ls`；
3. 与扩展 `CARGO_PKG_VERSION` 相同版本的官方 GitHub Release asset。

下载阶段通过 Zed streaming API 限制 body 大小，读取匹配 archive 的 `.sha256` sidecar，并对
archive bytes 做 SHA-256 校验。Linux/macOS 使用包含单个精确命名 `pdx-ls` 的 `.tar.gz`，
Windows 使用包含单个 `pdx-ls.exe` 的 `.zip`。reader 校验文件名、路径、压缩方式、CRC、archive
metadata 和大小上限，拒绝额外成员、目录、symlink/path traversal、未知压缩或损坏 archive。

解压后 executable 另算 SHA-256，和 binary 一起写入 extension work directory 的版本/target
cache。重用前会再次检查 regular-file 类型、非空、大小上限和 digest；缺少或被修改的 cache
会删除并重新下载。成功后非 Windows binary 设为可执行，server 以解析出的 binary 启动；扩展
不会自动附加 `--rules` 或其他规则路径参数，正式 `pdx-ls` 也拒绝这类输入。Zed settings 中
配置的 binary arguments 原样传给 server。

当前发布矩阵包含 Linux x86_64/ARM64、macOS x86_64/ARM64 和 Windows x86_64；扩展不自行编译
Rust server，也不运行 package-manager 安装脚本。

## 当前限制

- 扩展只注册 `Europa Universalis IV`，不注册独立 localisation 或 CSV 语言。
- 扩展不实现 Vanilla discovery/indexing，也不负责写入 Vanilla cache；相关流程由
  `pdx setup vanilla`、`pdx index vanilla` 或 `pdx-ls` server 处理。
- 扩展不提供规则 source、规则 override 或动态文件分类生成。
