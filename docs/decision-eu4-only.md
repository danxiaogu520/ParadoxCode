# 架构决策：EU4-only

日期：2026-07-16

状态：Superseded（2026-07-20）

本决策已被 [RFC 0013：通用 PDX 语言引擎与 EU4-first 产品策略](rfc/0013-generic-engine-eu4-first.md) 取代。本文仅保留为历史记录，不再约束当前实现。

## 决策

ParadoxCode 只支持 Europa Universalis IV（EU4）。项目不实现其他游戏适配，不保留可切换的 `game`/游戏规则抽象，也不为未来游戏预留替换式 parser 或规则接口。

EU4 的 Script、localisation 和 CSV 前端直接属于 `pdx-syntax`；公共 API 只表达 EU4 文件格式。EU4 语义规则由 `pdx-eu4` 和 `rules/eu4.pdxrules` 承载，`pdx-cwt` 只导入 EU4 CWT。

## 影响

- 旧通用规则 crate 更名为 `pdx-eu4`，运行时类型使用 `Eu4Rules`，规则 hash 使用 `Eu4RuleHash`。
- 原嵌套规则资源目录迁移为项目级 `rules/`；项目配置和 LSP initialize options 不再携带 `game` 字段。
- `pdx-ls` 不接受游戏选择参数，规则路径仍可通过 `--rules` 显式传入。
- grammar、fixture、Zed language metadata 和规则 schema 都按 EU4 固定，不通过游戏类型分派。
- 未来若要支持其他游戏，必须另行提出新的架构决策，不在当前兼容性承诺内。

## 迁移

这是 Phase 0 骨架阶段的破坏性重命名，没有稳定发布 API 或数据库需要兼容。后续实现以 `pdx-eu4`、`rules/`、`Eu4Rules` 和 `parse_eu4` 为唯一新入口。
