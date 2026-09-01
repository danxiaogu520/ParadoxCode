# ParadoxCode samply 采样性能分析报告

> **日期**: 2026-08-30
> **分支**: `perf/strictly-below-cwtools`
> **剖析目标**: `pdx-engine/examples/mem_probe`(覆盖规则加载、全库扫描、解析、HIR 降低、索引分片、诊断全链路)
> **语料**: 创意工坊模组 3047072888(7,907 个脚本/本地化文件)与 `PERF_HANDOFF.md` 基准语料一致
> **工具链**: samply 0.13.1(ETW/xperf 后端,约 1 kHz 采样)+ Windows ADK 10.1.26100.2454(仅 WPT 组件)
> **构建**: `cargo build --release`,`[profile.release] debug = "line-tables-only"`(工作区已配置),全符号带行号归因
> **机器**: 12 逻辑核 Windows 11 24H2

---

## 一、执行摘要

1. **主线程 CPU 的 64.9% 花在"堆分配 + String 哈希 + 内存拷贝"三类基础设施开销上**,而不是业务逻辑本身:
   - 堆分配器(ntdll/vcruntime 分配释放路径): **27.6%**(47.2s / 170.8s)
   - String 哈希(SipHash + hashbrown 插入/扩容): **20.5%**(35.0s)
   - memcpy/memmove(vcruntime): **16.8%**(28.6s)
2. **诊断 pass 是绝对主热点**:`validate_semantic_container` 包含时间 111.8s,占主线程 CPU 的 **65.4%**(进程内计时 137.0s,157,655 条诊断,与交接文档基线完全一致)。
3. **其中最大单项是 `semantic_selected_alternative`(包含 44.7%,76.4s)**——即重构后的"作用域转移/备选选择"路径;其自身 CPU 只占 5.1%,其余全是它引发的 String 比较、HashMap 查找、路径克隆与释放。
4. **并行扫描(rayon 12 线程,6.2s 墙钟 / 60.7 CPU-s)呈同样结构**:分配器 33.7% + 拷贝 9.9%,哈希仅 1.1%(扫描路径不做哈希查询)。
5. 结论:**采样数据强力佐证 `PERF_HANDOFF.md` 的 Phase A(arena + 词法期字符串驻留)应最优先实施**。它直接消灭上述三类的来源,而不只是边际优化。

---

## 二、方法与口径

- 运行方式:提权 PowerShell 中 `samply record --save-only -o performance-results/samply-memprobe.json.gz -- target/release/examples/mem_probe.exe <mod>`(ETW 内核会话需要管理员权限)。
- 符号化:`--save-only` 输出不含符号;经 `samply load` 本地服务器的 `/symbolicate/v5` API(Mozilla symbolication 格式)批量解析 15,032 个唯一帧,15,012 个成功(99.9%)。
- **CPU 口径**:samply 会给等待中的样本赋予大权重(睡眠/等待期,单样本可达数千 ms)。本报告所有百分比均按 **CPU-only 权重**(单样本权重 ≤ 2)计算:主线程 170,817ms CPU + 13,656ms 等待(即 `phase_rss` 的 4×4s 睡眠与缓存加载等待,主要落在 `NtWaitForSingleObject`,**不是 CPU 开销**)。
- 分析脚本:`performance-results/report-numbers.mjs`(最终数字)、`analyze-samply.mjs`、`symbolicate-samply.mjs`(均 gitignored,产物见文末)。

---

## 三、阶段墙钟(来自 mem_probe 计时输出)

| 阶段 | 墙钟 | 并行度 | 说明 |
|---|---|---|---|
| 规则加载 + 全库扫描 | 6.2s | 12 线程(rayon 全核) | 60.7 CPU-s,并行效率 ~82% |
| 逐文件重读/重解/重降(计时用,单线程) | read 0.4s / parse 0.3s / **lower 26.4s** | 1 | 解析本身极快,lower 是单线程大头 |
| 诊断 pass(单线程) | **137.0s** | 1 | 7,907 文件,157,655 条诊断 |
| 驻留统计 + 驱逐 + 缓存加载 + 采样睡眠 | ≈ 19s | — | 含 4×4s 供外部 RSS 采样的睡眠 |
| 全程 | 189s | — | |

