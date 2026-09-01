# 性能优化交接文档

> **分支**: `perf/strictly-below-cwtools`
> **日期**: 2026-08-30（2026-09-01 更新：六项已实施并测量）
> **目标**: pdx-ls 的 CPU 与内存全面严格低于 cwtools-rs
> **语料**: 创意工坊模组 3047072888 (7,907 脚本文件, 49MB txt + 15MB yml) + EU4 1.37 原版

---

## 一、2026-09-01 实施结果（本轮）

全部优化保持**诊断输出逐字节不变**（157,655 条，含消息中的规则溯源文本）。

| 提交 | 内容 | 单线程诊断 pass |
|---|---|---|
| `71a605b` | RuleSet FxHashMap 索引 + effect/trigger 上下文预计算 + GameProfile 作用域查找表 | 157.3 → 148.1s* |
| `f492e75` | 64 分片对齐驻留池 + 索引身份访问器 | （并入上行测量） |
| `f4e5e71` | ContextRuleView（合并视图 + 精确键桶 + **预分组 alternative**）+ ScriptProperty/ScopeContext/parent_path 全链路 `Arc<str>` + 引用分区 + membership 集合 | 148.1 → 74.4s |
| `cf465ff` | 嵌套 BTreeMap 定义查找（零分配探测）+ 查询缓存 FxHashMap/RwLock | 74.4 → **59.7s** |

\* 分阶段数字来自当轮测量（机器状态有 ±3s 波动）；同日基线 157.3s。

### 关键测量（最终）

| 指标 | 本轮前 | 本轮后 | cwtools-rs（交接基线） |
|---|---|---|---|
| 单线程诊断 pass（7,907 文件） | 157.3s | **59.7s（-62%）** | — |
| head-to-head LSP 全程 wall | ~72s | **50.9s** | 3.0s（热） |
| head-to-head LSP 全程 CPU | ~285 CPU-s | 214 CPU-s | 14.5 CPU-s |
| `pdx/ready` | ~16.3s | 9.8s | — |
| 峰值内存 | 2.84 GiB | 2.83 GiB | 0.30 GiB |

### 本轮已实施的技术（按收益排序）

1. **alternative 预分组**（`semantic_selected_alternative` 曾占诊断 CPU 的 45–54%）：
   每上下文构建 `ContextRuleView`，预计算合并源（上下文 + 继承 + `root:type`）、
   `exact_by_key` 精确键桶、非精确列表、空路径顶层成员、以及**完整的 alternative 分组**
   （组 id 驻留 `Arc<str>`、组员索引、精确键发现桶）。EU4 的 effect/trigger 各有 ~1850 条
   alternative 规则，原实现每容器扫描全量规则并以 SipHash `HashMap<&str>` 分组——
   这是最大单项。
2. **验证管线字符串驻留**：`ScriptProperty`（key/scalar/bare/operator）、`ScopeContext`
   寄存器、`parent_path` 全部 `Arc<str>`，经 64 分片进程池驻留；转移目标、子路径克隆
   变引用计数。
3. **零分配成员判定**：`workspace_member` 从 `format!` 键 + 全局缓存探测改为每修订
   eager 的 `(kind, 折叠名)` 计数集合（Index 域）+ overlay 隐藏计数（Documents 域），
   正确处理 overlay 打开/关闭的失效。
4. **每键查找免合并**：`semantic_rules_for_container_key` 不再每次调用做
   collect/extend/sort/dedup，直接从视图取桶（保序：单源 exact-first、合并源 id 序）。
5. **验证循环微整**：`semantic_selected_transition` 复用（原来同参数调用两次）、
   子块按引用分区（原来 `.cloned().partition` 深拷贝子树）、计数改线性扫描、
   trigger/effect 判定外提。
6. **索引/缓存**：定义嵌套 BTreeMap 零分配探测、查询缓存 FxHashMap + RwLock（并行
   worker 读锁共享）、`resolve()` 无 overlay 时跳过探测。

### 教训（更新）

- 容器规则列表全扫描过滤**不**优于精确键索引：EU4 每上下文 ~1900 条规则，exact 桶
  探测 + 非精确小扫描才是正确结构（与 cwtools 一致）。曾尝试全扫描导致 157→180s 回归。
- `semantic_selected_alternative` 的成本不在转移逻辑而在**数据结构操作**（分组哈希、
  每容器重复分组）——预计算到规则集视图后该项从 13s 降到 <1s（子集口径）。

---

## 二、当前状态与基线（更新）

- 本轮 4 个提交 + 此前 10 个提交；测试 522 全绿，clippy 零告警。
- 工作区无未提交实现代码；`PERF_HANDOFF.md` / `PERF_PROFILE_REPORT.md` 为档案文档。

## 三、与 cwtools-rs 的剩余差距与后续路线

差距仍约 **17× wall / 15× CPU / 9.4× 内存**（对比其热缓存场景）。按归因排序：

1. **A1：CST arena 化**（最大剩余项）——pdx-parser 的 `CstNode { kind, range,
   Vec<CstNode> }` 每节点独立 Vec 分配（10.19M 节点 = 389MiB 头 + 137MiB 子缓冲）。
   改为 cwtools 布局（`Leaf` ≈72B 含 `StringTokens`、`Child(u32)` 8B 连续存储）需要
   重写 pdx-parser 公共 API、格式化器、解析缓存格式与全部下游消费者。这同时是内存
   （峰值 2.83 GiB）与扫描 CPU 的根本项。
2. **I3：vanilla 缓存 rkyv+zstd 整体快照**——替换 SQLite 装载路径，缩短 `pdx/ready`
   与扫描段。
3. **V3：ScopeId(u32)**——作用域值经 `Arc<str>` 已缓解克隆成本；完整 u32 化需把
   profile 作用域名编入规则集，收益中等。
4. **L2：编辑失效"导出指纹门"**——防止无关文件编辑触发重验证，属交互体验项。
5. **验证 worker 数自适应**——当前默认 4；12 逻辑核机上空闲验证可更激进。

## 四、可用工具（不变）

| 工具 | 用途 | 状态 |
|---|---|---|
| `scripts/performance/head-to-head.mjs` | 真实模组全生命周期基准 | ✓ |
| `pdx-engine/examples/mem_probe.rs` | 进程内分相位内存/CPU 归因（含诊断 pass 计时） | ✓ |
| `performance-results/run-triple.mjs` | 三方对比采样器 (gitignored) | ✓ 本地 |
| samply / WPT | 采样剖析器（需提权终端） | ✓ 已装 |

测量注意：ETW 内核会话需要管理员权限；`mem_probe` 的诊断 pass 是驱逐后的独立查询，
使用驻留 frontend（CurrentMod 扫描保留），不含重解析成本。

## 五、快速命令

```bash
# 全库单线程诊断计时
cargo run --release -p pdx-engine --example mem_probe -- "C:/Program Files (x86)/Steam/steamapps/workshop/content/236850/3047072888"

# 真实 LSP 全生命周期
node scripts/performance/head-to-head.mjs --workspace "C:/Program Files (x86)/Steam/steamapps/workshop/content/236850/3047072888" --cache "$LOCALAPPDATA/ParadoxCode/cache/eu4/vanilla.pdxindex" --samples 6

# 全仓测试
cargo test --locked --workspace --all-features
```
