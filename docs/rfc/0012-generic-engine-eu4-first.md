# RFC 0012：通用 PDX 引擎与 EU4-first

- 状态：Current
- 当前交付范围：EU4 v0.1

## 当前产品边界

ParadoxCode 的核心是通用 `pdx-lsp` engine，但当前唯一完整支持和发布目标是 EU4。通用层
不根据编辑器名称、binary 名称或隐式 `game_id` 白名单实现语义；EU4 的路径、scope、command、
symbol 和特殊 lowering 集中在 `pdx-game::eu4` 与第一方规则数据中。

## 当前 crate 结构

```text
pdx-text -> pdx-parser -> pdx-engine -> pdx-analysis -> pdx-lsp
pdx-rules -> pdx-engine / pdx-analysis
pdx-rules -> pdx-bake
pdx-game::eu4 -> pdx-lsp composition root
```

- `pdx-text` 提供 offset、line index、UTF-8/UTF-16、URI 和 logical path。
- `pdx-parser` 提供 loss-aware CST、syntax error、增量 edit facade 和安全 formatter。
- `pdx-rules` 提供 source compiler、schema、canonical `rule_hash`、只读 `RuleSet` 和通用
  matcher/descriptor 类型。
- `pdx-game` 提供安装发现、用户级配置；其 `eu4` module 提供 EU4 `GameProfile`、安装描述、
  scan whitelist、bootstrap catalog、embedded first-party source 和 user-cache provider。
- `pdx-bake` 只负责维护者使用的严格 source validation 和临时 artifact/manifest 生成。
- `pdx-engine` 维护 VFS、roots、overlay、parse/HIR、index shard 和 snapshot；`pdx-analysis`
  对 snapshot 提供 editor-neutral 查询；`pdx-lsp` 只做协议生命周期和 DTO 转换。

`GameProfile` 和 `RuleSet` 从 composition root 显式传入 engine、analysis 与 LSP。EU4 profile
当前位于 `pdx-game::eu4`，不存在独立的 `pdx-game-eu4` crate。

## Script、localisation 与 CSV 边界

`pdx-parser::FileFormat` 当前只有 `Script` 和 `Localisation` 两个前端。`pdx-engine` 只对
`ParserKind::Script`/`Localisation` 产生 `ParsedSource` 和 HIR；`Asset` 与 `SyntaxOnly` 不进入
文本 parser 或 HIR。

EU4 profile 当前的 scan extensions 是 `txt`、`gfx`、`yml`。规则 catalog 可以把 `.csv` 表示
为 `syntax-only` 文件类别，但项目没有 CSV grammar、CSV parser、CSV HIR 或 CSV formatter，且
当前 profile 不把 CSV 纳入 workspace scan。这是当前限制，不应在 parser/API 文档中描述为已
支持 CSV。

## 通用性约束

- workspace、snapshot、index、analysis query、LSP transport、CLI 和 release 设施保持游戏无关；
- EU4 专属名称表和特殊规则不进入 generic LSP、engine 或 Zed extension；
- 优先用规则数据表达游戏差异，当前没有稳定的第三方 plugin ABI；
- 一个 workspace 当前只组合一个 game profile，其他 Paradox 游戏没有 v0.1 支持承诺；
- 增加第二个真实 profile 前，不为假想行为增加 trait 或复杂插件层。
