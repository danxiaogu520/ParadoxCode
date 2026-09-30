//! `rules-migrate` — the one-shot legacy → rules-v2 source conversion
//! (phase 2 of `docs/rules-redesign.md` §6).
//!
//! The tool reads the legacy manifest-driven corpus (`rules/eu4`), applies the
//! automatic conversions listed in the migration plan — flat `parent_path`
//! rows into nested schemas, `alternative_id` bundles into overloads/unions,
//! matchers into type expressions, row deduplication, the `on_action` fold,
//! pure scope-link rows into `scopes.links`, the profile tables per §4, and
//! `member_kind_aliases` normalisation — and writes a rules-v2 source tree
//! that `rulec check` accepts. Everything the automatic pass cannot convert
//! faithfully lands in the generated report (the manual checklist).
//!
//! The conversion is deterministic: repeated runs over an unchanged legacy
//! tree produce byte-identical output.

pub mod expr;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rules::rulec;
use rules::source::{
    BlockSchema, CompatSpec, ControlKind, ControlSpec, DefSpec, EnumSpec, ExtSpec, FieldOverloads,
    FieldSpec, FileRule, ImplSpec, LinkSpec, MapSpec, MixinSpec, RegisterSpec, RootSpec, RuleFile,
    SchemaSpec, ScopeEffect, ScopesSpec, Severity, SourceFileResolution, SubtypeCond, SubtypeSpec,
    TraitSpec, TypeSpec, TypeResolution,
};
use rules::{KeyMatcher, RuleShape, RulesModel, SemanticRule, ValueMatcher};

use crate::migrate::expr::Norm;

/// Command-line options of `rules-migrate`.
pub struct Options {
    /// Legacy rules source directory (contains the old `manifest.json`).
    pub source: PathBuf,
    /// Output directory for the rules-v2 source tree.
    pub out: PathBuf,
    /// Path of the generated migration report.
    pub report: PathBuf,
}

/// Runs the conversion and returns the summary printed to stdout.
pub fn run(options: &Options) -> Result<String, String> {
    let (_manifest, model) = rulec::load_source(&options.source)
        .map_err(|failure| format!("load legacy source: {failure}"))?;
    let mut converter = Cvt::new(&model, &options.source)?;
    converter.convert_all();
    converter.emit(&options.out)?;
    let report = converter.render_report();
    std::fs::write(&options.report, &report)
        .map_err(|failure| format!("{}: {failure}", options.report.display()))?;
    Ok(converter.summary(&options.report))
}

/// One deduplicated legacy rule row plus its provenance.
struct Row {
    rule: SemanticRule,
    /// Output file derived from the legacy fragment that carried the row.
    fragment: String,
}

