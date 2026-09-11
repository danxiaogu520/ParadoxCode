# EU4 中文本地化透明读写 — 方案文档

| | |
|---|---|
| 状态 | 草案（待评审） |
| 日期 | 2026-09-11（同日增补脚本内嵌形态 Format B 与数据库单源化 §7） |
| 上游 | 需求文档 `eu4-cjk-localisation-requirements.md`（本文聚焦其 FR-1/FR-4/FR-5/FR-8） |
| 范围 | **仅限**：用户通过 paradoxcode-vscode 扩展直接正常读写 loc 与含转码文本的游戏脚本（不含 git/CI/平台集成） |
| 两条铁律 | ① 正确性以 paratranz 官方服务为唯一裁判（已完成双向验证，见 §3）② 绝不二次编码/二次解码（见 §5） |

---

## 1. 目标

模组作者在 VS Code 里打开游戏编码文件——本地化 yml（如 `localisation/replace/*.yml`）
**以及内嵌转码文本的游戏脚本**（如 `history/countries/*.txt` 中的人名/王朝名）——
**看到的是正常中文**；编辑并保存后，**磁盘上落盘的是游戏可读的对应形态字节**。全程无
外部网站、无手动转换、无双文件同步。

明确不做（本轮）：git filter、CI、paratranz 平台 API、CLI 批处理、母本/发行本目录映射。

配套目标（§7）：本地化数据库（预览/hover/补全的取值来源）消费同一解码器，按全序规则
每 (键, 语言) 只产出一个有效值——消灭"两套本地化并存"的历史多值问题。

## 1.1 两种文件形态（同一转码核心）

| | 形态 A：本地化 yml（`utf8eu4`） | 形态 B：脚本 txt（`latin1eu4`） |
|---|---|---|
| 出现位置 | `localisation/**/*.yml` | `history/`、`common/`、`events/` 等脚本内引号字符串（人名、王朝、自定义文本等） |
| 文件编码 | UTF-8 **带 BOM**（EU4 yml 约定），CRLF | **无 BOM** 的 CP1252/Latin-1 字节流，LF 或 CRLF（保留原样） |
| 三元组落盘 | 每个 escape 字节经 CP1252 反查转为 Unicode 字符再写 UTF-8（如字节 0x8A → `C5 A0`） | **原始单字节直接落盘**（`10 99 6C` 就是三个字节） |
| 单字节 passthrough | 码点 < 0x100 原样 | 码点 < 0x100 → 原始字节；**U+20AC 等 27 个 CP1252 字符 → 对应单字节**（注释里的 ä é ' 等保持原样，实测必要） |
| 转义集 | 23 值（含 `0x2F`） | **23 值（含 `0x2F`，实测标定**；Gist 注释称 txt 模式不加 0x2F，与服务器实际行为不符，以 310 个真实文件为准） |
| 佐证 | EDG-KTP 文件对逐字符一致 | Workshop mod 3047072888 `history/countries` 310 文件对拍（§3.1） |

两形态共享：三元组算法（marker/low/high + 补偿）、CP1252 27 项映射、解码通用性。差异
仅在文件边界的字节↔字符映射与单字节规则——**单一 codec crate，双 profile**。

### 形态 B 的实测背景（用户报告 + 调查结论）

- paratranz 网页**文本框无法解码形态 B**：其字节流包含原始 C0/C1 控制字节（marker
  `0x10`–`0x13`、C1 `0x8A` 等）。经编辑器打开（常被按 UTF-8 误读为替换符 `U+FFFD`，
  实测 Mainz 文件中即存在此类历史损坏残骸）再经剪贴板粘贴，控制字节被破坏或替换，
  解码必然失败。**文件上传通道**按字节读取、可指定原始编码（auto/cp1252/latin1），
  因此可行。这正是"必须做文件级 codec"的实证。
- 转换器并非结构感知：注释同样参与逐字符转换，只是注释多为 ASCII + Latin-1
  （< 0x100 直通），故看似未动。

