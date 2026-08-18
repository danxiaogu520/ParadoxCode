# RFC 0014：EU4 任务树可视化编辑器

- 状态：**Superseded**（2026-08-18：独立 GPUI 编辑器退役，转为 VS Code 实时预览；下文历史设计保留为记录，不再定义新行为）
- 关联：RFC 0012（通用引擎与 EU4-first）、RFC 0002（语法、CST 与增量解析）、RFC 0013（第一方规则源）
- 目标平台：~~Windows 优先（macOS/Linux 由 GPUI 天然支持）~~ → 编辑器集成面为 VS Code 扩展（`editors/vscode`）

## 状态变更（2026-08-18）：独立编辑器退役，转为 VS Code 实时预览

**决策**：`apps/pdx-mission-editor`（GPUI 独立应用）整体删除；任务树的能力面改为
`pdx-lsp` 的只读预览协议 + VS Code 扩展内的 webview 渲染。Zed 扩展保持语言工具面不变
（其扩展 API 无自定义 UI 能力，已核到最新稳定 tag）。

**问题与动机**：产品目标从"独立 GUI 编辑器"收敛为"在编辑器内实时预览任务树"；
独立应用与编辑器工作流分离、维护两份 UI。

**替代方案**：Zed 面板（不可行——`zed_extension_api` 截至 v1.15.0 / v1.16.0-pre
均无自定义面板/渲染/请求通道）；GPUI app 加 watch 模式（可行但预览不跟随
未保存缓冲，且保留两套 UI）。

**影响与迁移**（已完成）：

- `pdx-mission-model` 保留并成为 `pdx-lsp` 依赖；网格几何（`layout` 常量、
  `layout_file`/`world_position*`）与 EMT 箭头几何（`arrow_geometry`，改为与渲染
  解耦的 `ArrowGlyph`）迁入 `pdx-mission-model::geometry`；游戏界面纹理面
  （`interface/*.gfx` spriteType 索引、DDS 解码 DXT1/3/5 + 未压缩、PNG data URL
  编码与缓存）迁入 `pdx-mission-model::texture`；
- `pdx-lsp` 新增 `pdx/missionPreview` 端点：`{path, text}` → 节点（世界坐标、字节
  span、诊断标记、`required`、本地化标题 `title`/`titleKey`、图标 sprite `icon`）/箭头
  （glyph + 对应 sprite 名）/组标签/跨文件桩/诊断 JSON/去重纹理表（sprite 名 → PNG
  data URL）；任务标题解析 `{id}_title`（复用 analysis 的 active 定义与英文优先语义，
  缺失时渲染端回退原始 id）；
- `editors/vscode`：语言客户端 + webview canvas 渲染器（实时刷新、点击跳转、
  错误/警告/根任务颜色编码、消费服务端 EMT 箭头几何与游戏纹理（图标/帧/箭头贴图）、
  本地化标题显示），与 Zed 扩展共用 `pdx-ls` 与 `.pdx/project.toml` 通用配置
  （`[server].binary` + source 字段自动发现）；
- GPUI 视图层、动效层（display）、DDS 图标管线、保存写回 UI 随 app 退役；
  `pdx-mission-model` 的 `write`/`edit`/`encoding`/`diff` 模块保留（写回与编码
  语义仍是被验证过的领域逻辑，供未来编辑面复用）；
- 依赖 id 使用 descriptor 原名（含空格/非 ASCII），`stable_dependency_root_id`
  按字节哈希，无约束冲突。

**验收标准（替代原 M0–M3 编辑器验收）**：VS Code 扩展加载真实 `pdx-ls` → 打开
`missions/*.txt` → 面板实时刷新任务树（依赖箭头使用服务端 EMT 兼容几何；文件内
依赖直接连线，跨文件前置引用以 `↥ id` 桩标注）→ 点击节点/组跳转对应文本块（已
达成并实测）。预览是单文档范围，跨文件依赖不解析其他文件，不在验收内。原独立
编辑器的交互验收（拖拽换位/连线/保存 diff）不再适用，随 app 退役。

## 已确认决策（2026-08-15）

