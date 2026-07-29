# ParadoxCode 执行计划

> 本文件只记录当前实现状态、剩余工作和执行顺序。产品能力、非目标和各阶段退出条件
> 由 [EU4 v0.1 MVP 验收基线](docs/mvp.md) 定义；架构边界由
> [总体架构](docs/architecture.md) 和已接受 RFC 定义。

## 当前状态

ParadoxCode 当前是 EU4 alpha，尚未发布 `v0.1.0`。通用 `pdx-lsp` 引擎与 EU4-first
边界已经建立，Phase 0–5 的主要能力和 Phase 6A 的代码链路已有自动化回归。以下事实
不能被表述为已经完成：

- 尚未用真实 tag 完成并复核五平台原生 release workflow；
- 尚未在干净机器上对实际发布的 Zed extension 和 server asset 做端到端安装验收；
- workspace-dependent scope transition 与冲突 alternative 的共同诊断仍不完整。

## 阶段状态

| 阶段 | 状态 | 说明 |
| --- | --- | --- |
| Phase 0：工程与设计基线 | `completed` | Workspace、质量门禁和设计基线已建立 |
| Phase 1：Zed 与 Tree-sitter | `completed` | 源码和自动化 grammar/Zed 检查已完成；发布前仍需宿主 smoke |
| Phase 2：最小 Language Server | `completed` | stdio、文档同步、取消与真实 JSON-RPC 回归已完成 |
| Phase 3：CST、诊断与 formatter | `completed` | 2026-07-29 按规范格式化设计重新验收 |
| Phase 4：规则与 Workspace Index | `completed` | 第一方规则、增量 shard、dependency 与 Vanilla cache 已完成 |
| Phase 5：语言功能 | `completed` | 查询、snapshot、worker 和 LSP integration 已完成 |
| Phase R：发布前架构修复 | `in progress` | 剩余 scope 语义闭环 |
| Phase 6A：Rename 与 v0.1 | `implemented, not released` | 代码链路已完成，发布退出条件未全部满足 |
| Phase 6B / Phase 7 | `future` | v0.1 前不启动 |

`completed` 表示相应代码与自动化退出条件已满足，不代表 ParadoxCode 已可发布。发布结论
必须同时满足 [MVP Phase 6A 验收基线](docs/mvp.md) 和
[发布清单](docs/releasing.md)。

## 当前执行顺序

### 1. 完成 scope 语义闭环

在不改变 RFC 0013 的通用引擎边界下完成：

- workspace-dependent dynamic transition；
- 不能由直接子 key 唯一消歧时，各 alternative 的安全共同诊断；
- `Unknown`/`any` 保守回退，禁止按规则顺序随机选择；
- HIR、analysis 与真实 LSP JSON-RPC 的分层回归。

退出证据：

- 唯一可解析 transition 得到确定 scope；
- unresolved 或冲突 transition 不产生虚假确定性或级联错误；
- diagnostics、completion 与 navigation 消费同一份 snapshot/HIR facts；
- 新回归通过 workspace 和 LSP 两层验证。

### 2. 完成真实发布演练

代码中的 exact-version 下载、SHA-256 校验、受限解压、五 target matrix、确定性打包和
verifier 已实现。剩余工作是用真实发布资产证明链路：

1. 从候选提交运行完整质量门禁；
2. 创建并审核测试 tag 的五平台产物和 checksum；
3. 在干净机器安装实际 Zed extension；
4. 验证下载、校验、缓存、启动、文件识别和主要语言功能；
5. 记录平台、版本、结果和未覆盖风险；
6. 仅在全部退出条件满足后发布 `v0.1.0`。

发布、tag、GitHub Release 和 Zed registry 更新属于维护者外部操作，执行时遵循
[发布流程](docs/releasing.md)。

### 3. 发布前文档同步

- `README.md` 只保留用户可验证的能力和真实限制；
- 本文件更新阶段状态和剩余工作；
- `docs/mvp.md` 只在验收范围或退出条件变化时修改；
- 架构/API/规则 schema 变化先更新对应 RFC；
- `CHANGELOG.md` 记录用户可见变化；
- 不把开发机 smoke、内部单元测试或代码骨架描述为发布闭环。

## 已完成的发布前架构切片

以下工作已经落地并有回归，不再在本计划复制实现细节：

- 通用 `pdx-rules`、`pdx-game` 与 EU4 profile 分层，删除迁移期 `pdx-eu4` facade；
- 第一方 `rules/eu4/*.json`、严格 `pdx-rulec`、内嵌 `eu4.pdxrules`，删除 CWT 和外部
  `--rules`；
- per-file `FileState`/HIR cache、共享不可变 snapshot、bulk index 与单 shard replacement；
- 稳定 `SourceFileId`、symlink 顺序、扫描资源限制和错误隔离；
- LSP worker、debounce、版本提交门、协作式取消和类型化协议 DTO；
- Current Mod、Dependency、overlay、持久化 Vanilla cache 和 watched-file 定向更新；
- 2,000 文件 synthetic benchmark 与“单次编辑只 parse/lower 一次”的计数回归；
- formatter 固定规范布局、quoted script 递归格式化、精确 edits 与 LSP/fuzz 回归；
- Zed exact-version installer、受限下载/解压、五 target 发布 contract 和矩阵 verifier。

更细的设计依据分别位于 [总体架构](docs/architecture.md)、[HIR 与 Scope RFC](docs/rfc/0005-hir-scope.md)、
[LSP RFC](docs/rfc/0009-lsp-runtime.md) 和
[内嵌规则 RFC](docs/rfc/0014-embedded-first-party-rules.md)。

## 质量门禁

正常提交通过版本化 pre-commit hook 调用：

```text
bash scripts/check-quality-gates.sh
```

只有诊断失败时才分别运行 `core`、`grammars`、`zed` 或 `release` 分组。CI 继续负责
Windows、MSRV 和依赖策略等环境专属门禁。任何未运行的检查都必须在交付说明中明确
记录原因和风险。

## 暂不开始

Phase R 和 Phase 6A 的发布退出条件满足前，不开始：

- Semantic Tokens、Code Action、Quick Fix；
- VS Code 客户端；
- 第二个游戏 profile、动态插件 ABI 或多游戏 workspace；
- 规则历史、diff、rollback UI；
- 自动监控或自动刷新 Vanilla 索引。