## 2. 转码规范（最终版，已全量标定）

### 2.1 证据链与权威选择

| 实现 | 转义集 | 地位 |
|---|---|---|
| matanki-saito/EU4SpecialEscape（C++） | 规范源头 | 历史规范 |
| paratranz 官方 Gist `special-escape.js` | EU4 档：22 基础值 + `0x2F` = **23 值** | **本文采用**（与实际文件逐字节一致） |
| paratranz 服务器文件转换器 | 同上（由产出文件反证） | 事实生产标准 |
| paratranz 线上内联转换器（部署版，2026-09-11 实测） | 22 值变体（`0x21` 替代 `0x23`/`0x2F`） | 已知上游分歧，不采用 |
| EU4dll `escape_tool.cpp`（游戏内） | 29 值超集 | 游戏侧解码器（只读不写） |

**关键定理（解码通用性）**：转义流自描述——marker（`0x10`–`0x13`）自带补偿方式，
任何一档转义集产出的文本都能被游戏 DLL、paratranz 工具与本工具正确解码。转义集差异
**只影响 encode 的字节选择，不影响任何 decode 的正确性**。因此：encode 采用规范档
（23 值），decode 接受一切合法转义流（包括超集与变体），即天然与整个生态互认。

### 2.2 编码（可读 → 游戏），逐 Unicode 码点 `c`

```
1. c < 0x100            → 原样输出（含 §、ASCII、命令字符；纯 ASCII 是 encode/decode 的不动点）
2. hex = c 的十六进制串；low  = 末 2 位；high = 首 2 位      （4 位 hex 时即高低字节）
3. esc = 0x10；high ∈ 转义集 → esc += 2；low ∈ 转义集 → esc += 1
4. esc ∈ {0x11,0x13} → low += 0x0E；esc ∈ {0x12,0x13} → high -= 0x09
5. 输出 [esc, low, high]（low 在前），每个字节经 CP1252 反查转为对应 Unicode 字符落盘
```

转义集（23 值）：`{0x00,0x0A,0x0D,0x20,0x22,0x23,0x24,0x2F,0x3B,0x3D,0x40,0x5B,
0x5C,0x5D,0x5F,0x7B,0x7D,0x7E,0x80,0xA3,0xA4,0xA7,0xBD}`；
CP1252 映射 27 项（`0x8A→U+0160` 等；`0x81/0x8D/0x8F/0x90/0x9D` 无映射，C1 直通）。

### 2.3 解码（游戏 → 可读），逐码点扫描

```
码点 ∈ {U+0010..U+0013}：读后两个码点 → CP1252 正查还原字节 low、high（顺序 low 在前）
    sp = (high << 8) | low；marker 0x11 → sp -= 0x0E；0x12 → sp += 0x900；0x13 → sp += 0x8F2
    输出 sp
其他码点：原样输出
```

### 2.4 文件级不变量

UTF-8 BOM、CRLF、键名、引号缩进、`§Y`/`$VAR$`/`@KTP`/`[…]` 命令、注释全部原样保留；
仅正文字符流变换；键集合与顺序不变。

### 2.5 已知边界（诊断必须覆盖）

| 输入 | 行为 | 处理 |
|---|---|---|
| 码点 `0x1000–0xFFFF`（含全部常用 CJK） | 完全正确 | 正常读写 |
| 码点 `0x100–0xFFF`（拉丁扩展/希腊/西里尔等） | 生态统一按 3 位 hex 切片，**往返会变字**（U+0160→U+1660） | encode 拒绝 + 诊断（EU4 中文 loc 不涉及） |
| 上行的例外：27 个 CP1252 字符中的 8 个落在 `U+0100..U+0FFF`（Š œ ƒ Œ 等） | 形态 A：仍拒绝（三元组路径必被切片破坏）；形态 B：**单字节路径精确往返**（310 文件语料实证必需） | `transcode` 已实现该例外并有单测锁定 |
| 码点 ≥ `0x10000`（emoji、扩展 B 区生僻字） | 生态统一破坏（5 位 hex 切片错乱），游戏字体亦无字形 | encode 拒绝 + 诊断 |
| 纯 ASCII | 恒等变换 | 无歧义 |
| 形态 B 的 `0x80–0x9F` 三元组外原始字节（注释里的 CP1252 字符） | 历史工具按当时编码检测结果或直通或编为三元组（两种形态实测并存） | decode 统一按 CP1252 显示（0x91→`'`）；encode 统一单字节回写；两种历史形态均正确解码，重编码可能有字节级差异（仅注释，游戏等价） |
| 损坏的 marker 序列（marker 后不足 2 码点） | decode 容错：剩余部分原样透传 + 诊断 | 不中断整文件 |
| 存量历史变体（`0x3A` 档等） | marker 自描述 → 解码正确；重编码不逐字节复现 | 解码后保存即归一化为规范档（功能等价） |

