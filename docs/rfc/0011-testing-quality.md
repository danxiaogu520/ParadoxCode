# RFC 0011：测试、Fuzz 与质量门禁

- 状态：Accepted
- MVP：EU4 v0.1

## 测试层次

### Unit tests

每个 crate 测试自己的不变量：

- `pdx-text`：offset、UTF-16、line endings、URI/path
- `pdx-parser`：typed CST、error extraction、revision-safe edit update
- `pdx-rules`：SQLite schema、只读加载、immutability、派生查询 index、canonical logical hash
- `pdx-bake`：strict source decoding、stable identity、invariant、artifact round-trip 和 manifest
- `pdx-hir`：context lowering、scope transition
- `pdx-engine`：root order、overlay、shard replacement
- `pdx-analysis`：feature query
- `pdx-parser`：trivia safety、idempotence

### Grammar corpus

每个 grammar case 保存 input 与期望 S-expression/结构。目录按主题划分：

```text
basic
operators
mixed-blocks
comments
strings
headers
parameters
errors
localisation
csv
```

从 Jomini 学到的特殊构造必须有独立 case。测试内容由 ParadoxCode 原创，不复制游戏文件。

### Golden tests

analysis fixture 使用小型自有 Mod workspace：

```text
vanilla/
dependency/
mod/
open-documents/
vanilla-cache/
expected/
```

golden 输出使用稳定、可读格式记录 diagnostics、completions、definitions、references、hover 和 rename plan。更新 golden 必须人工审查。

### LSP integration

以内存 transport 发送真实 JSON-RPC/LSP 消息，测试 capability fallback、版本、取消和结果转换。禁止只对 handler 内部函数做 mock 后称为 integration test。

### Zed smoke test

CI 能验证 extension manifest 和 Wasm build；文件识别、dev install、server download 和真实编辑体验保留一份发布前人工 checklist。若 Zed 提供稳定 headless harness，再转为自动测试。

## Fuzz targets

MVP 至少包含：

1. `parse_script(bytes)`
2. `parse_localisation(bytes)`
3. `edit_updates(seed, edit_sequence)` 与 full reparse 等价性
4. `typed_cst_walk(bytes)`
5. `hir_lower(bytes, minimal_rule_db)`
6. `format(bytes)` 的 idempotence 与 token preservation
7. `line_index(text, positions)`
8. `load_first_party_rules(source_tree)` 与 `compile_rules(source_tree)`
9. `parse_csv(bytes, dialect)`

不变量：

- 不 panic、越界或无限循环。
- 输出 tree/range 均在 source 范围内。
- 编辑更新的可观察 syntax tree 与 full parse 一致。
- formatter 不改变非 trivia token 序列。
- 输出大小和运行时间有合理上限。

发现的 crash 输入在修复后进入 regression corpus。CI 短时间运行 fuzz smoke，定时工作流运行更长 fuzz job。

## 性能测试

建立可再分发的 synthetic/原创 corpus，记录：

- cold workspace scan
- parse throughput 与单次 edit latency
- HIR lowering
- shard replacement
- completion/definition latency
- memory high-water mark

benchmark 结果用于发现回退，不在 MVP 早期设不现实的绝对吞吐承诺。

当前可再分发基准位于 `crates/pdx-engine/benches/synthetic_workspace.rs`，默认生成 2,000 个原创 EU4 event 文件并分别测量 cold scan/index、无变化全量刷新、单磁盘文件变化刷新和单 overlay 编辑。运行：

```bash
cargo bench -p pdx-engine --bench synthetic_workspace
```

可用 `PDX_BENCH_FILES` 调整文件数。2026-07-20 的开发机基线为 23.769 ms、14.222 ms、12.891 ms 和 0.004 ms；这些数字只用于同机趋势比较，不是跨机器验收阈值。计数回归另行断言一次 overlay 编辑恰好 parse/lower 各一次、stage/commit 不重复语义工作，并且全部磁盘 `FileState` 保持共享。

## CI 门禁

每个 pull request：

- `cargo fmt --check`
- clippy，workspace/all targets，warnings denied（第三方生成代码可显式例外）
- unit/integration/doc tests
- Tree-sitter corpus tests
- committed `eu4.pdxrules` schema/invariant validation
- manifest `rule_hash` 与数据库 canonical logical content 一致
- logical hash stability：插入顺序、SQLite index、VACUUM 和物理重建不改变 hash
- runtime loader smoke test
- Zed extension manifest/build check
- dependency license/advisory policy
- fuzz target compile 与短 smoke

定时任务：

- 长 fuzz
- 性能基准与趋势
- 跨平台 Windows/Linux/macOS build

## 代码质量

- 核心公开 API 有 rustdoc。
- 单文件出现多个变化原因时拆模块，不以固定行数作为唯一标准。
- `unsafe` 默认禁止；若第三方 binding 需要，封装在最小边界并记录 safety contract。
- 用户输入路径不得使用 `unwrap/expect`。
- diagnostic code、symbol kind 和 schema rule id 必须稳定。
- 所有 background task 支持取消或有明确的短时上限。

## Fixture 与版权

- 不提交 Vanilla EU4 文件或用户本地 Vanilla 索引缓存。
- 不提交外部规则 source tree；compiler unit test 使用最小原创第一方 JSON fixture。
- 不把参考仓库 corpus 复制进项目；需要同类 case 时编写最小原创样例。
- 引用外部行为或设计时在文档中记录项目和 commit。
- 任何第三方数据导入都必须先确认许可证和再分发条件。

## 发布门槛

`v0.1.0` 发布前必须满足：

- Phase 1–6A 的退出条件全部通过。
- 无已知 parser/formatter crash。
- 权威 Eu4Rules 声明的每个可支持文件类别都有 classification/parse fixture，所有主要 type family 有语义 fixture。
- 一次性 bootstrap import report 中没有未经批准的 unsupported/ignored construct。
- 提交的数据库通过 schema、foreign key、stable-id 和 `rule_hash` validation。
- rename 不写只读 source root。
- Zed 安装和启动在支持平台完成 smoke test。
- 文档与实际 initialize options、EU4 rules schema 一致。