进程内计时与采样归因互相印证:lower 26.4s 计时 vs `lower_with_profile` 包含时间 26.2s;诊断 137.0s 计时 vs `validate_semantic_container` 包含 111.8s(采样下限,差值为采样间隔损耗与等待)。

---

## 四、主线程 CPU 归因(总量 170.8s)

### 4.1 按类别

| 类别 | CPU-s | 占比 | 构成 |
|---|---|---|---|
| 堆分配器 | 47.2 | **27.6%** | ntdll 堆内部例程 23.4s、`RtlFreeHeap` 18.5s、`GetProcessHeap`/`RtlAllocateHeap`/`process_heap_alloc`/`__rdl_alloc` 5.3s |
| String 哈希 | 35.0 | **20.5%** | SipHash 15.4s、`hash_one` 11.1s、`HashMap::insert` 4.3s、`RawTable::reserve_rehash` 3.4s |
| memcpy/memmove | 28.6 | **16.8%** | vcruntime 拷贝例程 27.3s、`RtlCopyMemory` 1.4s |
| **三类合计** | **110.8** | **64.9%** | |

其余:业务函数自采样(规则匹配、路径匹配、容器验证等)约 20%,std/格式化/排序等约 15%。

### 4.2 自采样时间 Top 25(函数级)

| % | CPU-ms | 函数 | 热点行 |
|---|---|---|---|
| 12.11 | 20,686 | `fun_27e80`(ntdll 堆分配内部)¹ | — |
| 10.82 | 18,476 | `RtlFreeHeap` | — |
| 9.88 | 16,869 | `fun_1c440`(vcruntime,memcpy 族)² | — |
| 9.01 | 15,397 | `core::hash::sip::write<Sip13Rounds>` | `core/src/hash/sip.rs:298` |
| 6.08 | 10,385 | `fun_1e490`(vcruntime,memmove 族)² | — |
| 5.32 | 9,088 | `BuildHasher::hash_one<RandomState, &str>` | `core/src/hash/mod.rs:701` |
| 5.14 | 8,785 | `pdx_analysis::semantic::semantic_selected_alternative` | `crates/pdx-analysis/src/semantic.rs:570` |
| 2.97 | 5,080 | `pdx_analysis::semantic::semantic_parent_path_matches` | `semantic.rs:721` |
| 2.49 | 4,256 | `hashbrown::HashMap::insert<&str, usize>` | `hashbrown-0.17.1/src/map.rs:1812` |
| 1.99 | 3,403 | `hashbrown::RawTable::reserve_rehash` | `hashbrown-0.17.1/src/raw.rs:962` |
| 1.59 | 2,710 | `pdx_rules::profile::GameProfile::scopes_compatible` | `crates/pdx-rules/src/profile.rs:873` |
| 1.58 | 2,700 | `pdx_analysis::diagnostics::validate_semantic_container` | `crates/pdx-analysis/src/diagnostics.rs:1025` |
| 1.57 | 2,674 | `fun_277ee`(ntdll 堆分配内部)¹ | — |
| 1.54 | 2,624 | `pdx_engine::query_cache::SnapshotQueryCache::get<bool>` | `crates/pdx-engine/src/query_cache.rs:95` |
| 1.40 | 2,393 | 规则过滤闭包 `call_mut<&SemanticRule,…>` | `core/src/ops/function.rs:298` |
| 1.37 | 2,342 | `pdx_analysis::semantic::semantic_rules_for_container` | `semantic.rs:52` |
| 1.32 | 2,248 | `GetProcessHeap` | — |
| 1.27 | 2,175 | `pdx_engine::hir::semantics::semantic_type_path_matches` | `crates/pdx-engine/src/hir/semantics.rs:258` |
| 1.27 | 2,175 | `fun_2e48c`(未归属模块) | — |
| 1.20 | 2,047 | `BuildHasher::hash_one<RandomState, &str>`(第二实例化) | `core/src/hash/mod.rs:701` |
| 0.99 | 1,685 | `Vec::extend_desugared<&SemanticRule>` | `alloc/src/vec/mod.rs:4045` |
| 0.90 | 1,540 | `core::slice::sort::stable::drift::sort<&SemanticRule>` | `core/src/slice/sort/stable/drift.rs:94` |
| 0.80 | 1,370 | `RtlCopyMemory` | — |
| 0.74 | 1,265 | `RtlAllocateHeap` | — |
| 0.69 | 1,184 | `std::sys::alloc::windows::process_heap_alloc` | — |

