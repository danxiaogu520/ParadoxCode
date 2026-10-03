# 性能测量

性能工作流已迁入 Rust [tools](../../crates/tools/README.md#performance-measurements)。
从仓库根目录运行 `cargo run --locked -p tools -- perf --help`。

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