1. 放置方式：纳入 ParadoxCode workspace，新增 `crates/pdx-mission-model` 与 `apps/pdx-mission-editor`；
2. UI 语言：中英双语（内置 i18n 表，运行时切换）；
3. 1.30–1.34 老格式读取兼容：不进 MVP；
4. 图标 DDS 解码：进入首版（Windows 用 WIC，跨平台 DXT 软解为后续项）。

## 实现状态（M0–M3：完成）

- `crates/pdx-mission-model`：MissionFile/Tree/Mission 模型（含 `type`、`provinces_to_highlight`、
  mission 级 span）、CST 提取、规范化写回（缩进/行尾/块间距按文件检测）、文件级校验
  （重复 id、悬空引用、依赖环、position=0）；跨树 `required_missions` 是 EU4 1.35+ 分支
  任务合法特性，不诊断；`edit` 模块提供一致性编辑操作（删除清理引用、重命名联动）；
- `apps/pdx-mission-editor`：GPUI 窗口、文件/树侧栏、画布（EMT 式游戏贴图
  箭头拼装、节点卡片含游戏帧贴图、本地化标题与游戏图标；固定 1:1 无缩放，纯平移
  导航；无网格线）、选中高亮、依赖连线拖拽建立、属性面板（单行字段内联编辑 + 前置
  建议列表 + trigger/effect 多行模态编辑器）、任务/树 CRUD、撤销/重做、中英双语、
  DDS 图标解码（DXT1/3/5 + BGRA/ARGB，解码后 RGBA→BGRA 交换以匹配 GPUI 图集）、
  保存 diff 确认模态；
- 本地化：扫描 `localisation/*_l_english.yml`（按文件名排序合并，后文件覆盖重复键），
  标题用 `{mission_id}_title` 键；默认英文，不依赖 UI 语言；
- 画布坐标：鼠标事件为窗口坐标，canvas prepaint 读取 `window.element_offset()` 转为
  canvas 本地坐标，命中检测/平移/连线拖拽与节点定位统一在同一坐标系；
- 保存管线：`prepare_save` 手术式写回——只重写被编辑的 mission 块（或树级字段变化时
  整树），未编辑内容字节不变；对真实游戏文件验证：无编辑保存零 diff，单任务编辑
  仅 2 行 diff 且重新解析一致；
- 无头验证：`--dump <file>` 解析并打印模型与诊断（退出码 1 表示存在 error 诊断）；
- 验证：model 18 项 + editor 22 项单测通过（含 `arrow_geometry` 的 EMT 锚点对照测试）；
  clippy 无警告；真实游戏文件
  `English_Missions.txt` / `00_Generic_missions.txt` 解析 0 错误 0 警告、零 diff 保存；
  真实游戏贴图管线冒烟：frame/8 种箭头/任务图标 DDS 全部解码成功；本地化冒烟：
  126k+ 键、`eng_mighty_army_title` 命中；GUI 冒烟通过。

已知限制：标量字段的行尾注释在写回时丢失（00_Generic 风格），由保存 diff 面板兜底。
布局为字面坐标映射（X = slot-1，Y = position-1）：同 slot 树、相同 position 的任务在
画布上重叠显示，与文件内容一致。

## 1. 背景与目标

ParadoxCode 目前提供 `pdx-lsp`（编辑器内语义服务）。本 RFC 提出第二个产品面：一个独立的
**EU4 任务树可视化编辑器**（GUI 应用），用 Rust + GPUI 实现。

核心诉求：

1. 任务树/任务的 CRUD（增删改查），包括跨任务依赖关系（`required_missions`）编辑；
2. 实时渲染：图形画布上节点与依赖边随编辑即时更新；
3. 直接读写 EU4 mod 的标准 `.txt` 任务文件与 localisation `.yml`，保证 round-trip 保真；
4. 参考游戏内实际布局语义（`position` 列、`slot`），做到"编辑结果即游戏效果"。

本编辑器是 mod 作者的生产工具，不是游戏内修改器：它只产生 mod 数据文件，不修改游戏本体。

## 2. 现状调研

### 2.1 EU4 任务数据格式（本机 1.36+ 安装实测）

