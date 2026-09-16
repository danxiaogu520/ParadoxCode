# AGENTS.md — ParadoxCode 工作说明

给在本仓库工作的 coding agent 的持久说明：协作流程、验证职责、架构定案与平台事实。条目多为“非显然、重查成本高”的结论，改动相关领域前先读对应小节。会变化的执行流程以 `docs/validation.md` 与 `RELEASING.md` 为准，本文件记录长期不变量和可移植的参考事实。

## 1. 与用户的协作流程（契约，必须遵守）

1. 用户以 idea 开场；先调研（代码 / 游戏文件 / 本地 wiki 快照）验证可行性与代价，不空谈。
2. 出选择题让用户决策，每个选项附区别与代价。
3. 综合选择出完整方案，用户确认后才实施。
4. **提交只在用户命令下进行**——实施完成停在未提交状态等指令；提交切分方式也由用户当场指定。
5. 琐碎同步（测试断言、typo、契约脚本跟随主改动的同步）可并入已确认方案的实现，不必单独发问。

## 2. 常用命令与门禁

- 本地检查聚合器：`cargo tools gates [core|core-fast|vscode|policy|artifact|fuzz|perf|all]` （无参 = `all`）。`all` 只表示默认确定性本地检查（不含 opt-in 的 `perf`），不代表可合并或可发布；完整职责表见 `docs/validation.md`。 `.cargo/config.toml` 的 `tools`/`tq` 别名自带结尾 `--`，命令里不要再带。
- VS Code 扩展：`npm run check` + `npm run test:contract`（compile/smoke/extension/package/transcode 五契约）。
- 全量 Vanilla sweep 是按风险运行的本地开发工具，不是提交、PR 或 release 门禁。必须显式传入要验证的 `--server`；脚本会核对服务器实际内嵌的规则 hash 与当前 checkout，避免 stale 二进制产生假结论。报告只留在被忽略的 `performance-results/`，不得上传或提交。
- 黄金对拍：`PDC_UPDATE_GOLDEN=1 cargo test -p ide golden_gfx_sprite_semantics`。
- 规则改动后必须重烤并同步计数断言： `cargo run -p rules --bin bake -- build --source rules/eu4 --output <tmp> --manifest rules/manifest.json` + rulec.rs 计数。

## 3. 发布流程

- PR 的远端 `Conclusion` 是唯一合并权威；本地检查和 sweep 只提供开发反馈，不能替代它。
- 版本准备在普通 PR 中完成：同步 `Cargo.toml`、`editors/vscode/package.json` 与 lockfile，收编 `CHANGELOG.md`，运行受影响的本地分组，合并后等待 `main` 的 `Conclusion` 成功。
- 受保护的 annotated `vX.Y.Z` tag 启动发布。tag workflow 只使用仓库自有输入，构建五个原生归档、五个 SHA-256 sidecar 和一个 VSIX，并在一次发布前严格校验十一项资产。
- Vanilla 文件、路径、摘录、诊断报告、指纹和 sweep 输出不得进入 Actions、PR、issue 或 Release。需要 sweep 的改动在本机运行，只在 PR 中写不含敏感内容的结论；发现的问题缩成仓库自有的最小回归 fixture。
- 手动步骤：从公开 Release 安装 VSIX 做干净 profile 冒烟，再上传 Marketplace，并记录公开链接与已知限制。
- 发布 workflow 暂态失败且 tag 尚未发布时可原 tag 重跑；如需改代码或元数据，必须经新 PR 修复并顺延 patch tag。受保护 tag 与已发布资产永不移动、删除或覆盖。
- 详细清单以 `RELEASING.md` 为准，职责与失败归属以 `docs/validation.md` 为准。

## 4. 平台/工具链事实

不要使用 bash 命令，改为使用 PowerShell 命令。

有现代 CLI 工具可用时，优先使用它们，避免冗长的 PowerShell 等价写法：

- 文本或代码搜索用 `rg`，不用 `Select-String`。
- 文件发现用 `fd`，不用递归 `Get-ChildItem`。
- 读取文件用 `bat --style=plain --paging=never`。
- 只读取文件片段时用 `bat --line-range START:END`。
- JSON 处理用 `jq`。
- YAML 处理用 `yq`。
- 结构化代码搜索和 AST 感知重构用 `ast-grep`。

## 5. CI 与本地诊断基础设施

- 生命周期职责固定：本地组负责快速反馈；PR/main 的 `Conclusion` 负责合并；Security 与 Performance workflow 负责定时审计；tag workflow 负责可再分发资产；干净 profile 与 Marketplace 是人工验收。不要用一个阶段的结果替代另一个阶段的授权。
- Vanilla sweep 只比较和解释本地诊断/性能变化，不维护仓库指纹基线，也不作为 PASS/FAIL 发布契约。`--previous` 只生成辅助差异；工具错误、服务器失败和用户显式 `--fail-on` 仍会让本地命令失败。服务器真实规则 hash 取自 `server_messages`，checkout hash 取自 `rules/manifest.json`，两者不一致时拒绝继续。
- scripts 布局（`editors/vscode/scripts/`）：单词命名入口（diagnose/probe/compare/transcode/extension/package/host/smoke/sweep）+ lib 分工（options/workspace/overlay/diagnosis/report/client/sampler/paths）；入口全是薄壳，sweep 直接 import 相位函数。改诊断/性能链路先动 lib 再动入口。transcode.mjs 含 Rust↔TS 差分向量对拍（74,549 条，`PDC_SKIP_VECTORS=1` 可跳）。
- 服务端 config 在 **%APPDATA%（Roaming）**，非 LOCALAPPDATA；cache 根在 LOCALAPPDATA。
- npm `--prefix … run` 传相对 `--server` 路径会以 editors/vscode 为 cwd 解析而失败（用直接 node 调用或绝对路径）。
- sweep 冷协议：客户端对缺失的 vanilla 缓存放行（服务器端支持显式缓存缺失时自动发现并原位重建）。
- GitHub-hosted CI 不应依赖本机、游戏安装或持久缓存；可执行的远端证据必须来自仓库自有 source/fixture。远端 workflow 中出现 Vanilla 路径或 `sweep.mjs` 调用属于策略违规。

## 6. 本地调研资料约束

本地安装、mod 和资料快照只用于调研与人工验证；路径从用户配置、工具发现结果或当次任务的显式参数取得。不要把维护者用户名、绝对路径、硬件规格或安装位置写进仓库、CI、Release、公开报告或可移植性假设。

- EU4 原版真值以维护者合法安装为本地输入。优先读取 ParadoxCode 用户配置或运行 `cargo tools setup vanilla --game eu4` 完成发现，不假定 Steam/GOG/Epic 的固定目录。
- EDG (EU4中文模组`归墟之门`, `Entrance to the Desolate Ground`, 创意工坊模组ID：3047072888) 可作为本地人工验证语料。
- **查 EU4 wiki 资料优先使用维护者已有的 `eu4-wiki-encyclopedia` 本地快照**，位置由当次环境提供，不访问受 `_fs-ch-` JS 挑战保护的在线站。入口 `MODDING.md`、`INDEX.md|json`；正文位于 `md/<分类>/`，原始表格查 `raw/`，页面新旧看 frontmatter 的 revid/revision_ts。需要更新走快照仓库 `tools/` 的再生流程。API 坑：allpages 续传是 `apcontinue`（generator 是 `gapcontinue`）；新版分类字段是 `title:"Category:X"`；turndown-plugin-gfm 遇 `<caption>` 放弃转表格须先剥掉。内容许可 CC BY-SA 3.0，仅本地参考。
