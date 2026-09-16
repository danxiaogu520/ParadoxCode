# AGENTS.md — ParadoxCode 工作说明

给在本仓库工作的 coding agent 的持久说明：协作流程、验证职责、架构定案与平台事实。
条目多为“非显然、重查成本高”的结论，改动相关领域前先读对应小节。会变化的执行流程以
`docs/validation.md` 与 `RELEASING.md` 为准，本文件记录长期不变量和可移植的参考事实。

## 1. 与用户的协作流程（契约，必须遵守）

1. 用户以 idea 开场；先调研（代码 / 游戏文件 / 本地 wiki 快照）验证可行性与代价，不空谈。
2. 出选择题让用户决策，每个选项附区别与代价。
3. 综合选择出完整方案，用户确认后才实施。
4. **提交只在用户命令下进行**——实施完成停在未提交状态等指令；提交切分方式也由用户当场指定。
5. 琐碎同步（测试断言、typo、契约脚本跟随主改动的同步）可并入已确认方案的实现，不必单独发问。

## 2. 常用命令与门禁

- 本地检查聚合器：`cargo tools gates [core|core-fast|vscode|policy|artifact|fuzz|perf|all]`
  （无参 = `all`）。`all` 只表示默认确定性本地检查（不含 opt-in 的 `perf`），不代表可合并
  或可发布；完整职责表见 `docs/validation.md`。
  `.cargo/config.toml` 的 `tools`/`tq` 别名自带结尾 `--`，命令里不要再带。
- VS Code 扩展：`npm run check` + `npm run test:contract`（compile/smoke/extension/package/transcode 五契约）。
- 全量 Vanilla sweep 是按风险运行的本地开发工具，不是提交、PR 或 release 门禁。必须显式传入
  要验证的 `--server`；脚本会核对服务器实际内嵌的规则 hash 与当前 checkout，避免 stale
  二进制产生假结论。报告只留在被忽略的 `performance-results/`，不得上传或提交。
- 黄金对拍：`PDC_UPDATE_GOLDEN=1 cargo test -p ide golden_gfx_sprite_semantics`。
- 规则改动后必须重烤并同步计数断言：
  `cargo run -p rules --bin bake -- build --source rules/eu4 --output <tmp> --manifest rules/manifest.json` + rulec.rs 计数。

## 3. 发布流程

- PR 的远端 `Conclusion` 是唯一合并权威；本地检查和 sweep 只提供开发反馈，不能替代它。
- 版本准备在普通 PR 中完成：同步 `Cargo.toml`、`editors/vscode/package.json` 与 lockfile，
  收编 `CHANGELOG.md`，运行受影响的本地分组，合并后等待 `main` 的 `Conclusion` 成功。
- 受保护的 annotated `vX.Y.Z` tag 启动发布。tag workflow 只使用仓库自有输入，构建五个
  原生归档、五个 SHA-256 sidecar 和一个 VSIX，并在一次发布前严格校验十一项资产。
- Vanilla 文件、路径、摘录、诊断报告、指纹和 sweep 输出不得进入 Actions、PR、issue 或
  Release。需要 sweep 的改动在本机运行，只在 PR 中写不含敏感内容的结论；发现的问题缩成
  仓库自有的最小回归 fixture。
- 手动步骤：从公开 Release 安装 VSIX 做干净 profile 冒烟，再上传 Marketplace，并记录公开
  链接与已知限制。
- 发布 workflow 暂态失败且 tag 尚未发布时可原 tag 重跑；如需改代码或元数据，必须经新 PR
  修复并顺延 patch tag。受保护 tag 与已发布资产永不移动、删除或覆盖。
- 详细清单以 `RELEASING.md` 为准，职责与失败归属以 `docs/validation.md` 为准。

## 4. pdc 事件循环：新增 worker/命令的 6 处登记

漏任何一处都会产生难复现的响应丢失（漏 draining_shutdown 时 shutdown 响应后 reader 立即读入 exit、in-flight 命令响应静默丢失）：