¹ 调用链证据:`fun_277ee` 的唯一调用者是 `RtlAllocateHeap`,`fun_27e80` 只被 `fun_277ee` 调用——即 ntdll 堆分配器内部例程(RtlpAllocateHeap 路径,PDB 不公开符号)。
² 调用者分布证据:`fun_1c440` 的调用者 99% 是 `alloc::string::write_str`/`String::clone`/`semantic_selected_alternative`(即 String 字节拷贝);`fun_1e490` 的调用者是排序元素搬移、查询缓存键构建、`BTreeMap` 节点操作。

### 4.3 关键函数包含时间

| 函数 | CPU-s | 占主线程 CPU | 对应进程内计时 |
|---|---|---|---|
| `pdx_analysis::diagnostics::validate_semantic_container` | 111.8 | **65.4%** | 诊断 pass 137.0s |
| `pdx_analysis::semantic::semantic_selected_alternative` | 76.4 | **44.7%** | — |
| `pdx_engine::hir::lower_with_profile` | 26.2 | 15.4% | lower 26.4s |
| `pdx_analysis::semantic::workspace_member` | 15.2 | 8.9% | — |
| `pdx_analysis::semantic::semantic_rules_for_container` | 17.5 | 10.2% | — |
| `pdx_engine::hir::semantics::lower_semantics` | 11.1 | 6.5% | — |
| `pdx_engine::query_cache::SnapshotQueryCache::get<bool>` | 7.6 | 4.4% | — |
| `semantic_selected_transition`(重构后仅剩薄封装) | 1.1 | 0.7% | 消融基线 34.2s |
| `scripted_macro_type_context` | 1.0 | 0.6% | 消融宏展开 22.2s |

---

## 五、并行扫描线程(12 个 rayon worker,CPU 60.7s)

| % | CPU-ms | 函数 |
|---|---|---|
| 14.4 | 8,737 | `fun_27e80`(ntdll 堆分配内部) |
| 10.05 | 6,101 | `RtlFreeHeap` |
| 8.18 | 4,962 | 规则过滤闭包 `call_mut`(容器规则收集) |
| 6.09 | 3,698 | `fun_1c440`(memcpy 族) |
| 4.59 | 2,788 | `pdx_engine::hir::semantics::semantic_type_path_matches` |
| 3.65 | 2,213 | `pdx_engine::hir::semantics::scripted_macro_type_context` |
| 3.37 | 2,044 | `GetProcessHeap` |
| 3.13 | 1,899 | `fun_1e490`(memmove 族) |
| 2.77 | 1,684 | `pdx_engine::hir::semantics::lower_semantics` |
| 2.49 | 1,510 | `alloc::fmt::format::format_inner`(格式化分配) |

类别汇总:**分配器 33.7%、拷贝 9.9%、哈希 1.1%**。扫描侧(解析 + lower + 分片)同样由分配主导;`format_inner` 2.5% 说明扫描路径还有格式化字符串分配可省(路径构建)。

---

## 六、与消融实验(PERF_HANDOFF.md 第三节)的交叉验证

| 消融结论(旧代码,单线程 136s 基线) | 本次采样(当前代码,137s 基线) | 一致性 |
|---|---|---|
| 作用域转移 `semantic_selected_transition` +34.2s | `semantic_selected_alternative` 包含 76.4s(56%),自身仅 5.1%,子成本在分配/哈希/拷贝 | ✓ 一致且更精确:成本在数据结构操作,不在转移逻辑本身 |
| 容器规则收集 ~12.8s(单线程部分) | `semantic_rules_for_container` 包含 17.5s(12.8%),内含 0.9s 排序 + 1.0s Vec 扩容 + 大量 `&Rule` 过滤闭包 | ✓ 数量级吻合 |
| 宏展开 +22.2s | 无独立展开函数入榜;`scripted_macro_type_context` 仅 0.6% | ⚠️ 当前代码宏展开成本已内联进 `lower_semantics`/类型上下文路径,或已在重构中摊薄;无法单独对表,但不影响主结论 |
| "其余递归开销 66.2s(路径克隆/基数检查/子块遍历)" | 本次可见:路径匹配 3.0s + 路径字符串克隆(memcpy 调用者证据)+ `semantic_type_path_matches` 2.2s 等 | ✓ 吻合:递归开销的主体正是逐层 String 路径操作 |
| 教训"记忆化开销超过计算本身" | `HashMap::insert` + `reserve_rehash` 合计 4.5%,`SnapshotQueryCache::get` 4.4%(键为 String) | ✓ 直接解释了为什么 String 键记忆化是负收益 |

