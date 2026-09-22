# AGENTS.md — ParadoxCode 工作说明

给在本仓库工作的 coding agent 的持久说明：协作流程、验证职责、架构定案与平台事实。条目多为“非显然、重查成本高”的结论，改动相关领域前先读对应小节。会变化的执行流程以 `docs/validation.md` 与 `RELEASING.md` 为准，本文件记录长期不变量与可移植的参考事实。

## 1. 与用户的协作流程（契约，必须遵守）

1. 用户以想法开场；先调研（代码 / 游戏文件 / 本地 wiki 快照）验证可行性与代价，不空谈。
2. 出选择题让用户决策，每个选项附区别与代价。
3. 综合选择出完整方案，用户确认后才实施。
4. **Git 工作流（固定）**：
   - **超小改动直接提交**：不改变运行行为的内容（typo、注释、纯文档、格式化、测试断言、跟随主改动的契约脚本同步），确认无误后直接在 `main` 上提交并推送。
   - **行为变更一律走 PR**：凡改变服务端 / 扩展 / 规则 / 脚本的可观察行为，或改动协议、接口、配置默认值，一律开分支提交并发 PR，无需手动启用自动合并——`pr-autosync` 工作流会对维护者名下的 PR 自动开 squash auto-merge，并在 main 推进时自动刷新分支重跑 CI。main 的分支保护已把远端 `Conclusion` 设为必需检查（不对管理员强制，超小改动的直推不受影响），通过后 GitHub 自动合入。
   - 归类拿不准时按行为变更处理（走 PR）。
   - 提交默认按逻辑单元切分（服务端、客户端、文档分开），用户可当场指定其他切分方式。
   - 永不改写已推送的历史、不强推、不移动或删除受保护标签（见第 3 节）。
5. 琐碎同步（测试断言、typo、契约脚本跟随主改动的同步）可并入已确认方案的实现，不必单独发问。

## 2. 常用命令与门禁

- 本地检查聚合器：`cargo tools gates [core|core-fast|vscode|policy|artifact|fuzz|perf|all]`（无参数 = `all`）。`all` 只表示默认的确定性本地检查（不含需显式启用的 `perf`），不代表可合并或可发布；完整职责表见 `docs/validation.md`。`.cargo/config.toml` 的 `tools`/`tq` 别名自带结尾 `--`，命令里不要再带。
- VS Code 扩展：`npm run check` + `npm run test:contract`（assets / extension / package / i18n 契约）。
- 扩展 UI 双语（en + zh-cn）与术语：用户可见字符串一律走 `vscode.l10n.t()`（webview 走 `src/webviewI18n.ts` 字典，media 内置英文默认表），`scripts/i18n-test.mjs` 契约检查覆盖率与 bundle key 同步；术语译名以 `docs/glossary.md` 为准（Vanilla→原版、Mod→模组、sprite→图像）。服务端诊断消息暂保持英文；LLM-facing 字符串（`src/agent/`、languageModelTools 描述）不本地化。
- 全量 Vanilla sweep 是按风险运行的本地开发工具，不是提交、PR 或发布的门禁。必须显式传入要验证的 `--server`；脚本会核对服务器实际内嵌的规则哈希与当前 checkout，避免过期二进制产生假结论。报告只留在被忽略的 `performance-results/`，不得上传或提交。
- 黄金对拍：`PDC_UPDATE_GOLDEN=1 cargo test -p ide golden_gfx_sprite_semantics`。
- 规则改动后必须重烤并同步计数断言：`cargo run -p rules --bin bake -- build --source rules/eu4 --manifest rules/manifest.json` + rulec.rs 计数。
- 依赖更新策略：不使用 Dependabot / Renovate 等机器人（均已关闭，含安全警报）。CVE 由每周 `security.yml` 的 `cargo deny check advisories` 与 `npm audit --omit=dev` 定时扫描兜底，扫描亮红时立即定点升级受影响依赖并走 PR。例行新鲜度每月一次批量处理：根与 `fuzz/` 各跑 `cargo update`、`editors/vscode` 跑 `npm update`，本地 `cargo tools gates all` + `npm run check` + `npm run test:contract` 验证后走单个 PR；验证失败的依赖用 `cargo update -p <crate> --precise <旧版本>` 定点回退，不带进本批。GitHub Actions 引用按 SHA 锁定，人工按需升级。

