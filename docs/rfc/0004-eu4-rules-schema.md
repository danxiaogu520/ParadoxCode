# RFC 0004：EU4 规则 Runtime Schema

- 状态：Current

## 范围

本 RFC 只描述当前 `pdx-rules` runtime/schema 代码：规则如何表示为不可变 `RuleSet`、如何分类文件和查询 semantic matcher，以及 SQLite artifact 如何校验。规则 authority、外部规则输入和官方分发边界以 [RFC 0013](0013-first-party-rule-source.md) 为准，不在本文重复。

## Runtime model

```text
RulesModel
  game_id
  file_categories
  symbol_descriptors
  records
  semantic

FileCategory
  id, parser, resolution, matcher

SymbolDescriptor
  kind_id, resolution, case_sensitive

SemanticModel
  rules
  enum_values
  type_root_keys
  type_root_scopes
  type_descriptors
  localisation_bindings
```

`FileCategory` 的 `parser` 当前是 `Script`、`Localisation`、`Asset` 或 `SyntaxOnly`。artifact 兼容读取中的 CSV parser 名称会映射为 `SyntaxOnly`，不会使 runtime 获得 CSV parser。`resolution` 是 `ReplaceByRelativePath`、`Merge` 或 `ReplaceDirectory`。

`SemanticRule` 保存 stable source-derived id、context、parent path、key/value matcher、shape、child context、scope 操作、cardinality、severity、deprecated、documentation 和 source provenance。key matcher 当前包括 `Exact`、`Type`、`Enum`、`AnyScalar`、`Dynamic`；value matcher 包括 scalar、bool、数值、date、type/enum、scope、localisation、filepath、dynamic 和 `Opaque` 等形式。

## SQLite 校验

当前 `pdx-rules::CURRENT_SCHEMA_VERSION` 为 `17`。`RuleSet::load` 以 read-only 方式打开 SQLite，启用 foreign keys，并要求 metadata 至少包含 `schema_version`、`game_id` 和 `rule_hash`。schema 不匹配、metadata 缺失、game identity 不符或逻辑 hash 不符都会返回错误；runtime 不从错误 artifact 继续服务。

读取后会重建 `RulesModel` 和不可变 `RuleSet`。runtime 不暴露 insert/update/delete；`RuleSet` 只提供查询、分类、规则 hash 和 schema version。SQLite artifact 是存储格式，不能通过手工修改来改变运行时规则。

## Canonical hash

`RuleHash` 是 SHA-256 摘要，针对 canonical logical model 计算，而不是 SQLite 文件字节。当前 canonicalization 会纳入 `game_id`、file categories、symbol descriptors、normalized records 和 semantic model；物理 rowid、页布局、索引、VACUUM、时间戳等不属于逻辑内容。

`RuleSet::from_model` 在计算 hash 前按稳定字段整理 categories、descriptors、records、semantic rules 和 localisation bindings，并对部分成员集合排序去重。由此相同逻辑内容生成相同 hash；影响分类、匹配、文档或 resolution 的逻辑变更必须改变 hash。

hash 计算后，runtime 还建立按 case-insensitive exact key 和 semantic context 的派生查找索引。这些索引只用于查询加速，不改变 canonical hash，也不是第二份规则数据。

## Workspace 边界

artifact 保存静态 matcher 和规则描述，不保存 workspace 当前有哪些 dynamic members。`Type`/dynamic 成员由 workspace index 查询，规则只决定查询方式、匹配语义和冲突策略；因此工作区中新定义的 scripted effect 等不会修改 `rule_hash`。

## 当前限制

当前 runtime 只校验和加载已定义的 schema；不提供运行时规则编辑、外部规则 fallback、任意规则代码执行或历史游戏版本切换。具体 EU4 profile 行为由 `pdx-game::eu4` 的数据与 engine/analysis 的通用查询共同解释。
