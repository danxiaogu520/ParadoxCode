# ParadoxCode 性能报告

- 日期：2026-08-05
- 基线提交：`3bd1622`（Defer Vanilla cache loading during startup）
- 测量机器：Windows 11 IoT Enterprise LTSC / AMD Ryzen 5 9600X（6C12T）/ 23.1 GB RAM
- 测量方式：release / bench 优化构建（`opt-level = 3`），全部在本机实测

## 1. 结论摘要

| 关注点 | 结果 | 评价 |
| --- | --- | --- |
| 引擎扫描/索引 | 2000 文件 ~20 ms，10000 文件 ~110 ms（约 11 µs/文件） | 优秀，近线性扩展 |
| 单文件磁盘刷新 | 与初始扫描同量级（~20–115 ms，被目录扫描主导） | 观察项，见 §3 |
| Overlay 编辑 | 8 µs | 优秀 |
| `pdx` / `pdx-ls` CLI 启动 | ~3 ms（`--version`/usage） | 优秀 |
| LSP 冷启动（spawn→initialize） | ~145–162 ms（历史） | 旧版 artifact 内嵌基线；当前需重新测量首次编译与 cache-hit |
| 打开文件→首个诊断 | ~210 ms | 符合设计（200 ms 防抖 + ~10 ms 计算） |
| **Vanilla 缓存后台加载** | **316 MB 缓存约 12.2 s，期间所有快照请求被排队** | **最大 UX 风险，见 §5** |
| 稳态查询 | hover 0.8 ms / documentSymbol 0.5 ms；**definition ~660–700 ms** | definition 异常偏高，见 §5 |
| LSP 峰值内存 | ~1.06 GB（含 Vanilla 索引） | 偏高，见 §5 |

## 2. 测量方法

- **引擎基准**：`cargo bench -p pdx-engine`（`benches/synthetic_workspace.rs`，harness=false）。生成 N 个 EU4 event 文件，依次测量初始扫描/索引、无变化全量刷新、单文件磁盘刷新、单 overlay 编辑。默认 2000 文件（3 次取稳定值），另加跑 10000 文件验证扩展性。
- **CLI 启动**：release 构建后对 `pdx.exe` / `pdx-ls.exe` 各跑 10 次 `--version` 计时。
- **LSP 端到端**：以真实 stdio JSON-RPC 驱动 release 版 `pdx-ls`（可提交脚本 `scripts/performance/lsp-e2e.mjs`），测量冷启动、didOpen→diagnostics、hover/documentSymbol/definition、didChange→diagnostics，并采样进程工作集。脚本默认生成一个临时的一文件工作区，不写入用户目录。
- 本报告这次数据使用了本机用户配置中已存在、且 `rule_hash` 与测试 binary 匹配的 316 MB `vanilla.pdxindex`；脚本会按 `--cache`/`PDX_PERF_CACHE`、平台用户 `config.toml` 的 `[games.eu4].vanilla_cache` 顺序解析。脚本自身不写 cache，但若传入的 cache 与测试 binary 的 `rule_hash` 不一致，LSP 会按 RFC 0003 的启动流程重建并事务性写回；只做只读测量时应先确认 hash 匹配或使用 cache 副本。

## 3. 引擎：synthetic workspace 基准

| 指标 | 2000 文件 | 10000 文件 |
| --- | --- | --- |
| 初始扫描/索引 | 20.5 ms | 110.3 ms |
| 无变化全量刷新 | 20.5 ms | 117.8 ms |
| 单磁盘文件刷新 | 20.3 ms | 115.3 ms |
| 单 overlay 编辑 | 0.008 ms | 0.008 ms |

- 单文件全流程成本约 **11 µs**（目录发现 + 读取 + 解析 + 降级 + 建索引）；5 倍文件数带来约 5.4 倍耗时，近线性。
- **观察项**：无变化刷新与单文件刷新耗时 ≈ 初始扫描。增量路径（shard 替换只重建受影响桶）本身很快，但刷新成本被"扫描全部文件并校验状态"主导（Windows 下列 1 万个小文件约 100 ms 属正常范围）。若希望无变化刷新显著低于初始扫描，需要 mtime 快速路径；当前行为正确但未体现跳过收益。
- Overlay 编辑（解析+降级+提交）为 8 µs，快照不可变共享设计生效。

## 4. CLI 与二进制

| 项目 | 数值 |
| --- | --- |
| `pdx --version` 启动 | 平均 3.8 ms（最小 2.8 ms，10 次） |
| `pdx-ls --version` 启动 | 平均 3.3 ms |
| `pdx.exe` 体积 | 25.1 MB |
| `pdx-ls.exe` 体积 | 26.4 MB |

这份历史基线测量的是 SQLite artifact 直接内嵌 binary 的版本；当前实现改为内嵌 JSON source、首次启动在用户 cache 生成 SQLite。新的冷启动、首次编译和 cache-hit 数据需要重新测量，不能继续把旧的 21 MB artifact 内嵌结论当作当前基线。

## 5. LSP 端到端（真实 JSON-RPC，release）