## 3. 正确性验证（铁律 ① 的落地）

### 3.1 已完成的官方双向验证（2026-09-11）

以 paratranz 官方产物为裁判，双形态分别验证：

**形态 A（yml）**——EDG-KTP 两份真实文件：

| 方向 | 裁判 | 结果 |
|---|---|---|
| decode(发行本) | 线上部署版转换器（页面实测提取的函数逐字复刻执行） | **== 可读母本** |
| encode(母本) | 官方 Gist 参考实现（即服务器转换器算法） | **== 磁盘发行本，逐字符一致** |
| 部署版内联 encode | 与规范档全字节覆盖对比（512 字节位） | 仅 6 处分歧（`!`/`#`/`/` × 高低位），已确认为上游前端变体 |

**形态 B（脚本 txt）**——Workshop mod 3047072888 `history/countries` 全量 310 个含转码
文件（由 paratranz 工具链产出的真实语料）：

| 检验 | 结果 |
|---|---|
| decode → 再 encode → 逐字节比对原文件 | **305/310 完全一致** |
| 5 个例外归因（全部可正确解码） | 3 个为 `0x3A` 历史变体档（另一工具产物，marker 自描述故解码无碍）；1 个为注释 CP1252 歧义区（`' U+2018` 被历史工具编为三元组，见 §2.5）；1 个为源数据损坏（Mainz 的 Adolf 含 U+FFFD 残骸） |
| 0x2F 转义实证 | 20 个文件含低字节 0x2F 的汉字（伯/夯/启等）被转义 → txt 档同为 23 值集合 |
| 三元组外原始高字节 | 全部位于 `#` 注释（ä ü é 等），CP1252 单字节规则覆盖 |

**服务器 REST `POST /api/utils/encode`**：直连与页面内 fetch 均试，当前 500（服务端
故障或需登录态），待恢复后补服务器档黄金语料。验证方法可复现：部署代码从页面
`$options.methods` 提取并核对逐字节一致；Gist 与 DLL 源码取自官方仓库；过程脚本与
黄金样例随实现归档进仓库测试。

### 3.2 持续验证（进入仓库的测试基建）

1. **黄金语料**（`crates` 测试 + 扩展 TS 契约测试共用）：
   - EDG-KTP 文件对（双向逐字节）；
   - 全字节覆盖矩阵：每个转义集字节 × {low, high} 位置（构造 `U+4E00+b` / `(b<<8)|0x61`），
     加 27 个 CP1252 字符、不动点 ASCII、边界区字符；
   - paratranz 变体档样本（0x21 系列）→ 验证 decode 通用性。
2. **往返性质测试**：`decode(encode(x)) == x` 对任意 BMP 常用区文本成立。
3. **互认测试**：规范档 encode 的输出送部署版 decode（及反向）== 原文。
4. 服务器档：API 恢复后（或用户提供登录 Token）批量转换黄金语料归档，作为最高优先
   裁判源；在此之前以"部署版 decode + Gist encode + 用户实际文件"三重裁判。

## 4. 架构（扩展内闭环）

