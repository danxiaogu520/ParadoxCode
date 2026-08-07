# `pdx-game`

## 模块职责

`pdx-game` 负责支持游戏的安装发现、用户级 TOML 配置、平台路径，以及供通用引擎消费的游戏 profile。
安装发现逻辑本身是游戏无关的；当前唯一内置 profile 位于 `pdx-game::eu4`。
本 crate 依赖 `pdx-rules`，但不承担 workspace/index 或 LSP 生命周期。

## 安装发现公开入口

- `GameInstallDescriptor`、`PlatformExecutablePaths`：描述 game id、显示名、平台 executable marker、必需目录和候选安装目录名。
- `DiscoveryDepth::{Quick, Deep}`、`DiscoveryOptions`：选择候选位置和扫描广度。
- `DiscoveryToken`：为深度搜索提供 `new`、`cancel`、`is_cancelled` 协作式取消。
- `validate_installation`：按当前平台 executable 和必需目录校验安装根；`validate_installation_for_source` 允许显式跨平台 source 路径。
- `discover_installations`：返回确定顺序的 `DiscoveryReport`，其中记录安装根、不可读目录数和 cancelled 状态。

EU4 的 `eu4::INSTALL_DESCRIPTOR` 当前要求 `common`、`events`、`missions`、`decisions`、`localisation`，并使用平台对应的 `eu4.exe`、`eu4` 或 macOS bundle executable marker。

## 用户配置与路径

`UserConfiguration::load/save` 读写版本化 TOML；缺少文件时返回默认配置，未知字段、超过 1 MiB 的输入和不支持的版本会报 `UserConfigError`。
`GameUserConfiguration` 保存每个 game 的 discovery 状态、已验证 `vanilla_source` 和 `vanilla_cache` 路径。
`UserPaths::platform` 解析平台默认配置目录与 cache 根；`vanilla_cache(game_id)` 和 `rules_cache(game_id)` 只计算稳定路径，不负责构建对应 cache。

## `pdx-game::eu4` profile 与规则入口

- `eu4::GAME_ID` 是稳定身份 `"eu4"`；`eu4::SCRIPT_FOLDERS` 是 EU4 source-root 白名单数据。
- `Eu4Profile::game_id`、`data`、`bootstrap_rules` 分别提供身份、`pdx_rules::GameProfile` 数据和最小 bootstrap `RuleSet`。
- `eu4::profile()` 返回完整 data-only `GameProfile`，包含扫描目录/扩展、definition/reference、scope 和其他 EU4 解释选择。
- `eu4::bootstrap_model()`、`bootstrap_rules()` 只构造测试/启动所需的最小 catalog，不等同于完整第一方规则 source。

模块内私有的 `FIRST_PARTY_SOURCE` 用 `include_bytes!` 嵌入 `rules/eu4/` 的八个 JSON 文件。`first_party_rules()` 严格解析 embedded source、构造 `RuleSet` 并校验 `game_id`；`first_party_rules_cached(cache_path)` 是正式的规则 cache provider：先只读加载并比较 cache，缺失、损坏、过期或 hash/schema 不匹配时重新生成临时 SQLite，round-trip 校验成功后才替换 cache。

`first_party_rules_ephemeral()` 在无法解析平台 cache 目录时使用进程临时 artifact，并在加载后删除。cache path 是派生产物位置，不是可替换 embedded source 的外部规则入口。

## 数据流与明确边界

安装描述符 → 候选路径 → 安全校验/取消 → `DiscoveryReport`；用户配置只保存结果和路径。
embedded JSON → `pdx_rules::rulec::load_source_bundle` → `RulesModel`/`RuleSet` → 用户本地 `rules.pdxrules` → 只读 runtime。

本 crate 不解析 Script/Localisation，不实现 HIR、WorkspaceIndex、Vanilla index 构建、LSP 协议，也不执行游戏或 Mod 文件。
它不提供外部规则 source、CWT 导入或用户规则覆盖；EU4 动态 workspace 成员仍由上层索引提供。

## 当前限制与验证命令

当前 profile 只有 EU4；深度发现受固定遍历深度和取消 token 约束，校验不会执行或 hash 游戏文件。

```text
cargo test -p pdx-game
cargo test -p pdx-rules
```