- 数据位于游戏根目录 `missions/*.txt`（非 `common/`），每文件可含多个顶层 tree block；
- 当前版本格式（1.35+）：

```text
eng_british_conquest = {            # 顶层 block = 一棵任务树
    slot = 1                        # 游戏内槽位（列位置，从左上 1 开始）
    generic = no
    ai = yes
    potential_on_load = { ... }
    potential = { ... }             # 树级触发条件
    has_country_shield = yes

    eng_mighty_army = {             # mission block
        icon = mission_assemble_an_army
        required_missions = { eng_war_france }   # 依赖边：前置任务
        position = 1                # 游戏内列位置（拖拽即改此值）
        completed_by = 1450.1.1
        trigger = { ... }           # 完成条件（保留原文编辑）
        effect = { ... }            # 完成奖励（保留原文编辑）
    }
}
```

- 依赖方向：`required_missions = { A B }` 表示 A、B 是该任务的前置（全部完成才解锁）；
- 本地化：`localisation/*_missions_l_*.yml`，键为 `mission_id_title` / `mission_id_desc`；
- 图标：`interface/missionicons_*.gfx`（spriteType 定义）+ `gfx/interface/*.dds`（DXT 压缩）；
- 1.30–1.34 老格式（`country_missions = { missions = { ... } }`）已不用，但大量存量 mod
  仍在用，读取端应兼容。

### 2.2 EMT（mati-k/EMT）参考

GitHub 上最接近的现成项目，C#/WPF 实现，已克隆至本地 `C:/Code/EMT-ref` 作参考。要点：

- 模型：`MissionBranchModel`（树：slot/generic/ai/potential…）+ `MissionModel`
  （name/position/icon/required/trigger/effect），trigger/effect 用通用 NodeModel 树保留
  原始结构，非语义化编辑；
- 功能：树预览、图标选择器（解析 gfx + DDS）、本地化读写；
- 局限：仅 Windows（WPF）、无跨平台能力、无 GPUI、依赖较老（Pdoxcl2Sharp），
  对 1.35+ 新格式支持有限。

结论：借鉴其**数据模型思路与功能清单**，不借鉴技术栈。

### 2.3 ParadoxCode 可复用资产

- `pdx-parser`：loss-aware Script CST（语法错误也能产出 CST）、Localisation 解析、安全
  formatter——正好满足"坏文件也能打开编辑、写回不破坏未知内容"；
- `pdx-game::eu4`：游戏安装发现、路径规则；
- `pdx-analysis` / `pdx-rules`：任务文件校验（悬空引用、循环依赖）可按需复用规则管线，
  但 MVP 不依赖规则数据库。

## 3. 技术选型

| 项 | 选择 | 理由 |
| --- | --- | --- |
| 语言 | Rust 2024 edition | 与 ParadoxCode 一致，单二进制分发 |
| UI | `gpui`（Zed 开源 UI 框架） | GPU 加速 2D 画布 + 自绘节点/边；Windows/macOS/Linux 原生；无运行时 CSS，适合画布类应用 |
| 图布局 | 自研（Sugiyama 分层 + position 约束） | 任务树是小型 DAG（单树几十节点），无必要引入重型图库；布局算法纯函数、可单测 |
| 解析 | 复用 `pdx-parser` CST | 见 2.3 |
| 图标解码 | 首版支持：`windows` crate WIC 解码 DDS（Windows）；跨平台 DXT 软解为后续项 | 本机为 Windows，WIC 是官方稳定路径 |
| i18n | 内置 zh/en 字符串表，运行时切换（跟随系统 locale，设置可覆盖） | 双语确认；术语与 modding 社区一致 |

GPUI 风险：crates.io 版本迭代快、API 不稳定，需锁定版本并随升级同步；Windows 后端为
Zed 官方支持路径，本机可验证。

## 4. 总体架构

设计原则：**文档是唯一真相，图形画布是文档的视图**。与 pdx-lsp 的世界观一致：

