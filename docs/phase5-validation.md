# 阶段五本地验收记录

2026-10-01 至 2026-10-03。以下保留各轮冻结结果；当前收口状态以“最终清理与验收”和“退出标准”为准。
阶段五已按本次验收范围完成；89 条待核验资源项按用户明确要求排除。

## 最终清理与验收（2026-10-03）

用户明确排除此前待核验的 89 条资源诊断，继续旧实现清理和既定最终验收。
旧 `RulesModel`、`SemanticRule`、旧规则编译模块、旧规则源和兼容消费者已删除；
`RuleSet` 仅保留扫描目录及由 IR 派生的 Callable 目录。生产语义入口统一为 `first_party_ir()`。
本轮相对清理前快照删除 126 个文件，其中 114 个属于旧规则目录及 manifest。
§1.3 的旧控制流模块已删除，现有控制流、寄存器和 selector 消费者读取 IR 声明。

旧夹具已迁移到显式 IR；只验证已删除模型 API、旧转换器或旧启发式的测试按理由退役。
保留 90 项测试及辅助夹具的迁移/退役记录，不把该数目解释为删除了 90 个行为测试。
本轮 7 份 golden 更新保留前后内容和逐项理由，未批量接受未知差异。
迁移同时补齐引号脚本的补全、错误与 quick fix 范围映射，以及源文件不可用时的缓存名称定位；
静态作用域链候选从声明生成并按实际输入/目标作用域过滤。

最终 release 规则指纹保持 `c82f078ed894a5cd6c7416292599e55d87e0a4a6363eb06542cbb901443e6eea`，
源格式 13、SQLite 24。新建缓存的语料指纹与已审语料相同，587,163 个定义和 431,839 条引用的
完整多重集合均与已审冻结版一致。相对 `e39e045` 的引用移除和新增全部归类，未归类位置为零；
间接参数引用保留实际 Callable、参数使用域及引号 payload 绑定，作用域前缀保留声明证据。

最终全量诊断为 8,670 个文件、11,419 条诊断，其中 8,324 errors、2,631 warnings、464 infos，
工具错误为零。每条 error 的身份与上一轮审查完全一致：7,948 条冻结语料资源缺失、214 条原版问题、
73 条规范性诊断，另 89 条按用户决定排除。本次范围内待审 error 为零，资源诊断仍正常输出。
本轮完整工作区单元、集成及 doc 回归 976 项通过、零失败，IDE 432 项、LSP 129 项均包含其中；
最后一处机械清理后的规则搜索另单独通过。全部目标/特性 Clippy、rustdoc、fmt、
策略与嵌入产物 90 项检查、LSP/MCP smoke、VS Code 契约及 VSIX 打包通过。
既定 11 个完整补全集合及三组最终性能配对均已验收，六项退出标准全部满足本次范围。

最终性能以阶段五开始时 `96f50c8` 的冻结探针为参照，三组安静顺序配对均正常退出、
阶段采样完整且采样错误为零；各版本内部的三次诊断文件数、数量与 digest 一致。
旧版和 IR 规则语义不同，不要求它们的诊断数量相等。

| 指标（三次独立进程中位数） | `96f50c8` | 最终 IR |
| --- | ---: | ---: |
| OS 峰值 RSS | 1,146.75 MiB | 1,071.09 MiB |
| 全量诊断 | 46.0 s | 23.1 s |
| 完整探针（含采样驻留） | 85.9 s | 64.2 s |
| 仅规则加载进程 | 191.5 ms | 22.8 ms |
| 仅规则加载峰值 RSS | 40.66 MiB | 23.09 MiB |

峰值 RSS 中位数下降 6.60%，原内存与规则加载退出条件通过。
完整探针每次扫描 8,680 个文件，旧版诊断 10,463 条、digest `0xb7b5124ba391f79d`，
最终 IR 为 11,633 条、digest `0x9c3650c3de61c98d`；这是 Project 探针，不替代前文 Vanilla 审查。
RSS 范围为旧版 1,090.66–1,149.23 MiB，IR 1,062.97–1,081.94 MiB；OS 压缩和分配器影响绝对值。
首次加载与扫描 wall 中位数仍为 11.5 → 13.2 秒，两边均含 8 秒采样驻留，实际差距 1.7 秒，
作为既有风险保留。它不增加新的退出条件。完整探针旧版/IR 的总驻留分别为 24/32 秒。

完整证据保存在忽略的 `performance-results/phase5/legacy-cleanup/`：`retirement-proof.json`、
`retired-test-ledger.json`、`golden-review.json`、`reference-final-summary.json`、
`final-symbol-identity.json`、`error-final/acceptance-summary.json`、`completion-final-review.json`、
`memory-final/comparison.json`、`rules-loading-final.json` 和 `final/freeze.json`。
本地语料、缓存和冻结二进制继续保持忽略。

### PR CI 兼容性收尾（2026-10-03）

提交 `9277c8d` 后，PR CI 的 Rust 1.99 Clippy 发现原有 `AtomicUsize::fetch_update`
已弃用，当时仓库声明的最低 Rust 版本为 1.98。扫描和测试查询的 checkpoint 计数改用同等内存序的
`compare_exchange_weak` 循环，保留无限预算、耗尽后取消及克隆共享状态的行为。
这不改变规则源、IR 指纹或脚本语义；既定审查不因此重启。

拼写检查另识别一处纠错测试故意使用的 `country_falg`，按既有测试词白名单记录其意图；
审计脚本注释改用正常措辞。PR CI 的最终结果以当前提交为准。

用户随后要求降低 Rust 门槛。锁定依赖图的最高声明要求为 1.88，源码使用 let chains 和
`as_chunks`；最低版本、MSRV CI 和开发文档统一降至 1.88，不更换依赖版本。
实际安装 Rust 1.88.0 后，`cargo +1.88.0 check --locked --workspace --all-targets --all-features`
在本地通过。取消查询与扫描原子快照的两项定向回归、审计脚本四项测试和脚本语法检查通过。
随后使用本地 Rust 1.98.1 重跑全工作区、全目标、全特性 Clippy，`-D warnings` 通过。
以上是 PR 兼容性收尾，原六项语义与性能验收结论仍对应前文冻结版本；新的 MSRV CI 待远端确认。

### 规则目录命名统一（2026-10-03）

按用户要求，当前规则源统一命名为 `rules/eu4`，目录中的 89 个 JSON 文件按原内容移动。
构建入口、检查工具、第一方规则测试和开发文档的路径全部同步；旧规则模型仍保持退役。
本文历史新源命令也使用当前目录名，原始报告与冻结记录继续保留各自身份。
89 个规则文件逐一 SHA-256 比对均与冻结版本一致；规则 81 项回归、嵌入源与磁盘包一致性
回归、产物 2 项及策略 88 项检查通过。IR manifest 可复现，嵌入 IR 与当前源一致，指纹仍为
`c82f078ed894a5cd6c7416292599e55d87e0a4a6363eb06542cbb901443e6eea`。
Rust 1.88 的全工作区、全目标、全特性编译已在更名后再次通过。

## 放宽校验决策重新审核（2026-10-02）

用户重新明确产品目标：LSP 应帮助作者写出规范且语义明确的代码，游戏能够加载某种写法，
不能单独作为取消诊断的理由。此前阶段五的放宽决定全部重新审核。
用户随后完成审计，确认除 R09 外全部保持清单中的现状：R05 仍撤回并保持错误，其他规则及
既有边界保留当前行为。该确认取代下表此前的待审核标记；不是恢复 R05 的历史合并放行。
用户进一步明确 R09：“全面把两个作用域合并，只留下 province”。据此删除 trade_node 执行
作用域和旧兼容声明，全部入口、目标值域、寄存器和链接统一为 province；节点符号类型保留。
该决定是明确的产品模型选择，不把 5 处节点中心案例扩大解释为已证明的游戏运行时事实。

重复 `custom_attributes` 的合并决定已撤回，`government_reform_body.custom_attributes`
恢复 `card: 0..1`，回归改为要求 Cardinality error，错误 bool 值仍单独报错。
同一 `text` 的重复 `trigger`、事件重复 `goto`、国家历史重复 `government_rank` 同样保持错误。
国家作用域调用仅含 `remove_province_modifier` 的 helper，以及将省份 ROOT 直接当作国家参数，
均按用户确认保留诊断。这些是收紧或保留检查，不属于下表的待批准放宽。

盘点使用阶段四提交 `96f50c8` 与当前源的实际声明差异，展开 include 后另作去重，
并核对诊断/Callable 代码与历史验收记录。当前审查指纹是
`c82f078ed894a5cd6c7416292599e55d87e0a4a6363eb06542cbb901443e6eea`，
源格式 13、SQLite 24、1,235 schemas / 13,055 fields / 4,167 matchers / 138 file categories。
这是作用域合并及绑定恢复后的规则身份；新版 Vanilla 全量扫描与逐条审查见下节。
本段记录规则审核时的冻结身份；最终诊断与引用审查结果见前文。各轮完整补全及性能数据保留各自冻结边界。

“游戏事实”与“诊断策略”分开裁决：即使输入可以执行，重复、覆盖、冲突、冗余或无法证明的
语义仍可以要求诊断。每项可批示“保留当前规则”“恢复检查”“改为提示”“待定”；
选择提示时还需明确触发条件，不能整体降低值域或作用域错误的严重性。