```
┌─ VS Code ────────────────────────────────────────────────┐
│  编辑器视图（中文）                                        │
│      ↕ FileSystemProvider (scheme: pdcloc://)             │
│  readFile:  磁盘字节 → decode → 可读 UTF-8                │
│  writeFile: 可读 UTF-8 → [分类器 §5] → encode → 磁盘字节   │
│      ↕ LSP client（虚拟文档 textSync：didOpen/didChange    │
│                     只传解码后全文，服务端不读盘）           │
├─ pdc（现有）──────────────────────────────────────────┤
│  localisation 解析/诊断/补全 — 坐标即视图坐标，零映射        │
└───────────────────────────────────────────────────────────┘
  codec：Rust crate（规范档 + 变体档 decode）→ TS 孪生实现供扩展进程内调用（差分锁定）；
        同一 crate 供 pdc 原生链接（单一实现，杜绝三处漂移）
```

要点：

- **codec 单一来源**：Rust 纯函数 crate（建议 `transcode`，或并入 `parser`），
  TS 孪生（`editors/vscode/src/transcode.ts`）给扩展进程内调用；等价性由黄金语料 + 差分向量（`transcode-vectors`）锁定。
- **虚拟 scheme**：`pdcloc://file/c/…/replace/xxx.yml` ↔ 真实路径一一级联映射；
  打开方式：命令 `ParadoxCode: 以中文打开发行本`、文件资源管理器 context menu、或对
  `localisation` 目录下文件自动提示。
- **脚本文件接入**（形态 B）：同一 provider，按文件类型选档（yml→A，`history/`、
  `common/` 等脚本→B）；读门槛同为 `classify == Escaped`——从未转码的脚本文件不进
  透明视图（可读中文脚本本就能用编辑器正常编辑，是否转为游戏形态交给显式命令
  `ParadoxCode: 转码此脚本`，转换前自动备份）；范围可用
  `paradoxcode.localisation.transparentScriptGlobs` 配置（默认 `history/**`）。
- **LSP textSync**：虚拟文档走 `didOpen`/`didChange` 全量同步解码后文本；服务端索引
  这些 URI 时经"虚拟→真实→解码"访问层读盘（重命名/引用查找等跨文件操作）。
- **外部变更**：provider `watch` 真实文件，磁盘变化 → 重新分类+解码 → `onDidChangeFile`。
- **状态栏**：`EU4 发行本（已解码视图）`，明确告知用户当前所处视图。

## 5. 防二次编码/二次解码（铁律 ②）

### 5.1 分类器（一切变换的前置闸门）

`classify(bytes, 档位) → { Readable | Escaped | Mixed | Ascii }`，形态判定与内容判定
两步：

1. **形态/编码判定**（形态 B 必需）：字节流是合法 UTF-8 或带 UTF-8 BOM → 候选可读
   文本；否则按 CP1252 解码进入转义检测。转义 txt 几乎不可能是合法 UTF-8（含孤立
   `0x8A` 等高位字节），两者天然分离；
2. **内容判定**：扫描全部码点，统计合法转义三元组数 `E`、孤立/损坏 marker 数 `E'`、
   原始 CJK/全角码点数 `R`（`U+2E80..U+9FFF`、`U+3000..U+303F`、`U+FF00..U+FFEF` 等）；
3. `E ≥ 3 且 R == 0 且 E' == 0` → **Escaped**；`R > 0 且 E == 0` → **Readable**；
   `R > 0 且 E > 0`（或 `E' > 0` 且 `R > 0`）→ **Mixed**；`E == 0 且 R == 0` →
   **Ascii**（不动点，无风险）；
4. 档位选择：`localisation/**/*.yml` → 形态 A；脚本文件 → 形态 B；`classify` 在两档
   上结论需一致，不一致按 Mixed 处理；
5. 阈值说明：`E ≥ 3` 排除可读文本里偶然出现的控制字符；真实语料（EDG-KTP + 310
   文件语料）边界清晰。误判样本进黄金语料回归。

### 5.2 不变式（写入路径）