```text
missions/*.txt + localisation/*.yml
        |
        v
  pdx-parser CST（loss-aware）
        |
        v
  MissionModel（结构化：树/任务/依赖；trigger/effect 保留原文）
        |
        +-------> 图形视图（GPUI）：节点、边、画布交互
        |                ^
        |                | 编辑操作（CRUD、拖拽连线、属性面板）
        +----------------+
        |
        v
  写回：受控重写 + 未编辑部分原文保留 + 保存前 diff 确认
```

- 编辑操作只作用于 `MissionModel`，再通过"写回器"（renderer）生成新的文件文本；
- 视图层只读模型，通过变更通知重绘（无框架、无状态同步层）；
- 不允许 GUI 逻辑进入 `pdx-lsp` / `pdx-analysis`；GUI 只依赖模型层与 parser。

## 5. 数据模型

```rust
struct MissionFile { trees: Vec<MissionTree> }        // 一个文件可含多棵树

struct MissionTree {
    id: String,
    slot: u32,
    generic: bool,
    ai: Option<bool>,
    potential: Option<Block>,          // 原文保留（结构化为 Block）
    potential_on_load: Option<Block>,
    has_country_shield: Option<bool>,
    missions: Vec<Mission>,
    unknown_fields: Vec<RawField>,     // 未知字段原文保留、原序写回
}

struct Mission {
    id: String,                        // 全局唯一（本文件内）
    icon: Option<String>,
    required_missions: Vec<String>,    // 依赖边（引用同树任务 id）
    position: u32,                     // 游戏内列；拖拽布局写回此值
    completed_by: Option<String>,
    trigger: Option<Block>,            // 原文文本，可整体文本编辑
    effect: Option<Block>,
    unknown_fields: Vec<RawField>,
}
```

要点：

- `required_missions` 保持列表语义（顺序保留），不做 map 折叠（对齐项目"保留替代身份"的
  数据不变量）；
- `Block` 是"原文+可编辑文本"的容器：MVP 以文本编辑为主，后续再结构化；
- id 冲突、跨树引用在加载时诊断，不静默修复。

## 6. 文档保真与写回策略

1. 读：`pdx-parser` 产出 CST → 提取模型；CST 中未被模型化的内容（注释、未知字段、
   trigger/effect 原文）按源顺序附着在模型上；
2. 写：仅对被编辑的 tree block 重新生成文本；同一文件其他 tree 原字节保留；
3. 生成时保留原缩进风格（tab/空格、缩进宽度）与字段顺序（tree 级字段固定顺序 =
   游戏惯例，mission 级字段顺序固定为 icon/required/position/…，未知字段追加在后）；
4. 保存前弹出 diff 预览（新增/修改/删除行），允许取消；
5. 语法错误文件：仍可加载（loss-aware CST），编辑器只允许"重写整个 tree block"，
   并在状态栏提示该文件存在语法错误。

目标：重复打开-保存不产生额外 diff（幂等性测试覆盖）。

## 7. 渲染与交互

画布：

- 平移（空白拖拽平移；固定 1:1 不缩放）、无网格背景、缩略导航（mini-map 后续）；
- 节点卡片：游戏帧贴图 `GFX_mission_icons_frame`（103×123，1:1 无拉伸）为顶，
  **任务的具体图标（59×63，槽位 22,20）渲染在帧贴图的下层**——与游戏 GUI 声明顺序
  一致（`mission_icon` 先于 frame sprite），帧边框覆盖图标边缘；图标经帧的镂空区
  显示，缺失时回退占位块；+ 任务标题（本地化键解析，仅显示值，最多两行，缺失时
  不显示键名）+ 选中/诊断叠加边框；
- 依赖边：完全复刻 EMT `DrawArrows` 的**游戏贴图拼装**——同列用 `gfx_arrow_verticall_tile`
  + `gfx_arrow_verticall_skip_tier` 纵向铺贴，跨列用 `gfx_arrow_left/right_out` 出、
  `gfx_arrow_horizontal_skip_slot` 平铺、`gfx_arrow_left/right_in` 入、
  `gfx_arrow_end` 收尾，锚点坐标与 EMT `AddIcon` 逐条一致；无贝塞尔线；
- 环形依赖、悬空引用在画布上以红色标记，并在右侧面板列出。