| 编号 | 此前放宽或既有宽松行为 | 具体变化与复核边界 | 本轮状态 |
| --- | --- | --- | --- |
| R01 | 核心 trigger/effect 可重复 | `keys__trigger` 的 890 个键、`keys__effect` 的 596 个键改变 card，上限统一可重复；例如两条 `add_prestige` 不再报次数错误。应逐键区分多目标条件、连续操作与冲突/冗余，不能整批视为规范写法；modifier 的次数未作同样放宽 | 已确认保持现状 |
| R02 | 重复 Callable 标量参数取最后值 | 同一调用中的同名参数进入绑定表时后者覆盖；目前缺少相应重复参数诊断。参数转发也沿用该取值机制。游戏取值规则与 LSP 是否应报重复需分别决定 | 已确认保持现状 |
| R03 | 重复命名定义可继续分析 | subject、advisor、自定义本地化等根 pattern 可出现多个同名定义；符号覆盖/独立策略按类型处理，现有正例不把 Cardinality 当错误。需明确哪些位置允许并列、哪些应报告重复或覆盖 | 已确认保持现状 |
| R04 | 国旗 texture 可重复且可省略 | `custom_country_colors_body__textures.texture` 从 `1` 改为 `0..*`。重复与完全没有 texture 是两项效果；不应因多张纹理合法就自动批准空列表 | 已确认保持现状 |
| R05 | 多个 custom_attributes 合并 | 本轮曾把 `0..1` 改为 `0..*`，现已恢复；两个块即报 Cardinality error，不以游戏合并/覆盖行为放行 | 已撤回，保持错误 |
| R06 | 投资判断可不写 investment | `has_trade_company_investment_in_area.investment` 从 `1` 改为 `0..1`；investor 仍必填、未知 investment 仍错误。此前确认“允许省略”，现在需重新确认这种省略是否足够明确 | 已确认保持现状 |
| R07 | 节点省份围城修正合法 | `node_province_modifier` 使用专用 schema，仅接受该位置的 `siege_ability`、`artillery_levels_available_vs_fort`；其他国家修正与错误值仍拒绝；两项修正的局部覆盖统一使用 province | 已确认保持现状 |
| R08 | unit 中两项触发器合法 | `general_with_name`、`mercenary_company` 加入 unit 入口；province 入口仍拒绝。此前确认的游戏事实需重新确认对应代码规范 | 已确认保持现状 |
| R09 | 贸易节点与省份执行作用域合并 | 用户明确批准全量统一为 province；删除 trade_node scope 类型及单向兼容，将所有 scope 声明、scope 值表达式、寄存器、链接和旧夹具迁移。ref<trade_node> 定义/引用/导航保留；历史 5 处中心案例见 r09-scope-evidence.json，不作为任意省份语义的证明 | 已批准并落实全面合并 |
| R10 | 空 custom_tooltip 作为空行 | effect 标量接受 `loc | ''`；其余 loc 字段、未知非空 key 与块形式仍拒绝。需确认空行是否应使用这项明确写法 | 已确认保持现状 |
| R11 | count_one_per_area 可省略 | 投资判断中的该字段从 `1` 改为 `0..1`，值仍只接受 yes；这是此前源精修的另一项可选化，不能与 R06 合并批准 | 已确认保持现状 |
| R12 | 政府改革属性不全部必填 | 实际源差异中 76 个改革字段的最低次数降低，例如 min_autonomy、has_parliament、allow_banners；最大次数仍保持 1。可选属性与重复属性分别审核 | 已确认保持现状 |
| R13 | 变量运算的替代操作数结构 | 7 个 schema 的 `which`/`value` 等局部最低次数降低，由 forms 检查合法组合；允许两个 which 或其他明确组合，不是任意缺参都合法 | 已确认保持现状 |
| R14 | change_country_color 替代分支 | color/country 不再同时必填，forms 要求符合一个合法分支；无操作数、错误颜色元素等仍拒绝 | 已确认保持现状 |
| R15 | estate_loyalty 替代分支 | loyalty/higher_than_influence 可选择合法结构，estate 约束保留；需批准分支本身的明确性，不应把所有子字段设为可选 | 已确认保持现状 |
| R16 | 裸 payload 片段不重复要求外层 guard | `$body$` 展开在现有块内时，不把它当独立块再次要求 limit/factor 等；展开新块仍检查。需要区分误诊修复与取消真实 guard 检查 | 已确认保持现状 |
| R17 | 继承逻辑/迭代器不继承包装必填字段 | OR/AND/NOT、迭代器、加权 entry 等的内层 trigger/effect body 不再重复要求外层 limit/factor/tooltip；外层必填要求保留 | 已确认保持现状 |
| R18 | 文本条件保护的参数允许省略 | 仅替换位置被未激活的 `[[参数]]` 文本条件保护时可省略；普通 if/else、外层未保护的转发与 affix 替换仍必填。保留原先严格的文本替换判断 | 已确认保持现状 |
| R19 | custom_attribute 内置名称 | 加入 65 个明确 builtin 名称，例如 heir、queen、raze_province；其余名称必须来自 custom_attributes 定义，未知属性仍错误。名称白名单与重复块 R05 无关 | 已确认保持现状 |
| R20 | 动态国家标签种子 | country_tag 加入 F00–F99、T00–T99 共 200 个预留名字，不要求文本定义；其他未定义标签仍错误。需要审核完整保留范围 | 已确认保持现状 |
| R21 | 运行时 advisor ID | advisor_exists/is_advisor_employed 接受非负整数；无法从静态文件证明该编号实际存在。负数和任意文本仍拒绝，但“有效整数”不等于“有效顾问” | 已确认保持现状 |
| R22 | same_trade_node_as 接受省份 | 恢复省份 ID/省份目标值域；错误 ID/其他作用域仍检查 | 已确认保持现状 |
| R23 | grown_by_development 多入口 | country 与 province 均接受，数值类型仍检查；需审核两个位置的语义是否明确一致 | 已确认保持现状 |
| R24 | every_trade_node_member_country 从省份出发 | 增加 province 起点，内部进入 country；country 起点仍拒绝 | 已确认保持现状 |
| R25 | 宗教/文化 identity 的 variable: 值 | religion、religion_group、culture、primary/accepted culture 等接受 variable: 导出值；不把任意未知字面名称当成合法变量 | 已确认保持现状 |
| R26 | 删除字段条件及结构推导 subtype | when/unless、church aspect/blessing、改革 legacy/new、帝国改革等结构筛选退役，引用改用基础类型；部分旧限制因此不再表达。显式 country/province event subtype 保留；恢复这些限制涉及方案变更 | 已确认保持现状 |
| R27 | 条件 trait binding 改成可选展示 | 原 subtype 或字段条件驱动的 Localised/HasIcon 必需绑定改为统一、可选展示，例如帝国改革的各角色本地化；其他类型级必需绑定保留 | 已确认保持现状 |
| R28 | 参数化动态符号按声明模式匹配 | 接受由定义中的 `$param$`/affix 形成的名称模式，保留 overlay 遮蔽和实际引用约束；静态分析可能无法证明具体运行时成员存在 | 已确认保持现状 |
| R29 | 补回缺失的封闭结构/字段 | 恢复 modifier 继承及 age/technology/static diplomacy/GFX/map/ancestor/custom GUI 等入口；旧 UnknownKey 会减少，但仍按声明检查类型、card 和 scope。新增 28 个 schema 及其他结构恢复的完整声明差异单独留档，不作为“出现于原版就放行”的依据 | 已确认保持现状 |
| R30 | DLC event picture 枚举兜底 | 609 个明确的 dlc_event_pictures 枚举成员可在没有 sprite 定义时匹配；有实际声明则仍可导航。需审核白名单与真实资源证据，不能接受任意图片名 | 已确认保持现状 |
| R31 | 贴图 .tga/.dds 回退 | 精确路径缺失时按既有加载契约找替代扩展名；此前用户认可回退行为，现在复核是否仍应提示原写法与实际资源差异 | 已确认保持现状 |
| R32 | Unknown scope 暂不报 WrongScope | `scope_allows` 对 Unknown 返回 true，无法证明的 event_target/调用状态保留候选；Invalid 仍拒绝。不能把“未证明错误”表述为“已证明合法”，需明确提示策略 | 已确认保持现状 |
| R33 | 已声明 scalar 联合分支接受字面值 | `ref<T> | scalar` 等的 scalar 分支接受未解析名称，不再创建伪 unresolved reference；需逐项审查哪些结构本就允许任意文字，哪些应改成封闭名字 | 已确认保持现状 |
| R34 | 未解析 Callable 的参数保守处理 | 未知调用自身仍诊断，但没有签名时不臆造参数值域或连带参数错误；需决定是否明确发布“参数未验证”提示 | 已确认保持现状 |
| R35 | 3 个开放 schema | gfx_engine_object、open_script_file、scripted_localisation_body 允许未声明字段；是当前显式放宽点，包含新增开放入口与既有入口，均需逐项说明 | 已确认保持现状 |
| R36 | 6 个开放类型 | dynasty_name、event_target、global_event_target、named_unrest、saved_name、variable 不因没有静态定义直接报 unresolved；variable 是阶段五新增开放，其余含既有策略 | 已确认保持现状 |
| R37 | 83 个 fallback key | 与阶段四列表一致。生产 IR 的诊断不读取该高亮词表；semantic tokens 和旧兼容路径仍使用它。不是新增的 83 个全局 UnknownKey 豁免，但旧路径的诊断边界仍需随清理核对 | 已确认保持现状 |
| R38 | 本地化文件发布空诊断 | 沿用既有 prose/Localisation 的发布契约，仍索引符号与检查脚本中的 loc 引用；不代表本地化文本本身通过语法/重复/规范审查 | 已确认保持现状 |

逐键材料保留在忽略目录 `performance-results/phase5/relaxation-review/`：
`repeatable-keys.json` 列出 1,486 个核心键的旧/新 card；`optional-fields.json` 列出 99 条
真实声明的最低次数降低记录（包含上述 76 个政府改革字段和受 forms 约束的字段）；
`direct-rule-changes.json` 保存 1,156 个变化声明的前后内容；`source-changes.json` 与
`field-changes-grouped.json` 保存 include 展开后的逐位置对照。后者包括继承副本、收紧和
结构恢复，不把这些技术记录数当成独立的放行决策数。

架构调整、缓存失效、性能优化、导航恢复及审计语料缺失分类没有自动批准以上规则放宽。
旧冻结报告继续保持自己的指纹和版本，不修改其历史测试数字，不将本轮局部验证冒充新版全量验收。
本轮撤回后的生产 IR IDE 回归为 **444 passed、0 failed**，重复属性专项、fmt 和
diff whitespace 检查通过。该 444 条结果属于合并前、恢复重复属性错误的版本。
R09 合并后的验证另列下节；全量冻结与退出验收仍须对新身份独立执行。

## R09 全面合并：仅保留 province 执行作用域（2026-10-02）

按用户明确决定，取消 trade_node 执行作用域及 trade_node → province 兼容关系。
迁移 150 处 push、32 处入口、4 处寄存器设置、1 处链接起点和 7 处 scope 值表达式；
合并重复的 province 入口和 union 分支。贸易政策 FROM、node_province_modifier 的 ROOT/THIS、
节点名称块和所有节点迭代器均使用 province；原国家入口限制和内部 country/province 转换保留。
旧 profile 和旧语义夹具同步迁移，并重新生成其发布清单；该夹具身份为 source 10 /
a5d0fd02（8,463 semantic rules、124 文件类别），与上面的生产 IR 身份分别记录。
节点定义及 ref<trade_node> 仍使用节点符号命名空间，以保留节点名解析、补全和导航。

全量扫描新规则与旧夹具的执行作用域声明，trade_node 残留为 0。
对合并前 f1eb8e88 与合并后 967c5c5e 的完整烘焙 JSON 做结构归一化比较：仅映射该 scope、
消除重复 union/入口和重映射 matcher 编号；全部其他 IR 数据一致，没有改动另 37 项的语义。
schema、field 和文件入口数量不变；matcher 从 4,168 降为 4,166。
证据保存在 performance-results/phase5/province-scope-merge/ir-invariants.json。

新增回归检查任意省份入口、原节点专有条件/效果、迭代器及命名节点进入 province、
国家入口错误、省份补全、贸易政策 FROM，以及节点符号的定义导航。
通用语言的单向 compat 能力仍用虚构 district/province 夹具独立检查。
合并后验证：全工作区 **1,049 passed、0 failed**；独立生产 IR IDE 回归
**448 passed、0 failed**。strict Clippy（全 workspace/all-targets/all-features）、Rustdoc
（-D warnings）、fmt、3 个 artifact/policy 门禁、规则格式检查和 release 构建均通过。
rulec check 为 0 errors；268 条非错误信息分别为 255 个 UnusedDefinition warning、
4 个 CardLint warning 和 9 个 CardLint info。执行作用域不再有 trade_node 声明或 matcher。
验证清单与完整日志保存在 performance-results/phase5/province-scope-merge/validation.json
及同目录日志。当前 feat/rules-v2 的 HEAD 仍为 96f50c8，本轮修改尚未提交。
合并交付时既有 Vanilla/补全/性能冻结报告仍归属于原身份；随后重跑的语义审计见下节。

## 合并后语义审计与列表引用修复（2026-10-02）

重新冻结生产 server、索引工具和 manifest，分别保存 SHA-256，再独立重建 SQLite 24 缓存。
`ir-semantic-province-s24` 完整诊断 8,670 个文件：8,325 errors、2,631 warnings、464 infos，
工具错误为零。此前用户确认的 221 条决定全部精确匹配当前诊断身份，没有丢失或默默替换。
IR 仍为 source 13 / `967c5c5e…`；语料指纹为
`d86784fcc1743a5e652247e9147f67296777584e6434d865634c889c6a6e6b1c`。

发现并修复一处分析器错误：列表 schema 在收集引用时扫入嵌套字段的裸值，
把 area 的 `color` 分量误当省份引用。HIR 现在只让直接列表成员使用该列表的 matcher；
嵌套字段的值由自己的 schema 收集。新增空旧模型的生产 IR 回归验证颜色不产生省份导航、
真实省份仍可导航、未知省份仍报错、颜色非数值仍报错。未改变规则声明或放宽诊断。

修复后重新冻结 `ir-semantic-list-s24`，同一语料独立缓存及全量报告确认：
8,670 个文件，**8,324 errors、2,631 warnings、464 infos**，工具错误为零。
相对修复前仅移除 `map/area.txt` 颜色分量 `0` 的一条 InvalidValue，新增 error 为零。
定义仍为 587,104 个；完整缓存引用从 431,930 降为 431,780。
逐位置核对移除的全部 150 个引用：全部来自 `map/area.txt` 的 `color` 块，域为 `int[0..]`，
不是省份引用；没有其他位置的引用变化。11 个 LSP 探针及其前缀样本输出保持一致，
这项比较不把有截断的输出当作新的完整补全验收。