/// One folded scope link (from/to accumulated over the folded rows).
#[derive(Default)]
struct LinkInfo {
    from: BTreeSet<String>,
    to: String,
    count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FieldGroupKey {
    /// Exact key: lowercased spelling plus the display spelling.
    Exact(String, String),
    /// Pattern key: the rendered `key` expression.
    Pattern(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpecClass {
    Scalar,
    Block,
    Quoted,
    Clause,
}

/// Attributes that must match for two rows to merge into one field spec.
#[derive(Clone, Debug, PartialEq)]
struct FieldAttrs {
    card: String,
    scope: Option<ScopeEffect>,
    control: Option<ControlSpec>,
    severity: Option<Severity>,
    deprecated: Option<bool>,
}

/// A batch of row-derived field specs that merge into one field entry.
struct FieldGroup {
    key: FieldGroupKey,
    class: SpecClass,
    attrs: FieldAttrs,
    specs: Vec<FieldSpec>,
    docs: Vec<String>,
}

impl FieldGroup {
    fn mergeable(&self, other: &FieldGroup) -> bool {
        self.key == other.key
            && self.class == other.class
            && self.attrs == other.attrs
            && self.specs.len() == 1
            && other.specs.len() == 1
            && self.specs[0].body == other.specs[0].body
            && self.specs[0].list == other.specs[0].list
            && self.specs[0].map == other.specs[0].map
            && self.specs[0].def == other.specs[0].def
    }

    fn merge(&mut self, mut other: FieldGroup) {
        let left = self.specs.pop().expect("one spec");
        let right = other.specs.pop().expect("one spec");
        let value = expr::union(vec![
            left.value.clone().unwrap_or_default(),
            right.value.clone().unwrap_or_default(),
        ]);
        let mut merged = left;
        if !value.is_empty() {
            merged.value = Some(value);
        }
        self.specs.push(merged);
        for doc in other.docs {
            if !self.docs.contains(&doc) {
                self.docs.push(doc);
            }
        }
    }

    fn finish(mut self, _cvt: &mut Cvt<'_>) -> (FieldGroupKey, FieldSpec) {
        let mut spec = self.specs.pop().expect("one spec");
        if !self.docs.is_empty() {
            spec.doc = Some(self.docs.join("\n"));
        }
        (self.key, spec)
    }
}

/// Body reference carried by a block-valued row.
enum BodyRef {
    Schema(String),
    SelfBlock,
}

struct Cvt<'model> {
    model: &'model RulesModel,
    norm: Norm,
    /// Deduplicated rows in stable source order.
    rows: Vec<Row>,
    /// Row indexes grouped by legacy context name.
    by_context: BTreeMap<String, Vec<usize>>,
    /// Output source files keyed by output-relative path.
    files: BTreeMap<String, RuleFile>,
    /// Schema name → output file (placement and duplicate detection).
    schema_file: BTreeMap<String, String>,
    /// Context-set mixins materialised after schema building.
    mixin_contexts: BTreeSet<Vec<String>>,
    /// Mixin includes owed to schemas that have not been built yet.
    pending_includes: BTreeMap<String, Vec<String>>,
    /// `type:`-supplement mixins owed to schemas not built yet.
    pending_supplements: BTreeMap<String, String>,
    /// Subtype names collected per type.
    type_subtypes: BTreeMap<String, BTreeSet<String>>,
    /// Folded scope links (name → info).
    links: BTreeMap<String, LinkInfo>,
    /// Manual checklist: category → entries.
    manual: BTreeMap<String, Vec<String>>,
    /// Coverage counters.
    counters: BTreeMap<String, usize>,
    /// On-action names → (root scope, from scope).
    on_action_scopes: BTreeMap<String, (String, Option<String>)>,
    /// Types whose instances are collected by descriptors (def candidates).
    def_types: BTreeSet<String>,
    /// Types whose def position was found in the semantic tree.
    tree_defs: BTreeSet<String>,
    /// Non-language configuration (`game.json`).
    game_json: serde_json::Value,
    /// Output `manifest.json` file list.
    manifest_files: Vec<String>,
    /// Legacy `target_game_version`, carried into the new manifest.
    target_game_version: String,
}

impl<'model> Cvt<'model> {
    fn new(model: &'model RulesModel, source: &Path) -> Result<Self, String> {
        let mut enum_names: Vec<String> = model.semantic.enum_values.keys().cloned().collect();
        enum_names.extend(model.profile.enum_extra_members.keys().cloned());
        enum_names.sort();
        enum_names.dedup();
        let mut def_types: BTreeSet<String> =
            model.semantic.type_descriptors.keys().cloned().collect();
        def_types.extend(
            model
                .profile
                .definitions
                .iter()
                .map(|rule| rule.kind.clone()),
        );
        let mut norm = Norm {
            aliases: model.profile.member_kind_aliases.clone(),
            scope_aliases: model.profile.scope_member_aliases.clone(),
            enum_names,
            type_names: BTreeSet::new(),
        };
        norm.type_names = def_types
            .iter()
            .map(|name| norm.type_name(name))
            .collect();
        let mut converter = Self {
            model,
            norm,
            rows: Vec::new(),
            by_context: BTreeMap::new(),
            files: BTreeMap::new(),
            schema_file: BTreeMap::new(),
            mixin_contexts: BTreeSet::new(),
            pending_includes: BTreeMap::new(),
            pending_supplements: BTreeMap::new(),
            type_subtypes: BTreeMap::new(),
            links: BTreeMap::new(),
            manual: BTreeMap::new(),
            counters: BTreeMap::new(),
            on_action_scopes: BTreeMap::new(),
            def_types,
            tree_defs: BTreeSet::new(),
            game_json: serde_json::Value::Null,
            manifest_files: Vec::new(),
            target_game_version: String::new(),
        };
        converter.load_rows(source)?;
        Ok(converter)
    }

    // ---------------------------------------------------------------- loading

    /// Parses the legacy semantic fragments directly so every row keeps the
    /// fragment it came from (the output layout follows the input layout).
    fn load_rows(&mut self, source: &Path) -> Result<(), String> {
        let manifest_text = std::fs::read_to_string(source.join("manifest.json"))
            .map_err(|failure| format!("manifest.json: {failure}"))?;
        let manifest: serde_json::Value = serde_json::from_str(&manifest_text)
            .map_err(|failure| format!("manifest.json: {failure}"))?;
        self.target_game_version = manifest
            .get("target_game_version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let fragments = manifest
            .get("files")
            .and_then(|files| files.get("semantic"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "manifest.json: files.semantic is missing".to_owned())?;
        let mut seen = BTreeMap::new();
        let mut total = 0usize;
        for fragment in fragments {
            let name = fragment
                .as_str()
                .ok_or_else(|| "manifest.json: non-string semantic fragment".to_owned())?;
            let text = std::fs::read_to_string(source.join(name))
                .map_err(|failure| format!("{name}: {failure}"))?;
            let rows: Vec<SemanticRule> =
                serde_json::from_str(&text).map_err(|failure| format!("{name}: {failure}"))?;
            let out_path = fragment_out_path(name);
            for rule in rows {
                total += 1;
                if seen.insert(dedup_key(&rule), ()).is_some() {
                    self.count("rows deduplicated");
                    continue;
                }
                self.rows.push(Row {
                    rule,
                    fragment: out_path.clone(),
                });
            }
        }
        self.count_n("rows total", total);
        for index in 0..self.rows.len() {
            let context = self.rows[index].rule.context.clone();
            self.by_context.entry(context).or_default().push(index);
        }
        Ok(())
    }

    fn count(&mut self, key: &str) {
        *self.counters.entry(key.to_owned()).or_default() += 1;
    }

    fn count_n(&mut self, key: &str, value: usize) {
        *self.counters.entry(key.to_owned()).or_default() += value;
    }

    fn manual(&mut self, category: &str, item: String) {
        self.manual.entry(category.to_owned()).or_default().push(item);
    }

    // ------------------------------------------------------------- conversion

    fn convert_all(&mut self) {
        self.collect_on_action_scopes();
        self.prescan_type_supplements();
        self.convert_contexts();
        self.convert_on_actions();
        self.convert_scopes();
        self.convert_traits();
        self.convert_files();
        self.convert_enums();
        self.convert_types();
        self.convert_game_json();
        self.materialise_mixins();
        self.write_manifest();
    }

    fn file(&mut self, path: &str) -> &mut RuleFile {
        self.files.entry(path.to_owned()).or_default()
    }

    // -------------------------------------------------------- schema building

    fn convert_contexts(&mut self) {
        let contexts: Vec<String> = self.by_context.keys().cloned().collect();
        for context in contexts {
            if context == "root:on_action" {
                continue;
            }
            self.convert_context(&context);
        }
    }

    /// Registers `type:`-supplement mixins for `root:`-context bodies before
    /// the schemas are built.
    fn prescan_type_supplements(&mut self) {
        let contexts: Vec<String> = self.by_context.keys().cloned().collect();
        for context in contexts {
            if let Some(name) = context.strip_prefix("root:") {
                let type_context = format!("type:{name}");
                if self.by_context.contains_key(&type_context) {
                    let schema = self.root_schema_name(&context);
                    self.pending_supplements.insert(schema, name.to_owned());
                    self.mixin_contexts.insert(vec![type_context]);
                }
            }
        }
    }

    fn convert_context(&mut self, context: &str) {
        let Some(row_indexes) = self.by_context.get(context).cloned() else {
            return;
        };
        let mut positions: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for index in row_indexes {
            let rule = &self.rows[index].rule;
            if matches!(rule.shape, RuleShape::LeafValue) {
                // Bare-value rows are consumed by their position below.
            }
            positions
                .entry(pos_key(&path_of(rule)))
                .or_default()
                .push(index);
        }
        // Orphan check: every non-root position must be reachable from some
        // node row of the parent position.
        let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (key, rows) in &positions {
            // Rows at `key` declare the child positions of `key`.
            let segments = declared.entry(key.clone()).or_default();
            for &index in rows {
                let rule = &self.rows[index].rule;
                if matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause) {
                    segments.insert(expr::key_segment(&rule.key));
                }
            }
        }
        let mut orphans: Vec<String> = Vec::new();
        for key in positions.keys() {
            if key.is_empty() {
                continue;
            }
            let parent = parent_key(key);
            let segment = last_segment(key);
            let known = declared.get(&parent).is_some_and(|set| set.contains(segment));
            if known {
                continue;
            }
            // Loose fallback: pair a single unmatched segment with a single
            // unmatched non-exact node row (legacy template spellings).
            let unmatched_rows = positions.get(&parent).cloned().unwrap_or_default();
            let mut loose = false;
            let mut spare: Option<usize> = None;
            for &index in &unmatched_rows {
                let rule = &self.rows[index].rule;
                if !matches!(rule.shape, RuleShape::Node | RuleShape::ValueClause)
                    || matches!(rule.key, KeyMatcher::Exact(_))
                {
                    continue;
                }
                if declared
                    .get(key)
                    .is_some_and(|set| set.contains(&expr::key_segment(&rule.key)))
                {
                    continue;
                }
                if spare.is_some() {
                    spare = None;
                    break;
                }
                spare = Some(index);
            }
            if let Some(index) = spare {
                declared
                    .entry(parent)
                    .or_default()
                    .insert(expr::key_segment(&self.rows[index].rule.key));
                loose = true;
                self.count("positions correlated loosely");
            }
            if !loose {
                orphans.push(key.clone());
            }
        }
        let mut date_orphans: Vec<(String, usize)> = Vec::new();
        for orphan in &orphans {
            if last_segment(orphan) == "date_field" {
                if let Some(rows) = positions.get(orphan) {
                    date_orphans.push((orphan.clone(), rows.len()));
                }
                continue;
            }
            if let Some(rows) = positions.remove(orphan) {
                self.count_n("rows orphaned", rows.len());
                self.manual(
                    "structure",
                    format!(
                        "context `{context}`: position `{}` has no declaring row; its {} row(s) \
                         are dropped from the automatic output",
                        last_segment(orphan),
                        rows.len()
                    ),
                );
            }
        }
        let keys: Vec<String> = positions.keys().cloned().collect();
        for key in keys {
            let path = split_pos(&key);
            let schema_name = self.schema_name(context, &path);
            let rows = positions.get(&key).cloned().unwrap_or_default();
            let mut items: Vec<String> = Vec::new();
            let mut groups: Vec<FieldGroup> = Vec::new();
            let mut folded_link = false;
            for index in rows {
                let rule = &self.rows[index].rule;
                if matches!(rule.shape, RuleShape::LeafValue) {
                    items.push(expr::value_expr(&rule.value, &self.norm));
                    self.count("rows into items");
                    continue;
                }
                self.count("rows inspected for fields");
                match self.field_specs(context, index, &path) {
                    FieldOutcome::FoldedLink => {
                        self.count("rows folded into scopes.links");
                        folded_link = true;
                    }
                    FieldOutcome::FoldedRegister => self.count("rows folded as register shifts"),
                    FieldOutcome::Specs(built) => {
                        self.count("rows into fields");
                        for group in built {
                            match groups.iter_mut().find(|existing| existing.mergeable(&group)) {
                                Some(existing) => existing.merge(group),
                                None => groups.push(group),
                            }
                        }
                    }
                }
            }
            let mut block = BlockSchema {
                include: Vec::new(),
                fields: BTreeMap::new(),
                patterns: Vec::new(),
                items: (!items.is_empty()).then(|| expr::union(items)),
                open: false,
            };
            let mut exact: BTreeMap<String, (String, Vec<FieldSpec>)> = BTreeMap::new();
            for group in groups {
                let (key_kind, mut spec) = group.finish(self);
                if self.pending_includes.contains_key(&schema_name)
                    || self.pending_supplements.contains_key(&schema_name)
                {
                    spec.override_field = Some(true);
                }
                match key_kind {
                    FieldGroupKey::Exact(lower, display) => {
                        exact
                            .entry(lower)
                            .or_insert_with(|| (display, Vec::new()))
                            .1
                            .push(spec);
                    }
                    FieldGroupKey::Pattern(_) => block.patterns.push(spec),
                }
            }
            for (_, (display, specs)) in exact {
                let entry = if specs.len() == 1 {
                    FieldOverloads::One(Box::new(specs.into_iter().next().expect("one spec")))
                } else {
                    FieldOverloads::Many(specs)
                };
                block.fields.insert(display, entry);
            }
            order_patterns(&mut block.patterns);
            self.dedupe_patterns(&mut block.patterns);
            if folded_link {
                // §8: one `link` pattern replaces the folded scope-switch rows.
                block.patterns.push(FieldSpec {
                    key: Some("link".to_owned()),
                    body: Some("self".to_owned()),
                    card: "0..*".to_owned(),
                    ..empty_spec()
                });
            }
            if let Some(includes) = self.pending_includes.remove(&schema_name) {
                for include in includes {
                    if !block.include.contains(&include) {
                        block.include.push(include);
                    }
                }
            }
            if let Some(name) = self.pending_supplements.remove(&schema_name)
                && let Some(mixin) = self.type_supplement_mixin(&name)
                && !block.include.contains(&mixin)
            {
                block.include.push(mixin);
            }
            let owner = self.output_for_context(context);
            self.place_schema(&schema_name, &owner, block);
        }
        // Root schema (positions always contain the root; keep a fallback for
        // contexts whose rows all vanished as orphans).
        let root_name = self.root_schema_name(context);
        if !self.schema_file.contains_key(&root_name) {
            let owner = self.output_for_context(context);
            let mut block = BlockSchema::default();
            if let Some(name) = self.pending_supplements.remove(&root_name)
                && let Some(mixin) = self.type_supplement_mixin(&name)
            {
                block.include.push(mixin);
            }
            self.place_schema(&root_name, &owner, block);
        }
        // `date_field` is a pseudo segment: a date-keyed block. A position the
        // corpus never declares explicitly still has a shape — when the date
        // is the key, the block is one pattern on the `date` primitive.
        for (key, rows) in date_orphans {
            let path = split_pos(&key);
            let child = self.schema_name(context, &path);
            let parent = self.schema_name(context, &path[..path.len().saturating_sub(1)]);
            self.count_n("rows folded into date patterns", rows);
            self.add_schema_pattern(
                &parent,
                FieldSpec {
                    key: Some("date".to_owned()),
                    body: Some(child),
                    card: "0..*".to_owned(),
                    ..empty_spec()
                },
            );
        }
    }

    /// Appends one pattern to an already-placed block schema.
    fn add_schema_pattern(&mut self, schema: &str, spec: FieldSpec) {
        let Some(path) = self.schema_file.get(schema).cloned() else {
            self.manual(
                "structure",
                format!("schema `{schema}` was not generated for the date pattern"),
            );
            return;
        };
        let Some(file) = self.files.get_mut(&path) else {
            return;
        };
        let Some(SchemaSpec::Block(block)) = file.schemas.get_mut(schema) else {
            return;
        };
        block.patterns.push(spec);
    }

    /// Builds field specs for one row (a folded link row folds away).
    fn field_specs(&mut self, context: &str, index: usize, path: &[String]) -> FieldOutcome {
        let rule = self.rows[index].rule.clone();
        let row = &rule;
        // Pure scope-switch rows fold away (§8): link rows (`push_scope`) and
        // register-shift rows (`prev`, `from`, …) are covered by the `link`
        // pattern and the `scopes.registers` declaration.
        if matches!(row.shape, RuleShape::Node)
            && row.child_context.as_deref() == Some(context)
            && self.count_children(context, &pos_key(&join_key(path, &expr::key_segment(&row.key))))
                == 0
            && let KeyMatcher::Exact(name) = &row.key
        {
            let register_shift =
                row.push_scope.is_none() && !row.replace_scope.is_empty() && is_register_name(name);
            if row.push_scope.is_some() {
                let link = self.links.entry(name.clone()).or_default();
                link.count += 1;
                link.to = self.norm.scope_name(row.push_scope.as_deref().unwrap_or("any"));
                let from: BTreeSet<String> = row
                    .allowed_scopes
                    .iter()
                    .filter(|scope| !scope.eq_ignore_ascii_case("any"))
                    .map(|scope| self.norm.scope_name(scope))
                    .collect();
                if from.is_empty() {
                    link.from.insert("any".to_owned());
                } else {
                    link.from.extend(from);
                }
                return FieldOutcome::FoldedLink;
            }
            if register_shift {
                return FieldOutcome::FoldedRegister;
            }
        }
        let class = match row.shape {
            RuleShape::Node => SpecClass::Block,
            RuleShape::QuotedScript => SpecClass::Quoted,
            RuleShape::ValueClause => SpecClass::Clause,
            RuleShape::Leaf | RuleShape::LeafValue => SpecClass::Scalar,
        };
        let key = match &row.key {
            KeyMatcher::Exact(spelling) => FieldGroupKey::Exact(spelling.to_lowercase(), spelling.clone()),
            other => {
                if let Some(prefix) = expr::key_needs_manual(other) {
                    self.manual(
                        "magic-segments",
                        format!(
                            "context `{context}` position `{}`: template key uses strip_prefix \
                             `{prefix}`, which the expression grammar cannot spell",
                            path.join("/")
                        ),
                    );
                }
                if matches!(other, KeyMatcher::Type(_)) {
                    self.count("magic key segments");
                }
                FieldGroupKey::Pattern(expr::key_expr(other, &self.norm))
            }
        };
        let attrs = FieldAttrs {
            card: card_of(row),
            scope: self.scope_effect(row),
            control: control_of(row),
            severity: row.severity.map(|level| match level {
                2 => Severity::Warning,
                3 => Severity::Info,
                _ => Severity::Error,
            }),
            deprecated: if row.deprecated { Some(true) } else { None },
        };
        if matches!(row.value, ValueMatcher::TypedPrefix { .. }) {
            // §2.2 maps `prefix<name>` to the template
            // `'prefix:{ref<scripted_trigger>}'`. The legacy
            // `numeric_or_bool` operand filter was enforced by the analysis
            // layer over the resolved alias row and has no expression form;
            // it is a recorded coverage loss, not an item for the manual pass.
            self.count("typed-prefix operand filters dropped");
        }
        if matches!(row.value, ValueMatcher::Opaque(_)) {
            self.manual(
                "expressions",
                format!(
                    "context `{context}` position `{}`: opaque value matcher kept as `opaque`",
                    path.join("/")
                ),
            );
        }
        let doc = if row.documentation.is_empty() {
            Vec::new()
        } else {
            vec![row.documentation.join("\n")]
        };
        let mut specs = Vec::new();
        match class {
            SpecClass::Scalar | SpecClass::Quoted => {
                let value = if class == SpecClass::Quoted {
                    let target = row.child_context.clone().unwrap_or_else(|| context.to_owned());
                    format!("quoted<{}>", self.root_schema_name(&target))
                } else {
                    expr::value_expr(&row.value, &self.norm)
                };
                if let ValueMatcher::Type(name) | ValueMatcher::Dynamic(name) = &row.value {
                    let canonical = self.norm.type_name(name);
                    let (base, subtype) = split_subtype(&canonical);
                    self.note_type_use(base, subtype);
                }
                specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                    spec.value = Some(value);
                }));
            }
            SpecClass::Block => {
                let body = self.body_ref(context, index, path);
                let map = self.map_key_for(context, index);
                if let Some((map_key, def_type)) = map {
                    if let Some(name) = &def_type {
                        let (base, subtype) = split_subtype(name);
                        self.note_type_use(base, subtype);
                    }
                    let body_name = match &body {
                        BodyRef::Schema(name) => name.clone(),
                        BodyRef::SelfBlock => "self".to_owned(),
                    };
                    specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                        spec.map = Some(MapSpec {
                            key: map_key,
                            value: None,
                            body: Some(body_name),
                        });
                    }));
                } else {
                    let body_name = match &body {
                        BodyRef::Schema(name) => name.clone(),
                        BodyRef::SelfBlock => "self".to_owned(),
                    };
                    specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                        spec.body = Some(body_name);
                    }));
                }
            }
            SpecClass::Clause => {
                // A value clause: the scalar form plus the block form.
                let children_key = pos_key(&join_key(path, &expr::key_segment(&row.key)));
                let rows = self.by_context.get(context).cloned().unwrap_or_default();
                let mut items = Vec::new();
                let mut keyed = false;
                for &child in &rows {
                    let child_rule = &self.rows[child].rule;
                    if pos_key(&path_of(child_rule)) != children_key {
                        continue;
                    }
                    if matches!(child_rule.shape, RuleShape::LeafValue) {
                        let value = expr::value_expr(&child_rule.value, &self.norm);
                        if let ValueMatcher::Type(name) | ValueMatcher::Dynamic(name) =
                            &child_rule.value
                        {
                            let canonical = self.norm.type_name(name);
                            let (base, subtype) = split_subtype(&canonical);
                            self.note_type_use(base, subtype);
                        }
                        items.push(value);
                    } else {
                        keyed = true;
                    }
                }
                let scalar_value = if items.is_empty() {
                    expr::value_expr(&row.value, &self.norm)
                } else {
                    expr::union(items.clone())
                };
                specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                    spec.value = Some(scalar_value.clone());
                }));
                if keyed {
                    let body = self.body_ref(context, index, path);
                    let body_name = match &body {
                        BodyRef::Schema(name) => name.clone(),
                        BodyRef::SelfBlock => "self".to_owned(),
                    };
                    specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                        spec.body = Some(body_name);
                    }));
                } else {
                    let list_value = expr::union(items);
                    specs.push(self.spec_with(key.clone(), attrs.clone(), |spec| {
                        spec.list = Some(list_value.clone());
                    }));
                }
            }
        }
        FieldOutcome::Specs(vec![FieldGroup {
            key,
            class,
            attrs,
            specs,
            docs: doc,
        }])
    }

    fn spec_with(&self, key: FieldGroupKey, attrs: FieldAttrs, fill: impl FnOnce(&mut FieldSpec)) -> FieldSpec {
        let mut spec = empty_spec();
        if let FieldGroupKey::Pattern(rendered) = key {
            spec.key = Some(rendered);
        }
        spec.card = attrs.card;
        spec.scope = attrs.scope;
        spec.control = attrs.control;
        spec.severity = attrs.severity;
        spec.deprecated = attrs.deprecated;
        fill(&mut spec);
        spec
    }

    /// The `map` key expression of a def-carrying node row, when the row's key
    /// matcher names a type whose instances are collected here.
    fn map_key_for(&mut self, context: &str, index: usize) -> Option<(String, Option<String>)> {
        let rule = self.rows[index].rule.clone();
        let row = &rule;
        let KeyMatcher::Type(name) = &row.key else {
            return None;
        };
        let canonical = self.norm.type_name(name);
        let (base, _subtype) = split_subtype(&canonical);
        if !self.def_types.contains(base) {
            self.count("magic keys kept as ref patterns");
            self.manual(
                "magic-segments",
                format!(
                    "context `{context}`: key `ref<{canonical}>` kept as a reference pattern — \
                     verify it does not define instances"
                ),
            );
            return None;
        }
        if !self.context_is_definition_body(context) {
            self.count("magic keys kept as ref patterns");
            self.manual(
                "magic-segments",
                format!(
                    "context `{context}`: key `ref<{canonical}>` in a non-definition context kept \
                     as a reference pattern (call position)"
                ),
            );
            return None;
        }
        // A `{type: X}` node key is a definition site of X only when the
        // profile collects X's definitions from the documents this context
        // describes. `common/governments` naming a reform is a use, not a
        // second definition file.
        match self.context_definition_path(context, base) {
            Some(true) => {}
            Some(false) => {
                self.count("magic keys kept as ref patterns");
                self.manual(
                    "magic-segments",
                    format!(
                        "context `{context}`: key `ref<{canonical}>` kept as a reference pattern — \
                         the profile defines `{base}` in another directory"
                    ),
                );
                return None;
            }
            None => {
                self.manual(
                    "magic-segments",
                    format!(
                        "context `{context}`: key `ref<{canonical}>` converted to \
                         `map {{key: \"def<{canonical}>\", …}}` — verify this is the real \
                         definition site (no file path to compare)"
                    ),
                );
            }
        }
        self.count("magic keys converted to def maps");
        self.tree_defs.insert(base.to_owned());
        Some((format!("def<{canonical}>"), Some(canonical)))
    }

    /// Whether definitions of `type_name` come from the directory `context`
    /// describes.
    ///
    /// `None` means the comparison is not possible (one of the two has no
    /// profile path), so the caller keeps its previous behaviour but asks for
    /// a manual look.
    fn context_definition_path(&self, context: &str, type_name: &str) -> Option<bool> {
        let context_name = context.strip_prefix("root:")?;
        let path_of = |name: &str| {
            let descriptor = self.model.semantic.type_descriptors.get(name)?;
            let path = descriptor
                .path
                .as_deref()
                .unwrap_or_default()
                .trim_end_matches('/');
            if path.is_empty() {
                return None;
            }
            Some(path.strip_prefix("game/").unwrap_or(path).to_owned())
        };
        let context_path = path_of(context_name)?;
        let type_path = path_of(type_name)?;
        Some(context_path == type_path || context_path.starts_with(&format!("{type_path}/")))
    }

    /// The `body`/`map` target of a block-valued row.
    fn body_ref(&mut self, context: &str, row_index: usize, path: &[String]) -> BodyRef {
        let rule = self.rows[row_index].rule.clone();
        let row = &rule;
        let segment = expr::key_segment(&row.key);
        let child_path = join_key(path, &segment);
        let child_key = pos_key(&child_path);
        let child_rows = self.count_children(context, &child_key);
        let cc = row.child_context.clone();
        let nested_name = self.schema_name(context, &child_path);
        let mut includes: Vec<String> = Vec::new();
        if let KeyMatcher::Type(name) = &row.key {
            let canonical = self.norm.type_name(name);
            let (base, _subtype) = split_subtype(&canonical);
            if self.context_is_definition_body(context) && self.def_types.contains(base) {
                if let Some(mixin) = self.type_supplement_mixin(base) {
                    includes.push(mixin);
                }
            }
        }
        match (child_rows > 0, cc.as_deref()) {
            (false, None) => {
                let empty = format!("{}__empty", self.root_schema_name(context));
                if !self.schema_file.contains_key(&empty) {
                    let owner = self.output_for_context(context);
                    self.place_schema(&empty, &owner, BlockSchema::default());
                }
                BodyRef::Schema(empty)
            }
            (false, Some(target)) if target == context => BodyRef::SelfBlock,
            (false, Some(target)) => {
                if includes.is_empty() {
                    self.mixin_contexts.insert(vec![target.to_owned()]);
                    BodyRef::Schema(self.root_schema_name(target))
                } else {
                    self.mixin_contexts.insert(vec![target.to_owned()]);
                    includes.insert(0, mixin_name(&[target.to_owned()]));
                    let owner = self.output_for_context(context);
                    let block = BlockSchema {
                        include: includes,
                        ..BlockSchema::default()
                    };
                    self.place_schema(&nested_name, &owner, block);
                    BodyRef::Schema(nested_name)
                }
            }
            (true, None) => {
                if !includes.is_empty() {
                    self.pending_includes
                        .entry(nested_name.clone())
                        .or_default()
                        .extend(includes);
                }
                BodyRef::Schema(nested_name)
            }
            (true, Some(target)) => {
                self.mixin_contexts.insert(vec![target.to_owned()]);
                let mut all = vec![mixin_name(&[target.to_owned()])];
                all.extend(includes);
                self.pending_includes
                    .entry(nested_name.clone())
                    .or_default()
                    .extend(all);
                BodyRef::Schema(nested_name)
            }
        }
    }

    fn count_children(&self, context: &str, child_key: &str) -> usize {
        self.by_context.get(context).map_or(0, |rows| {
            rows.iter()
                .filter(|&&index| {
                    pos_key(&path_of(&self.rows[index].rule)) == child_key
                        && !matches!(self.rows[index].rule.shape, RuleShape::LeafValue)
                })
                .count()
        })
    }

    fn context_is_definition_body(&self, context: &str) -> bool {
        context.starts_with("root:") && !context.ends_with("_entries")
    }

    fn note_type_use(&mut self, name: &str, subtype: Option<&str>) {
        let entry = self.type_subtypes.entry(name.to_owned()).or_default();
        if let Some(subtype) = subtype {
            entry.insert(subtype.to_owned());
        }
    }

    fn type_supplement_mixin(&mut self, name: &str) -> Option<String> {
        let context = format!("type:{name}");
        if !self.by_context.contains_key(&context) {
            return None;
        }
        self.mixin_contexts.insert(vec![context.clone()]);
        Some(mixin_name(&[context]))
    }

    fn root_schema_name(&self, context: &str) -> String {
        if let Some(name) = context.strip_prefix("root:") {
            if let Some(base) = name.strip_suffix("_entries") {
                format!("{}_file", sanitize(base))
            } else {
                format!("{}_body", sanitize(name))
            }
        } else if let Some(name) = context.strip_prefix("type:") {
            format!("{}_type_body", sanitize(name))
        } else {
            sanitize(context)
        }
    }

    fn schema_name(&self, context: &str, path: &[String]) -> String {
        let mut name = self.root_schema_name(context);
        for segment in path {
            name.push_str("__");
            name.push_str(&sanitize(segment));
        }
        name
    }

    fn output_for_context(&self, context: &str) -> String {
        let mut votes: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(rows) = self.by_context.get(context) {
            for &index in rows {
                *votes.entry(self.rows[index].fragment.clone()).or_default() += 1;
            }
        }
        votes
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map_or_else(|| "core/misc.json".to_owned(), |(path, _)| path)
    }

    /// Places a schema, or merges it into an existing schema of the same name.
    ///
    /// A file entry can gather several types: one of them may have named the
    /// root through its `root_entries` context (already placed by the context
    /// pass), while the others contribute wrapper fields. Re-placing the name
    /// would drop those fields and report a bogus collision, so they merge.
    fn place_or_merge_root(&mut self, name: &str, owner: &str, block: BlockSchema) {
        let Some(path) = self.schema_file.get(name).cloned() else {
            self.place_schema(name, owner, block);
            return;
        };
        let Some(file) = self.files.get_mut(&path) else {
            return;
        };
        let Some(SchemaSpec::Block(existing)) = file.schemas.get_mut(name) else {
            return;
        };
        for (key, overloads) in block.fields {
            match existing.fields.get_mut(&key) {
                Some(current) => merge_overloads(current, &overloads),
                None => {
                    existing.fields.insert(key, overloads);
                }
            }
        }
        existing.patterns.extend(block.patterns);
        existing.include.extend(block.include);
        existing.open |= block.open;
        order_patterns(&mut existing.patterns);
    }

    fn place_schema(&mut self, name: &str, owner: &str, block: BlockSchema) {
        if let Some(previous) = self.schema_file.get(name) {
            if previous != owner {
                self.manual(
                    "structure",
                    format!("schema `{name}` is produced by both {previous} and {owner}"),
                );
            }
            return;
        }
        self.schema_file.insert(name.to_owned(), owner.to_owned());
        self.file(owner)
            .schemas
            .insert(name.to_owned(), SchemaSpec::Block(block));
    }

    /// The definition kind a conditional definition refines: the profile
    /// `definitions` rule whose path selects the same documents.
    fn conditional_base_kind(
        &self,
        rule: &rules::ProfileConditionalDefinitionRule,
    ) -> Option<String> {
        let pattern = rule.path.pattern.trim_end_matches('/');
        if pattern.is_empty() {
            return None;
        }
        self.model
            .profile
            .definitions
            .iter()
            .find(|definition| {
                let candidate = definition.path.pattern.trim_end_matches('/');
                !candidate.is_empty()
                    && (candidate == pattern
                        || candidate.ends_with(pattern)
                        || pattern.ends_with(candidate))
            })
            .map(|definition| definition.kind.clone())
    }

    /// Two legacy rows can describe the same position once their child
    /// resolves to the same schema (the declarative and the warning-fallback
    /// row of a `date_field`, for instance). Identical patterns are one
    /// pattern; a lenient `severity` from the dropped duplicate is kept, since
    /// that row's whole point was to tolerate unmodelled keys.
    fn dedupe_patterns(&mut self, patterns: &mut Vec<FieldSpec>) {
        let mut merged: Vec<FieldSpec> = Vec::with_capacity(patterns.len());
        for spec in patterns.drain(..) {
            if spec.key.is_some()
                && let Some(existing) = merged.iter_mut().find(|existing| {
                    existing.key == spec.key
                        && existing.body == spec.body
                        && existing.when == spec.when
                        && existing.unless == spec.unless
                })
            {
                if spec.severity.is_some() && existing.severity.is_none() {
                    existing.severity = spec.severity;
                }
                self.count("duplicate patterns merged");
                continue;
            }
            merged.push(spec);
        }
        *patterns = merged;
    }

    fn scope_effect(&self, row: &SemanticRule) -> Option<ScopeEffect> {
        let scope_in: Vec<String> = row
            .allowed_scopes
            .iter()
            .filter(|scope| !scope.eq_ignore_ascii_case("any"))
            .map(|scope| self.norm.scope_name(scope))
            .collect();
        let mut set: BTreeMap<String, String> = BTreeMap::new();
        for (register, scope) in &row.replace_scope {
            set.insert(register.to_lowercase(), self.norm.scope_name(scope));
        }
        let push = row
            .push_scope
            .as_ref()
            .map(|scope| self.norm.scope_name(scope));
        if scope_in.is_empty() && push.is_none() && set.is_empty() {
            return None;
        }
        Some(ScopeEffect {
            scope_in: (!scope_in.is_empty()).then_some(scope_in),
            push,
            set: (!set.is_empty()).then_some(set),
        })
    }

    // -------------------------------------------------------------- sections

    fn collect_on_action_scopes(&mut self) {
        if let Some(entries) = self.model.semantic.type_root_scopes.get("on_action") {
            for (name, scope) in entries {
                self.on_action_scopes
                    .insert(name.clone(), (scope.root.clone(), Some(scope.from.clone())));
            }
        }
    }

    /// Folds `root:on_action` into one parameterised schema plus one enum
    /// (§6 of the design: 1,130 rows → `on_action_body<S>` + `on_actions`).
    fn convert_on_actions(&mut self) {
        let Some(rows) = self.by_context.get("root:on_action").cloned() else {
            return;
        };
        // Group rows by their per-action variance: identical rows collapse.
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        self.count_n("rows into on_action fold", rows.len());
        for index in rows {
            groups.entry(fold_key(&self.rows[index].rule)).or_default().push(index);
        }
        let mut body_fields: BTreeMap<String, FieldOverloads> = BTreeMap::new();
        for (key, members) in &groups {
            if members.len() > 1 {
                self.count_n("rows folded into on_action_body<S>", members.len() - 1);
            }
            let representative = members[0];
            let row = self.rows[representative].rule.clone();
            let path = path_of(&row);
            if path.first().is_some_and(|segment| {
                segment == "events" || segment == "random_events"
            }) {
                // Covered by the design's `events` / `random_events` fields.
                continue;
            }
            if matches!(row.shape, RuleShape::ValueClause) {
                continue;
            }
            let spec = on_action_spec(&row, &self.norm);
            match &row.key {
                KeyMatcher::Exact(spelling) => {
                    let key = spelling.to_lowercase();
                    body_fields
                        .entry(key)
                        .or_insert_with(|| FieldOverloads::One(Box::new(spec)));
                }
                _ => {
                    self.manual(
                        "on-action",
                        format!(
                            "on-action leftover row with non-exact key `{}` has no shared-body \
                             encoding",
                            expr::key_expr(&row.key, &self.norm)
                        ),
                    );
                }
            }
            let _ = key;
        }
        // The design example shape for the two uniform families.
        let mut fields = BTreeMap::new();
        fields.insert(
            "events".to_owned(),
            FieldOverloads::One(Box::new(FieldSpec {
                list: Some("ref<event.$S>".to_owned()),
                card: "0..*".to_owned(),
                ..empty_spec()
            })),
        );
        fields.insert(
            "random_events".to_owned(),
            FieldOverloads::One(Box::new(FieldSpec {
                map: Some(MapSpec {
                    key: "int".to_owned(),
                    value: Some("ref<event.$S> | '0'".to_owned()),
                    body: None,
                }),
                ..empty_spec()
            })),
        );
        for (key, spec) in body_fields {
            fields.entry(key).or_insert(spec);
        }
        let body = BlockSchema {
            include: Vec::new(),
            fields,
            patterns: Vec::new(),
            items: None,
            open: false,
        };
        self.place_schema("on_action_body<S>", "events.json", body);
        // The file schema: enum-keyed map with the scope-column parameter,
        // plus the `starts_with` on-actions (`on_harmonized_*`) as a template
        // pattern.
        let mut file_patterns = vec![FieldSpec {
            key: Some("enum<on_actions>".to_owned()),
            body: Some("on_action_body<$key.scope>".to_owned()),
            ..empty_spec()
        }];
        if let Some(descriptor) = self.model.semantic.type_descriptors.get("on_action")
            && let Some(prefix) = &descriptor.starts_with
        {
            file_patterns.push(FieldSpec {
                key: Some(format!("'{}{{scalar}}'", expr::escape_literal(prefix))),
                body: Some("on_action_body<country>".to_owned()),
                ..empty_spec()
            });
            // Every `on_harmonized_*` action is country-scoped in the legacy
            // root-scope table, so the templated body parameter is `country`.
            self.count("starts_with on-actions folded into one pattern");
        }
        self.place_schema(
            "on_actions_file",
            "events.json",
            BlockSchema {
                include: Vec::new(),
                fields: BTreeMap::new(),
                patterns: file_patterns,
                items: None,
                open: false,
            },
        );
        self.file("events.json").files.insert(
            "on_actions".to_owned(),
            FileRule {
                path: "common/on_actions".to_owned(),
                ext: Some(ExtSpec::One("txt".to_owned())),
                file: None,
                strict: None,
                exclude: Vec::new(),
                parser: None,
                resolution: SourceFileResolution::ReplaceByPath,
                root: Some(RootSpec::Schema("on_actions_file".to_owned())),
            },
        );
        // The enum table: names from the root-key list, columns from the
        // root-scope table.
        let names = self
            .model
            .semantic
            .type_root_keys
            .get("on_action")
            .cloned()
            .unwrap_or_default();
        let mut rows: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for name in &names {
            let mut columns = BTreeMap::new();
            if let Some((scope, from)) = self.on_action_scopes.get(name).cloned() {
                columns.insert("scope".to_owned(), self.norm.scope_name(&scope));
                self.note_type_use("event", Some(&self.norm.scope_name(&scope)));
                if from.is_some() {
                    // The `from` column is dropped: grouping every row by all
                    // columns would monomorphise `on_action_body<S>` past the
                    // 64-instance cap (check 4).
                    self.count("on-action rows with a dropped `from` column");
                }
            }
            rows.insert(name.clone(), columns);
        }
        self.file("events.json").enums.insert(
            "on_actions".to_owned(),
            EnumSpec::Table {
                columns: BTreeMap::from([("scope".to_owned(), "scope_type?".to_owned())]),
                rows,
            },
        );
        // The `from` column stays dropped (the coverage counter above records
        // every affected row): carrying it would add a second parameter to
        // `on_action_body`, and the cap check sizes parameters by their
        // declared domain, not by the combinations the corpus uses, so the
        // `scope_type` domain squared exceeds the 64-instance cap. The loss is
        // that a body cannot bind the FROM register; §6 of the redesign notes
        // it under the phase-2 record.
    }

    fn convert_scopes(&mut self) {
        let profile = &self.model.profile;
        let registers: BTreeSet<&str> = ["root", "this", "prev", "from"].into_iter().collect();
        let mut types: Vec<String> = Vec::new();
        for name in &profile.scope_names {
            if name.eq_ignore_ascii_case("any") {
                continue;
            }
            if registers.contains(name.to_lowercase().as_str()) {
                continue;
            }
            let lowered = name.to_lowercase();
            if (lowered.starts_with("prev") || lowered.starts_with("from"))
                && !self.links.contains_key(name.as_str())
            {
                // Chain spellings (`prev_prev`, `previous`) stay registers.
                if !profile.scope_member_aliases.contains_key(name.as_str()) {
                    continue;
                }
            }
            if profile.scope_member_aliases.contains_key(name.as_str())
                || profile.dynamic_scope_prefixes.iter().any(|p| p == name)
            {
                continue;
            }
            types.push(name.clone());
        }
        let mut register_specs: BTreeMap<String, RegisterSpec> = BTreeMap::new();
        for name in ["root", "this", "prev", "from"] {
            register_specs.insert(
                name.to_owned(),
                RegisterSpec {
                    chain: (name == "prev" || name == "from").then_some(true),
                },
            );
        }
        let mut links: BTreeMap<String, LinkSpec> = BTreeMap::new();
        for (name, info) in &self.links {
            let mut from: Vec<String> = info.from.iter().cloned().collect();
            if from.is_empty() {
                from.push("any".to_owned());
            }
            links.insert(
                name.clone(),
                LinkSpec {
                    from,
                    to: if info.to.is_empty() {
                        "any".to_owned()
                    } else {
                        info.to.clone()
                    },
                },
            );
        }
        for (name, target) in &profile.scope_member_aliases {
            let entry = links.entry(name.clone()).or_insert_with(|| LinkSpec {
                from: vec!["any".to_owned()],
                to: self.norm.scope_name(target),
            });
            entry.to = self.norm.scope_name(target);
        }
        for prefix in &profile.dynamic_scope_prefixes {
            links.insert(
                format!("{prefix}:{{ref<{prefix}>}}"),
                LinkSpec {
                    from: vec!["any".to_owned()],
                    to: "any".to_owned(),
                },
            );
        }
        let compat = profile
            .scope_compatibilities
            .iter()
            .map(|entry| CompatSpec {
                actual: self.norm.scope_name(&entry.actual),
                expected: self.norm.scope_name(&entry.expected),
            })
            .collect();
        self.file("core/scopes.json").scopes = Some(ScopesSpec {
            types,
            registers: register_specs,
            links,
            compat,
        });
    }

    fn convert_enums(&mut self) {
        let mut enums: BTreeMap<String, EnumSpec> = BTreeMap::new();
        for (name, members) in &self.model.semantic.enum_values {
            if name == "on_actions" {
                // Replaced by the folded `on_actions` table in `events.json`.
                continue;
            }
            let mut merged = members.clone();
            if let Some(extra) = self.model.profile.enum_extra_members.get(name) {
                for member in extra {
                    if !merged.iter().any(|existing| existing == member) {
                        merged.push(member.clone());
                    }
                }
            }
            enums.insert(name.clone(), EnumSpec::Members(merged));
        }
        for (name, extra) in &self.model.profile.enum_extra_members {
            if name == "on_actions" {
                continue;
            }
            if !enums.contains_key(name) {
                enums.insert(name.clone(), EnumSpec::Members(extra.clone()));
            }
        }
        // Referenced enums with no legacy member list become empty stubs.
        let snapshot = serde_json::to_value(&self.files).expect("files serialize");
        let mut used = BTreeSet::new();
        collect_kind_refs(&snapshot, "enum<", &mut used);
        for name in used {
            if name == "on_actions" {
                continue;
            }
            if !enums.contains_key(&name) {
                self.manual(
                    "enums",
                    format!(
                        "enum `{name}` is referenced but the legacy corpus declares no members \
                         for it — emitted as an empty stub"
                    ),
                );
                enums.insert(name, EnumSpec::Members(Vec::new()));
            }
        }
        self.file("values/enums.json").enums = enums;
    }

    fn convert_traits(&mut self) {
        let mut traits: BTreeMap<String, TraitSpec> = BTreeMap::new();
        traits.insert(
            "Localised".to_owned(),
            serde_json::from_value(serde_json::json!({
                "params": {"name": "$", "desc": null},
                "bindings": {
                    "name": {"loc": "{name}", "required": true},
                    "desc": {"loc": "{desc}"}
                }
            }))
            .expect("trait deserializes"),
        );
        traits.insert(
            "HasIcon".to_owned(),
            serde_json::from_value(serde_json::json!({
                "params": {"sprite": "GFX_$"},
                "bindings": {"icon": {"sprite": "{sprite}"}}
            }))
            .expect("trait deserializes"),
        );
        traits.insert(
            "ModifierSource".to_owned(),
            serde_json::from_value(serde_json::json!({}))
                .expect("trait deserializes"),
        );
        traits.insert(
            "Callable".to_owned(),
            serde_json::from_value(serde_json::json!({
                "params": {"body": "schema"},
                "capabilities": ["replacement", "condition", "dynamic_key", "opaque_text"]
            }))
            .expect("trait deserializes"),
        );
        self.file("core/traits.json").traits = traits;
        self.manual(
            "trait-impl",
            "`ModifierSource` is emitted without the design's `requires: {include: \
             \"modifier_block\"}` because `semantic_context_inheritance` types carry modifier \
             fields in the body itself, not in a `modifier` sub-block — settle the final trait \
             contract in the manual pass"
                .to_owned(),
        );
        self.manual(
            "trait-impl",
            "`Callable` capabilities include `opaque_text` (the legacy usage flags set it); \
             the design example lists three capabilities"
                .to_owned(),
        );
    }

    fn convert_types(&mut self) {
        let mut types: BTreeMap<String, TypeSpec> = BTreeMap::new();
        let mut names: BTreeSet<String> = self.type_subtypes.keys().cloned().collect();
        names.extend(self.def_types.iter().map(|name| self.norm.type_name(name)));
        names.extend(self.model.profile.open_world_value_kinds.iter().cloned());
        names.extend(self.model.profile.closed_dynamic_kinds.iter().cloned());
        names.extend(self.model.profile.engine_set_flags.keys().cloned());
        names.extend(
            self.model
                .semantic
                .type_root_keys
                .keys()
                .map(|name| self.norm.type_name(name)),
        );
        // Every `ref<>`/`def<>` name that reached an expression must be
        // declared, whatever path rendered it.
        let mut referenced = BTreeSet::new();
        let snapshot = serde_json::to_value(&self.files).expect("files serialize");
        collect_kind_refs(&snapshot, "ref<", &mut referenced);
        collect_kind_refs(&snapshot, "def<", &mut referenced);
        for name in referenced {
            let (base, subtype) = split_subtype(&name);
            self.note_type_use(base, subtype);
            names.insert(base.to_owned());
        }
        for name in names {
            let canonical = self.norm.type_name(&name);
            let spec = types.entry(canonical.clone()).or_default();
            if game::eu4::resolved_symbol_kind(&canonical) {
                spec.resolution = Some(TypeResolution::Replace);
            }
            if self
                .model
                .profile
                .open_world_value_kinds
                .iter()
                .any(|kind| self.norm.type_name(kind) == canonical)
            {
                spec.open = Some(true);
            }
            if let Some(flags) = self.model.profile.engine_set_flags.get(&name) {
                spec.builtin = Some(flags.clone());
            }
            if let Some(subtypes) = self.type_subtypes.get(&name) {
                for subtype in subtypes {
                    spec.subtypes
                        .entry(subtype.clone())
                        .or_insert_with(SubtypeSpec::default);
                }
            }
        }
        // `conditional_definitions` → subtype `when` predicates (§4).
        //
        // The legacy rule does not replace a definition: it pushes a *second*
        // definition of its own kind for the same property, gated on nested
        // fields. In rules-v2 that is a subtype of the type the profile
        // already collects at that path, granted by a `when` predicate.
        for rule in &self.model.profile.conditional_definitions {
            let canonical = self.norm.type_name(&rule.kind);
            let base = self
                .conditional_base_kind(rule)
                .map(|kind| {
                    let kind = self.norm.type_name(&kind);
                    kind.split('.').next().unwrap_or(&kind).to_owned()
                })
                .unwrap_or_else(|| canonical.split('.').next().unwrap_or(&canonical).to_owned());
            let entry = types.entry(base).or_default();
            let subtype = canonical
                .split_once('.')
                .map_or_else(|| canonical.clone(), |(_, tail)| tail.to_owned());
            entry.subtypes.insert(
                subtype,
                SubtypeSpec {
                    when: Some(SubtypeCond(BTreeMap::from([
                        (
                            rule.required_field.clone(),
                            Some(expr::literal(&rule.required_value)),
                        ),
                        (rule.absent_field.clone(), None),
                    ]))),
                    trait_impls: BTreeMap::new(),
                },
            );
        }
        // Localisation / sprite bindings → `Localised` / `HasIcon` impls.
        for (binding_type, bindings) in group_bindings(&self.model.semantic.localisation_bindings) {
            let canonical = self.norm.type_name(&binding_type);
            let base = canonical.split('.').next().unwrap_or(&canonical).to_owned();
            let entry = types.entry(base.clone()).or_default();
            let mut args: BTreeMap<String, String> = BTreeMap::new();
            let mut subtype_args: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
            for binding in bindings {
                let param = match binding.name.as_str() {
                    "name" => Some("name"),
                    "desc" | "description" => Some("desc"),
                    _ => None,
                };
                let Some(param) = param else {
                    self.manual(
                        "trait-impl",
                        format!(
                            "localisation binding `{}/{}` has no `Localised` parameter — \
                             extend the trait or fold it by hand",
                            binding_type, binding.name
                        ),
                    );
                    continue;
                };
                let Some(key) = &binding.key else {
                    self.manual(
                        "trait-impl",
                        format!(
                            "localisation binding `{}/{}` is field-sourced (`field`), which the \
                             trait-argument form cannot express",
                            binding_type, binding.name
                        ),
                    );
                    continue;
                };
                if binding.condition.is_some() {
                    self.manual(
                        "trait-impl",
                        format!(
                            "localisation binding `{}/{}` carries a structural `condition` \
                             (key_prefix) that trait arguments cannot express",
                            binding_type, binding.name
                        ),
                    );
                }
                let target = match &binding.subtype {
                    Some(subtype) => {
                        self.note_type_use(&base, Some(subtype));
                        subtype_args.entry(subtype.clone()).or_default()
                    }
                    None => &mut args,
                };
                target.insert(param.to_owned(), key.clone());
            }
            if !subtype_args.is_empty() {
                self.manual(
                    "trait-impl",
                    format!(
                        "`{binding_type}`: subtype-scoped `Localised` bindings folded into one \
                         type-level impl (a type may impl one trait only once) — restore the \
                         subtype split in the manual pass if the trait model grows per-subtype \
                         arguments"
                    ),
                );
                for (_subtype, extra) in subtype_args {
                    for (param, value) in extra {
                        args.entry(param).or_insert(value);
                    }
                }
            }
            if !args.is_empty() {
                entry.trait_impls.insert("Localised".to_owned(), ImplSpec(args));
            }
        }
        for (binding_type, bindings) in group_bindings(&self.model.semantic.sprite_bindings) {
            let canonical = self.norm.type_name(&binding_type);
            let base = canonical.split('.').next().unwrap_or(&canonical).to_owned();
            let entry = types.entry(base).or_default();
            for binding in bindings {
                let Some(key) = &binding.key else {
                    self.manual(
                        "trait-impl",
                        format!(
                            "sprite binding `{}/{}` is field-sourced (`field`), which the \
                             trait-argument form cannot express",
                            binding_type, binding.name
                        ),
                    );
                    continue;
                };
                entry
                    .trait_impls
                    .entry("HasIcon".to_owned())
                    .or_insert_with(|| ImplSpec(BTreeMap::from([("sprite".to_owned(), key.clone())])));
            }
        }
        // `dynamic_definition` types → `Callable`.
        for (name, descriptor) in &self.model.semantic.type_descriptors {
            if let Some(dynamic) = &descriptor.dynamic_definition
                && dynamic.enabled
            {
                let canonical = self.norm.type_name(name);
                types
                    .entry(canonical)
                    .or_default()
                    .trait_impls
                    .insert(
                        "Callable".to_owned(),
                        ImplSpec(BTreeMap::from([(
                            "body".to_owned(),
                            self.root_schema_name(&dynamic.body_context),
                        )])),
                    );
            }
        }
        // `semantic_context_inheritance` → `ModifierSource` on `type:` rows.
        for (context, inherited) in &self.model.profile.semantic_context_inheritance {
            if let Some(name) = context.strip_prefix("type:")
                && inherited.iter().any(|target| target == "modifier")
            {
                let canonical = self.norm.type_name(name);
                types
                    .entry(canonical)
                    .or_default()
                    .trait_impls
                    .entry("ModifierSource".to_owned())
                    .or_insert_with(|| ImplSpec(BTreeMap::new()));
            }
        }
        self.file("core/types.json").types = types;
    }

    // ---------------------------------------------------------- files and defs

    /// Builds the `files` section and the def positions (§4: file categories
    /// and type descriptors → `files` + `types` + def locations).
    fn convert_files(&mut self) {
        // File entries from the legacy file-category catalog.
        let mut entries: BTreeMap<String, FileRule> = BTreeMap::new();
        let mut entry_key: BTreeMap<(String, Option<String>), String> = BTreeMap::new();
        for category in &self.model.file_categories {
            let matcher = &category.matcher;
            let (path, file) = match (&matcher.path_exact, &matcher.path_prefix, &matcher.path_suffix) {
                (Some(exact), _, _) => {
                    let (dir, name) = exact.rsplit_once('/').unwrap_or(("", exact));
                    (dir.to_owned(), Some(name.to_owned()))
                }
                (_, Some(prefix), _) => (prefix.clone(), None),
                (_, _, Some(suffix)) => match suffix.rsplit_once('/') {
                    Some((dir, name)) => (dir.to_owned(), Some(name.to_owned())),
                    None => (String::new(), Some(suffix.clone())),
                },
                _ => (String::new(), None),
            };
            let ext = match matcher.extensions.len() {
                0 => None,
                1 => Some(ExtSpec::One(matcher.extensions[0].clone())),
                _ => Some(ExtSpec::Many(matcher.extensions.clone())),
            };
            let exclude = matcher.path_exclude_prefixes.clone();
            if matcher.extensions.len() > 1 {
                self.count("file categories with extension lists");
            }
            let name = sanitize(&category.id);
            entry_key.insert((path.clone(), file.clone()), name.clone());
            entries.insert(
                name,
                FileRule {
                    path,
                    ext,
                    file,
                    strict: None,
                    exclude,
                    parser: Some(match category.parser {
                        rules::ParserKind::Script => rules::source::SourceParser::Script,
                        rules::ParserKind::Localisation => {
                            rules::source::SourceParser::Localisation
                        }
                        rules::ParserKind::Asset => rules::source::SourceParser::Asset,
                        rules::ParserKind::SyntaxOnly => rules::source::SourceParser::SyntaxOnly,
                    }),
                    resolution: match category.resolution {
                        rules::FileResolutionPolicy::ReplaceByRelativePath => {
                            rules::source::SourceFileResolution::ReplaceByPath
                        }
                        rules::FileResolutionPolicy::Merge => {
                            rules::source::SourceFileResolution::Merge
                        }
                        rules::FileResolutionPolicy::ReplaceDirectory => {
                            rules::source::SourceFileResolution::ReplaceDirectory
                        }
                    },
                    root: None,
                },
            );
        }
        // Descriptor-driven root synthesis.
        let descriptors: Vec<(String, rules::TypeDescriptor)> = self
            .model
            .semantic
            .type_descriptors
            .iter()
            .map(|(name, descriptor)| (name.clone(), descriptor.clone()))
            .collect();
        let mut entry_types: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, descriptor) in &descriptors {
            if name == "on_action" {
                // Handled by the `on_action` fold in `events.json`.
                continue;
            }
            let path = descriptor
                .path
                .as_deref()
                .unwrap_or_default()
                .strip_prefix("game/")
                .unwrap_or_default()
                .to_owned();
            let file = descriptor.path_file.clone();
            let key = (path.clone(), file.clone());
            let entry_name = entry_key.get(&key).cloned().unwrap_or_else(|| {
                let generated = sanitize(&format!(
                    "files_{}{}",
                    path.replace('/', "_"),
                    file.as_deref()
                        .map(|name| format!("_{name}"))
                        .unwrap_or_default()
                ));
                entry_key.insert(key, generated.clone());
                entries.insert(
                    generated.clone(),
                    FileRule {
                        path,
                        ext: descriptor
                            .path_extension
                            .clone()
                            .map(ExtSpec::One),
                        file,
                        strict: descriptor.path_strict.then_some(true),
                        exclude: Vec::new(),
                        parser: Some(rules::source::SourceParser::Script),
                        resolution: SourceFileResolution::ReplaceByPath,
                        root: None,
                    },
                );
                generated
            });
            entry_types.entry(entry_name).or_default().push(name.clone());
        }
        // Root schemas per entry.
        let mut roots: BTreeMap<String, String> = BTreeMap::new();
        for (entry_name, types) in &entry_types {
            let entry = entries.get(entry_name).expect("entry exists");
            let mut block = BlockSchema::default();
            let mut instance_root: Option<FieldSpec> = None;
            let mut root_name = format!("{}_file", sanitize(&entry.path.replace('/', "_")));
            for type_name in types {
                let descriptor = &self.model.semantic.type_descriptors[type_name];
                let canonical = self.norm.type_name(type_name);
                let body_name = self.type_body_schema(&canonical, descriptor);
                let def_name = def_name_of(descriptor, &self.model.profile);
                let def = DefSpec {
                    type_name: canonical.clone(),
                    name: def_name.name,
                    strip_prefix: def_name.strip_prefix,
                    strip_suffix: def_name.strip_suffix,
                };
                if self.tree_defs.contains(canonical.split('.').next().unwrap_or(&canonical)) {
                    continue;
                }
                if let Some(entries_context) = &descriptor.root_entries {
                    // The entry context already describes the file root; the
                    // def fields are patched below. A context the corpus never
                    // populated has no schema, so the entry falls through to
                    // the open root instead of referencing a missing one.
                    root_name = self.root_schema_name(&format!("root:{entries_context}"));
                    if self.schema_file.contains_key(&root_name) {
                        roots.insert(entry_name.clone(), root_name.clone());
                    }
                    continue;
                }
                if descriptor.type_per_file {
                    let mut spec = empty_spec();
                    spec.def = Some(DefSpec {
                        type_name: def.type_name.clone(),
                        name: def.name.clone(),
                        strip_prefix: def.strip_prefix.clone(),
                        strip_suffix: def.strip_suffix.clone(),
                    });
                    spec.body = Some(body_name.clone());
                    if let Some(scope) = self.type_root_scope_effect(&canonical.split('.').next().unwrap_or(&canonical).to_owned())
                    {
                        spec.scope = Some(scope);
                    }
                    if instance_root.is_none() {
                        instance_root = Some(spec);
                    } else {
                        self.manual(
                            "files",
                            format!(
                                "entry `{entry_name}`: more than one `type_per_file` type \
                                 ({types:?}); only the first becomes the file root"
                            ),
                        );
                    }
                    continue;
                }
                let placement = descriptor
                    .skip_root_paths
                    .iter()
                    .filter(|path| !path.iter().any(|segment| segment.eq_ignore_ascii_case("any")))
                    .cloned()
                    .collect::<Vec<_>>();
                let filter = descriptor.type_key_filter.clone();
                match (placement.is_empty(), filter) {
                    (true, Some((keys, false))) => {
                        // Instances are child blocks with these structural
                        // keys (named by `name_field`).
                        for key in keys {
                            let mut spec = empty_spec();
                            spec.def = Some(def.clone());
                            spec.body = Some(body_name.clone());
                            block.fields.insert(key, FieldOverloads::One(Box::new(spec)));
                        }
                    }
                    (true, _) => {
                        // Every child key names an instance.
                        block.patterns.push(FieldSpec {
                            key: Some(format!("def<{canonical}>")),
                            body: Some(body_name.clone()),
                            ..empty_spec()
                        });
                    }
                    (false, filter) => {
                        for wrapper in &placement {
                            let mut wrapper_block = BlockSchema::default();
                            match filter.clone() {
                                Some((keys, false)) => {
                                    for key in keys {
                                        let mut spec = empty_spec();
                                        spec.def = Some(def.clone());
                                        spec.body = Some(body_name.clone());
                                        wrapper_block
                                            .fields
                                            .insert(key, FieldOverloads::One(Box::new(spec)));
                                    }
                                }
                                _ => {
                                    wrapper_block.patterns.push(FieldSpec {
                                        key: Some(format!("def<{canonical}>")),
                                        body: Some(body_name.clone()),
                                        ..empty_spec()
                                    });
                                }
                            }
                            // Wrapper chain: root field → W0 → W1 → … → container.
                            let mut names = Vec::new();
                            for depth in 0..wrapper.len() {
                                names.push(format!(
                                    "{root_name}__{}",
                                    wrapper[..=depth]
                                        .iter()
                                        .map(|part| sanitize(part))
                                        .collect::<Vec<_>>()
                                        .join("_")
                                ));
                            }
                            self.place_schema(
                                names.last().expect("wrapper names"),
                                "core/files.json",
                                wrapper_block,
                            );
                            for depth in 0..wrapper.len().saturating_sub(1) {
                                let mut schema = BlockSchema::default();
                                let mut spec = empty_spec();
                                spec.body = Some(names[depth + 1].clone());
                                schema.fields.insert(
                                    wrapper[depth + 1].clone(),
                                    FieldOverloads::One(Box::new(spec)),
                                );
                                self.place_schema(&names[depth], "core/files.json", schema);
                            }
                            let mut spec = empty_spec();
                            spec.body = Some(names[0].clone());
                            block.fields.insert(
                                wrapper[0].clone(),
                                FieldOverloads::One(Box::new(spec)),
                            );
                        }
                    }
                }
            }
            if let Some(instance) = instance_root {
                roots.insert(entry_name.clone(), root_name.clone());
                if let Some(entry) = entries.get_mut(entry_name) {
                    entry.root = Some(RootSpec::Instance(Box::new(instance)));
                }
            } else if !block.fields.is_empty() || !block.patterns.is_empty() {
                roots.insert(entry_name.clone(), root_name.clone());
                self.place_or_merge_root(&root_name, "core/files.json", block);
            }
        }
        // `type_root_keys` def fields on entry-context schemas.
        let root_keys = self.model.semantic.type_root_keys.clone();
        let root_scopes = self.model.semantic.type_root_scopes.clone();
        for (type_name, keys) in &root_keys {
            if type_name == "on_action" {
                // The folded `on_actions` enum map owns these entries.
                continue;
            }
            let canonical = self.norm.type_name(type_name);
            let Some(descriptor) = self.model.semantic.type_descriptors.get(type_name) else {
                continue;
            };
            let Some(entries_context) = &descriptor.root_entries else {
                continue;
            };
            let schema_name = self.root_schema_name(&format!("root:{entries_context}"));
            let body_name = self.type_body_schema(&canonical, descriptor);
            let def_name = def_name_of(descriptor, &self.model.profile);
            for key in keys {
                let mut spec = empty_spec();
                let subtype = root_scopes
                    .get(type_name)
                    .and_then(|scopes| scopes.get(key))
                    .map(|scope| scope.root.clone())
                    .filter(|scope| !scope.eq_ignore_ascii_case("any"))
                    .map(|scope| self.norm.scope_name(&scope));
                if let Some(subtype) = &subtype {
                    self.note_type_use(&canonical, Some(subtype));
                }
                spec.def = Some(DefSpec {
                    type_name: match &subtype {
                        Some(subtype) => format!("{canonical}.{subtype}"),
                        None => canonical.clone(),
                    },
                    name: def_name.name.clone(),
                    strip_prefix: def_name.strip_prefix.clone(),
                    strip_suffix: def_name.strip_suffix.clone(),
                });
                spec.body = Some(body_name.clone());
                if let Some(scope) = root_scopes.get(type_name).and_then(|scopes| scopes.get(key)) {
                    let mut set = BTreeMap::new();
                    set.insert("root".to_owned(), self.norm.scope_name(&scope.root));
                    set.insert("this".to_owned(), self.norm.scope_name(&scope.this));
                    if !scope.from.eq_ignore_ascii_case("any") {
                        set.insert("from".to_owned(), self.norm.scope_name(&scope.from));
                    }
                    spec.scope = Some(ScopeEffect {
                        scope_in: None,
                        push: None,
                        set: Some(set),
                    });
                }
                self.patch_entry_field(&schema_name, key, spec);
            }
        }
        for (name, rule) in entries {
            self.file("core/files.json").files.insert(name, rule);
        }
        // A catalog entry that duplicates a dedicated entry (same path,
        // extension, and file name) would fight it for the same documents;
        // the dedicated entry wins and the redundant catalog entry is dropped.
        let mut dedicated: BTreeSet<(String, Option<String>, Option<String>)> = BTreeSet::new();
        for (output, file) in &self.files {
            if output == "core/files.json" {
                continue;
            }
            for rule in file.files.values() {
                if rule.root.is_some() {
                    dedicated.insert(file_entry_key(rule));
                }
            }
        }
        // Attach roots to entries.
        let files_section = &mut self.file("core/files.json").files;
        let mut needs_open_root = false;
        for (entry_name, root) in roots {
            if let Some(entry) = files_section.get_mut(&entry_name)
                && entry.root.is_none()
            {
                entry.root = Some(RootSpec::Schema(root));
            }
        }
        // D14: a `script` entry without a root validates nothing silently.
        // Entries the legacy corpus never described get one explicit open
        // schema, which says "structure unknown" rather than guessing.
        let redundant: Vec<String> = files_section
            .iter()
            .filter(|(_, rule)| rule.root.is_none() && dedicated.contains(&file_entry_key(rule)))
            .map(|(name, _)| name.clone())
            .collect();
        for name in redundant {
            files_section.remove(&name);
        }
        let parser_of = |entry: &FileRule| {
            entry
                .parser
                .unwrap_or(rules::source::SourceParser::Script)
        };
        for entry in files_section.values_mut() {
            if parser_of(entry) == rules::source::SourceParser::Script && entry.root.is_none() {
                entry.root = Some(RootSpec::Schema("open_script_file".to_owned()));
                needs_open_root = true;
            }
        }
        if needs_open_root {
            let mut block = BlockSchema::default();
            block.open = true;
            self.place_schema("open_script_file", "core/files.json", block);
        }
    }

    /// The body schema name of one type, creating an empty body when the
    /// corpus has no `root:{type}` context.
    fn type_body_schema(&mut self, canonical: &str, descriptor: &rules::TypeDescriptor) -> String {
        if let Some(body_context) = &descriptor.body_context {
            let name = self.root_schema_name(body_context);
            return name;
        }
        if let Some(dynamic) = &descriptor.dynamic_definition {
            return self.root_schema_name(&dynamic.body_context);
        }
        let root_context = format!("root:{}", canonical.split('.').next().unwrap_or(canonical));
        let name = self.root_schema_name(&root_context);
        if !self.schema_file.contains_key(&name) {
            let mut block = BlockSchema::default();
            if let Some(mixin) = self.type_supplement_mixin(canonical.split('.').next().unwrap_or(canonical)) {
                block.include.push(mixin);
            }
            self.place_schema(&name, "core/files.json", block);
        }
        name
    }

    fn type_root_scope_effect(&self, type_name: &str) -> Option<ScopeEffect> {
        let scopes = self.model.semantic.type_root_scopes.get(type_name)?;
        let scope = scopes.get("*")?;
        let mut set = BTreeMap::new();
        set.insert("root".to_owned(), self.norm.scope_name(&scope.root));
        set.insert("this".to_owned(), self.norm.scope_name(&scope.this));
        if !scope.from.eq_ignore_ascii_case("any") {
            set.insert("from".to_owned(), self.norm.scope_name(&scope.from));
        }
        Some(ScopeEffect {
            scope_in: None,
            push: None,
            set: Some(set),
        })
    }

    /// Replaces one exact field of an already-placed schema (def patching of
    /// entry-context rows such as `country_event`).
    fn patch_entry_field(&mut self, schema: &str, key: &str, spec: FieldSpec) {
        let Some(path) = self.schema_file.get(schema).cloned() else {
            self.manual(
                "files",
                format!("entry field `{key}`: schema `{schema}` was not generated"),
            );
            return;
        };
        let Some(file) = self.files.get_mut(&path) else {
            return;
        };
        let Some(SchemaSpec::Block(block)) = file.schemas.get_mut(schema) else {
            return;
        };
        let entry = block
            .fields
            .entry(key.to_owned())
            .or_insert_with(|| FieldOverloads::One(Box::new(empty_spec())));
        *entry = FieldOverloads::One(Box::new(spec));
    }

    // -------------------------------------------------------------- game.json

    fn convert_game_json(&mut self) {
        let profile = &self.model.profile;
        self.game_json = serde_json::json!({
            "game_id": profile.game_id,
            "source_encoding": profile.source_encoding,
            "scan_roots": profile.scan_roots,
            "scan_root_max_depths": profile.scan_root_max_depths,
            "scan_root_files": profile.scan_root_files,
            "scan_extensions": profile.scan_extensions,
            "scan_archive_roots": profile.scan_archive_roots,
            "scan_archive_extensions": profile.scan_archive_extensions,
            "scripted_localisation_directories": profile.scripted_localisation_directories,
            "fallback_keys": profile.fallback_keys,
            "hover_cards": profile.hover_cards,
        });
    }

    // ----------------------------------------------------------------- mixins

    /// Materialises the context-set mixins registered during schema building.
    fn materialise_mixins(&mut self) {
        let sets: Vec<Vec<String>> = self.mixin_contexts.iter().cloned().collect();
        for set in sets {
            let name = mixin_name(&set);
            let owner = self.output_for_context(&set[0]);
            let mut fields: BTreeMap<String, FieldOverloads> = BTreeMap::new();
            for context in &set {
                self.flatten_root_fields(context, &mut fields);
            }
            self.file(&owner)
                .mixins
                .insert(name, MixinSpec { fields });
        }
    }

    /// Copies one context's root fields (plus its inherited contexts') into a
    /// flattened field map.
    fn flatten_root_fields(&self, context: &str, into: &mut BTreeMap<String, FieldOverloads>) {
        let mut stack = vec![context.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.clone()) {
                continue;
            }
            if let Some(inherited) = self.model.profile.semantic_context_inheritance.get(&current) {
                for target in inherited {
                    stack.push(target.clone());
                }
            }
            let root_name = self.root_schema_name(&current);
            let Some(path) = self.schema_file.get(&root_name) else {
                continue;
            };
            let Some(Some(SchemaSpec::Block(block))) = self
                .files
                .get(path)
                .map(|file| file.schemas.get(&root_name))
            else {
                continue;
            };
            for (key, overloads) in &block.fields {
                match into.get_mut(key) {
                    Some(existing) => merge_overloads(existing, overloads),
                    None => {
                        into.insert(key.clone(), overloads.clone());
                    }
                }
            }
        }
    }

    // ------------------------------------------------------- emit and report

    fn write_manifest(&mut self) {
        let mut names: Vec<String> = self.files.keys().cloned().collect();
        names.sort();
        self.manifest_files = names;
    }

    fn emit(&self, out: &Path) -> Result<(), String> {
        std::fs::create_dir_all(out).map_err(|failure| format!("{}: {failure}", out.display()))?;
        for (path, file) in &self.files {
            let target = out.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|failure| format!("{}: {failure}", parent.display()))?;
            }
            let rendered = render_json(file);
            std::fs::write(&target, rendered)
                .map_err(|failure| format!("{}: {failure}", target.display()))?;
        }
        let game = out.join("game.json");
        let rendered = serde_json::to_string_pretty(&self.game_json)
            .map_err(|failure| format!("game.json: {failure}"))?;
        std::fs::write(&game, rendered + "\n")
            .map_err(|failure| format!("{}: {failure}", game.display()))?;
        let manifest = serde_json::json!({
            "game_id": self.model.game_id,
            "target_game_version": self.target_game_version,
            "files": self.manifest_files,
        });
        let rendered = serde_json::to_string_pretty(&manifest)
            .map_err(|failure| format!("manifest.json: {failure}"))?;
        std::fs::write(out.join("manifest.json"), rendered + "\n")
            .map_err(|failure| format!("manifest.json: {failure}"))
    }

    fn render_report(&self) -> String {
        let mut report = String::new();
        report.push_str("# rules-v2 migration report\n\n");
        report.push_str("Generated by `cargo run -p tools --bin rules-migrate`. Deterministic: \
re-running over an unchanged legacy tree reproduces this file byte for byte.\n\n");
        report.push_str("## Coverage\n\n| counter | rows |\n|---|---:|\n");
        let mut counters: Vec<(&String, &usize)> = self.counters.iter().collect();
        counters.sort();
        for (key, value) in counters {
            report.push_str(&format!("| {key} | {value} |\n"));
        }
        report.push_str("\n## Manual checklist\n\n");
        let mut categories: Vec<(&String, &Vec<String>)> = self.manual.iter().collect();
        categories.sort();
        for (category, items) in categories {
            report.push_str(&format!("### {category} ({})\n\n", items.len()));
            let mut sorted = items.clone();
            sorted.sort();
            sorted.dedup();
            for item in sorted {
                report.push_str(&format!("- {item}\n"));
            }
            report.push('\n');
        }
        report
    }

    fn summary(&self, report: &Path) -> String {
        format!(
            "rules-migrate: {} output file(s), {} schema(s), report written to {}",
            self.files.len(),
            self.schema_file.len(),
            report.display()
        )
    }
}

