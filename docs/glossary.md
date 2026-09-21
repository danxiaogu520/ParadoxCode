# 术语词典（中英对照）

ParadoxCode 的统一术语表。用途有三：扩展 UI 双语文案（`editors/vscode/l10n/` 与 `package.nls.zh-cn.json`）保持译名一致；文档与 Release 说明用词一致；维护者与协作 agent 沟通时口径一致。改译名时同步改本表，并跑 `npm run check`（内含 i18n 契约检查）。

## 保留英文（不翻译）

| 英文 | 说明 |
| --- | --- |
| ParadoxCode | 产品名 |
| pdc | 服务器二进制名 |
| EU4 | Europa Universalis IV 的通用缩写 |
| EU4dll | 汉化转码库，透明转码的底层机制名 |
| paratranz / latin1eu4 | 转码档位名（`paradoxcode.localisation.transparentScriptGlobs` 语境） |
| Problems 面板 / 视图 | VS Code 界面元素名，官方中文界面同样显示英文 |
| owner/name | 发布仓库的形式描述 |

## 统一译法

| 英文 | 中文 | 语境 |
| --- | --- | --- |
| Vanilla | 原版 | 游戏原版数据与符号（Vanilla index → 原版索引；Vanilla symbols → 原版符号）。UI 不再保留 "Vanilla" 英文写法 |
| Mod / mod | 模组 | 一律译"模组"，不保留 Mod |
| sprite | 图像 | `.gfx` 引用名级别的资源（sprite name → 图像名）。与 texture 区分 |
| texture | 贴图 | `texturefile` 指向的位图文件（贴图图片、任务树纹理） |
| icon | 图标 | 任务 `icon` 字段语义（任务图标） |
| mission | 任务 | mission tree → 任务树；mission preview → 任务预览 |
| series | 系列 | 任务树的系列（槽位列） |
| slot | 槽位 | 任务树槽位编号 |
| localisation | 本地化 | localisation key → 本地化键 |
| dependency | 依赖 | 依赖模组、依赖索引缓存 |
| workspace | 工作区 | |
| language server | 语言服务器 | |
| diagnostics | 诊断 | |
| walkthrough | 分步引导 | VS Code Walkthrough（不再用"新手引导"） |
| live scan | 实时扫描 | 依赖加载策略之一 |
| persistent index cache | 持久索引缓存 | `.pdcindex` |
| loaded files | 已加载文件 | 侧栏视图 |
| checksum sidecar | 校验和 sidecar | SHA-256 摘要文件 |
| transparent transcoding / transparent encoding | 透明转码 / 透明编码 | |
| decoded view | 解码视图 | `pdcloc://` 视图 |
| raw view / raw form | 原始视图 / 原始形态 | 磁盘上的转码形态 |
| peek | 窥视 | 窥视原始转码形态（临时切换） |
| escape marker / escape triplet | 逃逸标记 / 逃逸三元组 | EU4dll 编码单元 |
| scoped form | scoped 形态 | 只转码引号内内容的形态（技术名保留 scoped） |
| encode / decode | 编码 / 解码 | 手动转码命令动词 |
| follow-up completion | 后续补全 | 补全项插入后再次请求建议列表 |
| vanilla index cache | 原版索引缓存 | |

## 内部沟通专用（不出现在 UI）

| 英文 | 中文 | 说明 |
| --- | --- | --- |
| sweep | 全量扫描 | 本地 Vanilla 诊断/性能扫描工具（`scripts/sweep.mjs`） |
| golden | 黄金对拍 | `golden_gfx_sprite_semantics` 基准测试 |
| Conclusion | （保留英文） | PR/main 的远端必需检查名 |
| zone | 区 | 脚本区 / 本地化区（script/localisation zone isolation） |
| contract test | 契约测试 | `npm run test:contract` 五（现六）项检查 |

## 服务端诊断消息

Rust 服务端的诊断消息（Problems 面板内文）目前只有英文，暂不本地化；扩展侧产生的诊断（如透明转码的逃逸标记提示）已随 UI 双语化。