| 当前 error 归类 | 数量 | 审查边界 |
| --- | --- | --- |
| 审计语料缺失 | 7,948 | 沿用相同语料、相同诊断身份的冻结资源证据，未进行新的安装核查 |
| 用户确认或已留证的原版问题 | 214 | 本轮新增五处 modifier/primary_culture 作用域和一处省份 set_saved_name 调用，用户均确认不合法 |
| 保留的规范性诊断 | 73 | 包含此前 13 条；新增 21 条同一父块重复声明、17 条未保护的参数替换、14 条明确作用域/guard 问题，以及 8 条定义契约或闭合引用域问题 |
| 资源待审查 | 89 | 60 条 event picture 名称、29 条贴图/着色器路径 |

本轮人工决定共 287 条，每条保存实际位置、源码上下文、原因与证据。
重复声明逐块核对两处位置，不推断游戏覆盖/合并行为；文本条件保护与未保护的外层转发按
用户保留的 R18 判断；国家/省份限制与真实外层 guard 不因原版使用而放宽。
剩余 89 条全部属于资源项，不将相似名称候选当作合法别名或已证明的替换目标。

用户确认 EU4 已卸载，原安装路径不可用。新增审计脚本的冻结资源复用入口，要求前后缓存
语料指纹、根目录和各自规则身份一致，并核对原报告 SHA-256；仅继承精确匹配的诊断身份。
报告记录 `historical-exact-diagnostic-identities` 与 `live_installation_checked: false`，
保存原资源清单 SHA-256；旧 ZIP 读取结果属于历史证据。新增回归验证安装缺失时精确复用、
新资源诊断不继承旧结论，以及语料漂移、规则身份错配和原报告改动的拒绝。

当前缓存相对语义基线 `e39e045` 的完整引用位置清单已导出，包含实际范围、多重出现次数及
大小写折叠后的独立对照。旧定义名称成员只用于筛选复核，不能证明旧引用正确或新引用丢失；
筛出的 3,587 个位置中，3,473 个已按实际类型域、字面量优先、绑定范围变化及可选展示政策解释，
114 个继续待审：50 个 rebel demands 派生本地化、40 个 ancestor 条件描述绑定、
13 个 papacy 命名定义/结构字段，以及 11 个 tutorial 文件 schema 覆盖位置。
这 114 个位置保存前后定义、当前引用和声明证据；不因旧版产生过引用便自动恢复，
也不把绑定声明缺失默认为已批准的功能退役。R26/R27 的既有决定保持不变。
该候选子集不是完整引用验收：其他移除位置及新增位置的域与绑定归类仍须继续。
资源审查、完整引用验收及旧模型最终删除均未完成。
当前全量语义验收仍未通过，不改写旧冻结报告的历史数字。

证据保存在忽略目录 `performance-results/phase5/semantic-review-list-s24/`：
`summary.json`、`errors.json`、`decisions.json`、`review-queue.json`、`resource-blocker.json`、
`fix-comparison.json`、`list-reference-fix-proof.json`、`reference-candidate-summary.json`、
`reference-candidate-open-findings.json` 及完整引用位置清单。
生产二进制、源快照和缓存身份位于 `ir-semantic-list-s24/frozen-identity.json`；
修复前报告与重复声明逐块证据保留在各自目录。源格式、IR hash 和另 37 项已审核政策均未改变。

本轮工作区全特性回归 **1,050 passed、0 failed、0 ignored**；独立
`PDC_TEST_FIRST_PARTY_IR=1` IDE 回归 **449 passed、0 failed**，不与工作区数字累加。
审计工具 Python 回归 **2 passed**；严格 Clippy、Rustdoc、格式及 whitespace 检查通过，
artifact/policy 的 3 项门禁通过。完整命令、日志与身份保存于本轮 `validation.json`。

## 引用差异续审与绑定恢复（2026-10-03）

上一轮 114 个待审位置已逐项分类，发现并修复三处绑定遗漏，不恢复全局字段猜测或旧条件模型。

| 原待审组 | 审查与处理 |
| --- | --- |
| rebel demands：50 处 | 原版 50 个字段使用 49 个不同基础名，全部有配对的基础键与 `_desc` 键，英文语料也全部具备。基础键是短标题，派生键是说明文本；结合旧规则及保留的 `rebel_demand_loc` 必需描述契约，恢复字段的该类型定义，并保留基础键的必需检查。缺少任一键仍错误；这是完整语料与已有契约的证据，未执行游戏运行时验证 |
| ancestor 描述：40 处 | 规范类型迁移时漏掉描述角色，恢复 `desc_ancestor_$_personality` 的可选展示。缺少描述不报错，普通引用索引不收录该可选角色；名称仍必需，遵守已批准的 R27，未恢复 subtype/字段条件 |
| papacy：13 处 | 9 个既有行动字段关联现成的 `papal_policy` 类型及其必需本地化。另 4 个位置是已声明的结构容器或 modifier 位置，旧宽泛 descriptor 将其误作行动定义；继续按原结构处理，不给它们补上策略定义 |
| tutorial：11 处 | 文件只进入显式 `open_script_file`，没有专用 effect/trigger schema。D9 要求引用来自结构声明，因此不沿用旧全局字段猜测；这里的类型校验和导航覆盖仍是明确的能力限制，不能算作已实现 tutorial 支持 |

新增生产 IR 回归验证需求的两项必需绑定、任一缺失及空值错误、基础键导航、描述 hover，
以及行动定义与结构字段的区分。祖先回归补充描述展示与缺失描述仍合法。
新增两项测试，不将同一组 IDE 测试的不同运行次数累加。

重新冻结 `ir-semantic-bindings-s24`，IR 为 source 13 / `c82f078e…`，独立重建 SQLite 24。
完整缓存相对上一轮精确增加 59 个定义（50 个需求身份、9 个教廷行动）及 59 个本地化引用，
没有移除记录或其他位置变化。59 个引用全部恢复在原候选的精确范围；40 个祖先描述按可选
展示策略恢复，不使普通引用计数增加。逐位置证据保存于 `candidate-review.json` 和
`cache-binding-fix-diff.json`。

相同语料指纹下，全量扫描仍诊断 8,670 个文件：8,324 errors、2,631 warnings、464 infos，
工具错误为零；相对上一轮所有诊断的 code、severity、位置与消息均无增删。
287 条人工决定全部精确匹配；error 仍为 7,948 条冻结资源缺失证据、214 条原版问题、
73 条规范性诊断，以及因游戏卸载无法现场核验的 89 条资源待审项。
11 个 LSP 补全探针及既有前缀样本完全一致；这不替代有截断时的完整补全验收。

完整引用位置清单已按新缓存刷新。相对语义基线 `e39e045`，完整缓存为 431,839 个引用，
移除位置 1,601,566 个、增加位置 224,878 个。这些总数不是语义批准。
另逐类核对 110 种 `eu4-path-*` 分类标签：旧缓存没有对应的活动定义，生产类型目录也没有
对应符号类型，故其 1,127,883 条移除记录属于旧文件分类伪引用；这项证明解释记录移除，
不代替其源操作数的语义检查。其他移除位置及新增位置仍须继续核对域、绑定与导航。
原 114 项的待定原因已解释，tutorial 覆盖限制保留；完整引用验收、资源核验和旧模型最终
删除仍未完成，不宣布阶段五通过。

本轮冻结身份及构建输入清单位于 `ir-semantic-bindings-s24/`；审查、全量差异、完整引用清单、
测试日志和验证身份位于忽略目录 `performance-results/phase5/semantic-review-bindings-s24/`。
现有原版本地化内容仅保存于该忽略目录的语料证据文件，不进入公开文档或发布产物。
工作区使用 `PDC_TEST_FIRST_PARTY_IR=1` 的全特性回归为 **1,052 passed、0 failed、0 ignored**，
其中 IDE 为 **451 passed**；独立生产 IR IDE 运行也为 451 passed，不累加为额外覆盖数量。
release 构建、严格 Clippy/Rustdoc、Rust/规则格式、whitespace 和 artifact/policy 三项门禁通过。
完整命令、日志与冻结身份见本轮 `validation.json`。

## 后续实现：D16 与任务树能力边界（2026-10-02）

- 源加载器递归读取所有普通 JSON 文件（根 game.json 是配置），统一按相对路径排序；
  删除 eu4 源 manifest，将 source_format_version 和 target_game_version 移入 game.json。
  非 JSON 文件不影响编译；链接、逃逸/别名路径和重复输入拒绝；新增文件无需改清单。
  真实源回归不再因缺少 manifest 而静默跳过。
- SQLite 升为 24，持久缓存记录 analyzer build_id。戳覆盖分析器代码、规则包、Cargo 配置与
  lock、编译器、target 和编译选项；Vanilla/依赖缓存按戳重建，失败拒绝陈旧缓存。
  刷新和安装也核对戳；rule_hash/ir_hash 保留作报告身份，不触发失效。
  语法树缓存同步升为 schema 7，按同一构建戳失效，旧前端作为普通 miss 重解析。
- game.json.mission_view 声明路径、节点命名空间、字段名和稳定写回顺序；缺少能力时关闭
  该视图。IDE 的任务诊断与 hover、pdc 预览经能力接口调用，不按 EU4 身份分支。
  通用依赖图组装、迭代式环检测、有序字段写回和块替换移入 engine::structure；
  网格/EMT 形态及布局约束留在游戏包。规则配置拒绝不完整字段、重复名字和非法顺序。

架构调整后、下述规则修正前，完整烘焙 JSON 对比（非抽样）确认：除新增 game.profile.mission_view 配置外，
所有 arena、字符串、schema、matcher、字段、文件入口和既有 profile 逐字节结构相等。
该中间版本指纹为 ce150411220ad945f790d31d6cc623ba873bda54428895debfe0eb642ffa58a6，
源版本仍为 13，1,234 schemas / 13,046 fields / 4,168 matchers / 138 file categories。

本轮原版规则语义判定及旧模型最终删除仍需独立验收，不能由架构回归通过推断。
以下冻结版本 13 / SQLite 23 的审计与性能数字保持其原版本边界。

## 原计划低风险收尾（2026-10-02）

- 新增 `rulec fmt <source-dir>`、`--expanded` 和只读 `--check`。整套 88 文件在临时副本上
  逐模式验证幂等性；紧凑与默认值展开模式均保持相同 IR 指纹。仓库源采用紧凑规范格式。
- 安装事实从 Rust 常量移入 `game.json.install`；`crates/game/build.rs` 根据该数据生成原静态
  descriptor。各平台执行标记、校验目录、发现目录名和 Steam 身份不变，新增嵌入配置与
  实际 descriptor 的完整一致性回归。安装配置不参与脚本匹配或符号收集。
- 删除 8 个未使用 mixin，将另 28 个展开到 32 个源使用位置，移入 1,938 个字段键；
  只保留 4 个使用 25–86 次的共享词汇 mixin。字段原有重载顺序、scope、card 和 control 保持。
- 对收尾前后烘焙 IR 比较全部命名 schema 和匿名结构、文件入口、matcher、类型、trait、enum
  和 scope 语义：864 个命名 schema、138 个文件类别、175 个类型一致。仅排除 arena 编号、
  真实源文件/JSON pointer 和新增安装元数据；不把溯源或身份变化当作行为变化。
- 当前源格式 13、SQLite 23；指纹变为
  `fd079aa53a8e61ee46baa0767b18ca4d67bd9e29ec9b72229f69df2f1f54218b`，
  仍为 1,234 schemas、13,046 fields、4,168 matchers、138 file categories。
  `rulec check` 为 88 文件、0 errors、268 个未使用声明 warnings；无未使用 mixin。

本轮不裁决保留的 268 个 Vanilla error 身份，不删除仍依赖旧夹具的兼容路径，未改写既有
冻结审计和性能证据。当前身份的全量语义/性能验收未重新执行；剩余实施项见
`rules-redesign.md` §8.1。

本轮验证：workspace/all-features 单元、集成与 doc 测试 **1,036 passed、0 failed**（36 个 suite）；
`PDC_TEST_FIRST_PARTY_IR=1` 的 IDE **438 passed、0 failed**。全目标全特性 Clippy 与 Rustdoc
均在 `-D warnings` 下通过，fmt、源格式 `fmt --check` 和 diff whitespace 通过；
artifact **7/7**、policy/editor syntax **88/88** 通过。新增三项回归分别验证格式化后的 IR 与
有序分派、CLI 完整解析预检和只读检查、安装 descriptor 与嵌入配置的完整一致性。

