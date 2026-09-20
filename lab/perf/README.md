# ParadoxCode 性能实验室（lab/perf）

ParadoxCode 的本地性能优化循环工作流。**脚本与文档入库公开；所有运行产物（基线、报告、
语料副本、对照组二进制、机器路径覆盖）都被 gitignore，绝不进入 CI、PR 或 Release**——这与
`docs/validation.md` 对 Vanilla 数据的处理约束一致。

## 布局：什么入库，什么本地

| 路径 | 入库 | 内容 |
| --- | --- | --- |
| `lab/perf/perf.sh`、`config.sh`、`README.md` | ✅ | 工作流脚本、可移植默认配置、本手册 |
| `lab/perf/config.local.sh` | ❌ | 机器本地覆盖（对照组来源仓库与规则路径等） |
| `lab/perf/bin/` | ❌ | native 对照组二进制（本地构建的 cwtools） |
| `lab/perf/runs/`、`baselines/`、`profiles/` | ❌ | 单次运行记录、基线快照、画像产物 |
| `data/`（整个目录） | ❌ | 游戏语料的本地副本（见下） |
| `performance-results/`（仓库根） | ❌ | `sweep.mjs` 的默认输出根（沿用仓库既有约定） |

`data/` 内是**本地持有的合法游戏数据副本**：EU4 vanilla 文本数据（`data/vanilla`）与
EDG 归墟之门模组语料（`data/mods/entrance_to_the_desolate_ground`）。只搬 LSP 会解析的
文本类别（以 `crates/game/src/eu4/mod.rs` 的权威分类为准：`.txt/.gui/.gfx/.asset/.sfx/
.json/.lua/.yml/.yaml`，外加 `.mod` 描述符），不搬 exe/dll、图片音频等二进制。不上传、
不入库、不进任何远端工作流。

## 公平性设计（实验组 vs 对照组）