---

## 七、结论与建议

### 7.1 核心结论

当前 20 倍于 cwtools-rs 的 CPU 差距,**主因不是算法量级,而是每个语义判定都在重复"分配 String → SipHash → HashMap 探测 → 比较 → 释放"**。三类基础设施开销(65%)叠加在 ~44.7% 的 `semantic_selected_alternative` 调用树上,即诊断 pass 约 2/3 的 CPU 在为 String 付费。

### 7.2 对 Phase 计划的验证与排序建议

| 优先级 | 措施(对应交接文档) | 采样证据支撑 | 预期收益 |
|---|---|---|---|
| 1 | **A2/A3:词法期字符串驻留(StringTable,u32 token)** | 消灭 SipHash 9.0% + hash_one 6.5% + insert/rehash 4.5% ≈ 20%,并把海量 String 比较(路径匹配、别名匹配)降为 u32 比较 | 诊断 pass ≥ 30-40% 缩减 |
| 2 | **A1:CST/HIR arena 化** | 消灭分配器 27.6% 的大部分(CST 1019 万节点逐节点 Vec、HIR 路径 String、索引分片 String 池外的残余分配) | 内存峰值与 CPU 双降 |
| 3 | **V1:装载期建 FxHashMap 规则索引(替代每容器收集+排序+过滤)** | `semantic_rules_for_container` 10.2% 内含 sort 0.9% + Vec 扩容 1.0% + 闭包过滤 1.4%,worker 侧同项 8.2% | 规则匹配路径减半可期 |
| 4 | **I2/I3:索引与缓存紧凑化** | memcpy/memmove 16.8% 主要由 String 克隆与 Vec 搬移驱动,arena 后自动受益;position ranges 178 万条 ×64B = 109MiB 支持紧凑化 | 内存与扫描 CPU |
| 5 | 查询缓存键改 u32 复合键 | `SnapshotQueryCache::get<bool>` 4.4%,且其内部 memmove(缓存键拼装)已被采样捕捉 | 与 V1 联动 |

Phase A 之后重新评估:交接文档 Phase V 预计 136s → <30s;按本次归因,仅消灭 String 基础设施(≈65% 中的大头)理论上即可把诊断 pass 压到 ~50s,叠加 V1 规则索引有望逼近 <30s 目标。

### 7.3 内存侧佐证(mem_probe 输出)

源文本 62MiB,CST 1,019 万节点(389MiB 头 + 137MiB 子节点缓冲),HIR 属性头 102MiB、路径 String 192MiB、引用 136MiB、位置表 109MiB——**CST 每节点独立 Vec 与 HIR 路径 String 是内存双雄**,与 Phase A1/A3 一一对应。

---

## 八、复现步骤

```bash
# 1. 构建(工作区已含 line-tables-only,无需额外配置)
cargo build --release -p pdx-engine --example mem_probe

# 2. 采集(必须提权终端;ETW 内核会话需要管理员)
samply record --save-only -o performance-results/samply-memprobe.json.gz \
  -- target/release/examples/mem_probe.exe "C:/Program Files (x86)/Steam/steamapps/workshop/content/236850/3047072888"

# 3. (可选)交互查看:samply load performance-results/samply-memprobe.json.gz
# 4. 符号化 + 分析
node performance-results/symbolicate-samply.mjs   # 需先 samply load 并替换脚本中的 token URL
node performance-results/report-numbers.mjs
```

---

## 九、局限与数据产物

