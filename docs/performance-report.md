# ParadoxCode 性能与复现

- 状态：Current measurement guide
- 范围：当前 engine benchmark、CLI 启动和真实 LSP transport 测量

本文不保存旧机器的固定数字，也不把单次测量当作跨平台保证。性能回归应在同一机器、同一规则 source 和同一工作区规模下比较。

## Engine benchmark

`crates/pdx-engine/benches/synthetic_workspace.rs` 生成原创 EU4 event 文件，测量：

- 初始目录扫描、parse/lower 和 index；
- 无变化的全量 refresh；
- 单个磁盘文件变化的定向 refresh；
- 单个 open-document overlay 编辑。

运行：

```text
cargo bench --locked -p pdx-engine --bench synthetic_workspace
```

默认生成 2,000 个文件；可用 `PDX_BENCH_FILES` 调整规模。该 benchmark 用于同机趋势比较，不设置跨机器绝对阈值。

## CLI 与 LSP

构建当前 server：

```text
cargo build --locked --release -p pdx-lsp --bin pdx --bin pdx-ls
```

LSP 端到端脚本 `scripts/performance/lsp-e2e.mjs` 使用真实 stdio JSON-RPC，覆盖 initialize、didOpen、diagnostics、hover、documentSymbol、definition 和 didChange，并可采样进程工作集。默认工作区是临时原创 fixture，不写入用户目录。

无 Vanilla cache 的 smoke：

```text
node scripts/performance/lsp-e2e.mjs --no-cache
```

使用 Vanilla cache 时通过 `--cache PATH` 或 `PDX_PERF_CACHE` 指定与当前规则 `rule_hash` 匹配的 cache：

```text
node scripts/performance/lsp-e2e.mjs --cache PATH_TO_VANILLA_PDXINDEX
```

也可以通过 `--workspace`、`--document`、`--project-config`、`--line`、`--character`、`--timeout-ms` 等参数测量指定工作区；运行 `--help` 查看完整列表。脚本不会把用户 Vanilla 文件或 cache 写入仓库。

## 需要观察的指标

- scan/index 总耗时和每文件耗时；
- 无变化 refresh 是否被目录 metadata 检查主导；
- overlay edit 的 parse/lower/commit 次数；
- initialize 到首个 diagnostics 的延迟；
- Vanilla cache load/rebuild 的耗时和峰值内存；
- completion、hover、definition、document symbol 的稳态延迟；
- 取消和过期结果是否按版本门丢弃。

当前代码的优化边界是 per-file `FileState`/shard replacement、不可变 snapshot 共享和可取消 worker。性能测量不能把 Vanilla 游戏文件、用户 cache 或外部规则 source 提交到仓库。