/// Collects every constructor name spelled in rendered expressions
/// (`ref<>`/`def<>` with `"ref<"`/`"def<"`, enums with `"enum<"`).
fn collect_kind_refs(value: &serde_json::Value, marker: &str, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::String(text) => {
            for (start, _) in text.match_indices(marker) {
                let head = start + marker.len();
                let Some(relative) = text.get(head..) else {
                    continue;
                };
                let Some(end) = relative.find('>') else {
                    continue;
                };
                let name = &relative[..end];
                if name.starts_with("impl ") || name.contains('$') {
                    continue;
                }
                out.insert(name.to_owned());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_kind_refs(item, marker, out);
            }
        }
        serde_json::Value::Object(map) => {
            for entry in map.values() {
                collect_kind_refs(entry, marker, out);
            }
        }
        _ => {}
    }
}

/// Render a `RuleFile` as canonical pretty JSON (nulls and empty containers
/// pruned).
fn render_json(file: &RuleFile) -> String {
    let value = serde_json::to_value(file).expect("file serializes");
    let pruned = prune(value);
    serde_json::to_string_pretty(&pruned).expect("value renders") + "\n"
}

fn prune(value: serde_json::Value) -> serde_json::Value {
    prune_inner(value, false, false)
}

/// `keep_nulls` marks the inside of a `params` / `when` object, where a `null`
/// member is a statement ("no default" / "the field must be absent") rather
/// than an omitted value.
fn prune_inner(value: serde_json::Value, in_enums: bool, keep_nulls: bool) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, entry) in map {
                // A `null` value carries meaning inside trait parameters and
                // subtype predicates, both as the container's own value
                // (`"when": null` = no predicate) and as a member of the
                // container (`{"legacy_equivalent": null}` = absent field).
                let container = key == "params" || key == "when";
                if entry.is_null() && !container && !keep_nulls {
                    continue;
                }
                let child_in_enums = in_enums || key == "enums";
                let pruned = prune_inner(entry, child_in_enums, container || keep_nulls);
                let empty_array = pruned.as_array().is_some_and(Vec::is_empty);
                let empty_object = pruned.as_object().is_some_and(serde_json::Map::is_empty);
                if empty_array && key != "from" && !in_enums {
                    continue;
                }
                if empty_object && is_container_key(&key) {
                    continue;
                }
                out.insert(key, pruned);
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| prune_inner(item, in_enums, keep_nulls))
                .collect(),
        ),
        other => other,
    }
}

