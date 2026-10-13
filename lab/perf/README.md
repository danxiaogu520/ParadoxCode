# 性能测量

性能工作流已迁入 Rust [tools](../../crates/tools/README.md#performance-measurements)。
从仓库根目录运行 `cargo run --locked -p tools -- perf --help`。

0.5.1 后的性能修复覆盖规则身份复用、工作区符号查询、补全候选筛选和任务树排版。
补全基准启用生产 Rules IR 并要求实际产生候选。任务树排版基准测量 JavaScript 计算，
真实 GPU 帧率需要在编辑器宿主中另行测量。

冷索引深度优化以上一轮修复后的冻结二进制为基线，对 8,687 个 Vanilla 文件进行三轮
顺序配对：扫描中位数由 16,515 ms 降至 6,150 ms，含缓存写入的总耗时由 19,131 ms
降至 8,760 ms。完整索引内容一致（仅生成时间不同），补全与任务树的 LSP 响应一致。
测量使用全新索引路径，未清空操作系统文件缓存。详细记录、二进制和语料相关证据
保留在忽略的本地输出目录；仓库仅记录汇总结论和自有回归测试。

| 命令 | 职责 |
| --- | --- |
| `perf bench` | 预热、重复基准、原始日志和中位数 |
| `perf baseline / ab` | 冻结二进制、基线和阈值比较 |
| `perf sweep` | 全量语料、查询电池、阶段和进程资源 |
| `perf probe / compare` | 真实 LSP 生命周期、打开/编辑发布、hover 与补全延迟 |
| `perf memory` | 顺序运行 mem_probe，记录被测子进程峰值、各阶段 RSS 和诊断签名 |
| `perf profile` | 调用 samply 或 Linux perf |
| `perf init / status / import-corpus / control` | 环境检查、语料导入和 native 对照组 |

## 内存配对

`mem_probe` 保留在 engine 示例中：`cargo build --locked --release -p engine --example mem_probe`。
`perf memory --require-identical-diagnostics` 核对每次测量的诊断文件数、数量和 digest。
精确峰值使用 Unix time 的 wait4；LSP 的周期 RSS 是独立的采样指标。缺失采样会显式保留。

机器配置使用忽略的 `target/perf/config.local.toml`，参数优先。环境变量仍可选择本地输入；
原 Shell 配置不再执行。导入的语料留在 `data/`，测量、缓存、画像和对照组构建留在 `target/`。
冷测量创建独立的新缓存，构建时间单列为 `cache_build_ms`，保留已有缓存。

诊断正确性归[语义审计](../audit/README.md)。远端 [Performance 工作流](../../.github/workflows/performance.yml)
只测仓库自有 benchmark。平台依赖、具体命令和统计边界见 tools 指南。