布局：
- 布局：**字面坐标映射**——**X = 树 `slot` - 1，Y = 任务 `position` - 1**（稀疏
  slot 保留空列，与 EMT `(slot-1)` 坐标一致）；无 `position` 的任务按文件顺序补位
  （前一个任务的 `position` + 1，游戏对无 position 树的约定）；不做前置重排（不采用
  EMT `RecalculateRealPosition` 的"前置拉下"语义，画布所见即文件数值，拖拽写回
  `position` 可直接预测）；同 slot 树/同行任务直接重叠，由校验诊断提示；前置位于
  依赖者下方时箭头钳到前置行；手动拖拽位置写回 `position`（游戏语义），不引入私有
  布局文件；提供"重置自动布局"；
- 实时性：单树节点数 < 100，编辑后全量重算即可，无需增量布局。

编辑操作（MVP）：

- 新增/删除/重命名任务（重命名同步改本地化键？——不，本地化键保持原 id，重命名任务
  id 时联动提示）；
- 拖拽节点出线到目标任务 = 建立依赖（自动校验环）；
- 右键菜单：删除依赖、删除任务（级联删除入边）、复制；
- 属性面板：icon（下拉自 gfx 索引）、position、completed_by、trigger/effect 文本编辑、
  树级字段（slot/generic/ai/potential…）；
- 撤销/重做（命令式编辑操作，模型级 undo stack）。

## 8. 功能范围

MVP（M0–M3）：

1. 打开游戏目录（自动发现）+ 选择 mod 的 missions 目录，列出全部 tree；
2. 解析并渲染单棵树：节点 + 依赖边 + 本地化标题；
3. 节点 CRUD、依赖连线增删、拖拽布局（写回 position）；
4. 属性面板（含 trigger/effect 文本编辑）、保存写回 + diff 确认；
5. 校验：环、悬空引用、id 重复、position 冲突，画布标记 + 面板列表；
6. 本地化读写（选择语言，编辑 title/desc 写回 yml）。

后续（非 MVP）：

- mini-map、多选框选、复制粘贴；
- trigger/effect 结构化编辑器（结合 EU4 规则数据提示）；
- 1.30–1.34 老格式读取兼容、任务类型（type/branch）可视化；
- 导出截图/分享图。

图标（M4）：解析 `interface/missionicons_*.gfx` 的 spriteType 索引，Windows 用 WIC 解码
`texturefile` 指向的 DDS 并裁剪出对应 sprite 子图；无法解码时回退占位色块。

## 9. 模块划分与依赖方向

建议纳入 ParadoxCode workspace，新增两个成员：

```text
crates/pdx-mission-model    # 模型 + CST 提取 + 写回器 + 校验（纯逻辑，可单测）
apps/pdx-mission-editor     # GPUI 应用壳：画布、面板、命令、undo（依赖 model）
```

依赖方向（保持单向）：

```text
pdx-mission-model -> pdx-parser / pdx-text
apps/pdx-mission-editor -> pdx-mission-model / pdx-game::eu4（安装发现）
```

边界：`pdx-mission-model` 不依赖 GPUI；GUI 层不包含解析、写回、校验逻辑；任务树相关
规则只存在于 EU4 语义中（本编辑器天然 EU4-only，与"EU4-first"一致）。

备选：独立仓库 + path/git 依赖 ParadoxCode。两者均可，见开放问题 1。

## 10. 里程碑与验证

| 里程碑 | 内容 | 验证 |
| --- | --- | --- |
| M0 骨架 | workspace 成员 + GPUI 窗口 + 画布 pan/zoom + 解析本机 `English_Missions.txt` 渲染节点/边 | 启动应用可渲染；model 单测：解析 1 个真实文件 |
| M1 CRUD | 节点增删改、拖拽、依赖连线、属性面板、undo | 单测：操作序列 → 模型断言 |
| M2 写回 | 写回器 + diff 确认 + 本地化读写 | golden 测试：编辑前后文本 diff 幂等；对游戏真实文件做"打开→保存→无编辑 diff 为空" |
| M3 校验 | 环/悬空/重复检测 + 画布标记 | 单测：构造坏树断言诊断；对全量游戏 missions 目录做加载冒烟 |
| M4 体验 | 图标、mini-map、多选、老格式兼容 | 手工验收 + 冒烟 |