- provider `writeFile` **只执行 encode，且仅当** `classify(缓冲) ∈ {Readable, Ascii}`；
  **绝不**对 Escaped/Mixed 缓冲 encode——保存被拒绝，弹出解释（"缓冲中检测到已编码
  序列（N 处），疑似把转码文本粘进了可读视图；请撤销粘贴或先解码"）。
- 编辑器缓冲由 provider 的 decode 派生（结构性保证：正常编辑流程产生的缓冲必为
  Readable）；分类器是第二道独立防线，捕获"用户粘贴了转码文本"这类路径外输入。
- 结构论证：对任何含原始 CJK 的 Readable 文本，encode 输出必含 marker 三元组（CJK
  码点 ≥ `0x1000` 恒走三元组路径）；对已是 Escaped 的文本再 encode 会产生嵌套
  marker——分类器在变换前拦截，两种情况都不可能落盘。

### 5.3 不变式（读取路径）

- provider `readFile` **只执行 decode，且仅当** `classify(磁盘文本) == Escaped`；
- `Readable`（如误把可读中文存进了发行本路径）→ 原样打开 + **警告诊断**
  `LocalisationNotTranscoded`（"此文件位于游戏读取路径但内容未转码，游戏内将显示乱码"）；
- `Mixed` → 原样打开（不 decode，避免半转码文件被二次变换）+ **错误诊断**
  `LocalisationMixedEncoding`（列出首处位置）；
- `Ascii` → 原样打开（不动点）。

### 5.4 其他入口同样受闸门约束

- LSP 的"跨文件解码访问层"（§4）读盘时走同一 `classify`；
- 未来任何命令（手动转码按钮等）复用同一分类器，无旁路。

## 6. 诊断码（纳入 `docs/diagnostics.md` 体系）

| 码 | 触发 | 级别 |
|---|---|---|
| `LocalisationNotTranscoded` | 游戏读取路径上出现 Readable 中文 | Warning |
| `LocalisationMixedEncoding` | 同文件混合两种形态 | Error |
| `LocalisationEscapeRefused` | 保存被防二次转码闸门拒绝 | Error（弹窗+诊断） |
| `LocalisationUnencodableCodePoint` | 边界区码点（§2.5）出现在可读文本 | Warning |
| `LocalisationBrokenEscapeSequence` | 损坏 marker 序列（decode 容错透传处） | Warning |
| `ScriptLegacyEscapeVariant` | 形态 B 存量文件命中历史变体档（如 `0x3A` 档、CP1252 歧义区三元组），解码正确但重编码将归一化 | Hint |

## 7. 本地化数据库单源化（解码器的第三个消费方）

**目标**：本地化数据库（预览/hover/补全的取值来源）消费同一解码器，从"同键多定义、
消费方碰巧取一"改为**每 (键, 语言) 恰一有效值**，且值取自解码后的发行本真实文本。
`l_english/` 母本树与 `replace/` 发行本树并存期间，数据库始终反映游戏侧；终态删除
母本树，数据库无感。

### 7.1 现状核查（2026-09-11）

| 事实 | 证据 |
|---|---|
| ✅ 层序已存在：Vanilla 0 < Dependency 1..n（声明序）< CurrentMod n+1 | `engine/src/scan.rs:629`、`model.rs:67` |
| ✅ localisation 符号策略 `replace-by-symbol` + 大小写不敏感，层间遮蔽已生效 | `rules/eu4/catalog/symbol-descriptors.json:4369`、`engine/src/index.rs:1095` |
| ❌ 同层并列：同层多文件同键**全部 active**（`== highest` 即胜），胜者由消费方 (优先级, 路径字典序, range) 排序碰巧选出；`l_english/` 字典序先于 `replace/`，与游戏"replace/ 后加载、后者胜"**相反** | `index.rs:1145`、`ide/src/resolution.rs:952` |
| ❌ 值未解码：形态 A 是合法 UTF-8，读取直接成功（不触发 1252 回退），preview 存转码形态乱码 | `scan.rs:430`、`model.rs:529` |
| ❌ 版本号（`key:2`）在 HIR 收集时丢弃 | `hir/collector.rs:77` |
| 纠错（调查初版结论有误） | `localisation/replace/` **确实被索引**：扫描根 `localisation` 无深度限制、递归遍历、yml 在白名单（`filesystem.json:128`、`rules/src/profile.rs:906`）——两棵树如今都在库里，这正是多值现场 |