## Vanilla 语义审查口径（2026-10-02）

按用户确认，验收覆盖当前冻结版本的全部 error，不仅是相对旧版新增的 error。
旧版只作定位参考，不要求数量或行为完全对齐。每条诊断保留位置、上下文、原因和证据，
区分原版脚本问题、规则声明问题、分析器问题及审计语料缺失。无法确定的游戏语义，
用具体脚本、实际诊断和需要确认的行为向用户提问；未确认项保持待审查。
规则和分析器误报需修复并回归，不能以放开全部未知值或降低严重性来替代解释。

`lab/perf/audit-errors.py` 按冻结 manifest 核对全量报告，导出每条 error 的稳定身份、
源码上下文、资源证据与审查状态。原安装与 DLC ZIP 只读检查；报告及源码片段
保留在忽略的 `performance-results/` 中。资源存在但文本副本缺失可解释为语料缺失；
在当前安装中找不到资源，不能直接推断原版有错，仍需检查安装完整性、命名与加载语义。

## 当前全量 error 审查（源版本 13，2026-10-02）

冻结 `ir-semantic-v13` 的运行时与索引工具，分别记录二进制 SHA-256。
IR 指纹 `37322dd0516fe88aafe62427df912f6277b59612a5f3a54bee339d0e479d3de3`，
SQLite 23；独立缓存索引 8,681 个文件，语料指纹与此前文本副本一致。
全量报告诊断 8,670 个文件：8,343 errors、2,634 warnings、464 infos，工具错误为零。
本轮是完整 error 审查，不重新测量补全集合或性能。

| 审查结论 | error 数 | 证据 |
| --- | --- | --- |
| 审计语料缺失 | 7,948 | 6,849 个贴图路径在原安装或 DLC 中存在；674 个路径按现有 `.tga`/`.dds` 解析契约找到资源；425 个事件图片引用有 DLC 内直接 `spriteType` 声明，其中 4 个按现有符号查询的大小写折叠匹配 |
| 原版问题 | 114 | 40 个调用漏传无条件提示使用的参数、67 个违反已确认的首都链接起点约束、4 个政府机制名拼错、同一处漏写等号产生的 2 个错误、1 个贴图扩展名拼错 |
| 用户确认保留的诊断 | 13 | 1 个未知 `all_owned_province_cumulative`；12 个参数展开后引用未定义 `custom_attribute` 的错误 |
| 待确认／继续审查 | 268 | InvalidValue 67、Cardinality 55、WrongScope 114、EmptyScopeContract 3、UnknownTexturePath 29 |

这 8,343 条诊断在 `semantic-review-v13/errors.json` 中逐条记录，身份唯一且与报告总数一致；
每条已归类项保留证据，未解释项保留原诊断与源码上下文。ZIP 读取错误为零。
`decisions.json` 保存人工归类，`review-queue.json` 列出剩余全部身份和分组。
报告、语料、缓存、冻结二进制及这些证据均在忽略目录中。

用户已裁决 `all_owned_province_cumulative` 继续报未知，未定义的 `ref<custom_attribute>`
继续现有诊断。这 13 条按 `valid-diagnostic` 留证；不新增迭代器声明、补入属性名或改变
模板参数检查的严重性。该裁决确认诊断策略，不额外推断游戏引擎是否识别这些名称。
其余待审查项包括重复字段、作用域要求和少数未找到的资源。
未确认的游戏语义不据此修改规则。当前 Vanilla 语义验收仍未通过。

用户于 2026-10-02 决定本轮 Vanilla error 审查到此为止，转入下一模块。
保留 268 条待审查身份和上述四类待裁决问题；停止本轮逐条追问，不将它们默认批准。
下一模块为固定位置补全验收，核对当前代码下的完整候选及其来源、作用域和插入内容。

用户随后确认空 `custom_tooltip` 可作为空行，且 `.tga`/`.dds` 回退符合游戏加载行为。
effect 的标量 `custom_tooltip` 精确接受 `loc | ''`，其余 localisation 字段继续拒绝空值，
块形式继续拒绝。新增空旧模型 IR 正反例；source 格式仍为 13，当前指纹变为
`0f0a2046bff103edb0aee05ab17ccc211ad800f678bde1241d8f673020954294`，matcher 数 4,168。
生产 IR IDE 432 passed、0 failed；artifact 7 项、fmt 和 diff whitespace 通过。
上表的全量审计对应修复前冻结版本，不直接减去 3 个空提示 error 来宣称新版审计已通过。
剩余项目仍待语义确认和后续同版重跑。下一批具体问题为：投资判断省略 `investment`（1 条），
贸易政策 `node_province_modifier` 使用两种围城修正（4 条），同一政府改革重复
`custom_attributes`（3 条），以及 `.gfx` 的 shader `effect` 引用 `.lua` 而安装中
只有对应 `.shader`（5 条）。这些问题均保留原诊断，未按原版出现次数放宽规则。

`audit-errors.py` 的回归验证同一报告中重复身份的完整覆盖、DLC 声明与扩展名回退证据、
不能把语料实际持有的 ZIP 当作缺失、冻结规则不匹配、错误总数不一致及过期人工决定的拒绝。
全量实际报告再次核对每条 error 恰好出现一次，脚本回归与 diff whitespace 通过。

## 迁移器退役（2026-10-02）

删除 `crates/tools/src/bin/rules-migrate.rs`、`crates/tools/src/migrate.rs`、
`crates/tools/src/migrate/expr.rs` 及模块导出，Cargo 不再提供 `rules-migrate` binary。
新规则源直接维护；`docs/rules-migrate-report.md` 标记为历史转换记录，覆盖率表和人工清单原样保留。
本次不删除旧模型、旧规则加载器或 `rules/eu4`；其最终删除仍依赖阶段五语义验收。

迁移器的 10 个专属测试删除后，行为断言由以下直接新源/IR 测试接管。
新增 `crates/rules/tests/first_party_ir.rs` 通过 bundle loader 加载完整新源并执行 checked lowering，
不读取旧规则，也不调用转换器；新源缺失或编译失败会使测试失败。

| 原迁移断言的行为 | 当前回归位置 |
| --- | --- |
| 常量谓词、普通 bool、trigger/effect 迭代器、scope link、可选调用与加权分支元数据 | 新增 `predicates_iterators_and_weighted_branches_keep_their_behavior`；IDE 的 `ir_logic_constant_lints_require_a_declared_constant_predicate` 验证诊断行为 |
| mission 无额外 map 包装、gfx wrapper 与 sprite 定义、war 使用文件名 | 新增 `nested_definitions_use_their_actual_body_and_name_source`；增强 `ir_file_named_war_definitions_keep_the_filename_identity`，要求恰有一个文件名定义 |
| 继承 modifier 字段与类型模板、numeric/bool trigger 集 | 新增 `inherited_modifiers_and_numeric_trigger_domains_validate_values`，同时验证非法成员和值拒绝；IDE 的 `ir_modifier_containers_inherit_fields_and_templates_with_value_checks` 验证实际诊断 |
| 原结构标志不限制基础类型引用，显式 event subtype 仍有限制 | IDE 的 `ir_structural_flags_do_not_filter_symbol_references_or_completion`、`ir_event_subtypes_restrict_on_action_references` |
| 静态 enum 与动态引用分开处理 | IDE 的 `ir_enum_literals_are_validated_without_workspace_reference_errors` 与 `ir_country_tags_validate_against_definitions_and_runtime_seeds` |
| 字面量转义、模板洞 | expr 的 `literal_escapes_and_nested_literals`；新增 modifier 模板 IR 匹配正反例 |
| 已删除文件覆盖策略的拒绝 | source 的 `unused_trait_metadata_enum_tables_and_file_policy_are_rejected` |

旧别名归一、旧 parent_path 段拼写、JSON 表达式渲染与转换器自身错误信息属于转换专属断言，随工具删除。
可选调用断言保留最小次数为 0，不恢复已经被精修源改掉的 `add_prestige` 最大次数 1；
modifier 继承检查实际可用的字段和值域，不再依赖旧转换器生成的 mixin 名字。

退役验证：workspace 1,022 passed（新增 3 个直接新源测试，删除 10 个迁移专属测试），
其中 tools 24 passed；`PDC_TEST_FIRST_PARTY_IR=1` 的 IDE 431 passed。
Clippy（all targets）与 Rustdoc 均在 `-D warnings` 下通过，fmt 和 diff whitespace 通过；artifact 7 项通过。
Cargo metadata 确认 tools 仅有 `tools` binary；代码内没有迁移器引用，历史报告的统计正文逐字节未改。
本次不改变规则源或 IR 指纹，不重新执行全量 Vanilla 与性能验收。

## 当前源版本与 IR 机制简化（源版本 13）

2026-10-02 删除通用 trait 声明的 `params`、`bindings`、`requires`，
`ref<impl Trait>`、Callable capability 标签、枚举属性列与自动行分组，
以及未使用的 `ai_personality.ai` 分类和规则源/IR 的 `replace-directory` 选项。
trait 保留名称标记，Callable 由其 trait 身份和类型 impl 的 body 识别；
类型 impl 的 194 个参数和 binding 逐项保持不变。显式 on_action scope 分组、
参数化 schema、forms、ModifierSource 诊断和 VFS 文件层覆盖继续使用。

当前 IR 指纹为 `37322dd0516fe88aafe62427df912f6277b59612a5f3a54bee339d0e479d3de3`，
共 1,234 schemas、13,046 fields、4,167 matchers、138 file categories。
指纹已写入 `rules/ir-manifest.json`，索引缓存沿用现有 `ir_hash` 不匹配失效机制。
当时 JSON Schema 和迁移器同步更新；迁移器遇到旧 `replace-directory` 规则明确拒绝转换，随后按上节退役。

规则源检查：88 个源文件，0 errors、275 个 `UnusedDefinition` warnings。
迁移器两次输出的 91 个文件与报告逐字节一致，生成结果为 89 个规则源文件，
0 errors、267 个 `UnusedDefinition` warnings，且无上述已删除机制或重复枚举定义。
验证结果：workspace 1,029 passed、生产 IR IDE 431 passed；Clippy、Rustdoc
（均 `-D warnings`）、fmt 和 diff whitespace 通过，artifact 7 项通过。
回归覆盖删除语法的拒绝、类型 binding、Callable 参数替换与重放、ModifierSource
诊断、事件 subtype 引用限制、完整 on_action 补全以及迁移源的完整语义检查。
本轮未重新运行全量 Vanilla 与性能对照；以下源版本 11 的数据仍是历史冻结记录。

## 条件语义简化记录（源版本 12）

2026-10-02 移除字段 `when`/`unless`、结构推导 subtype 和 subtype trait 门控，
源格式升为 12。所有字段、本地化和图标 binding 统一可用；原条件必需 binding 改为
可选展示，原有类型级必需 binding 保持要求。显式定义位置 subtype 与脚本控制流继续使用。
当时 IR 指纹为 `25bd71e41e62e739ed667b179d7ae7915cd8f9040e63497610e941eeaa3b5660`，
共 1,234 schemas、13,047 fields、4,168 matchers、138 file categories。
以下全量 Vanilla 和性能对照对应简化前冻结版本，不代表当前源版本的全量验收。

本次简化验证：workspace 1,030 passed、生产 IR IDE 431 passed；最终迁移器修复后
tools 33 passed。Clippy、Rustdoc（均 `-D warnings`）、fmt 和 diff whitespace 通过，
artifact 7 项通过。`rulec check` 检查 88 个源文件，0 errors、276 个
`UnusedDefinition` warnings。迁移器两次输出的 91 个文件逐字节一致；生成结果和当前
精修源均无 `when`/`unless`、subtype trait impl 或旧结构推导 subtype 引用。
173 个原类型级 trait 参数保持不变，22 个 subtype binding 已提升，14 个条件必需
binding 改为可选。新增回归覆盖统一绑定、同文不同标签的 hover，以及基础类型引用和补全。

## 历史全量对照冻结版本（源版本 11）