对照组是 [cwtools-rs](https://github.com/MillenniumDawn/cwtools)。实验室的一切**执行与写入都留在
WSL/Linux 侧**：不运行 Windows 二进制、不向 Windows 侧写任何文件；对 `/mnt/c` 只有只读的
源访问（`import-corpus` 搬语料、对照组从参考 checkout 读源码构建）。公平比较的三个对齐
条件，缺一不可：

1. **平台统一**：`control` 用 **Linux native 构建**的 cwtools（`control --build` 后收在
   `lab/perf/bin/cwtools`），与 ParadoxCode 同为 Linux 进程。
2. **文件系统统一**：两边都读 `data/vanilla`（WSL ext4 原生盘），排除 9p 挂载 `/mnt/c`
   的系统性慢读。
3. **语料统一**：同一棵 vanilla 树（`import-corpus` 的清单写入 `data/manifest.json`）。

在此前提下对照仍只是**量级参照**，不是公平竞速：两边规则集、诊断语义、工作内容都不同。
它回答"我们的绝对耗时是否还在合理数量级"，不回答"谁更快"。

## 快速开始

```bash
cd lab/perf
./perf.sh init --install      # 自检 + 补装 hyperfine / perf 等系统依赖
./perf.sh import-corpus all   # 搬运 vanilla + EDG 文本语料到 data/（约 170MB）
./perf.sh control --build     # 构建并安装 native 对照组二进制（首次约 1-5 分钟）
./perf.sh status              # 语料 / 对照组 / 基线 总览
```

机器本地路径（对照组来源仓库 `CWTOOLS_REPO`、`.cwt` 规则目录 `CWTOOLS_RULES`）写在
`config.local.sh`（模板见 `config.sh` 注释）。语料来源默认取标准 Steam 安装/workshop
路径，可用 `PDC_VANILLA_ORIGIN` / `EDG_WORKSHOP_SOURCE` 覆盖。

## 命令手册

`./perf.sh help` 有一页速览。

### baseline —— 建基线

```bash
./perf.sh baseline main-20260920            # 完整基线（默认基准单遍 + 全量 sweep）
./perf.sh baseline stable-x --repeat 3      # 稳定模式：基准跑 3 遍取中位数
./perf.sh baseline quick-x --skip-sweep     # 只要基准
```

产物在 `baselines/<name>/`：release 二进制副本、`bench.tsv`（基准指标）、
`sweep-summary.json`（全量 sweep 快照）、`metadata.json`（提交/分支/dirty、二进制 SHA、
规则哈希、**语料路径**、`bench_repeat`）。语料或规则变更后旧基线不再可比，重建即可
（`--force` 覆盖同名）。`baselines/current` 符号链接指向最新基线。

### bench / ab —— 基准与 A/B

```bash
./perf.sh bench --repeat 3                      # 稳定模式取数
./perf.sh bench -- -p engine --bench index_cache
./perf.sh ab                                     # 当前工作区 vs current 基线
./perf.sh ab stable-x --repeat 3 --fail-over 10  # 稳定模式；任一指标回退>10% 则非零退出
./perf.sh ab --bench-only                        # 只碰引擎内环时跳过 sweep
```

基准口径：**跑两遍取第二遍**（首遍预热，编译后的冷偏斜实测可达 10%+）；`--repeat N>1`
再叠加 N 遍取每指标中位数。报告按 |Δ%| ≤ `NOISE_PCT`（默认 3%）标 `≈`（噪声）或 `!`
（值得关注）；诊断面漂移（files/diagnostics 计数变化）会显式提示"计时不可比"。

**指标稳定性分级**（零改动 A/B 实测）：

- 稳定（±1% 内）：`index_cache` 的 dense/mixed 系列、`synthetic_workspace` 的刷新类指标
  ——数百 ms 量级，**决策看这些**。
- 抖动大（±20% 也出现过）：亚 10ms 微指标（`mission_preview` 热态、`one overlay edit`）
  ——只看趋势，别当证据。

### sweep —— 全量端到端

```bash
./perf.sh sweep --label try-mmap
./perf.sh sweep --cold --label cold-cache      # 冷跑（先手动删 .pdcindex，脚本只校验不代删）
./perf.sh sweep --previous baselines/<name>/sweep-summary.json
```

指标面：`phases.scan/session_boot/classify/diagnose/query_samples/total_ms`、
`server_phases.rules_ready/source_roots_ms`、`resources.peak_working_set_bytes`、诊断计数。
语料切换（如从原安装换到 `data/vanilla`）后首次 sweep 会重建 `.pdcindex`（冷跑），之后命中
缓存（热跑）——两种状态的数字不要混在一条曲线里。

### control —— 对照组

```bash
./perf.sh control                    # native：lab/perf/bin/cwtools，读 data/vanilla
./perf.sh control --runs 8 --warmup 2
./perf.sh control --timings-only     # 只要 CWTOOLS_TIMINGS 相位
./perf.sh control --build            # 重建二进制（CWTOOLS_REPO 有更新时）
```

每次记录 hyperfine 的 min/mean/max/stddev、`[t] load / validate-config / validate-loc`
相位、报告规模，追加到 `runs/control-history.tsv` 看跨会话趋势。注意：cwtools 以 linter
语义退出（1 = 存在 Error 级诊断，属正常，脚本已适配）。

### import-corpus —— 语料搬运

```bash
./perf.sh import-corpus all          # vanilla + EDG
./perf.sh import-corpus vanilla --prune   # --delete 同步：源里删了的本地也删
```

过滤器与 `crates/game` 的扫描分类保持一致；`data/manifest.json` 记录来源、时间、文件数与
字节数。**语料变更会使既有基线的 sweep 数字不可比**——重要对比期间不要更新语料。

### profile —— 采样画像

```bash
./perf.sh profile bench:index_cache          # 首选：无头复现热路径
./perf.sh profile sweep                      # 整场 sweep（server 经 shim 包进采样器）
./perf.sh profile bench:index_cache --flavor perf
```

samply 优先（`cargo install samply`，产物可拖 Firefox Profiler），否则 perf（Ubuntu 的
`/usr/bin/perf` 包装脚本会因 WSL2 内核版本拒跑，脚本自动改用
`/usr/lib/linux-tools/<ver>/perf` 实体二进制）。仓库自身的 `PDC_TRACE=<path>` 可记录 LSP
帧级往返，作为补充。

## 标准循环

```
① ./perf.sh baseline <name>（参照系）
② ./perf.sh profile bench:<热点>（定位：hotspots.txt / Firefox Profiler）
③ 分支上实现优化（行为变更走 PR，见 AGENTS.md）
④ ./perf.sh ab <name> --repeat 3（验证：目标指标 ↓，无 ! 回退；诊断面不漂移）
⑤ ./perf.sh control（大改动时校准绝对量级）
⑥ 合并后回 ①，以新 main 重建基线；周度趋势由远端 performance.yml 负责
```

## 平台事实（WSL 侧，踩过的坑）

- 实验室只在 Linux 侧执行与写入：不跑 Windows exe，不做 `wslpath` / `WSLENV` 这类跨系统
  操作；`/mnt/c` 仅只读（搬语料、读构建源码），且 9p 慢读已由 `data/` 本地化绕开。
- hyperfine 的每个位置参数是一条独立命令：带参数命令需整体 `%q` 转义为单个字符串。
- Ubuntu 的 `/usr/bin/perf` 包装脚本会因 WSL2 内核版本拒跑，用
  `/usr/lib/linux-tools/<ver>/perf` 实体二进制（脚本已自动选择）。

## 与仓库既有设施的关系

| 设施 | 关系 |
| --- | --- |
| `cargo tools gates perf` | 本 lab 跑的就是这条基准命令，外加解析/快照/对比层 |
| `editors/vscode/scripts/sweep.mjs` | 直接调用（显式 `--server`、`--output`），不改动它 |
| `docs/validation.md` 职责表 | 本 lab 全部是"开发者反馈"，不是任何门禁；合并权威是远端 `Conclusion` |
| 周度 `performance.yml` | 远端趋势线（仓库自有 fixture）；本 lab 管本地真实语料的深入分析 |

## 已知限制

- 基线二进制副本在规则哈希演进出 checkout 后无法再被 sweep 接受（sweep 的防呆设计）；
  其历史使命由当时记录的 `sweep-summary.json` 承担。
- cwtools 的 EU4 `.cwt` 配置自身带少量规则告警，且两工具诊断语义不同——对照只看吞吐量级。
- 画像 sweep 尾部样本可能因进程被 kill 丢失；优先画像 bench。
- `data/` 语料是 vanilla 的文本子集（无二进制资源）：绝对数字与完整安装略有出入，
  但两边工具看同一棵树，对比公平性不受影响。