## 3. 发布流程

- PR 的远端 `Conclusion` 是唯一合并权威；本地检查和 sweep 只提供开发反馈，不能替代它。
- 版本准备在普通 PR 中完成：同步 `Cargo.toml`、`editors/vscode/package.json` 与 lockfile，收编 `CHANGELOG.md`，运行受影响的本地分组，合并后等待 `main` 的 `Conclusion` 成功。
- 受保护的 annotated `vX.Y.Z` 标签启动发布。标签工作流只使用仓库自有输入，构建五个原生归档、五个 SHA-256 sidecar 和一个 VSIX，并在发布前严格校验十一项资产。
- Vanilla 文件、路径、摘录、诊断报告、指纹和 sweep 输出不得进入 Actions、PR、issue 或 Release。需要 sweep 的改动在本机运行，只在 PR 中写不含敏感内容的结论；发现的问题缩成仓库自有的最小回归 fixture。
- 手动步骤：从公开 Release 安装 VSIX 做干净 profile 冒烟，再上传 Marketplace，并记录公开链接与已知限制。
- 发布工作流出现暂态失败且标签尚未发布时，可按原标签重跑；如需修改代码或元数据，必须经新 PR 修复并顺延 patch 标签。受保护标签与已发布资产永不移动、删除或覆盖。
- 详细清单以 `RELEASING.md` 为准，职责与失败归属以 `docs/validation.md` 为准。

## 4. 平台与工具链事实

- 命令行语法按所在平台选择：Windows 侧一律使用 PowerShell 语法，WSL 侧一律使用 bash 语法；两侧都不写对方的专有语法。
- 有现代 CLI 工具可用时优先使用，避免冗长的 PowerShell 等价写法：
  - 文本或代码搜索用 `rg`，不用 `Select-String`。
  - 文件查找用 `fd`，不用递归 `Get-ChildItem`。
  - 读取文件用 `bat --style=plain --paging=never`。
  - 只读取文件片段时用 `bat --line-range START:END`。
  - JSON 处理用 `jq`；YAML 处理用 `yq`。
  - 结构化代码搜索与 AST 感知重构用 `ast-grep`。

## 5. CI 与本地诊断基础设施