/// Map-valued keys whose empty form means "nothing declared" and may be
/// omitted. Struct-shaped empties (`{}` register specs, empty schemas) are
/// meaningful and kept.
fn is_container_key(key: &str) -> bool {
    matches!(
        key,
        "fields"
            | "patterns"
            | "include"
            | "types"
            | "subtypes"
            | "impl"
            | "params"
            | "bindings"
            | "capabilities"
            | "registers"
            | "links"
            | "compat"
            | "rows"
            | "columns"
            | "files"
            | "schemas"
            | "mixins"
            | "enums"
            | "traits"
            | "scan_roots"
            | "scan_root_max_depths"
            | "scan_root_files"
            | "scan_extensions"
            | "scan_archive_roots"
            | "scan_archive_extensions"
            | "scripted_localisation_directories"
            | "fallback_keys"
            | "hover_cards"
    )
}

/// Def-name derivation shared by the synthesis paths.
struct DefName {
    name: Option<String>,
    strip_prefix: Option<String>,
    strip_suffix: Option<String>,
}

fn def_name_of(descriptor: &rules::TypeDescriptor, profile: &rules::GameProfile) -> DefName {
    let name = if let Some(field) = &descriptor.name_field {
        Some(format!("field:{field}"))
    } else if descriptor.name_from_file {
        Some("file".to_owned())
    } else {
        None
    };
    let mut strip_suffix = descriptor.name_strip_suffix.clone();
    if strip_suffix.is_none() {
        if let Some(rule) = profile
            .member_name_suffixes
            .iter()
            .find(|rule| rule.kinds.iter().any(|kind| kind == &descriptor.name))
        {
            strip_suffix = Some(rule.suffix.clone());
        }
    }
    DefName {
        name,
        strip_prefix: descriptor.name_strip_prefix.clone(),
        strip_suffix,
    }
}