- 单轮采样,约 1 kHz;百分比误差约 ±0.5 个采样点,但类别级结论(>15%)远超噪声。
- 4 个 `fun_*` 热点无公开符号,身份由调用链证据推断(见 4.2 脚注),不影响类别汇总。
- `semantic_selected_transition`/宏展开与旧消融的对应关系为推断(函数已重构),已在上表标注。
- mem_probe 在末尾安装 vanilla 缓存时因根类型不匹配 panic(示例只配置了 CurrentMod 根;`install_index_cache` 期望 Vanilla 根)——发生在全部被剖析阶段之后,不影响数据;建议后续给示例补一个 Vanilla 源根。
- 主线程 `NtWaitForSingleObject` 的 7.4%(13.7s)是睡眠/等待权重,不计入 CPU。
- 产物(均在 gitignored 的 `performance-results/`):`samply-memprobe.json.gz`(3.4MB 原始 profile)、`samply-symbols.json`(15,012 条符号)、`samply-analysis.json`、`report-numbers.json`、`memprobe-stdout.log`,以及脚本 `analyze-samply.mjs` / `symbolicate-samply.mjs` / `report-numbers.mjs`。

---

## 十、2026-09-01 复采样:Wave 4 + Wave I 之后(HEAD 7d027df)

方法与口径同前(同一语料、同一命令、CPU-only weight≤2、同一符号化流程),产物
`samply-memprobe-after-waveI.json.gz`(2.6MB)、`report-numbers-after-waveI.json`、
`report-numbers-after-waveI.mjs`(指向新 profile 的数字脚本副本)。符号化 15,065/15,089
(99.8%)。进程内计时:诊断 pass **59.3s,157,655 条**,与逐字节一致基准完全吻合。
(工具链备注:本版 `samply load` 不再在 stdout 打印 token;从 `http://127.0.0.1:3000/`
根页面 HTML 的链接里抓取。)

### 10.1 主线程 CPU 对比

| 指标 | 8/30 基线 | 9/1 复采样 | 变化 |
|---|---|---|---|
| 主线程 CPU 总量 | 170.8s | **94.4s** | **-45%** |
| 诊断 pass(进程内计时) | 137.0s | **59.3s** | **-57%** |
| 堆分配器 | 47.2s(27.6%) | 28.6s(30.3%) | 绝对 -39% |
| String 哈希 | 35.0s(20.5%) | **1.0s(1.1%)** | **消灭**(FxHash 替代 SipHash) |
| memcpy/memmove | 28.6s(16.8%) | 14.3s(15.1%) | 绝对 -50% |

### 10.2 关键函数包含时间对比

| 函数 | 基线 | 复采样 | 说明 |
|---|---|---|---|
| `validate_semantic_container` | 111.8s(65.4%) | 35.4s(37.6%) | 绝对 -68% |
| `semantic_selected_alternative` | 76.4s(44.7%) | **9.0s(9.5%)** | 本轮主攻目标,-88%,如约退出榜首 |
| `workspace_member` | (未单列) | **18.8s(19.9%)** | **新晋第一业务查询** |
| `lower_with_profile` | 26.2s | 25.2s(26.7%) | 未优化,符合预期 |
| `semantic_rules_for_container` | — | 9.3s(9.9%) | |
| `lower_semantics` | — | 10.6s(11.2%) | |
| `semantic_selected_transition` | — | 0.8s(0.9%) | 已可忽略 |

self 侧新面孔(analyze 口径,含等待权重):`semantic_parent_path_matches` 3.96%、
`GameProfile::member_kind_alias` 2.17%、`GameProfile::scopes_compatible` 2.09%。

### 10.3 结论与路线修正

1. **Wave 4/I 完全兑现**:alternative 选择路径包含时间 -88%,SipHash 类开销从 20.5% 降到
   1.1%,诊断 pass 137.0s→59.3s 且诊断输出逐字节一致。
2. **诊断路径下一个目标是 `workspace_member`(19.9%)**——每次 `KeyMatcher::Type/Enum`
   匹配都查询 `WorkspaceMembership`(kinds FxHashMap 嵌套查询 + 后缀扫描)。候选手段:
   per-context 成员快照(排序 Vec + 二分或 FxHashSet 直查)、把同 (kind,name) 的重复查询
   在单个容器验证内折叠。
3. **分配器 + memcpy 合计 45.4% 仍是基础设施天花板**,且 profile 规则匹配类 self
   (`member_kind_alias`/`scopes_compatible`)本质也是每次比较时的重复折叠——**A1
   (CST arena + 词法期驻留)依旧是最大结构性杠杆**;内存侧佐证不变:CST 389+137MiB、
   HIR 路径 String 192MiB、位置表 109MiB。
4. `lower_with_profile` 26.7% 未动,V3(ScopeId u32)与其配套,A1 落地时一并受益。
5. 扫描 worker 群 CPU 60.8s 基线持平,分配器占 31.8%——arena 化将同时惠及扫描路径。