| 指标 | 数值 | 说明 |
| --- | --- | --- |
| 冷启动（spawn→initialize 响应） | 145–162 ms（历史） | 旧版 SQLite artifact 内嵌基线；当前需分别测量首次规则编译和 cache-hit |
| didOpen→首个 publishDiagnostics | ~210 ms | 其中 200 ms 是设计内防抖（`DIAGNOSTIC_DEBOUNCE`），实际计算 ~10 ms |
| **Vanilla 缓存后台加载** | **~12.2 s** | 316 MB 缓存；完成后发送 `window/showMessage` |
| 首个 hover（受上面阻塞） | ~12.0 s | 快照请求在 Vanilla 加载期间全部被排队（`lib.rs:588-593`） |
| documentSymbol | 0.4–0.6 ms | 稳态 |
| hover | 0.75–0.8 ms | 稳态 |
| **definition** | **657–701 ms（两次一致）** | 稳态，1 文件工作区 + Vanilla 索引下仍然如此 |
| didChange→publishDiagnostics | ~210 ms | 防抖主导 |
| 峰值工作集 | ~1.06 GB | 加载 316 MB Vanilla 缓存后 |

### 主要发现

1. **Vanilla 缓存加载阻塞首个交互（最高优先级）**。启动后约 12 s 内，hover/definition 等所有快照请求被延迟到缓存加载完成。Zed 用户打开文件立刻触发请求时，会感知到明显卡顿。这与"Defer Vanilla cache loading during startup"的方向相反——加载被推迟到了启动后，但一旦开始加载就独占事件循环语义。建议：加载期间用旧快照（或空 Vanilla 索引）服务请求，加载完成后再切换；或显著优化 316 MB 缓存的加载速度。
2. **definition 稳态 ~660–700 ms 异常偏高**。hover（0.8 ms）与 documentSymbol（0.5 ms）都很快，唯独 definition 慢两个数量级，且不是一次性冷开销（连续两次一致）。大概率是定义解析触碰了 Vanilla 符号/作用域索引的热路径。需要 profiling（perf / samply）定位，这是查询延迟优化的首要目标。
3. **峰值内存 ~1.06 GB**。316 MB SQLite 缓存展开成内存索引后翻了三倍多。对编辑器语言服务器偏重，需确认是否常驻（而不是仅加载期峰值）；若常驻，考虑按需加载或更紧凑的索引表示。
4. 诊断延迟（~210 ms）主要是设计内 200 ms 防抖，符合预期，不需要改动。

## 6. 未测量项与风险

- **真实 EU4 工作区扫描**：基准使用合成小文件；真实 Mod（含大量嵌套、历史文件、本地化）的扫描成本未测。合成文件单行内容，解析占比低，真实文件解析占比会上升。
- **`pdx check` / `pdx setup vanilla` 全量缓存构建**：未测（需完整游戏源，且 `auto_discovery_attempted=true` 会跳过自动发现）。建议以 `pdx setup vanilla` 构建 316 MB 缓存的耗时与内存作为基准回归指标。
- **Zed 扩展侧**：未测量（服务器拉起、查询配置转发等）。
- **fuzz 目标**：未纳入本报告（属正确性回归，非性能）。
- 单次机器测量，无历史基线可对比；本报告数字可作为后续优化（尤其 §5 发现 1/2）的对照基线。

## 7. 复现

前置条件：Node.js（支持 ES modules，建议 20+）、release `pdx-ls`（先执行下方构建命令），以及与当前内嵌规则匹配的 Vanilla `vanilla.pdxindex`。脚本默认从平台用户配置读取 cache；也可以显式传 `--cache PATH` 或设置 `PDX_PERF_CACHE`。找不到 cache 时会清楚报错；`--no-cache` 仅用于不含 Vanilla 的 smoke 测试，所得数值不能与本报告的 cache-backed 数字比较。默认工作区是临时 fixture；用 `--workspace DIR --document FILE` 可测指定工作区。Windows 通过 `tasklist` 采样工作集，其他平台使用 `ps`，不可用时报告降级而不会伪造数值。

```powershell
# 引擎基准（2000 文件；可用 $env:PDX_BENCH_FILES=10000 加跑）
cargo bench -p pdx-engine

# CLI 启动与体积
cargo build --release -p pdx-lsp
Measure-Command { target\release\pdx.exe --version }

# LSP 端到端（cache 默认来自用户配置；输出不自动写回报告）
node scripts\performance\lsp-e2e.mjs

# 若 cache 不在用户配置中，显式指定（PowerShell 示例）
$env:PDX_PERF_CACHE = 'D:\path\to\vanilla.pdxindex'
node scripts\performance\lsp-e2e.mjs
```

脚本支持 `--server`、`--workspace`、`--document`、`--cache`、`--project-config`、`--line`、`--character`、`--timeout-ms` 等参数；运行 `node scripts\performance\lsp-e2e.mjs --help` 查看完整列表。报告中的既有测量数字保持原实测值，不因脚本整理而重新估算或填充。