`ir-bindings-acceptance` 使用 IR 指纹
`3250e7ac96cac3f2259d53e8f2433704c3d3b2a1bd5e41ed9e1037c915fefa62`、SQLite 23。
规则版本 11，共 1,234 schemas、13,047 fields、4,180 matchers、138 file categories。
运行时、索引工具和内存探针分别固定二进制 SHA-256；完整记录在本地
`binaries/ir-bindings-binaries.json`。下表对应这一冻结版，后文早期轮次仅保留定位依据。

| 项目 | 结果 |
| --- | --- |
| 常规 Rust 门禁 | 1,033 passed；fmt、Rustdoc、Clippy、artifact 7 项、policy 88 项通过 |
| 生产 IR 对照 | 430 passed，其中 80 项使用空旧模型 |
| VS Code | 最终完整 CI 通过；冻结 release 的 LSP、startup、serverPath、MCP smoke 通过；VSIX 441 文件、864.67 KB |
| 全量 Vanilla | 8,670 文件；8,344 errors、2,635 warnings、464 infos；无工具错误 |
| 完整缓存 | 8,681 文件；587,104 definitions、432,652 references；成功加载 |
| 新增诊断 | 精确身份新增 741、移除 520；按位置新增 286、移除 65 |
| 按位置新增分类 | UnknownKey 3、InvalidValue 101、Cardinality 93、WrongScope 89 |
| 定义差异 | 31 类计数依据已按此缓存重新核对；引用差异为 194 类，需继续核对位置和语义 |
| 固定补全 | 11 个完整集合，前 512 项全部与 LSP 一致；7 组相同、4 组差异，与 roundtrip 轮集合相同 |
| 安静规则加载 | 三组顺序配对：中位耗时 190.9 → 19.2 ms；峰值 RSS 中位数 40.95 → 23.23 MiB |
| 完整内存 | 三组顺序配对均成功；峰值中位数 1,013.83 → 931.61 MiB（下降 8.11%）；释放后 RSS 902.27 → 510.55 MiB |

最新对照为 `comparison-bindings.json`；完整报告在 `ir-bindings-acceptance/`。
逐类定义依据为 `definition-review-bindings.json`，引用位置差异为
`reference-site-review-bindings.json`，新 error 位置为 `new-error-locations-bindings.json`。
全量报告与缓存均保持忽略。

本轮修复包括：联合值域误记自由文本为国家标签、未知作用域下选错值域、
重叠 subtype/namespace 丢失实际引用、预览与效果重放造成属性摘要重复、
18 个原版脚本函数被 68 个精确字段声明遮蔽、标量参数缺少实际引用、legacy 改革分支断链。
脚本参数导航只索引直接名字和精确的类型洞；整目标的部分词缀尚无安全的重命名编辑契约，
不将它们伪装成完整名字引用。参数中的写入也不按标量名字推造定义。
另恢复计数器说明和联邦提示的本地化引用，以及 `ai_` 名称模式和改革块的类型绑定。
当时 `when` 中的 `opaque` 检查字段存在性，其他非空类型条件要求标量；这些结构条件现已删除。
补齐已知 province 上下文中的作用域值重载选择，消除 21 个 `change_religion` 国家标签误报。
缓存版本 22 对应前一轮联合匹配修复，23 对应本轮结构条件与符号生产者。

仍在核对的规则问题：生成式 estate 属性、`fixed_dynasty` 等非布尔内置属性、
`all_owned_province_cumulative` 迭代器，以及空 `custom_tooltip`。这些没有通过放开未知名称解决。
旧兼容路径和 §1.3 的最终删除仍待语义验收收口。

## 已落地

- `game/build.rs` 强制执行检查、lower 和 bake，嵌入版本 12 的 IR；运行时解码 arena 并恢复索引。
  `rules/ir-manifest.json` 保存可复现的指纹和计数。非法源拒绝创建和覆盖产物的负例已通过。
- `tools index` 与 LSP 使用同一份 IR、规则指纹和文件目录；缓存安装保留 Callable 引用供调用图使用。
- 修复 whole-file schema 的 `open` 与入口作用域丢失、继承的 pattern 丢失、只读枚举误建引用、
  国家标签误转为配置元数据枚举、多个文件根 schema 的类型串线、技术组和自定义理念容器断链。
- 修复逻辑、作用域和透明包装的继承：内层块回到 effect/trigger schema，外层 `limit`、
  `factor`、`tooltip` 的必填约束仍保留。新增正反例，不删除原有断言，不批量更新 golden。
- 恢复 policy、static/triggered modifier、personality 等 18 个容器的 modifier 字段和模板继承；
  迁移器识别只有继承、没有自有规则行的类型上下文。非法数值和未知键仍会报错。
- 新增声明式 `forms`：变量运算和 estate loyalty 的合法结构分支分别检查，不再合并成同时必填，
  也不把全部字段改成可选。编译器拒绝无效分支，IR 指纹包含分支约束，负例拒绝覆盖 bake 产物。
- `def<T>` 的字段定义简写仅适用于键位置。标量定义保留值的声明，取消重复符号和命令名伪定义；
  `ref<T strip_prefix ...>` 在 HIR 中恢复定义前缀，匹配、引用记录和跳转使用同一个名字。
- 补全恢复枚举和种子的书写大小写、裸列表值、substring 匹配、按具体键计数的 cardinality，
  并读取声明的作用域限制和本地化候选。任务树标题从 IR 的 `Localised` 声明展开。
- 完整 HIR 使用独立的 32 项缓存，与便宜的文档查询共享失效时机但不共享容量；
  新增过量缓存释放、跨 revision 失效和拒绝旧 worker 回填的测试。
- 本地化发布诊断沿用空诊断契约，提前返回以避免无用的 HIR 构建；其索引和符号分析仍可用。
  HIR 直接移动降低后的集合，避免再复制一份 definitions/references/facts。
- 审计握手拒绝未索引或索引了其他 Vanilla 目录的结果。默认使用 IR manifest，旧二进制可显式
  选择旧 manifest。新增缓存完整计数与补全截断检查，避免把协议执行成功误当成语义验收成功。
- Callable 参数约束改为按调用绑定重放 IR：条件块按参数存在性选择，跨调用转发保留 affix、
  键位置、引号脚本和调用作用域。补全、参数 hover、诊断共享这些约束；重复标量参数使用最后值。
- `THIS` 保持当前作用域；ROOT、PREV 和链式寄存器块切换到选中的寄存器作用域。
  寄存器用必填 `role` 声明运行时槽位，任意改名后仍能切换和链接；switch 用 `selector_schema` 绑定选择表。
  修复引号 payload 的源码范围映射，恢复调用中的跳转、重命名及未打开文件中的引用。
  无效标量调用不再作为可导航的 Callable 引用。
- 新增声明式 `constant` control，只允许 scalar bool 谓词。只有显式声明的常量参与逻辑折叠，
  普通 bool 谓词不会被当作常量。规范、JSON Schema、迁移器和负例检查同步更新。
- 字段 hover 恢复当前作用域、不可用的重载、声明来源和贴图来源；文件路径补全保留目录前缀。
  补全继承字段排序、owner 参数和类型本地化预览分别有回归检查。
- 参数化 flag 名称按声明的替换模式参与符号匹配，保留 overlay 编辑、关闭和缓存遮蔽的行为；
  未知 Callable 的参数键保持保守处理。numeric trigger-value 使用声明的数值/布尔 trigger 集。
- 2026-10-02 简化：移除字段条件、结构推导 subtype 与 subtype trait 门控。
  church aspect、祝福、帝国改革、潜在贸易品等统一使用基础类型引用；本地化和图标统一展示，
  原条件必需 binding 改为可选展示。显式 event subtype 引用限制继续保留。
- 政府改革属性改为可选；`has_government_attribute` 接受声明的内置属性及 `custom_attributes` 定义。
  内置属性判断采用用户最新更正，撤销之前“只能引用 custom_attributes”的限制。
  恢复 ancestor personality、custom GUI、peace treaty、trade node、estate modifier 等入口的实际 body。
- 裸 payload 替换是外层块的片段，取消对片段重复要求外层 guard；新建的内层块仍检查必要字段。
- IDE 与索引共用 Callable 重放；引号参数内的定义、引用和链接进入缓存，overlay 签名变化会使其失效。
  扫描按符号事实稳定后提交，SQLite 语义生产者版本先升为 18；参数化符号成员判断与 IDE 对齐后升为 19；保留缓存往返和依赖链接测试。
- 修复 weighted entry 的作用域/Callable pattern、历史文件根的 scripted effect 调用、多 namespace、
  重复 advisor/customizable localisation 定义和省份 triggered modifier；补齐七个 modifier 容器的模板。
  GFX 文本尺寸/颜色及动画坐标、帧列表恢复结构验证，错误元素仍拒绝。
- 补齐时代 objectives 中的 trigger/allow、科技 monarch_power/ahead_of_time/tables，
  静态外交动作使用独立入口，保留动态动作的必填约束；图表和全局文字颜色接入原有结构。
  顾问编号允许非负整数，same_trade_node_as 支持省份；任意文本和非法数值仍拒绝。
- switch 参数分支键读取 selector_schema 中选定谓词的值域；条件块内的选择字段和参数转发均参与。
  引号 payload 必须在每个实际展开位置都有效；同一位置的重载仍作为 alternatives。
  旧测试曾允许“任一位置有效”，对应断言保留在旧路径，IR 路径另断言 trigger 中拒绝 effect。

本轮用户确认：`THIS` 沿用当前作用域，country/province 要求冲突不能跳过；
`PREFIX_$param$_END` 中的参数不可省略。所有 `[[条件]]` 外的替换都按必填处理，
普通 runtime `if/else` 不放宽文本替换要求。
条件转发由用户授权自行决定，按同一原则处理：外层未保护的 `$optional$` 仍必填；
把整个转发调用放入 `[[optional]]` 后可省略。提示预览块也不豁免文本替换。旧测试的对应差异逐项保留说明；未批量更新 golden。
其余确认包括：`any_*` 只用于 trigger/limit；裸 `ruler_age` 导出无效，应使用 `monarch_age`
或 `trigger_value:ruler_age`；作用域链接必须满足起始作用域，包括大写 `CAPITAL`。
church aspect 的 `is_blessing` 默认 `no`。原版 11 处 `exile_ruler_as` 均使用 `{ name = ... }` 块形式，
没有找到标量形式；对应旧测试按原版证据修正。

追加确认：`every_trade_node_member_country` 可从 province 出发；宗教、宗教组、culture 和
primary culture 的 `variable:` 比较有效。大写 `CAPITAL` 不豁免链接起点检查。

## 数据与基线

本机 EU4 1.37.5 的文本副本在忽略的 `data/vanilla`。共有 9,013 个文本文件；
扫描索引 8,681 个文件，诊断 8,670 个文件。两组缓存的 source fingerprint 均为
`d86784fcc1743a5e652247e9147f67296777584e6434d865634c889c6a6e6b1c`。
没有复制游戏可执行文件、动态库、图片、音频或其他二进制资源。

切换前对照来自 `e39e045` 的 release 二进制，使用版本 10 的旧规则，完整索引同一份文本副本。
其结果为 8,123 errors、590,247 definitions、1,808,527 references。
早先未正确加载缓存、索引为零的运行已排除，不能作为验收对照。

