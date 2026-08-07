# RFC 0011：测试、Fuzz 与质量门禁

- 状态：Current
- 适用版本：EU4 v0.1

## 当前测试

核心 Cargo workspace 的 crate 主要使用源码内 `#[cfg(test)]` 单元测试；当前覆盖面包括：

- `pdx-text`：offset、line index、UTF-8/UTF-16、URI/path；
- `pdx-parser`：Script/localisation CST、error recovery、增量 edit 等价性和 formatter 安全性；
- `pdx-rules`/`pdx-bake`：严格 JSON source、稳定身份、schema/foreign key、canonical hash、
  SQLite round-trip、embedded source 和 user-cache 校验；
- `pdx-game::eu4`：profile、bootstrap catalog、内嵌规则和 cache provider；
- `pdx-engine`：HIR、scope、source-root scan、overlay、shard replacement、Vanilla cache；
- `pdx-analysis`：diagnostics、completion、hover、definition/references、symbols 和 rename；
- `pdx-lsp`：真实内存 JSON-RPC transport、initialize/sync、UTF-16、capability、取消、过期
  diagnostics、watched files、Vanilla cache 和各语言请求；
- `editors/zed`：平台映射、checksum sidecar、tar/zip 受限解包和 executable cache 校验。

测试主要与各 crate 源码 colocated；LSP 的 transport 回归在 `pdx-lsp` 源码测试中直接驱动 framing 和消息生命周期，不是 handler mock。

Tree-sitter 的现有 corpus 只有 `grammars/tree-sitter-eu4/test/corpus/eu4.txt` 与
`errors.txt`。`scripts/check-grammars.sh` 运行 grammar generate/test，并运行
`pdx check grammar-fuzz` 的删除字符恢复检查。CSV grammar、CSV parser/formatter 测试、仓库
顶层 `tests/` 目录和顶层 analysis golden fixture 目录当前未实现。

## Fuzz targets

当前恰好有五个 `cargo-fuzz` target：

1. `parse-script`：Script parse、token 和 CST range 边界；
2. `parse-localisation`：localisation parse、CST 和 error range 边界；
3. `incremental-edits`：增量结果与 full reparse 的可观察结构等价性；
4. `format-script`：安全格式化、幂等性和非 trivia token 保留；
5. `lower-hir`：通用及 EU4 profile-aware HIR lowering 的范围和事实边界。

CI 当前用 nightly 构建全部 target，并只运行 `lower-hir` 的 `-runs=100` invariant smoke。没有
独立的 `line_index`、`parse_csv` 或 first-party rule-source fuzz target，也没有定时或长时间
fuzz workflow；这些均是当前未实现的质量面。

## Benchmark

唯一的 Cargo benchmark 是 `crates/pdx-engine/benches/synthetic_workspace.rs`，运行：

```text
cargo bench -p pdx-engine --bench synthetic_workspace
```

它默认生成 2,000 个原创 EU4 event 文件，测量 initial scan/index、无变化 refresh、单磁盘文件
refresh 和单 overlay edit；`PDX_BENCH_FILES` 可调整数量。benchmark 用于本机趋势观察，没有
跨机器绝对阈值，也没有定时 benchmark job。

## 当前 CI 与质量门禁

- Linux `core`：`cargo fmt --all -- --check`、locked workspace `check`/`test`、clippy
  `-D warnings` 和 `cargo doc --locked --workspace --no-deps`。
- Rust 1.97.1 MSRV：locked workspace/all-targets/all-features `cargo check`。
- Windows：locked workspace `check`、`test`、clippy `-D warnings` 和 release `pdx`/`pdx-ls`。
- grammar：Node/Tree-sitter generate、corpus test 与 `grammar-fuzz`。
- Zed：扩展 fmt、native test、`wasm32-wasip1` check/release build 和 clippy。
- fuzz：locked metadata、nightly target build、上述短 invariant smoke。
- release smoke：server build、Zed all-target check、规则 source temporary compile/round-trip、
  manifest/hash/checksum/game/schema 检查及无 `--rules` 检查。
- dependency policy：`cargo deny check advisories licenses bans sources`。

tag release workflow 另外在五个平台构建 `pdx-ls`，验证版本、archive/checksum 和完整矩阵后才
发布。workspace lint 禁止 `unsafe`；用户输入路径不以 `unwrap`/`expect` 逃逸错误。
