# RFC 0013：First-party rule source and compiler

- 状态：Current
- 适用版本：EU4 v0.1
- 规范地位：当前规则 authority 的唯一规范

## Authority

`rules/eu4/` 是静态 EU4 规则的唯一 source authority。当前 source format 为 `7`，manifest
记录 `game_id = "eu4"` 和目标游戏版本。source bundle 由以下 JSON 组成：

- `manifest.json`
- `catalog.json`
- `semantic-rules.json`
- `enum-values.json`
- `type-root-keys.json`
- `type-root-scopes.json`
- `type-descriptors.json`
- `localisation-bindings.json`

这些文件定义 file classification、semantic rule alternatives、type/root 描述、symbol/reference
metadata 和 localisation bindings。`type-descriptors.json` 中的 `scripted_macro` 声明宏的
body context、启用状态和 usage capability 元数据；当前 runtime 使用启用状态/body context
进行宏 lookup，并对当前 EU4 使用到的 scalar matcher 与 `AnyScalar`/`Opaque` block
shape 做静态校验，具体成员和从定义体归纳的参数签名仍来自 workspace index，不在 source 中静态列举。workspace/
scope-dependent matcher 和 usage capability 的完整行为门控仍是 Partial。稳定 identity、source order、
alternative identity 与重复 key 的语义由 compiler 保留；生成 SQLite 或 manifest 不是第二个
authority。

## 编译与验证

`pdx-rules::rulec` 是唯一 source compiler。它只接受固定的第一方 JSON layout，拒绝 unknown
field、缺文件、重复 stable identity、无效 cardinality/severity/type identity 和不一致的
cross-record invariant。`pdx-bake` 供开发/发布检查使用，输出 caller-selected 的临时或发布
artifact；官方 runtime 不把该输出当 source。

runtime SQLite schema 当前为 `18`。编译后必须把 artifact 读回为 `RuleSet`，检查 schema、
foreign key、`game_id`、记录和 semantic model 与 source round-trip 一致。`rule_hash` 取
canonical logical content，而不是 SQLite 文件 bytes；rowid、插入顺序、页面布局、索引、
VACUUM 或时间戳不构成规则语义。

## 官方 runtime

官方 `pdx`/`pdx-ls` binary 在 `pdx-game::eu4` 中内嵌上述 JSON source bundle。启动时先严格
解析 source 并计算当前 `rule_hash`，再查找用户本地 SQLite cache。只有 schema `18`、`game_id`
和 logical `rule_hash` 都匹配时才只读加载 cache；cache 缺失、损坏、过期或不匹配时，从内嵌
source 重新生成，写入临时 SQLite，读回并完成 round-trip 后再替换用户 cache。

当前实现不把跨平台原子替换、无中断切换或旧规则 fallback 作为保证；未通过验证的临时文件
不得成为 runtime `RuleSet`。用户 cache 是加速和持久化产物，永远不能反向成为 source authority。

正式 server 不接受 `--rules`、规则路径、初始化选项、环境变量、项目设置、搜索路径、下载的
规则库或 user override；也不读取、更新或导入外部规则 source。`.cwt` 不属于当前编译、测试、
运行时或更新流程。Zed extension 只获取和启动 server，不解释或分发 semantic rules。

## 当前限制

- 当前 source authority 只有 EU4；规则修改需要更新 source、验证结果并发布新的 server 版本。
- 生成 SQLite artifact 不手工维护、不作为仓库 authority，也不是 server 的嵌入输入。
- 当前没有用户规则覆盖、历史规则选择、自动游戏文件抽取或外部规则兼容入口。
