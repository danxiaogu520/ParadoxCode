# `pdx-rules`

## 模块职责

`pdx-rules` 提供游戏无关的规则 schema、规范化运行时模型、SQLite artifact 读写、canonical hash，以及第一方 source compiler。
规则解释不放在 LSP；运行时使用冻结的 `RuleSet`，而 source authoring 只接受仓库约定的 JSON layout。

## 内部布局

`src/lib.rs` 是公共 facade。规则模型和 matcher 分别位于 `model.rs`、`matcher.rs`、`profile.rs`；
`runtime.rs` 提供不可变 `RuleSet` 查询，`canonical.rs` 负责逻辑 hash，`sqlite.rs` 负责 schema、
读写和 round-trip 编解码；`rulec.rs` 仍是固定第一方 JSON source compiler。测试位于 `tests.rs`，
并与 compiler 测试保持独立。

这些文件拆分只改变 crate 内部组织，`pdx_rules::*` 和 `pdx_rules::rulec::*` 公共路径保持稳定。

## 主要公开类型与入口

- `CURRENT_SCHEMA_VERSION`：当前 runtime SQLite schema，值为 `16`。
- `RuleHash`：32 字节 canonical SHA-256；可用 `from_bytes`、`as_bytes`、`to_hex`、`from_hex` 操作。
- `ParserKind`、`FileResolutionPolicy`、`SymbolResolutionPolicy`、`FileMatcher`、`FileCategory`、`SymbolDescriptor`：文件分类和覆盖/符号冲突策略。
- `RulesModel`、`RuleRecord`、`SemanticModel`、`SemanticRule`、`KeyMatcher`、`ValueMatcher`：规范化 catalog 与可执行语义 matcher 数据。
- `RuleSet::from_model`、`model`、`classify`、`exact_semantic_rules`、`semantic_rules_for_context`：构造和查询运行时规则。
- `RuleSet::load`：以 SQLite read-only 方式读取并校验 schema、metadata、game_id 和 hash；`write_sqlite` 仅用于产出 artifact。
- `RuleSet::schema_version`、`rule_hash`、`game_id`、`ensure_game`、`load_embedded`：读取身份、版本和嵌入 artifact。

## source compiler、`pdx-bake` 与 authority

RFC 0013 规定 `rules/eu4/` 是静态 EU4 规则的唯一 source authority。当前 source format 为 `5`，固定输入包括 `manifest.json`、`catalog.json`、`semantic-rules.json`、`enum-values.json`、`type-root-keys.json`、`type-root-scopes.json`、`type-descriptors.json` 和 `localisation-bindings.json`。

公开的 `pdx_rules::rulec` 入口是 `load_source`、`load_source_bundle` 和 `compile`；相关类型包括 `SourceBundle`、`SourceManifest`、`ArtifactManifest`、`CompileError`。compiler 拒绝 unknown field、缺文件、重复 stable identity 及无效 cross-record invariant，并在发布前读回 SQLite 做 round-trip 比较。

`Cargo.toml` 声明的 `pdx-bake` binary 是维护/发布检查入口：它只实现 `build` 子命令，并调用 `rulec::compile`。

```text
cargo run -p pdx-rules --bin pdx-bake -- build --source rules/eu4 --output <artifact.pdxrules> --manifest <manifest.json>
```

生成的 SQLite 和 release manifest 都是派生产物，不是第二个 authority；`rule_hash` 哈希 canonical logical content，不依赖 rowid、插入顺序或 SQLite 页面布局。`.cwt`、下载规则库和用户 override 不属于当前 source compiler 或正式 runtime 输入。

## 输入、输出与数据流

`rules/eu4/*.json` → `rulec::load_source`/`load_source_bundle` → `RulesModel` → `RuleSet::from_model`（排序并计算 `RuleHash`）→ 临时 SQLite → `RuleSet::load` 校验 → artifact 与 `ArtifactManifest`。

运行时 `RuleSet` 本身是不可变模型；`RuleSet::load` 不写入输入数据库，hash/schema/game 不匹配时返回 `RulesError`，不会返回未验证的规则。

## 明确不负责的边界与当前限制

- 不提供 LSP、workspace/index、磁盘扫描或编辑器协议；动态 scripted effects/triggers 等成员来自上层 `WorkspaceIndex`，不在这里硬编码。
- compiler 是固定第一方 JSON source 编译器，不是 CWT/外部规则语言兼容层。
- 当前 runtime schema 为 `16`，不接受其他 schema；规则身份由 `game_id` 和 canonical `rule_hash` 校验。
- 正式 server 的用户入口不接受外部规则路径、下载 source 或用户规则覆盖；developer `compile` 的 source path 仅用于受控构建流程。

## 验证命令

```text
cargo test -p pdx-rules
cargo run -p pdx-rules --bin pdx-bake -- build --source rules/eu4 --output <artifact.pdxrules> --manifest <manifest.json>
```