1. `background_busy`（event_loop.rs 主循环两处计算）
2. `shutdown_owes_work`（deferred exit 的 replay 闸门）
3. `draining_shutdown`（loop 底部 reader-arm 闸门）——**最易漏**
4. exit-time cancel 列表（`ServerState::Exited` 块内逐 task cancel）
5. `$/cancelRequest` 传播
6. spawn 链插入 + document_events.rs 的 executeCommand 回退错误文案

集成测试用 frames() 一次性喂 initialize→executeCommand→shutdown→exit 全帧序列（参照 tests/format_command.rs）。事件循环新增决策点须同步加 `trace_decision` 行。

## 5. 契约同步清单

**missionPreview / preview 配置**（动 wire 字段或 `paradoxcode.preview.*` 时，三个脚本会红）：

- `editors/vscode/scripts/smoke.mjs` — 逐字段断言节点 payload
- `editors/vscode/scripts/extension.mjs` — requiredSettings 清单（含双语 nls 键）+ renderer marker 字符串清单
- `editors/vscode/test/suite/extension.test.js` — 配置默认值断言（CI 跑）

改完全局 grep 被删标识符（含 scripts/ 目录），再跑 `npm run check` + `npm run test:contract`。

**扩展侧诊断**：本地化 `LocalisationNotTranscoded` 只有客户端 pdcloc provider 一个报告者（服务端已退役）；auto-open 只重定向 escaped 文件，可读 CJK 的 replace/ yml 两侧静默是用户明示接受的行为。

## 6. 架构定案与红线

### 6.1 typed-reference membership 推断

- hir 对多 Type kind 的值位置**刻意不产出引用**（no-guess；disk-index 逐文件纯 lowering 拿不到全局成员表），membership 推断放在 ide 的 semantic_data 组装层（`collect_inferred_typed_references`，`hir.scope_fact_at` O(log n) 防 history 大文件 O(N²)）。
- **不可在 semantic_data 路径内构造 `DirectResolutionContext::new`**——它对每个 overlay 文档回调 semantic_data，无限递归。
- membership 成员集与 resolution.resolve 同源（都来自索引定义），唯一胜出 ⇒ 可解析。

### 6.2 本地化值级解码的位置

- 值级解码在 preview 派生处（`decode_value`），**不进 scan.rs ingest**：文档文本保持磁盘原样，跳转/hover 坐标永不错位，offset 映射问题整体不存在。
- 本地化全序：current mod > dependency（声明序）> vanilla；同层一律平等、按读取序后来居上（replace/ 天然靠后胜出）；版本号不参与。
- 本地化 .yml 文档**零诊断零补全**（用户裁定）：ide 侧双早退，命令误报/语法/转码 pass 全不发。

### 6.3 引号标量值的补全/hover 机制（.gfx 对齐时踩过，.gui 会再遇到）

- 补全上下文会把引号标量值折进 `container_property`（property=None、该键追加到 parent_path 尾部）——查"属性自己的规则"必须剥掉尾段再查（先例：`semantic_value_hover_at`、`texture_path_completion`）。
- 路径补全不能走 word_range（`is_word_byte` 不含 `/`），从标量起始引号到光标自算 prefix/replacement（先例：`dynamic_parameter_completion`）。
- 诊断发射链：值 matcher 的存在性覆盖要加在 `semantic_property_matches`（semantic.rs）；只加 `semantic_matcher_accepts` 不够（诊断/hover 的 valid 计算不走它）。
- vanilla `chatfonts.gfx` 根键是 `2-bitmapfonts`（数字前缀加载序约定）；bitmapfont 的 `color` 是叶子 ARGB 字面量（`0xffffffff`）；`textcolors` 在容器级与 bitmapfont 块内都可出现。

### 6.4 调试模式