### 7.2 全序规则（2026-09-11 用户裁定）

> 层序不变：**current mod > dependency（按声明序）> vanilla**。
> 同层内**一律平等**（不区分 `l_*` 与 `replace/`），按**读取序后来居上**。

- **解析单位 = (键, 语言)**：不同语言文件（`l_english/` vs `l_french/`）同键互不覆盖，
  与游戏一致；hover 现有"每语言取第一条"的去重（`localisation.rs:587`）与之天然对齐。
- **读取序** = 同层扫描序，规范定义为逻辑路径的**逐段字典序、前缀段短者先**（等价于
  深度优先、目录项按名排序的遍历序；同文件内按行序）。它是逻辑路径的纯函数，不依赖
  运行期计数器——新建/改名文件与会话重启后结论不变。
- **有效值** = 全序（层优先级，读取序）中该 (键, 语言) 的**最后**一个定义。EDG-KTP
  布局下 `localisation/replace/**` 读取序天然靠后 → 发行本自然胜出，与游戏行为一致，
  **无需为 `replace/` 设任何特殊档位**。
- 输家不删：遮蔽条目保留，供跳转定义、"被 X 覆盖"提示与漂移诊断（FR-3）使用。
- **版本号不参与全序**（纯后来居上），暂不入 HIR，仅作未来可选诊断素材。
- overlay（编辑器缓冲，优先级 20,000）仍压过一切，不变。

### 7.3 实现落点（2026-09-11 已落地，与初版设计有一处偏离）

**关键偏离**：解码不进 scan.rs ingest（文档级换文本、坐标错位、需 offset 映射），而是
**值级解码在 preview 派生处**（`transcode::decode_value`）。理由：

- 文档文本保持磁盘原样 → 跳转定义/hover/诊断的 range 永不错位，**offset 映射问题整体不存在**；
- 缓存安装路径（`model.rs` preview 派生）与懒解析路径（`localisation.rs` CST 回退）共用同一
  `decode_value`，两条路一致；
- 转码三元组的 marker（U+0010..=U+0013）是合法 UTF-8 码点，解析器天然接受，无需特殊语法；
- `decode_value` 闸门 = 值内**存在任一 marker** 即解码（无阈值，单三元组值也解码）；可读值
  无 marker，恒等直通——铁律 ② 在数据面自动成立。

| 改动 | 位置 | 内容 | 状态 |
|---|---|---|---|
| 值级解码（缓存侧） | `engine/src/model.rs` | `localisation_previews_from_parsed` 去引号后接 `decode_value`，再截断进 preview | ✅ |
| 值级解码（懒解析侧） | `ide/src/localisation.rs` | `localisation_preview` CST 回退同样接 `decode_value` | ✅ |
| ingest 三元组豁免 | `engine/src/scan.rs` | `sanitize_recovered_text` 的坏字符标记循环**整组消费合法三元组**（marker+2 payload，index+=3），转码 marker 不再被空格化；孤儿 marker 仍标记（payload 安全性由逃逸集保证：三元组内不含引号/换行/#） | ✅ |
| 单值出口 | `ide/src/resolution.rs`、`localisation.rs` | `effective_localisation_candidate`：候选按（层优先级降序, 读取序升序）排序后，取头部同层运行段的**最后**一个；`localisation_values_by_key`（mission preview）与 `localisation_previews_for_name`（hover，按语言分组、同层后来居上）均走该出口 | ✅ |
| 索引层不动 | `index.rs` | active 语义保持现状（`== highest` 同层并列）；跨语言同键依赖并列，按语言分组是消费方职责 | ✅（维持原判） |
| 缓存失效 | `index_cache` | schema 13→14；`write.rs` 落 `codec_version` 元数据（=`transcode::CODEC_VERSION`），`read.rs` 读取时不匹配即按 `UnsupportedSchema` 整体失效重扫 | ✅ |
| 坐标一致性 | 跳转定义 | 由值级解码决策**消解**：文档坐标即磁盘坐标，无需映射；P2 的 `pdcloc://` 视图独立于本项，不互相依赖 | ✅（问题不存在） |