- 生命周期职责固定：本地分组负责快速反馈；PR 与 `main` 的 `Conclusion` 负责合并；Security 与 Performance 工作流负责定时审计；标签工作流负责可再分发资产；干净 profile 与 Marketplace 是人工验收。不要用一个阶段的结果替代另一个阶段的授权。
- PR 自动驾驶：`.github/workflows/pr-autosync.yml` 对维护者本人名下的开放 PR 自动启用 squash auto-merge，并在 main 每次推进后用 `update-branch` 刷新全部分支、使 CI 针对最新 main 重跑；合入仍由必需检查 `Conclusion` 把关，冲突只记日志等人工解决。所有调用走 `AUTOMERGE_TOKEN` 仓库 secret（fine-grained PAT，Contents 与 Pull requests 读写、仅本仓库）——`GITHUB_TOKEN` 的推送刻意不触发其他 workflow，用它刷新分支会让 auto-merge 永远等不到 CI。
- Vanilla sweep 只比较和解释本地诊断 / 性能变化，不维护仓库指纹基线，也不作为 PASS/FAIL 发布契约。`--previous` 只生成辅助差异；工具错误、服务器失败和用户显式 `--fail-on` 仍会让本地命令失败。服务器真实规则哈希取自 `server_messages`，checkout 哈希取自 `rules/manifest.json`，两者不一致时拒绝继续。
- scripts 布局（`editors/vscode/scripts/`）：单词命名入口（diagnose / probe / compare / extension / package / host / smoke / sweep / mcp）+ lib 分工（options / workspace / overlay / diagnosis / report / client / sampler / paths / mcpServer / mcpTools / mcpBoot）；入口全是薄壳，sweep 直接 import 相位函数。改诊断或性能链路时先动 lib 再动入口。透明本地化的编解码已协议化：`crates/transcode` 是唯一实现，扩展经 `pdc/transcodeDecode`/`pdc/transcodeEncode` 请求委托（字节以 hex 传输），TS 孪生实现与 Rust↔TS 差分向量 harness 已退役（Rust 侧 corpus/matrix/fuzz 测试保留）。mcp.mjs 是 stdio MCP 服务器（手写 ndjson JSON-RPC，零 npm 依赖；工具清单运行时读 package.json 的 `languageModelTools`，工具整形与 `src/agent/tools.ts` 是行为孪生，改其一必须同步另一个）。
- 服务端用户目录按平台解析（权威在 `crates/game` 的 `UserPaths::platform`）：Windows 的 config 位于 **%APPDATA%\ParadoxCode（Roaming，不是 LOCALAPPDATA）**，缓存根位于 %LOCALAPPDATA%\ParadoxCode\cache；WSL/Linux 的 config 位于 `~/.config/paradoxcode/`，缓存根位于 `~/.cache/paradoxcode/`。
- npm 的 `--prefix … run` 传相对 `--server` 路径会以 editors/vscode 为工作目录解析而失败（用直接 node 调用或绝对路径）。
- sweep 冷启动协议：客户端对缺失的 vanilla 缓存放行（服务器端支持在显式缓存缺失时自动发现并原位重建）。
- GitHub 托管的 CI 不应依赖本机、游戏安装或持久缓存；可执行的远端证据必须来自仓库自有 source 与 fixture。远端工作流中出现 Vanilla 路径或 `sweep.mjs` 调用属于策略违规。

## 6. 本地调研资料约束

本地安装、mod 和资料快照只用于调研与人工验证；路径从用户配置、工具发现结果或当次任务的显式参数取得。不要把维护者用户名、绝对路径、硬件规格或安装位置写进仓库、CI、Release、公开报告或可移植性假设。

- EU4 原版真值以维护者合法安装为本地输入。优先读取 ParadoxCode 用户配置或运行 `cargo tools setup vanilla --game eu4` 完成发现，不假定 Steam / GOG / Epic 的固定目录。
- WSL 侧开发时，EU4 原版、EDG 与 wiki 快照仍在 Windows 盘，经 `/mnt/c/…` 只读访问；用户配置里的 `/mnt/c` 路径属于本地配置，不算入库。经 9p 挂载做全量扫描明显慢于原生路径，sweep 耗时按需权衡。
- EDG（EU4 中文模组“归墟之门”，`Entrance to the Desolate Ground`，创意工坊模组 ID：3047072888）可作为本地人工验证语料。
- **查询 EU4 wiki 资料优先使用维护者已有的 `eu4-wiki-encyclopedia` 本地快照**，位置由当次环境提供，不访问受 `_fs-ch-` JS 挑战保护的在线站。入口 `MODDING.md`、`INDEX.md|json`；正文位于 `md/<分类>/`，原始表格查 `raw/`，页面新旧看 frontmatter 的 revid / revision_ts。需要更新走快照仓库 `tools/` 的再生流程。API 坑：allpages 续传用 `apcontinue`（generator 用 `gapcontinue`）；新版分类字段是 `title:"Category:X"`；turndown-plugin-gfm 遇 `<caption>` 放弃转表格，须先剥掉。内容许可 CC BY-SA 3.0，仅本地参考。