- **客户端是唯一过滤点与全量追踪点**：过滤只作用于 appendLine；`updateVanillaContext` 靠解析 INFO 文本驱动，服务端绝不因 debug off 停发 INFO。
- vscode-languageclient v10 两处关键事实：`setTrace(sendNotification:false)` 不会给服务端发 `$/setTrace`（热切换时扩展自发 `client.sendNotification('$/setTrace',…)`）；`outputChannel`/`traceOutputChannel` 都要求 **LogOutputChannel**（`{log:true}` 创建）。
- 已知边界：`$/setTrace` 在 initialize worker 完成前被 deferred，verbose 开启前的决策不被追溯记录（测试钉住该语义）。

### 6.5 有意保留、不要"修"的点

- `package.json` 里 `"Localisation"/"localisation"` 双条目 = glob 大小写语义的保险。
- scripts 未引入 vscode-uri：node pathToFileURL 与 url crate 互通已验证，无行为缺口。
- `logical_path` 的回退链（`LogicalPath::parse` 兜底）意味着相对文档路径不是惰性的，测试 fixture 依赖此语义。
- `crates/ide/src/tests/completion.rs` 的 `globa` 是故意截断的补全探针（在 `_typos.toml [default.extend-words]` 白名单）；新增同类探针词要同步白名单。
- 归档/机器契约里的 pdc 字样（crate 名、`.pdcindex`、`pdcloc://`、`pdc/` 方法前缀、`PDC_*` 环境变量、诊断码 `pdc-*`）是有意保留，用户可见处才是 ParadoxCode。

## 7. 规则与游戏数据事实

- 改 rules/eu4 静态数据前先对照原版地面真值（EU4 安装见 §11）：`common/government_mechanics/*.txt` 内嵌 `powers = {}` 定义全部政府机制 power（37 个；reform_progress/church_power/splendor/karma/piety **不是** power）；`common/estates/*.txt` 顶层键带 `estate_` 前缀，修饰符家族用去前缀短名（`nobles_loyalty_modifier`，`monthly_<power>` 家族按 power 名自动生成）；`common/estates_preload/00_estate_modifiers.txt` 是 76 条特殊 estate 修饰符官方清单；`localisation/modifers_l_english.yml`（文件名就是拼错的）是 modifier 显示名真值。
- 任务预览 14 色全部定稿自原版 `interface/core.gfx`（全局 textcolors 11 色 + vic_18 bitmapfont 自带 G/R/Y 覆盖任务标题），contract 测试逐色钉死 hex；一张平表通吃双语。渲染全 TS（webview），Rust 只出纯文本 payload；§ 解析在 loc-format.js（UMD，webview+Node 测试共用）；中英字体不打包（版权）走 workshop 自动发现（`paradoxcode.preview.chineseFontMod` 可覆盖）；字体选择按内容 CJK 检测而非 l_english 头（EDG-KTP replace 文件头英文内容中文）；字形缺失回退链 本字体→另一字体→系统 fillText。

## 8. 平台/工具链事实

- VS Code hover 渲染图片三件套：data URI + `isTrusted` + `|width=N` 尾缀（单边另一边自适应）；`file://` 与自定义 scheme 在 hover 不渲染 → hover 贴图必须"服务端发名字、客户端解码拼接"。
- 字节级 fixture（`crates/transcode/tests/corpus/`）用 `.gitattributes` `-text` 钉住 CRLF；改 .gitattributes 后必须 `git add --renormalize`（stat 未变时直接 add 是 no-op）。GitHub windows runner 默认 autocrlf=true 会掩盖换行差异，只有 ubuntu leg 真实暴露。改 edg_ktp 对拍文件必须 master/release 两边同时重生成。
- ubuntu-latest 新镜像不预装 rustup：CI 里装 cargo 工具用 `${CARGO_HOME:-$HOME/.cargo}` + `mkdir -p`（`~` 在双引号内不展开）。
- 恢复备份用 copy2 保留旧 mtime 会让 cargo 误用旧测试二进制——验证前 touch 源码。
- pdcloc 盘符 URI（`pdcloc:///C:/...`）断言只在 Windows 成立（POSIX 保留前导 `/`），已 cfg(windows) 门控。
- Bash 工具层会吞反斜杠：heredoc python 处理含反斜杠文本必须用 Edit 工具或程序化构造。