测试锁定：`engine/src/tests/localisation.rs`（双树预览解码 + 单三元组值）、
`ide/src/tests/hover.rs::localisation_values_by_key_apply_the_layer_then_read_order_total_order`
（层序压倒读序、`replace/` 后读胜、法语单语言键照常解析）。

### 7.4 双树过渡与终态

1. 规则生效即：双树同键的胜者 = `replace/` 发行本（读取序靠后），preview 派生处值级
   解码后值为可读中文——数据库从此反映游戏真实文本，而非母本。
2. 母本改动未转码 → 两树漂移 → FR-3 漂移诊断报告差异（删除母本树之前的安全网）。
3. 终态：只留发行本一棵树，经 P2 透明读写维护；数据库对"少了一棵树"无感。

## 8. 实施计划

| 阶段 | 内容 | 产物 |
|---|---|---|
| P1 ✅（2026-09-11 完成） | `transcode` crate：双档 encode/decode/classify + §3.2 黄金语料（native；语料含 EDG-KTP 文件对、310 文件脚本语料样本、全字节覆盖矩阵） | crate + 测试（37 项全绿，EDG-KTP 双向逐字节通过；语料位于 `crates/transcode/tests/`） |
| P2 ✅（2026-09-11 完成） | VS Code 扩展透明读写：`pdcloc://` FileSystemProvider（yml A 档 + 脚本 B 档，双档读门槛同为 `classify == Escaped`）+ 写入防二次编码闸门（Escaped/Mixed 缓冲拒绝保存并弹窗）+ 边界码点拒绝 + 状态栏 + 打开/揭示/转码（备份 `.pre-transcode.bak`）命令 + 打开原始路径自动提示；LSP 虚拟文档 textSync 经 `documentSelector` 的 `pdcloc` scheme 项 | `editors/vscode/src/transparentLoc.ts`、开关 `paradoxcode.localisation.transparentEncoding`（默认开）+ `transparentScriptGlobs`（默认 `history/**`）；**扩展侧绑定形态（2026-09-11 改版）：TS 孪生实现 `editors/vscode/src/transcode.ts` 直接进程内调用，不再经 WASM**（首版曾用手写 `extern "C"` ABI 的 `transcode-wasm` cdylib + 随包 `pdx_codec.wasm`，后按维护者决策移除：算法固定、双实现 + 强测试防护优先于单源编译）；等价性防护 = EDG-KTP 黄金语料逐字节（`scripts/codec-ts-test.mjs`）+ Rust↔TS 差分向量（`cargo run -p transcode --bin transcode-vectors` 出全码点扫描/随机序列，74,549 条全对拍，已接入 `npm run test:ci`/`test:contract`）；服务端 `uri.rs` 接受 `pdcloc://`（空 authority，路径即真实文件），虚拟文档挂接并遮蔽磁盘分片，坐标即视图坐标 |
| P3 ✅（2026-09-11 完成） | 诊断六件套接入 LSP/`docs/diagnostics.md`：服务端 `ide` 新增 `transcode` 诊断 pass（挂在 localisation 命令诊断之后，逐文档分类+按码上报）——`LocalisationNotTranscoded`（仅 `localisation/**replace**` 发行路径、锚首个裸 CJK）/ `LocalisationMixedEncoding`（Error，锚首个证据字符）/ `LocalisationBrokenEscapeSequence`（"转码为主+孤立 marker"细分支：E≥3 且裸 CJK=0 时逐孤立 marker Warning，不降级为 Mixed）/ `LocalisationUnencodableCodePoint`（profile 感知，Script 豁免 27 个 CP1252 映射字符，上限 32）/ `ScriptLegacyEscapeVariant`（decode→re-encode 字符级往返比对检出历史变体档，Hint）/ `LocalisationEscapeRefused`（仅客户端保存闸门，注册供过滤/覆盖，服务端不发）；`pdc` 走既有 codeDescription 管线（锚=小写码名）；扩展端 `handleDiagnostics` 中间件对 `pdcloc://` 视图抑制 `LocalisationNotTranscoded`（解码视图可读 CJK 是设计使然） | `crates/ide/src/transcode.rs` + `types.rs` 六码 + 测试 `tests/transcode.rs`（6 项：release 路径根挂接、主树安静、混合首证据、孤立 marker、profile 边界 Š/😀、canonical vs DllFull 变体）；`docs/diagnostics.md` 六节 + 迁移注记；P2 客户端 3 码与保存闸门维持原状，全工作区 795+ 测试/clippy/fmt 绿、扩展五项契约绿 |
| P4（可选） | 服务器档黄金语料（待 paratranz API 恢复/用户提供 Token） | 语料更新 |
| P5 ✅（2026-09-11 完成） | 数据库单源化（§7）：preview 派生处**值级解码**（偏离初版 ingest 方案，见 §7.3）+ 分析层 (键, 语言) effective resolver（层序压倒读序、同层后来居上）+ `sanitize_recovered_text` 三元组豁免 + 索引缓存 schema 14 + `codec_version` 元数据 | 单值本地化出口；引擎/分析双层测试锁定（验收 7/8/9） |

