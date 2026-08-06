# RFC 0012：通用 PDX 语言引擎与 EU4-first 产品策略

- 状态：Accepted
- 日期：2026-07-20
- 当前交付目标：EU4 alpha → EU4 v0.1
- 取代：EU4-only 架构决策（原 `decision-eu4-only.md`，已删除）

## 背景

ParadoxCode 已经实现 EU4 规则导入、语法前端、workspace、analysis 和 LSP 的首轮原型，但现有边界把通用 PDX 基础设施与 EU4 专有语义绑定在一起。项目所有者决定将长期产品定位调整为通用 `pdx-lsp` 引擎，同时仍把 EU4 作为当前唯一有交付承诺的游戏。

本决策不要求现在实现第二个游戏，也不要求建立动态插件 ABI。它只要求当前重构不继续把 EU4 特有知识写入未来应由所有游戏复用的基础设施。

## 决策

ParadoxCode 采用“通用引擎、游戏 profile、EU4-first”的结构：

- `pdx-lsp`、workspace/snapshot、文件索引、分析查询框架、CLI 和发布设施保持游戏无关；
- Script、localisation 和 CSV 的可复用语法能力属于 syntax 层；确实存在游戏差异时由 profile 选择 parser 配置或专用前端；
- 规则 schema、artifact 读取、canonical hash、文件类别和 symbol/reference 描述迁入通用规则 runtime；
- EU4 的路径、scope、command、特殊 symbol、规则补丁和语义 fallback 集中在 EU4 profile；
- v0.1 只发布和验收 EU4 profile；其他游戏支持没有版本承诺，优先级低；
- 优先用规则数据表达游戏差异，只有第二个真实游戏证明数据不足时才增加行为 trait。

## 用户体验

EU4 v0.1 不要求用户显式选择游戏。官方 binary 内嵌携带 `game_id = "eu4"` 的第一方 JSON source bundle，在 composition root 选择 EU4 profile，并在用户 cache 物化 SQLite artifact；不存在规则路径参数。

未来可以增加 workspace 自动识别或显式配置，但 server 不应依赖 binary 名称、编辑器名称或硬编码默认路径来判断游戏。

## 目标边界

```text
pdx-text
  range / line index / UTF-8 / UTF-16 / logical path

pdx-parser
  Script / localisation / CSV loss-aware frontends

pdx-rules
  RuleSet / RuleHash / artifact schema / matcher / descriptors

pdx-game
  data-only install markers / platform discovery / user-local game configuration

pdx-game-eu4
  EU4 profile / install descriptor / scopes / path semantics / special lowering

pdx-bake
  strict first-party rule source compiler CLI; validates/emits temporary EU4 rule artifacts

pdx-engine
  VFS / roots / overlays / HIR lowering / per-file state / snapshots / index shards

pdx-analysis
  editor-neutral queries over snapshots

pdx-lsp
  protocol lifecycle and DTO conversion
```

## 迁移策略

> 实现进度（2026-08-06）：步骤 2–6 已完成。`pdx-rules` 持有通用 source compiler/runtime 和 data-only `GameProfile`，`pdx-game::eu4` 持有 EU4 profile、内嵌 JSON source bundle 与用户 artifact cache provider，迁移期 `pdx-eu4` re-export 已删除。schema 16 与第一方 source format 5 已落地；Zed 主脚本语言使用 `Europa Universalis IV`/`eu4`，通用 parser family 使用 `Script`。CLI → LSP → host → snapshot 显式传递并校验 profile。semantic root context、初始 `ScopeState`、静态 nested transition/intrinsic 与多段 exact link lowering 已进入 HIR。

原 `pdx-eu4` 同时包含通用规则 runtime 与 EU4 数据；迁移按以下步骤分阶段完成：

1. 先为现有行为建立回归测试和性能计数器；
2. 将不引用 EU4 名称或路径的规则类型移动到 `pdx-rules`；
3. 保留临时 re-export，避免一次提交同时修改全部调用方；
4. 将 EU4 scope、path、command 和特殊 symbol 逻辑集中到 `pdx-game-eu4`；
5. 让 HIR lowering 显式消费 `RuleSet` 与 EU4 profile；
6. 删除过渡 re-export，并更新 artifact schema metadata；
7. 第一方规则编译器保持独立，不把 authoring concern 引入 runtime。

迁移期间每一步必须保持 workspace 可构建、现有 EU4 行为不变，且不得与 snapshot/index 性能修复混在同一个不可审查的大提交中。

## 不做的事情

- v0.1 不实现 EU5、HOI4、CK3、Stellaris 或其他游戏；
- 不设计稳定的第三方插件 ABI；
- 不支持一个 workspace 同时加载多个游戏 profile；
- 不建立在线规则市场或运行时执行 Lua/Wasm/Rust 插件；
- 不因为“未来可能需要”而给每个类型增加 trait；
- 不降低 EU4 规则覆盖率来换取表面通用性。

## 验收条件

本决策完成迁移时应满足：

1. `pdx-lsp`、`pdx-engine` 和通用 index 类型中没有 EU4 command/name/path 白名单；
2. 规则 artifact 声明 `game_id`，runtime 校验 artifact 与 profile 一致；
3. EU4 专有行为可以从明确的 EU4 crate/module 定位；
4. 添加第二个 profile 不需要重写 LSP transport、snapshot 或 WorkspaceIndex；
5. EU4 的现有 parser、diagnostics、completion、navigation 和 rename 回归继续通过；
6. 没有只为假想第二游戏存在、且无法由测试说明价值的公共抽象。

安装发现使用已经由 EU4 首次配置闭环证明需要的数据结构：通用层只消费平台可执行文件
标志、验证目录和常见安装目录名；EU4 值由 `pdx-game-eu4` 提供。它不是动态 profile
插件 ABI，也不意味着第二款游戏已经获得产品支持。

## 影响

- 原 `Eu4Rules`、`Eu4RuleHash` 等名称会在分阶段迁移后成为通用 `RuleSet`、`RuleHash`；
- 原 `pdx-eu4` crate 会拆分为通用规则 runtime 与 EU4 profile；
- `docs/decision-eu4-only.md` 保留为历史记录，但不再是有效产品约束；
- EU4 仍是当前所有端到端、性能和发布测试的唯一目标。