enum FieldOutcome {
    /// Folded into `scopes.links`.
    FoldedLink,
    /// Folded into the `scopes.registers` chain semantics.
    FoldedRegister,
    Specs(Vec<FieldGroup>),
}

/// Whether a key spelling names a scope register (including chain forms).
fn is_register_name(name: &str) -> bool {
    let lowered = name.to_lowercase();
    matches!(lowered.as_str(), "root" | "this" | "prev" | "from")
        || lowered.starts_with("prev")
        || lowered.starts_with("from")
}

// ---------------------------------------------------------------------------- helpers

fn pos_key(path: &[String]) -> String {
    path.join("\u{1}")
}

fn split_pos(key: &str) -> Vec<String> {
    if key.is_empty() {
        Vec::new()
    } else {
        key.split('\u{1}').map(str::to_owned).collect()
    }
}

fn parent_key(key: &str) -> String {
    match key.rsplit_once('\u{1}') {
        Some((head, _)) => head.to_owned(),
        None => String::new(),
    }
}

fn last_segment(key: &str) -> &str {
    key.rsplit('\u{1}').next().unwrap_or(key)
}

fn join_key(path: &[String], segment: &str) -> Vec<String> {
    let mut joined = path.to_vec();
    joined.push(segment.to_owned());
    joined
}

