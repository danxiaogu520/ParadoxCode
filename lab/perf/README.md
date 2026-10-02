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

Vanilla 的逐条 error 审查使用 `audit-errors.py`，不要求与旧版错误数量对齐。
它检查完整报告和冻结 manifest，保存每条诊断的稳定身份、上下文、原安装及 DLC
资源证据和待确认状态；支持与实际资源解析器一致的 `.tga`/`.dds` 回退。
人工决定必须按诊断身份提供解释与证据；过期决定、报告不完整及错误总数不匹配均拒绝导出。
输出只允许放在忽略的 `performance-results/` 中。

```sh
python3 lab/perf/audit-errors.py \
  --report performance-results/phase5/selected-run/project-TIMESTAMP.json \
  --manifest performance-results/phase5/binaries/selected-manifest.json \
  --installation /path/to/installed/game \
  --output performance-results/phase5/semantic-review
```

阶段五规则切换的语义对照使用 `compare-rules.py`，详见
[本地验收记录](../../docs/phase5-validation.md)。这是跨平台的只读报告比较工具；
definition/reference 计数来自完整 SQLite 缓存，包含延迟加载的引用。
它校验语料和规则身份，标记截断的补全探针，不会自动批准语义差异。
输出强制位于忽略的 `performance-results/` 中。

每个固定补全探针还使用相同的 17 个确定性前缀采样。脚本按 LSP 的 UTF-16 位置
替换完整单词，采样后恢复原文；比较报告分别列出每个前缀的增加、移除和截断情况。
这些是代表性样本，不能证明枚举了整个候选域。基础探针或采样仍被 512 上限截断时，
即使两边数量相同，也仍标记为未验收。

`completion-inventory.py` 可以在指定源码快照上构建临时 harness，直接调用实际 IDE
补全入口，导出固定探针在 LSP 的 512 上限之前的全部候选标签及插入文本、替换范围、kind/detail。
独立 harness 的构建戳不同时，按缓存记录的源目录重新索引，不绕过生产缓存门禁。
它校验规则指纹、新版基线的 golden 输入 SHA-256、输出项数和连续序号，
并要求前 512 项的标签集合与冻结的 LSP 结果一致。
这完整覆盖这些固定位置的 IDE 输出；集合差异仍需审查，也不代表覆盖所有脚本位置。

只验收补全时，可在 `baseline.mjs` 上使用 `--completions-only`。
该模式加载指定 Vanilla 缓存并获取符号上下文和探针，不执行全量文本诊断；
输出使用 `paradoxcode-completion-baseline` kind 和显式 coverage，不生成 full diagnostic report。

```sh
python3 lab/perf/completion-inventory.py \
  --repo /path/to/selected/source --mode ir \
  --cache performance-results/phase5/ir-subtypes-acceptance.pdcindex \
  --baseline performance-results/phase5/ir-subtypes-acceptance/baseline-latest.json \
  --output performance-results/phase5/completion-inventory-ir-latest
```

`memory-pairs.py` 顺序运行三组独立 `mem_probe` 配对。每组使用 `wait4` 读取被测
子进程自身的 OS 峰值；遇到探针的 `PHASE` 标记时，只查询该子进程 PID 的 RSS，
包含缓存清除和宿主释放后的阶段。若系统不允许读取 RSS，报告记录采样失败，不能
把缺失值当作零。该脚本需要支持 `wait4` 和 `ps` 的 Unix 环境，OS 压缩与分配器
保留仍会影响 RSS；诊断工作量不同的计时不能直接当作性能结论。
报告同时保留完整诊断阶段的耗时、文件数、诊断数与 digest。优化同一版本的规则时，
使用 `--require-identical-diagnostics` 要求所有运行的文件数、诊断数、digest 一致；
缺少摘要或发生漂移会非零退出。旧规则与新规则的比较允许语义差异，需另外解释。

```sh
python3 lab/perf/memory-pairs.py \
  --before /path/to/frozen/legacy/mem_probe \
  --after performance-results/phase5/binaries/ir-mem-subtypes-acceptance \
  --output performance-results/phase5/memory-subtypes-pairs --repeat 3
```

同规则的性能优化对照：

```sh
python3 lab/perf/memory-pairs.py \
  --before /path/to/frozen/ir-before/mem_probe \
  --after /path/to/frozen/ir-after/mem_probe \
  --output performance-results/phase5/performance-same-rules-pairs \
  --repeat 3 --require-identical-diagnostics
```

- 基线二进制副本在规则哈希演进出 checkout 后无法再被 sweep 接受（sweep 的防呆设计）；
  其历史使命由当时记录的 `sweep-summary.json` 承担。
- cwtools 的 EU4 `.cwt` 配置自身带少量规则告警，且两工具诊断语义不同——对照只看吞吐量级。
- 画像 sweep 尾部样本可能因进程被 kill 丢失；优先画像 bench。
- `data/` 语料是 vanilla 的文本子集（无二进制资源）：绝对数字与完整安装略有出入，
  但两边工具看同一棵树，对比公平性不受影响。