corpus 轮完整审计使用 IR 指纹
`0ad8f8fc527d0e399a25e7c97b72c518f23cadab276267300f0183f3ffd777ee`：
9,531 errors、587,002 definitions、331,089 references。
相较 reviewed 冻结版本 9,876 errors 减少了 345；相较旧版仍有 1,408 个净新增 error，不能当作已验收。
逐类型差异为 36 个 definition kinds、189 个 reference kinds。
完整结果及对照见本地 `performance-results/phase5/comparison-corpus.json`。
该文件由 `lab/perf/compare-rules.py` 生成；引用数从只读 SQLite 缓存汇总，包含 lazy references。
LSP 的 `workspaceSummary` 只显示提前加载的引用，不能代替这个完整计数。
新增/移除诊断按 path、code、range、message 的精确身份计算，措辞改变也会造成差异；
该轮精确身份新增 1,928、移除 520；只忽略消息措辞、仍比较 path/code/range 后为新增 1,470、移除 62。
两种口径都保留，忽略措辞不代表批准语义差异。
该轮冻结的 release runtime、tools、mem_probe 副本以 `*-corpus-acceptance` 命名，
全量报告在忽略的 `performance-results/phase5/ir-corpus-acceptance/`。
随后 payload index 冻结版本使用相同 IR、SQLite 18，完整索引引用为 330,762；尚未用它完成 LSP 全量审计。
本轮源码已追加上述规则和控制声明修复，随后 nested 冻结版本 `2f4392076437b84bdd82e86468b73ede03f45d7b0395b09e2fd99b2e6a9756ac`
完成了全量审计：8,532 errors、587,004 definitions、332,041 references，
相较旧版精确诊断身份新增 929、移除 520；按位置新增 471、移除 62。
对照见 `comparison-nested.json`，31 类 definition 差异的逐类说明在本地 `definition-review-nested.json`。
本轮源码继续补齐时代目标、科技、静态外交动作、图表入口、数值顾问编号、运行时 identity，
并修复 switch 参数键域和引号 payload 的多处使用检查。415 项 IR 回归通过，含 65 项空旧模型专项。
refined 冻结版本 `cb4cccea748bd7f4ee9dcb02b6c63286aaf99843944206f70a45c0111bd9d464`
使用 SQLite 19，完整审计已完成：8,371 errors、587,054 definitions、332,069 references。
精确身份新增 768、移除 520；按位置新增 313、移除 65，分别为 8 个 UnknownKey、130 个 Cardinality、
53 个 InvalidValue 和 122 个 WrongScope。31 类 definition、191 类 reference 仍需逐项解释。
refined 轮 11 个完整补全集合的前 512 项均与 LSP 一致，仍为 7 组相同、4 组差异。
本轮继续修复 subject type 前置空声明、重复历史效果、GFX 包装和颜色、旗帜贴图列表、
旧政体映射键及叛军名字参数；恢复规范类型本地化和数值 professionalism modifier。
索引和导航恢复提示预览中的引号符号、scope 联合值域中的真实引用及 enum 回退中已安装的 sprite，
SQLite 语义生产者版本先升为 20；未知作用域的标量重载保留至检查寄存器值。
完整语料随后暴露同一 payload 在多个作用域重放时属性摘要重复；HIR 合并摘要后升为 21，
读取端继续拒绝重复数据。新增缓存往返回归覆盖 effect 与提示预览同时使用同一 payload。
roundtrip 冻结版本使用指纹 `a5a61f2c99410c9ad6f8ba0699ff0681a0e3059aad66578f9afcb5dfb1765fdf`、SQLite 21，
424 项 IR 对照、1,027 项常规门禁及 Clippy 全通过；完整缓存成功加载并诊断 8,670 个文件。
其结果为 8,845 errors、587,104 definitions、427,593 references，精确身份新增 1,242、移除 520；
按位置新增 787、移除 65（InvalidValue 602、UnknownKey 3、Cardinality 93、WrongScope 89）。
这轮发现联合匹配的实际回归：自由文本王朝被误记为国家标签，未知当前作用域下的重载也产生了
另一值域的未解析引用；basic/new 同时命中的改革及跨类型同名符号则丢失了引用。
这些已新增正反例并修复，SQLite 生产者版本升为 22；当时 426 项 IR 对照通过；后续冻结审计见本文顶部。
`has_government_attribute` 撤销自由文本回退，未知名称按源声明的 warning 级别诊断；
原来的 open-world 断言保留在旧路径，IR 路径验证声明后告警消失。
roundtrip 的新增条件贴图诊断中，77 处在原安装 GFX（含 DLC ZIP）中找到声明，5 处未找到。
证据在 `sprite-added-proof-roundtrip.json`，未复制二进制资源，也未放开未知贴图。
上述审计数字只代表各自冻结版本，不能混用。

已确认的部分差异：

| 差异 | 判断依据 | 状态 |
| --- | --- | --- |
| `culture` 定义 577 → 426 | 旧计数包含 151 个 culture-group 容器名 | 已解释该类计数变化 |
| `unit_type` 定义 3,228 → 329 | 旧兜底把单位文件的配置字段也计为单位名；IR 使用文件定义位置 | 已解释该类计数变化 |
| 旧 `eu4-path-*` 等兜底引用消失 | IR 按 schema 的 ref 位置收集，不再对所有词套全局表 | 预期方向；全部引用种类仍待逐项核对 |
| `scripted_effect` / `scripted_trigger` 引用数大幅下降 | 旧缓存有大量名字不属于相应定义集的兜底引用 | 不能单凭总数下降认定无引用丢失 |
| flag 定义误增 | 曾把 `set_ruler_flag` 等命令名也记作定义，且值重复记录；已修复该 HIR 错误 | `ruler_flag` 为 238；event target 恢复为 1,751，全局 flag 347、全局 event target 39 均与旧版一致；country flag 在 roundtrip 轮为 3,745，旧写入位置无丢失；新增 30 个为引号 payload 的实际写入，19 个提示预览位置已恢复 |
| 7,553 个 gfx-path errors | 只读核对原安装及 76 个 ZIP 的中央目录：6,843 个独立文件存在、6 个存在于 DLC ZIP、704 个未找到 | 已解释前 6,849 个为文本语料未包含的资源；其余来源仍待核对 |

上述 gfx 核对没有复制二进制，也没有为测试制造占位贴图。

## 最新新增 error 的分组

按位置的 286 个新增 error 分为：

| 分组 | 数量 | 当前证据 |
| --- | --- | --- |
| 条件贴图 | 82 | 77 处在原安装 GFX（含 DLC ZIP）有声明；5 处未找到，完整安装依赖和拼写仍需核对 |
| 其他值/参数 | 16 | 包括属性模板展开、flag namespace、province 0 及作用域参数；未全部批准 |
| 空 custom_tooltip | 3 | 用户已确认空行有效，规则已修复；冻结全量报告仍为修复前版本 |
| 必填参数 | 57 | 按已确认的文本替换规则检查；未被激活条件保护的替换不可省略 |
| 字段重复 | 35 | 保留声明的 cardinality，未通过统一放开重复消除；具体游戏行为继续核对 |
| 缺少 investment | 1 | 原版调用块缺少声明的必填字段 |
| capital 链接起点 | 67 | 大小写 CAPITAL/capital/capital_scope 均按链接起点检查，用户已确认不豁免 |
| 其他作用域 | 22 | 按声明及实际 scope state 检查，包含原版错位括号；仍需逐项留证 |
| UnknownKey | 3 | 两处来自原版漏写等号；用户已裁决 all_owned_province_cumulative 继续报未知 |

精确消息身份的 741 个新增还包含已存在诊断的措辞变化，不等同于新增错误位置。
上述分组是审查目录，未批准的部分继续阻止阶段五退出。

## 当前补全模块验收（源版本 13，2026-10-02）

按用户指示转入补全模块后，修复 `spellings_with_state` 对联合值域遗漏作用域过滤的问题。
例如 `scope<province> | ref<province_id>` 原先只收集候选，没有逐分支核对链接起点和寄存器目标。
现在每个分支先按当前 scope state 过滤，再合并并去重；引用和字面量分支保留各自候选。
空旧模型回归覆盖国家事件、省份事件及已推断国家入口的 Callable，修复前会提示错误的
`home_province`，修复后不再提示 `home_province`、`location`、`sea_zone` 或已知目标不匹配的 `THIS`。

当前冻结服务器与工具为 `ir-completion-v13` / `ir-tools-completion-v13`，二进制 SHA-256 已记录。
源指纹保持 `0f0a2046bff103edb0aee05ab17ccc211ad800f678bde1241d8f673020954294`，SQLite 23。
独立缓存 8,681 文件，语料指纹与此前一致。`baseline.mjs --completions-only` 只加载符号上下文并
运行固定探针及前缀采样，不执行 Vanilla 全量诊断；输出的 kind/coverage 明确区分此用途，
不生成全量诊断报告。其 workspace summary 的引用仍为延迟加载结果，不用作全量引用计数。

11 个固定位置的全部 144,533 个候选均已导出，前 512 项全部与本版 LSP 一致。
导出同时记录每项的 kind/detail、插入文本和替换范围，并验证 golden 输入 SHA-256。
相对旧模型 7 组集合相同，4 组差异均逐项留证，共 1,974 个新增或移除身份：

| Probe | 新增 / 移除 | 当前解释 |
| --- | --- | --- |
| 作用域目标 | 14 / 68 | 新增 FROM/PREV 的声明拼写和链式形式；移除被旧模型当作标量作用域链接的块语句、迭代器、复合链与包装。这里入口为 country，`add_core` 的该重载接受 province 目标及 province ID；此前把目标域写成 country 的分组说明已更正 |
| 作用域链关键字 | 1 / 2 | 声明拼写 ROOT；移除旧 root 拼写及未声明的 start_from_root |
| limit 内 trigger | 920 / 22 | 新增 200 个内置 tag、15 个寄存器拼写、679 个 event target、26 个 global event target；移除宗教配置/分组伪定义及旧作用域辅助名 |
| if 内 effect | 920 / 27 | 同一批声明及索引目标；移除 16 个单位元数据伪定义、旧作用域辅助名和三个不能在 country 入口执行的 Callable |

新增作用域块候选的插入形式逐项核对；作用域值保持标量插入。三个被过滤的 Callable 另经真实
LSP 核对，在 country 不出现、在 province 出现，并保存插入 payload 与定义来源。
逐项解释为 `completion-review-v13.json`；完整输出为 `completion-inventory-ir-v13/inventory.json`，
补充作用域证据为 `completion-probes-v13/callable-scope-evidence.json`，均保留在忽略目录中。
生产 IR IDE 433 passed、0 failed；JS 审计测试 4 passed；IDE Clippy、fmt 与 diff whitespace 通过。

固定 11 个位置的补全退出项通过；这不证明所有脚本位置均已枚举，也不替代暂停的 Vanilla error
审查或全量引用位置审查。本轮没有重新测量性能。

## 当前性能优化（源版本 13，2026-10-02）

按用户指定，固定补全验收后进入性能模块。本轮规则源与 IR 指纹不变，仍为
`0f0a2046bff103edb0aee05ab17ccc211ad800f678bde1241d8f673020954294`，SQLite 23。
冻结优化前和各轮 `mem_probe`，保存二进制 SHA-256、manifest 与生产源码 SHA-256；
CPU 栈及完整测量均保留在忽略的 `performance-results/phase5/` 中。

以下对照使用阶段五开始时 `96f50c8` 的旧模型探针作为基线；远端 `origin/main` 另行冻结对照。
最终版本完成三组安静顺序配对，六个完整探针都正常退出、必要阶段全部采到、采样错误为零。
三次 IR 都扫描 8,680 个文件，诊断 11,649 条，digest 为 `0xd8432448a0e077f6`，
与同规则优化前的冻结探针一致。旧模型每次为 10,463 条、digest `0xb7b5124ba391f79d`；
旧模型与 IR 的规则语义不同，诊断数量不要求相等。Project 探针不替代前文 8,670 文件的 Vanilla error 审查。

| 指标（三次独立进程中位数） | 阶段五开始时旧模型基线 | 当前 IR |
| --- | ---: | ---: |
| 全量诊断 | 45.8 s | 23.0 s |
| 完整探针耗时 | 86.3 s | 74.2 s |
| OS 峰值 RSS | 1,065.56 MiB | 999.73 MiB |
| 释放宿主后 RSS | 955.89 MiB | 697.28 MiB |
| 仅规则加载 | 194.4 ms | 22.8 ms |
| 仅规则加载峰值 RSS | 40.77 MiB | 23.23 MiB |

峰值中位数下降 6.18%，诊断耗时下降 49.78%；相对该基线的内存与规则加载退出项通过，此前诊断延迟劣化已修复。
首次冷扫描 wall 中位数仍为 11.6 → 21.0 秒，两边都含两次 4 秒阶段驻留；
IR 扫描需要重放跨文件符号事实，冷扫描仍是后续可优化的热点。完整探针的阶段驻留为旧模型 24 秒、
IR 32 秒，表中完整耗时包含这些驻留时间。
测量期间没有并发编译、测试或全量审计；macOS 压缩与分配器仍影响 RSS，三次峰值范围为
旧模型 987.55–1,071.11 MiB、IR 876.47–1,005.89 MiB，不将单组值当作稳定结论。