fn path_of(rule: &SemanticRule) -> Vec<String> {
    rule.parent_path
        .iter()
        .map(|segment| segment.to_lowercase())
        .collect()
}

fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            out.push(character);
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// Orders patterns from most to least specific (§3.2 fall-through).
fn order_patterns(patterns: &mut [FieldSpec]) {
    patterns.sort_by_key(|pattern| {
        let rank = match pattern.key.as_deref() {
            None => 5,
            Some(key) if key == "link" => 6,
            Some(key) if key == "scalar" => 5,
            Some(key) if key.starts_with("def<") => 4,
            Some(key) if key.starts_with("int") || key == "date" => 3,
            Some(key) if key.starts_with("ref<") || key.starts_with("enum<") => 1,
            Some(key) if key.starts_with("'") => 0,
            _ => 2,
        };
        (rank, pattern.key.clone().unwrap_or_default())
    });
}

fn split_subtype(name: &str) -> (&str, Option<&str>) {
    match name.split_once('.') {
        Some((base, subtype)) => (base, Some(subtype)),
        None => (name, None),
    }
}

/// Identity of a `files` entry, for redundant-catalog-entry detection.
fn file_entry_key(rule: &FileRule) -> (String, Option<String>, Option<String>) {
    (
        rule.path.clone(),
        rule.ext
            .as_ref()
            .map(|ext| ext.iter().collect::<Vec<_>>().join("|")),
        rule.file.clone(),
    )
}