## 十一、2026-09-01 第二轮:profile 驱动的分配/锁/结构优化(五提交)

语料与门槛不变(3047072888,7,907 文件,诊断 157,655 条逐字节不变)。
本轮基于真实 LSP 流程 samply 剖析(CPU-only 聚合,pdx-ls 进程 13 个验证 worker)。

### 11.1 提交与内容

| 提交 | 内容 | 单线程诊断 pass |
|---|---|---|
| `68c9f93` | 热路径去分配:`root_context_types` 预计算集、`semantic_type_path_matches_lowered` 悬挂小写路径 + 零分配 ci 前后缀比较、query-cache 探针线程本地缓冲 | 50.2 → 34.9s |
| `db11a1d` | membership 三视图 worker 线程本地槽(消灭 RwLock+downcast 探针)+ `scripted_macro_contexts` 预计算映射 | (并行收益,见 11.2) |
| `91cfab2` | ContextRuleView parent-path 分桶:全字面量路径哈希桶 + 动态段按长度桶,替代每容器 1900 条规则全扫 | 34.7 → 31.3s |
| `5efbb57` | MembershipBundle 单槽合并三视图探测 + `is_case_sensitive` 借用式探测 | 31.3 → 27.2s |

### 11.2 关键测量(本轮后)

| 指标 | 本轮前(本日) | 本轮后 | 本会话累计 |
|---|---|---|---|
| 单线程诊断 pass | 50.2s | **26.4–27.2s** | 157.3s(原始)→ **-83%** |
| head-to-head LSP CPU | 196.4 CPU-s | **73.2–74.1 CPU-s** | **-63%** |
| head-to-head LSP wall | 47.1s | **40.3–40.6s** | idle 窗口固定 20s 为下限 |
| `pdx/ready` | +10.97s(cpu 52.9s) | **+6.8s(cpu 27.4s)** | |
| idle 20s CPU | +78.7s(393%) | **+42.6s(213%)** | |
| 峰值内存 | 2.83 GiB | **2.62 GiB** | |

剖析侧:SipHash `RandomState` insert/rehash/hash_one 从 worker CPU 的 ~30% 归零;
`SnapshotQueryCache::get`(锁+downcast)从 ~12% 归零;`semantic_type_path_matches`/
`semantic_root_context_with_confidence` 的 format! 内联帧消失。

### 11.3 与 cwtools-rs 对标(热缓存基线)

| 指标 | pdx-ls(本轮后) | cwtools-rs | 差距 | 本会话前差距 |
|---|---|---|---|---|
| CPU | 73.2 CPU-s | 14.5 CPU-s | **5.0×** | 13.6× |
| wall | 40.3s* | 3.0s | 13.4× | 15.7× |
| 峰值内存 | 2.62 GiB | 0.30 GiB | **8.7×** | 9.4× |

\* 脚本含固定 20s idle 观察窗与 4×(open/edit/close) 采样段,wall 下限 ≈34s,CPU 为可比口径。

### 11.4 剩余差距归因(下一轮)

1. **内存 8.7×(最大结构项)**:峰值窗内保留 CST arena 204MiB、HIR 路径 192MiB、
   引用 136MiB、位置表 109MiB(mem_probe 驱逐后口径)。候选:验证期分批驱逐
   frontend、HIR 路径段驻留复用、位置表按需化。
2. **启动 lower 11.0s(mem_probe)** / `pdx/ready` cpu 27.4s:HIR 降低是启动 CPU 主项,
   V3(ScopeId u32)+ lower_semantics 结构优化是正路。
3. **wall**:测量口径下限受脚本固定窗约束;真实编辑延迟(open→diag p50 277ms)是
   下一个体验目标,L2 指纹门属此类。
4. 验证 worker 数自适应只影响 wall/延迟,不改变 CPU 总量(对标主口径),暂缓。

### 11.5 跳过项记录

- **I3(vanilla 缓存 rkyv+zstd)**:实测 `IndexCache::load_cancellable_for_install`
  2.13s(285MB/8671 文件),占 ~47s wall 的 4.5%,重写 ROI 不足,跳过。
- 测量工具链:samply record 服务器会随录制结束退出,重剖析用
  `samply load --no-open --port <p> <file>` 起新实例后调 `/symbolicate/v5`。
