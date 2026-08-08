# RFC 0001：系统边界与 Crate 依赖

- 状态：Current

## 当前范围

ParadoxCode 的核心是与编辑器无关的 PDX 引擎，当前完整规则 profile 为 EU4。workspace、HIR、索引、analysis 和 LSP runtime 保持通用边界；EU4 的路径、文件分类和语义数据由 `pdx-game::eu4` 与 `pdx-rules` 提供。

规则权威、外部规则输入和官方规则分发边界以 [RFC 0013](0013-first-party-rule-source.md) 为准；本 RFC 只记录 crate 职责和依赖方向。

## 依赖方向

```text
pdx-text
pdx-parser -> pdx-text
pdx-rules  -> pdx-text
pdx-game   -> pdx-rules
pdx-engine -> pdx-text + pdx-parser + pdx-rules
pdx-analysis -> pdx-engine + pdx-rules
pdx-lsp -> pdx-engine + pdx-analysis + pdx-parser + pdx-rules + pdx-game
```

`pdx-rules` package 的 `pdx-bake` binary 复用同 package 的规则编译核心，不成为 analysis 或 runtime 的依赖。`editors/zed` 是薄客户端，不属于 Rust 核心依赖图。

## 职责边界

| 模块 | 当前职责 |
|---|---|
| `pdx-text` | `TextRange`、位置、行索引、UTF-16 和 `LogicalPath` 等文本基础设施 |
| `pdx-parser` | Script/Localisation 的 loss-aware CST、syntax error、编辑重解析和安全 formatter |
| `pdx-rules` | 规则模型、SQLite runtime schema、只读 `RuleSet`、canonical `rule_hash` 和 source compiler |
| `pdx-game` | 安装发现、用户级配置以及 EU4 profile 和内嵌第一方规则 source |
| `pdx-engine` | source roots、VFS 文档、parse/HIR 状态、文件 shard、workspace index 和 snapshot |
| `pdx-analysis` | 基于 snapshot 的 diagnostics、completion、hover、navigation、symbols 和 rename 查询 |
| `pdx-lsp` | JSON-RPC/LSP 生命周期、协议 DTO、URI/position 转换、取消和结果新鲜度 |

`pdx-parser` 不读取 workspace 或规则；`pdx-analysis` 不直接读磁盘，也不依赖 LSP 类型；只有 `pdx-lsp` 处理协议类型。Zed 扩展不实现 symbol 提取、scope 推导或诊断。

## Host 与 Snapshot

```text
AnalysisHost       可变状态的唯一 owner
AnalysisSnapshot   不可变查询视图
FileState          一个文件 revision 的 source + ParsedSource + HIR + shard
WorkspaceIndex     合并后的 definitions/references 与查找结构
RuleSet            一个 game profile 使用的不可变规则模型
```

事件循环向 `AnalysisHost` 提交文档、source-root 和磁盘变化；查询先取得一个 `AnalysisSnapshot`，之后不持有 host 的可变状态。后台 parse、lower、诊断和语言查询结果提交前必须检查文档版本或取消状态。

## 当前不变量

- source 内容优先级为 `Vanilla < Dependency < Current Mod < Overlay`。
- 每个文件独立生成并替换 `FileIndexShard`；失败刷新不替换上一个有效 snapshot。
- syntax error 不阻止局部 CST；HIR 对未知结构保留 `UnknownConstruct`，不因未知规则 panic。
- 外部规则路径、用户规则覆盖和运行时规则修改不属于核心 API。
- 绝对路径字符串和 CST node pointer 不作为跨请求的 symbol 身份。

## 当前限制

`pdx-parser` 当前没有 CSV parser；CSV 由规则分类为 `SyntaxOnly`/opaque 时不会产生 CSV CST、HIR 或列级语义诊断。HIR scope 仍是 profile/rule 驱动的部分实现，详见 RFC 0005；analysis 的完整语言功能也只对已有 profile 和规则覆盖的语义提供结果。