fn mixin_name(contexts: &[String]) -> String {
    let mut name = String::from("keys__");
    for (index, context) in contexts.iter().enumerate() {
        if index > 0 {
            name.push('_');
        }
        name.push_str(&sanitize(context));
    }
    name
}

fn empty_spec() -> FieldSpec {
    FieldSpec {
        key: None,
        value: None,
        body: None,
        list: None,
        map: None,
        // Overwritten by every construction path that knows the real arity;
        // the placeholder is the corpus-neutral `0..1`.
        card: "0..1".to_owned(),
        scope: None,
        def: None,
        when: None,
        unless: None,
        control: None,
        doc: None,
        severity: None,
        deprecated: None,
        override_field: None,
    }
}

/// Flattens one field map into another, merging same-key overloads.
fn merge_overloads(existing: &mut FieldOverloads, incoming: &FieldOverloads) {
    let mut combined: Vec<FieldSpec> = match &*existing {
        FieldOverloads::One(spec) => vec![(**spec).clone()],
        FieldOverloads::Many(list) => list.clone(),
    };
    match incoming {
        FieldOverloads::One(spec) => {
            if !combined.iter().any(|item| item == spec.as_ref()) {
                combined.push((**spec).clone());
            }
        }
        FieldOverloads::Many(list) => {
            for spec in list {
                if !combined.iter().any(|item| item == spec) {
                    combined.push(spec.clone());
                }
            }
        }
    }
    *existing = if combined.len() == 1 {
        FieldOverloads::One(Box::new(combined.pop().expect("one spec")))
    } else {
        FieldOverloads::Many(combined)
    };
}