涉及落点：crate `crates/transcode`（含差分向量生成器 `transcode-vectors` bin）与扩展 TS
孪生 `editors/vscode/src/transcode.ts`；`editors/vscode/src/transparentLoc.ts`（P2 已落地：provider + 分类调用 + URI 映射 +
命令 + 状态栏 + 自动提示）；`pdc` 客户端 `documentSelector` 已注册 `pdcloc` scheme、
服务端 `uri.rs` 已接受 `pdcloc://`；诊断 pass 落于 `crates/ide/src/transcode.rs`
（P3 已落地：六码服务端上报 + pdcloc 视图客户端抑制）；
数据库单源化已落地在 `engine/src/model.rs` + `scan.rs`（值级解码与三元组豁免）、
`ide/src/localisation.rs` / `resolution.rs`（单值出口）与 `index_cache`
（schema 14 + codec 版本号）。

## 9. 验收标准

1. 打开 `localisation/replace/*.yml` 显示正常中文；改字保存后游戏实机显示正确。
2. `encode(母本) == 磁盘发行本`、`decode(发行本) == 母本`（EDG-KTP + 黄金语料，逐字节，
   含 TS 孪生版本）。
3. 打开 `history/countries/*.txt`（形态 B）显示正常中文人名/王朝名；修改保存后逐字节
   往返一致（规范档内），游戏实机显示正确。
4. 把发行本内容整段粘贴进可读视图后保存 → **保存被拒绝并给出解释**（铁律 ② 演练）。
5. 把可读中文直接写入 `replace/` 路径文件（绕过扩展）→ 打开时出现
   `LocalisationNotTranscoded` 警告且不做任何变换。
6. 全字节覆盖矩阵与双重语料（EDG-KTP + 310 脚本文件）测试全绿；5 个已知例外文件解码
   正确且携带 `ScriptLegacyEscapeVariant` 提示。
7. 人为让 `replace/` 与 `l_english/` 同键值不同：预览（missionPreview）与 hover 显示
   `replace/` 的解码值（可读中文），而非 `l_english/` 母本值。
8. 同层构造三个文件（逻辑路径字典序递增）同键同语言：有效值取最后一个；新建或改名
   文件后结论保持（读取序是逻辑路径的纯函数）。
9. 同键跨语言（`l_english/` + `l_french/`）互不覆盖：hover 双语各显一值。