优化前 CPU 采样运行诊断为 191.0 秒，仅作为热点定位证据，未把采样耗时当作安静计时基线。
最终冻结探针为 `ir-mem-performance-index-lifetime-v13`，SHA-256 为
`243b583bb4390536c50da8c670b30af7bc93e4bc65a0a7a623952ce5b672dc47`。
完整对照为 `memory-performance-index-lifetime-v13-pairs/comparison.json`，
加载对照为 `rules-loading-index-lifetime-v13.json`，校验为 `performance-index-lifetime-v13-verification.json`。
校验检查六次正常退出、二进制/生产源码 SHA-256、manifest、必要阶段和同规则诊断摘要。

热点优化保留原有诊断契约：

- 本地化建议按 Unicode 字符长度筛选候选，复用查询字符和编辑距离缓冲区，只计算距离带内的格子；
  ASCII 大小写、最短长度、重复项和等距歧义规则不变。
- 数量约束的重载组按规则缓存，跳过完全无上下限要求的组；保留同组所有重载，并按当前位置的作用域确定约束。
- Callable 参数读取按源码范围缩小属性遍历；相同参数路径合并成一次扫描，保留原来的路径匹配与覆盖规则。
- schema 范围索引替代每个属性、标量的全表查询，保留最短包含范围、源顺序取舍和空范围边界。
- 循环检测读取调用参数只加载语法树；没有参数引用的文件省去参数所属定义的查找。
- 引用校验通过二分查找定位属性，保留精确 key range 和源顺序取舍；作用域模板直接比较原字符串，
  省去重复的小写分配与通用字符串搜索，保留 ASCII 大小写和贪婪匹配契约。
- 磁盘文件的全量诊断不再向交互缓存填入临时 HIR；仍复用已有缓存，交互查询继续保留 HIR。
- 符号稳定性检查结束后，先释放候选索引，再构建最终服务索引，减少两个完整索引同时存活的峰值。

最终全工作区 1,031 项测试、生产 IR IDE 438 项测试、HIR 45 项测试、fmt 与全目标全特性 Clippy 通过。
新增回归将带状编辑距离与完整 Unicode 编辑距离比较，并验证范围索引与线性查找在重叠、
同长度、空范围、schema 过滤及边界上的同一对象选择；另覆盖嵌套属性、作用域重载的数量限制，
以及临时 HIR 的释放、交互缓存复用和两条输入路径的完整诊断一致性。
`memory-pairs.py` 记录完整诊断耗时与摘要；`--require-identical-diagnostics` 可用于同规则优化的严格对照。

### 与 origin/main 的首次直接性能对照（2026-10-02，专项优化前）

远端只读查询确认 `origin/main` 为 `e771357ec848171aed60ae88750c446e140f9303`。
导出该提交的源码独立 release 编译；457 个归档普通文件逐字节核对，
仅临时 `mem_probe` 增加规则加载计时、仅加载开关和两个采样节点，生产源码保持该提交内容。
当前列为 `feat/rules-v2` 本地未提交的 IR 性能冻结版，规则指纹仍为 `0f0a2046…`。

三组顺序配对的六个完整进程正常退出；相同语料、相同八个采样节点、每次均驻留 32 秒，
必要阶段全采到、采样错误为零。没有并发编译、测试或全量审计。

| 指标（三次独立进程中位数） | origin/main | 当前 IR |
| --- | ---: | ---: |
| 全量诊断 | 46.1 s | 22.9 s |
| 完整探针 wall（两边均含 32 秒驻留） | 95.5 s | 72.9 s |
| 完整探针扣除采样驻留 | 63.5 s | 40.9 s |
| 首次加载并扫描，扣除两次 4 秒驻留 | 3.6 s | 13.0 s |
| OS 峰值 RSS | 756.75 MiB | 869.13 MiB |
| 仅规则加载 | 52.4 ms | 22.9 ms |
| 仅规则加载峰值 RSS | 88.72 MiB | 23.17 MiB |

相对 `origin/main`，诊断耗时下降 50.33%，规则加载改善，但峰值 RSS 中位数上升 14.85%，
内存退出项未通过；首次扫描也仍有劣化。此前相对阶段四基线的通过结论只覆盖那套对照。
RSS 受本机压缩和分配器影响，三次峰值范围为 main 642.89–809.52 MiB、IR 798.94–874.20 MiB，
原始数据完整保留，可检查波动范围。
两边都诊断 8,680 个文件，main 每次 10,463 条、digest `0xb7b5124ba391f79d`，
IR 每次 11,649 条、digest `0xd8432448a0e077f6`；规则语义不同，跨版本不要求摘要相等。
当前 IR 摘要与同规则优化前的冻结探针一致。

完整证据为 `origin-main-e771357-provenance.json`、
`memory-origin-main-e771357-v13-pairs/comparison.json`、
`rules-loading-origin-main-e771357-v13.json` 和 `origin-main-e771357-v13-verification.json`，
均保留在本地忽略目录。校验状态为 `measured-with-regression`。

### 峰值 RSS 与首次扫描专项优化（2026-10-02）

扫描采样中，主线程约 89% 的样本落在跨文件符号事实重放。此次修改：

- HIR 记录是否读取过 workspace 符号事实，包括缺失符号和首轮尚未取得的 Callable 模板。
  无此依赖的文件复用原 shard；依赖文件继续按完整符号事实迭代，保留 32 轮稳定性检查。
- 闭合磁盘文件使用索引专用 lowering，省去立即丢弃的 schema、field、scope 验证表。
  定义、引用、属性、flag 写入、Callable 模板与参数信息仍完整提取；交互和诊断使用完整 HIR。
- 重放只更新变动 shard 的索引桶；引用变化复用已解析的定义桶，flag 写入变化重建其成员视图。
  稳定的候选索引直接用于服务，取消最终的完整索引重建。
- 大于 256 KiB 的依赖文件在调用线程串行降低，其余文件最多使用 4 个 worker，
  并遵守扫描的 `max_workers`。每轮读取同一个不可变候选事实，完成后再提交新 shard。

最终全工作区 1,033 项（35 个 suite）、生产 IR IDE 438 项、HIR 46 项测试通过，
fmt 与全目标全特性 Clippy 通过。新增回归覆盖缺失符号依赖、引用更新与优先级遮蔽、
flag 模式添加和删除；含大文件的 Callable payload 链路同时比较串行、并行和每轮全量重算，
最终 shard 完全一致。规则指纹仍为 `0f0a2046…`，源格式 13，SQLite schema 23。

使用三个冻结版本轮换顺序，各运行三次完整独立进程，共九次；测量期间没有并发编译、
测试或全量审计。语料为 `data/vanilla` 的 Project 根，无已安装 Vanilla 缓存。
每次八个采样节点均成功，采样错误为零、退出码为零。首次加载并扫描扣除两次 4 秒驻留；
完整探针扣除八次共 32 秒驻留。峰值取被测子进程的 `wait4` rusage。

| 指标（三次进程中位数） | origin/main `e771357` | 优化前 IR | 优化后 IR |
| --- | ---: | ---: | ---: |
| OS 峰值 RSS | 977.06 MiB | 1,028.39 MiB | 935.45 MiB |
| 首次加载并扫描，扣采样驻留 | 3.5 s | 12.9 s | 5.2 s |
| 全量诊断 | 45.9 s | 23.1 s | 22.8 s |
| 完整探针扣采样驻留 | 62.61 s | 40.73 s | 33.23 s |

同规则优化使峰值 RSS 中位数下降 9.04%，首次加载并扫描下降 59.69%。相对同期 main，
峰值 RSS 低 4.26%，诊断耗时低 50.33%；首次加载并扫描仍多 1.7 秒（48.57%）。
RSS 受 macOS 压缩与分配器影响：main 三次为 976.72–1,049.05 MiB，
优化前 IR 为 968.95–1,059.61 MiB，优化后 IR 为 698.28–994.92 MiB。
结论来自本轮中位数，不把不同轮次的绝对 RSS 直接相减。

优化前后六次 IR 均诊断 8,680 个文件、11,649 条，digest 均为 `0xd8432448a0e077f6`；
定义 587,104 条、引用 431,930 条，规则指纹一致。main 三次为 10,463 条、
digest `0xb7b5124ba391f79d`；跨规则版本不要求相同摘要。
最终二进制 SHA-256 为 `9394535387a83af01340acb27b9d207c098304044f9bad35100965798599c925`，
优化前 IR 为 `243b583b…`，main 为 `d6fc6164…`。二进制、生产源码及 manifest 的 SHA 校验通过。

另以最终冻结二进制重测三组仅规则加载进程：main 51.7 ms / 88.20 MiB，
IR 23.1 ms / 23.02 MiB。内存与规则加载退出项在这轮 main 中位数对照中通过，
首次扫描仍有上述差距；本轮不替代剩余语义审查或旧兼容路径清理。
完整数据、校验报告及复测脚本保留在本地忽略目录
`performance-results/phase5/scan-rss-final-three-way/`，
规则加载另见 `rules-loading-origin-main-scan-rss-final-v13.json`。

## 此前 bindings 冻结版本的补全差异归类

| Probe | 新增 / 移除 | 依据 |
| --- | --- | --- |
| 作用域目标 | 18 / 68 | 历史输出包含当前已修复的四个作用域候选；当时尚未逐项留证。当前来源和目标域的正确解释见上节 |
| 作用域链关键字 | 1 / 2 | 使用声明的 ROOT 拼写；start_from_root 没有寄存器声明 |
| limit 内 trigger | 920 / 22 | 200 个内置 tag、15 个寄存器、679 个 event target、26 个 global event target；移除宗教配置/分组和单位元数据伪定义及旧寄存器拼写 |
| if 内 effect | 920 / 27 | 同一批作用域目标；移除 16 个单位元数据键、旧寄存器拼写，以及三个仅允许 province 的 Callable |

逐组说明为本地 `completion-review-bindings.json`；完整候选集为
`completion-inventory-ir-bindings/inventory.json`。范围仍限定于这 11 个固定位置。

## 回归与门禁

- refined 冻结版本 `core-fast` 的 workspace/all-features 单元、集成和 doc 测试：1,018 tests passed，0 failed。
- `cargo clippy --workspace --all-targets --all-features --offline -- -D warnings`：通过。
- Rustdoc (`-D warnings`)、fmt、diff whitespace、artifact gate：通过；artifact 7 项检查全通过。
  `core-fast artifact policy` 共 8 个 gate actions 通过，policy 88 项通过。
- VS Code `npm run test:ci`：编译、真实 LSP smoke、startup、MCP、assets、localisation format、i18n、
  审计契约、extension/package 契约和 VSIX 包验证通过。
- refined 冻结版本的空旧模型 IR 专项回归：65 passed、0 failed，包括确认规则、引号导航、寄存器块和常量谓词。
- refined 冻结版本的 `PDC_TEST_FIRST_PARTY_IR=1` 对照：415 passed、0 failed。包含补全、Callable、诊断、导航、
  hover、作用域；依赖旧内部查询的断言改为验证等价 IR facts 与最终功能。
- 固定 11 个 completion probes 已通过临时 IDE harness 导出 512 上限之前的完整集合，
  每个集合的前 512 项与对应冻结 LSP 报告一致。7 个集合完全相同，4 个存在差异。
  这项导出补齐了截断证据，但不自动批准差异；已发现并修复值补全选错重载以及 `TEA`/`tea`
  不同结构被折叠的问题，nested 轮完整集合已重跑，仍为 7 组相同、4 组差异；bindings 轮全集合已核对，与 roundtrip 轮相同。该 harness 覆盖固定位置，不代表全部脚本位置。
- 每个固定 probe 另配相同的 17 个前缀，187 组全部成功配对：51 组相同、52 组未截断但有差异、
  84 组仍截断。前缀样本显示寄存器大小写、链式拼写和作用域筛选等差异；这些样本不是完整枚举，
  不能据此批准基础集合。UTF-16 位置、整词替换及请求失败后恢复夹具有审计契约回归。