fn card_of(rule: &SemanticRule) -> String {
    let min = rule.min_occurs.unwrap_or(0);
    match (min, rule.max_occurs) {
        // D14: `card` is mandatory, so the former implicit `0..1` default is
        // written out like every other value.
        (0, Some(1)) => "0..1".to_owned(),
        (1, Some(1)) => "1".to_owned(),
        (m, Some(n)) => format!("{m}..{n}"),
        (0, None) => "0..*".to_owned(),
        (1, None) => "1..*".to_owned(),
        (m, None) => format!("{m}..*"),
    }
}

fn control_of(rule: &SemanticRule) -> Option<ControlSpec> {
    let KeyMatcher::Exact(key) = &rule.key else {
        return None;
    };
    let spec = |kind: ControlKind,
                guard: Option<&str>,
                chain: Option<Vec<&str>>,
                op: Option<&str>,
                on: Option<&str>| {
        ControlSpec {
            kind,
            guard: guard.map(str::to_owned),
            chain: chain.map(|links| links.into_iter().map(str::to_owned).collect()),
            op: op.map(str::to_owned),
            on: on.map(str::to_owned),
        }
    };
    match key.to_lowercase().as_str() {
        "if" => Some(spec(
            ControlKind::Branch,
            Some("limit"),
            Some(vec!["else_if", "else"]),
            None,
            None,
        )),
        "else_if" => Some(spec(
            ControlKind::BranchContinue,
            Some("limit"),
            None,
            None,
            None,
        )),
        "else" => Some(spec(ControlKind::BranchContinue, None, None, None, None)),
        "limit" => Some(spec(ControlKind::Guard, None, None, None, None)),
        "and" => Some(spec(ControlKind::Logic, None, None, Some("AND"), None)),
        "or" => Some(spec(ControlKind::Logic, None, None, Some("OR"), None)),
        "not" => Some(spec(ControlKind::Logic, None, None, Some("NOT"), None)),
        "random_list" => Some(spec(ControlKind::Weighted, None, None, None, None)),
        "random" => Some(spec(ControlKind::Chance, None, None, None, None)),
        "trigger_switch" => Some(spec(
            ControlKind::Switch,
            None,
            None,
            None,
            Some("on_trigger"),
        )),
        "hidden_effect" => Some(spec(ControlKind::Transparent, None, None, None, None)),
        "tooltip" => Some(spec(ControlKind::DisplayOnly, None, None, None, None)),
        _ => None,
    }
}

/// The deduplication key: everything but provenance and documentation.
fn dedup_key(rule: &SemanticRule) -> String {
    let mut value = serde_json::to_value(rule).expect("rule serializes");
    if let Some(object) = value.as_object_mut() {
        for key in ["id", "alternative_id", "source_file", "line", "documentation"] {
            object.remove(key);
        }
    }
    serde_json::to_string(&value).expect("rule serializes")
}

/// The on-action fold key: the per-action event subtype becomes `$S`.
fn fold_key(rule: &SemanticRule) -> String {
    let mut value = serde_json::to_value(rule).expect("rule serializes");
    if let Some(object) = value.as_object_mut() {
        for key in ["id", "alternative_id", "source_file", "line", "documentation"] {
            object.remove(key);
        }
        if let Some(ValueMatcher::Type(name)) = Some(&rule.value)
            && name.starts_with("event")
            && let Some(value) = object.get_mut("value")
        {
            *value = serde_json::json!({"type": "event.$S"});
        }
    }
    serde_json::to_string(&value).expect("rule serializes")
}

/// Renders one folded on-action row as a shared body field.
fn on_action_spec(rule: &SemanticRule, norm: &Norm) -> FieldSpec {
    let mut spec = empty_spec();
    if let KeyMatcher::Exact(spelling) = &rule.key {
        spec.key = None;
        let _ = spelling;
    }
    match rule.shape {
        RuleShape::LeafValue => {
            spec.list = Some(expr::value_expr(&rule.value, norm));
        }
        RuleShape::ValueClause => {
            spec.list = Some("ref<event.$S>".to_owned());
        }
        _ => {
            if expr::is_exact(&rule.key) {
                if let KeyMatcher::Exact(spelling) = &rule.key {
                    spec.value = Some(expr::value_expr(&rule.value, norm));
                    let _ = spelling;
                }
            } else {
                spec.map = Some(MapSpec {
                    key: expr::key_expr(&rule.key, norm),
                    value: Some(expr::value_expr(&rule.value, norm)),
                    body: None,
                });
            }
        }
    }
    spec.card = card_of(rule);
    spec.scope = None;
    spec
}

/// Groups bindings by their type spelling.
fn group_bindings(bindings: &[rules::SymbolBinding]) -> Vec<(String, Vec<&rules::SymbolBinding>)> {
    let mut groups: BTreeMap<String, Vec<&rules::SymbolBinding>> = BTreeMap::new();
    for binding in bindings {
        groups
            .entry(binding.type_name.clone())
            .or_default()
            .push(binding);
    }
    groups.into_iter().collect()
}

/// Legacy fragment path → output file path.
fn fragment_out_path(fragment: &str) -> String {
    match fragment {
        "semantic/contexts/trigger.json" => "core/trigger.json".to_owned(),
        "semantic/contexts/effect.json" => "core/effect.json".to_owned(),
        "semantic/contexts/modifier.json" => "core/modifier.json".to_owned(),
        "semantic/contexts/on-action.json" => "events.json".to_owned(),
        "semantic/contexts/special.json" => "core/special.json".to_owned(),
        other => other
            .strip_prefix("semantic/definitions/")
            .unwrap_or(other)
            .to_owned(),
    }
}