质量门禁沿用仓库 `scripts/check-quality-gates.sh` 对应组（`core` 组 + 新增 crate 测试）。

## 11. 风险与开放问题

风险：

- GPUI 版本漂移：锁定版本，升级单独提交；
- DDS 图标解码：Windows WIC 依赖 `windows` crate 绑定，失败时回退占位色块；
- 写回保真：真实文件存在大量注释/自定义格式，diff 确认机制是安全阀；幂等性测试兜底；
- 与 pdx-lsp 产品线的关系：编辑器不依赖 LSP 服务运行，两者共享 parser 资产，互不阻塞。

边界声明：本编辑器是 EU4-only 的独立工具，GUI 逻辑不进入 `pdx-lsp` / `pdx-analysis`；
任务树相关规则与字段含义只存在于编辑器与 EU4 语义中。

## 12. 设计变更（2026-08-16）：摘除"树"概念，只保留任务与依赖边

> 状态：Current（已实施）。这是对 §1–§10 的产品概念重构；文件格式、写回保真、
> 校验管线与模块边界不变。代码内部（`MissionTree`、crate 名、block 结构）保留。

### 12.1 动机

任务树是文件格式的容器，不是用户的创作意图；1.35+ 跨树 `required_missions` 合法后，
树的边界本就名不副实。重构后编辑器只暴露两种概念：**任务**（节点）与**依赖关系**
（有向边），树、slot、position 全部降级为实现细节：

- 用户操作面：双击空白格创建空白任务（自动 id、自动归属）；拖拽节点移动（纵向 =
  `position`，横向 = 换组，空列自动建新组）；拖拽右侧圆点建立依赖边；拖拽列顶组名
  标签整组换列、双击改名。
- 树级字段降级为任务级共享字段（组设置）：组名、列、generic/ai/potential 等在任意
  组员的面板中编辑，一处修改整组生效（写回仍为整块重写）。
- 侧栏树列表移除，替换为可筛选的任务列表（按组分组显示）。新建树/删树操作、
  Ctrl+Shift+N、Ctrl+Delete 全部移除；Ctrl+N 改为在选中任务下方或视口中心附近创建。

### 12.2 空间合法性规则

边 A→B（A 是 B 的 `required_missions` 前置）合法当且仅当（在**有效 position** 上
判定，无 `position` 任务按文件顺序补位）：

```text
position_A == position_B − 1        // 上一行，任意列
或 slot_A == slot_B 且 position_A < position_B   // 同列正上方，可隔行
```

- 这是游戏贴图可正常渲染的全部布局；推论：position 沿边方向严格递减，环与自环在
  创建端几何上不可能存在。
- 创建端强制：`add_required`/`set_required` 拒绝自环、重复、空间非法边（未知 id
  放行，交校验标黄——可能是合法跨文件引用）。
- 加载端警告：`illegal-edge-placement`（Warning）。对真实原版 1.37.5 全量审计
  （228 文件 / 4813 任务 / 3882 边）：可判定边合法率 98.23%，50 条违规多为刻意
  堆叠布局（Tatar 同格 5 连、Maya 阶梯），游戏接受但渲染欠佳；约 23% 任务无
  `position`（旧格式脚本顺序布局），有效 position 补位后自然满足相邻行规则。
- 会话内新产生的违规 = 红边 + 阻止保存；存量文件违规 = 黄边 + 可保存（保真优先）。

### 12.3 校验宇宙

加载端 `validate_in(focus, universe)` 按整个 mod 的 missions 目录解析悬空引用：
跨文件引用合法（原版 DLC 续接树实测 25 条，0.64%）；创建端仍限当前文件。

### 12.4 新增模块与 API

- `crates/pdx-mission-model/src/graph.rs`：扁平"任务+边"概念层——`effective_layout`、
  `is_spatially_legal`、`spatial_violations`、`creation_target`/`group_target`
  （组归属派生：空列建新组、单组列加入、多组列拒绝）、`group_members`。