- `rules-migrate` 两次输出目录中的 91 个文件逐字节相同；reviewed 轮与当时人工精修源有 37 个文件差异；角色、switch 声明更新后的两遍输出再次逐字节相同；nested 轮与精修源有 40 个文件差异；roundtrip 轮两次 91 文件仍逐字节相同，与精修源有 41 个文件差异。
  未用生成结果覆盖精修源，当前 artifact 可复现性由重新 check/lower/bake 与 manifest 比较验证。
  转换器回归同时验证 `always` 带 constant、普通 `is_capital` 不带 constant。
- reviewed 轮 `rulec check rules/eu4`：88 files、0 errors、264 warnings（未使用的声明）。

早先的失败和 golden 差异报告保留于本地；它们是历史定位资料，不能代表当前冻结版本的回归状态。
两份独立的 IR golden 记录 `THIS` 冲突、trigger/effect 可用上下文、实际类型说明及最近所属容器范围；
旧模型 golden 仍保留。共享 modifier golden 仅统一了既有消息中一个形容词。

全目标测试附带的百万条 debug 性能夹具因资源压力中止；以上完整单元/集成/doc 测试已独立重跑，
没有宣称该性能夹具通过，也不使用其中的跑分进行性能验收。

## 退出标准

本节是当前收官清单，依据 `rules-redesign.md` §6 的六项退出标准及旧模型删除要求。
验收范围固定为本分支的规则迁移、现有 Vanilla 语料和既定补全位置；不以诊断归零、
新旧引用数量相等或所有游戏能力均已实现为完成条件。新发现仅在违反这些既定要求时
进入本次修复清单；D20 提案、扩展 tutorial schema 和新增性能指标不自动成为退出条件。

| 标准 | 当前结论 |
| --- | --- |
| golden 全通过，更新逐项归类 | 通过。最终工作区 976 项通过、零失败，包含 IDE 432 项及全部保留的 golden；旧夹具迁移/退役按理由记录，7 份本轮 golden 更新逐项归类 |
| Vanilla 每个 error 有原因和证据；定义/引用按实际语义验证 | 通过本次范围。最终 8,324 errors 的身份与上一轮逐条一致：7,948 条冻结资源缺失、214 条原版问题、73 条规范性诊断，89 条按用户决定排除。最终新缓存的完整定义/引用多重集合与已审版一致，全部差异已归类，待审项为零 |
| 固定位置补全集合相等或已解释 | 通过。最终既定 11 个完整集合中 7 组相等、4 组差异；2,637 个差异身份全部有解释。保留此前 1,974 个身份的证据，并补充 662 个声明式静态作用域链及 R09 删除 trade_node 候选的证据。每组前 512 项与 LSP 结果一致 |
| `mem_probe` 内存、规则加载不劣于切换前 | 通过。最终相对 `96f50c8` 的三组安静配对，峰值 RSS 中位数 1,146.75 → 1,071.09 MiB，规则加载 191.5 → 22.8 ms；阶段采样完整、无采样错误。首次加载扫描仍多 1.7 秒，保留为已知风险 |
| §1.3 硬编码全部删除 | 通过。旧控制流、ScopeContext 和 matcher 消费者已删除；当前消费者读取 IR 的控制流、寄存器和 selector 声明，旧模型标识及旧树的 catalog/semantic/types 结构无生产残留 |
| 检查失败拒绝 bake | 通过。mandatory check、负例和 artifact gate 已验证 |

另有明确的交付要求：删除旧 `model`、旧规则编译模块、`SemanticRule`、全部旧规则文件及
兼容消费者；本轮已完成并保留逐文件删除证据。仅目录/扫描用途的 IR 派生桥接不再重建旧语义。

收口时，引用审查以完整差异清单为边界，按类型、源结构和绑定关系归类，保留位置证据及
必要的正反例。每条差异必须归入有证据的类别，真正的绑定缺口修复后复核；未归类位置
和已确认的实现缺陷均归零后，该项结束。数量相等、抽样正常或“旧版没有目标”单独都不能
替代这个条件，已经解释且未受后续修改影响的类别也不重复从头审查。

用户于 2026-10-03 明确要求“资源项排除吧，接下来进行旧实现清理和最终验收”。
据此排除此前待核验的 89 条资源诊断，逐条身份和授权记录保存于
`performance-results/phase5/legacy-cleanup/resource-exclusion.json`。排除只影响本次验收范围，
不放宽规则、不抑制用户诊断，也不将这些资源判断记为正确。其余既有退出条件保持不变。

最终冻结实现已完成身份一致的全量诊断、引用清单、既定完整补全、原性能指标和
工程门禁复核，上表及删除要求均满足，本次验收结束。最后移除规则搜索的冗余 `return` 后，
已重建二进制和缓存，复核完整符号多重集合、全量诊断及 LSP 探针完全一致，规则搜索另单独通过。
不因文档整理或未影响某验收面的修改而重启已经完成的全部审查。

仅规则加载的三个独立进程比较使用阶段五开始前的 `96f50c8` 与对应冻结版本的 bake 路径。
这与 Vanilla 语义对照的 `e39e045` 是不同参照；不能混用。完整测量保留于
`performance-results/phase5/rules-loading-latest.json`。这份早期记录加载中位数 201.8 → 19.1 ms，
峰值 RSS 中位数 40.77 → 22.36 MiB；其他本地工作并发，不能把绝对耗时视为稳定跑分。
并发编译和 debug 性能夹具期间的扫描时间受资源竞争影响，不能用于性能退出判断。

## 内存定位

首次完整 `mem_probe` 对照的峰值为旧版 804.86 MiB、新 IR 1,302.52 MiB。
新 IR 的 shard symbol vector 已比旧版小（约 49 对 105 MiB），峰值增加不能归因于引用数量。
该探针的诊断阶段会遍历全部文件；IR HIR 曾进入 32,768 项文档缓存，保留了大量前端。
已增加独立的 32 项 `Frontends` 缓存，与 Documents 共用 revision 失效、容量独立。
限额复测为 1,041.34 MiB；同一源下诊断 digest `0xd1478a137e659ade` 与限额前相同。
之后加入本地化提前返回和集合移动，前一版的独立配对峰值曾为 719.58 → 550.84 MiB，
但释放宿主后 IR 仍约 402 MiB RSS，不能据此宣布内存全部通过。

恢复 modifier 继承后、Callable 修复前的完整配对为旧版 648.89 MiB、IR 720.22 MiB，
记录在 `memory-legacy-modifiers-full.{json,log}` 与 `memory-ir-modifiers-full.{json,log}`。
IR 的 Project 探针为 8,680 files / 85,650 diagnostics，digest `0xb4868ec319b8621f`。
该 IR 二进制在最后一次 HIR 前缀引用修复前构建；这组结果已由本轮完整配对补充。
这些均为单次独立进程测量，存在并发工作与 macOS 内存压缩影响，不是稳定的三遍跑分。
源规则变化会改变诊断工作量；Project 探针的诊断总数也不能替代 Vanilla 审计的 error 数。
剩余 RSS 可能包含分配器保留、共享句柄或线程局部视图，尚未完成归因。

本轮冻结的 `ir-mem-callable-acceptance` 完整扫描同一份文本副本，与旧版分别独立运行。
使用 `getrusage(RUSAGE_CHILDREN)` 读取单个被测子进程的 OS 峰值，旧版 929.69 MiB、
IR 829.17 MiB（下降约 10.8%），均正常退出。该口径能捕获轮询未采到的短时峰值，
不能将它与前文按秒轮询的峰值混合比较；也不能由峰值推断释放后 RSS。
记录为 `memory-{legacy,ir}-callable-full.{json,log}`，含二进制 SHA-256。
IR Project 探针为 8,680 files / 75,348 diagnostics，digest `0x0e54798027a267a1`。
这是一次完整配对，存在并发验证；尚未以三次完整配对证明内存不劣化。

随后对 `ir-mem-subtypes-acceptance` 完成三次顺序独立进程配对，使用 `wait4` 读取每个
被测子进程自身的 OS 峰值，以 `ps` 只采样该进程 PID 的阶段 RSS。六个进程均正常退出，
必要阶段均采到，采样错误为零。峰值中位数旧版 1,172.20 MiB、IR 1,183.36 MiB，
增加约 0.95%；释放宿主后的 RSS 中位数 977.77 → 786.88 MiB，下降约 19.5%。
峰值尚未满足“不劣于”要求，不能把此前单次峰值改善作为最终结论。
完整结果在 `memory-subtypes-pairs/comparison.json`；二进制与单次记录含 SHA-256。
IR 的诊断 digest 三次相同，为 `0x3d58cd1a3573ed58`，Project diagnostics 为 13,652。
首组部分时间与前一轮验证并发，其余组运行时没有并发编译或全量审计；仍受 OS 压缩和分配器影响。
这组测量对应上述冻结指纹，不覆盖其后的源声明和补全修复。

refined 冻结版本另完成三次完整顺序配对，六个进程均正常退出、全部必要阶段均采到、采样错误为零。
OS 峰值中位数旧版 1,114.06 MiB、IR 1,023.50 MiB，下降 8.13%；释放宿主后 RSS 中位数
901.61 → 559.56 MiB。IR 三次 Project diagnostics 均为 11,637，digest 均为
`0x33ac41af75455bfb`。没有并发编译或全量审计，仍受 macOS 压缩和分配器影响。
旧版诊断阶段约 46 秒、新版约 169 秒；这项延迟没有随 RSS 改善消失。
报告在 `memory-refined-pairs/comparison.json`，仅覆盖 `cb4cccea…` 冻结产物。

bindings 冻结版本完成最新三组安静顺序配对，六个进程均正常退出、全部必要阶段均采到、
采样错误为零，二进制 SHA-256 与冻结记录一致。OS 峰值中位数旧版 1,013.83 MiB、
IR 931.61 MiB（下降 8.11%），释放宿主后 RSS 中位数 902.27 → 510.55 MiB。
三次 IR Project diagnostics 都为 11,657，digest 均为 `0x9de50380217a6626`；
旧版都为 10,463，digest 均为 `0xb7b5124ba391f79d`。这里的 Project 探针不替代 Vanilla error 数。
六次运行期间没有并发编译、全量审计或补全导出；只做轻量文档整理。
报告为 `memory-bindings-pairs/comparison.json`，校验记录为 `memory-bindings-verified.json`。
诊断阶段中位数 45.8 → 194.5 秒，完整探针总耗时约 86 → 277 秒，延迟劣化明确保留。
仅规则加载的安静三组配对另记录为 `rules-loading-bindings.json`，不用早期并发测量替代。

## 交付顺序

引用差异归类、旧实现清理和最终冻结复核均已完成；89 条资源项按用户决定排除。
本次本地验收已结束，后续进入提交和 PR review，合并前通过 PR CI。
相关代码、规则、回归夹具、文档及可复用的审计工具随 PR 提交；原始报告、游戏语料、
缓存和冻结二进制保持本地。独立的后续重构提案不追加为本次验收要求。

## 复现

先保存要对比的两组 release 二进制，分别用对应的 `tools index` 生成独立缓存。
旧组必须使用该旧版本保存的 manifest，新组使用同身份的 IR manifest。下例使用本地保留的
最终冻结产物；这些文件不随 PR 分发，其他机器需要自行构建并提供获授权的本地语料。

```sh
node --max-old-space-size=6144 editors/vscode/scripts/baseline.mjs \
  --server performance-results/phase5/legacy-cleanup/final/paradoxcode \
  --rules-manifest performance-results/phase5/legacy-cleanup/final/ir-manifest.json \
  --vanilla-source data/vanilla \
  --vanilla-cache performance-results/phase5/legacy-cleanup/final.pdcindex \
  --output performance-results/phase5/legacy-cleanup/final-recheck --label phase5-final-recheck

python3 lab/perf/compare-rules.py \
  --before performance-results/phase5/legacy-prefix-samples \
  --before-cache performance-results/phase5/legacy.pdcindex \
  --after performance-results/phase5/legacy-cleanup/final-recheck \
  --after-cache performance-results/phase5/legacy-cleanup/final.pdcindex \
  --output performance-results/phase5/comparison-latest.json

CARGO_INCREMENTAL=0 cargo test --locked --workspace --all-features --no-fail-fast
```

旧夹具对照开关 `PDC_TEST_FIRST_PARTY_IR` 已随旧实现退役；当前回归直接使用显式 IR 和生产 IR。
所有 Vanilla 语料、缓存、报告和对照二进制留在忽略目录，不提交、不上传。