## 9. CI 与本地诊断基础设施

- 生命周期职责固定：本地组负责快速反馈；PR/main 的 `Conclusion` 负责合并；Security 与
  Performance workflow 负责定时审计；tag workflow 负责可再分发资产；干净 profile 与
  Marketplace 是人工验收。不要用一个阶段的结果替代另一个阶段的授权。
- Vanilla sweep 只比较和解释本地诊断/性能变化，不维护仓库指纹基线，也不作为 PASS/FAIL
  发布契约。`--previous` 只生成辅助差异；工具错误、服务器失败和用户显式 `--fail-on` 仍会
  让本地命令失败。服务器真实规则 hash 取自 `server_messages`，checkout hash 取自
  `rules/manifest.json`，两者不一致时拒绝继续。
- scripts 布局（`editors/vscode/scripts/`）：单词命名入口（diagnose/probe/compare/transcode/extension/package/host/smoke/sweep）+ lib 分工（options/workspace/overlay/diagnosis/report/client/sampler/paths）；入口全是薄壳，sweep 直接 import 相位函数。改诊断/性能链路先动 lib 再动入口。transcode.mjs 含 Rust↔TS 差分向量对拍（74,549 条，`PDC_SKIP_VECTORS=1` 可跳）。
- 服务端 config 在 **%APPDATA%（Roaming）**，非 LOCALAPPDATA；cache 根在 LOCALAPPDATA。
- npm `--prefix … run` 传相对 `--server` 路径会以 editors/vscode 为 cwd 解析而失败（用直接 node 调用或绝对路径）。
- sweep 冷协议：客户端对缺失的 vanilla 缓存放行（服务器端支持显式缓存缺失时自动发现并原位重建）。
- GitHub-hosted CI 不应依赖本机、游戏安装或持久缓存；可执行的远端证据必须来自仓库自有
  source/fixture。远端 workflow 中出现 Vanilla 路径或 `sweep.mjs` 调用属于策略违规。

## 10. 性能燃烧教训（da43389 / 0.3.5 两案已修）

- 根因模式：**写 per-property 缓存 probe 时必须同时想清楚**——(1) token 是否接外层取消（内部 `CancellationToken::new()` + uncancelled 对诊断取消无效）；(2) 插入被 revision 闸门 drop 时会形成 miss→全量重建→再被丢的无限循环。
- 长跑后台任务判过期用 `AnalysisHost::live_revision`（克隆共享的 AtomicU64；派生 Clone 的 revision 字段是冻结值）。
- query cache 按域（Documents/Definitions/Index）跟踪 revision；dynamic 三报告缓存在 Definitions 域，host 按"文档 HIR 声明 dynamic 定义"推进 watermark——调用方按键不再失效。
- 诊断工具（gitignored target/ 下，可能被 clean）：`st_driver.py`（`--type 5 --interval 500 --post-idle 15` 复现燃烧；`--interval 2000` 不触发）、`dbg_driver.py`（读真实 settings、initialize 带 trace、等 pdc/ready、逐秒采样 CPU，产物 target/dbg-runs/）、`target/wrap/`（FFI 线程枚举 + 穷人栈扫描 + dbghelp；rustc PDB 必须按映像文件 base=0 加载再算 delta）。
- 0.3.5 修复后真实复测事实：EDG-KTP 工作区打字期 ~0.65–1.55 核、停手 2s 内归零；每按键全文件诊断 ~0.3 core-s（增量同步不省重诊断）；wswd pass ~0.5s/文件；冷缓存 ready 18.3s / 热 7.5s；合并索引 22500 文件、dynamic 定义 4517。易误判点：停手后 0 条 publishDiagnostics 可能只是字节相同批次去重（SuppressedIdentical），不是症状。
- 遗留未修：client 的 semanticTokens capabilities 只有 requests 形状而无 tokenTypes/formats 时，诊断发布会静默死掉（真客户端不受影响）；workspace-burn 的 4-worker pool 触发之谜无日志可考。

## 11. 本地调研资料约束（非项目门禁）