- `MissionFile::move_mission`：块移动（跨组拖拽），保留引用语义（目标 id 仍在文件内）。
- `EditError::{IllegalEdgePlacement, CellOccupied, AmbiguousColumn}`。
- 编辑器：`validate_in` 宇宙解析、`blocked_mission_ids`（会话内违规集合）、
  双击创建、节点/组名拖拽、跨文件引用"↥ id"标签桩、画布尺寸追踪（视口中心创建）。

### 12.5 验证

- 模型 22 项 + 编辑器 30 项单测通过（含 Q7 空间合法性矩阵、组归属派生、
  新组追加写回、跨组移动写回幂等）；clippy 无警告。
- 真实文件：`English_Missions.txt` / `00_Generic_missions.txt` 0 错误 0 警告、
  `Tatar_Missions.txt` 0 错误 5 警告（同格堆叠链，符合审计预期）。

### 12.6 后续修正（2026-08-16）：旧版单字节编码文件支持

`DOM_French_Missions.txt` 等存量文件使用 Windows-1252/Latin-1 单字节编码（实测该文件
140KB 中仅一个 `0xF4` 字节，即 `"Basse-Côte"` 的 `ô`），此前 `read_to_string` 直接
拒绝加载。新增 `pdx-mission-model::encoding`：

- 无损解码：合法 UTF-8 走原路径；否则按 Windows-1252 表逐字节解码（未定义字节
  走恒等映射，全部 256 字节双射，单测覆盖）。
- 会话记录编码，保存时逆编码回原编码——未编辑内容字节完全不变（编辑后的
  内容若含无法编码的字符，如向旧文件粘贴中文，明确报错而非静默损坏）。
- 加载路径（会话、mod 宇宙、localisation）与 `--dump` 全部接入。
- 验证：`DOM_French_Missions.txt` 0 错误 7 警告（同格/同行堆叠链，符合审计）、
  91 任务全量解析、0 语法错误；模型 16 项 + 编辑器 31 项单测通过；clippy 无警告。

### 12.7 后续修正（2026-08-16）：拖拽系统体验改良

对 §12.1 定义的四类拖拽交互的体验修正（纯编辑器层，`pdx-mission-model` 未改动）：

**交互语义**

- **点击/拖拽阈值**：按下后光标移动超过 4px 才"武装"拖拽（与双击判定同距）；
  点击选中、双击创建、拖拽移动/连线/换列从此互不干扰，平移同样受阈值约束。
- **落点交换**：节点拖到已占用的格子 = 与占用任务交换——同组互换 `position`，
  跨组则 `move_mission` 换组并互换有效位置；无 `position` 的任务按有效位置物化
  为显式值（补位链不漂移）。被占格的目标组由占用任务唯一确定，多组列在此路径
  不再歧义；空格落点维持原语义（单组列加入、空列建新组、多组列拒绝）。
- **落点一致性修复**：松手落点改为取拖拽中最后一次预览的格（此前松手时重新计算
  且未减 `grab` 偏移，抓点在节点边缘时预览格与落点格不一致）。

**拖拽反馈**

- 连线：悬停目标绿=可落 / 红=会被拒（自环、重复、空间非法），连线同色；合法
  目标集在按下时一次计算（`is_spatially_legal` + 去重，模型拖拽中不变）。
- 节点移动：会破坏的依赖边实时预览——端点红框 + 红直线覆盖（箭头贴图不可染色，
  不遮挡贴图）；不做自动重排（保持"字面坐标"设计）。
- 组换列：幽灵标签跟随光标 + 目标列高亮（绿=空列 / 红=被占 / 灰=自身列）。
- 连线把手：视觉随缩放，热区最小 12px（此前 zoom 0.35 时仅 3.5px）。

**自动滚动**：拖拽（节点/连线/组）进入视口边缘 32px 带时按动画帧推进平移，
  光标静止也持续滚动，直到拖拽结束；tick 循环在拖拽武装时启动、结束时取消。

**验证**：编辑器新增 8 项拖拽单测（同组/跨组交换、无 position 物化、连线合法
  目标集、边破坏预览、组列合法性、阈值判定），共 39 项通过；clippy 无警告；
  RFC 12.2 的空间规则、"字面坐标"与阻止保存语义不变。