本地安装、mod 和资料快照只用于调研与人工验证；路径从用户配置、工具发现结果或当次任务的
显式参数取得。不要把维护者用户名、绝对路径、硬件规格或安装位置写进仓库、CI、Release、
公开报告或可移植性假设。

- EU4 原版真值以维护者合法安装为本地输入。优先读取 ParadoxCode 用户配置或运行
  `cargo tools setup vanilla --game eu4` 完成发现，不假定 Steam/GOG/Epic 的固定目录。
- EDG-KTP 可作为本地人工验证语料；它声明 workshop 依赖 3047072888。任何完整 mod 文件、
  扫描报告和物理路径都留在本机，发现的问题只以最小仓库自有 fixture 固化。
- **查 EU4 wiki 资料优先使用维护者已有的 `eu4-wiki-encyclopedia` 本地快照**，位置由当次
  环境提供，不访问受 `_fs-ch-` JS 挑战保护的在线站。入口 `MODDING.md`、`INDEX.md|json`；
  正文位于 `md/<分类>/`，原始表格查 `raw/`，页面新旧看 frontmatter 的
  revid/revision_ts。需要更新走快照仓库 `tools/` 的再生流程。API 坑：allpages 续传是
  `apcontinue`（generator 是 `gapcontinue`）；新版分类字段是 `title:"Category:X"`；
  turndown-plugin-gfm 遇 `<caption>` 放弃转表格须先剥掉。内容许可 CC BY-SA 3.0，仅本地参考。
- 中文字体自动发现当前选中 workshop 3047072888（归墟之门，捆绑与汉化基础包 md5 相同的 zh-hans-16.fnt，newest-mtime 选择，无害）；vic_18=218 字形/334 kerning，zh-hans-16=21563 字形/无 kerning。

## 12. EU4 中文本地化转码：硬事实

- 算法无码表纯算术（源自 matanki-saito/EU4dll `escape_tool.cpp`，MIT）：BMP 码元 → `[marker(0x10-0x13), low, high]` 三元组。**最终转义集 = Gist EU4 档 23 值**（22 基础 + 0x2F，含 0x23）；A/B 两档转义集相同。
- 形态 A（yml）= 转义字节经 CP1252→UTF-8 带 BOM（utf8eu4 档）；形态 B（脚本内嵌）= 原始单字节三元组、无 BOM（latin1eu4 档），encode 需 27 个 CP1252 字符单字节回写（其中 8 个落在 U+0100..U+0FFF——A 档拒绝、B 档例外接受，语料实证必需）。
- 两条铁律：正确性唯一裁判是 paratranz；绝不二次编码/解码（保存闸门拒绝）。
- 已知边界：0x100-0xFFF 与非 BMP 码点 encode 直接拒绝。
- 扩展侧是 TS 孪生实现（算法固定、Rust/TS 分开写 + 差分向量防护，见 §9）。

## 13. 悬案登记

- **hover "Allowed value types" 作用域分组**：方案已交付、用户明确"先不做"，待确认后动手。要点：`semantic_rule_hover_for_candidates` 按作用域分组（分组键用拆出的结构化 scope 数据，不解析自己拼的字符串）；allowed_scopes 全空成 any 组；一条规则声明多作用域列成一个组（`` `country`, `province`: … ``，此取舍待用户确认）；删 "at least 1"（两处 min 过滤阈值 >0→>1；诊断侧基数检查一概不碰）。**数据层前提**：effect.json 里 add_claim/core 家族四变体全声明 `['country','province']`（数据没编码语义随作用域切换，需拆完变体 allowed_scopes 三处才自动精确）；`has_claim` 整个规则集缺席需补；`required: true` 在 rules/eu4/semantic/ 出现 0 次（抑制分支是死代码）；min=1/max=1 是生成器标量默认基数。真值源在 rules/eu4/semantic/definitions/*.json + contexts/*.json。
- mod 根目录有一个 183MB 孤儿 `.pdxindex`（旧版 path-join bug 产物），可让用户删。
