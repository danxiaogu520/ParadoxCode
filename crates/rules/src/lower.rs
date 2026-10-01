//! Lowering: the rules-v2 source model → the runtime IR
//! (`docs/rules-redesign.md` §5.3 step 1: *check, expand, monomorphise,
//! index*).
//!
//! [`lower`] first runs the compile-time semantic checks ([`compile::check`])
//! and refuses to produce an IR while any of them is an error: the IR is a
//! *closed* arena, so it cannot represent an unresolved reference or an
//! unreachable overload. That refusal is what phase 5 makes the mandatory
//! precondition of `bake`.
//!
//! Everything the language defers to compile time happens here:
//!
//! - `include` expands mixins into flat exact-key maps (§7.2);
//! - a parameterised schema is monomorphised once per argument tuple, and a
//!   `map`/`pattern` whose `enum<E>` key feeds `$key.<column>` into its payload
//!   expands into one pattern per distinct column group, each carrying the
//!   group's row set (§3.3, §6);
//! - every mini-syntax string becomes an entry of one deduplicated matcher
//!   arena (§2.2);
//! - field provenance (source file + JSON pointer) is derived, never authored
//!   (§10.4).

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use rustc_hash::FxHashMap;

use crate::compile::{
    self, Actual, Diagnostic, SchemaRef, first_name, parse_card, parse_column, parse_def_type,
    parse_schema_key, parse_schema_ref,
};
use crate::expr::{self, Argument, Expr, LiteralPart, Param, Primary, ScalarKind, Segment};
use crate::ir::{
    Binding, BitSet, Card, Control, DefName, DefSpec, DocumentParser, EnumColumn, EnumId, EnumInfo,
    EnumRow, Field, FieldId, FieldValue, FileResolution, FileRule, GameConfig, Gate, Interner,
    LinkInfo, Matcher, MatcherId, Provenance, RefTarget, RegisterInfo, RootRule, RulesIr, Schema,
    SchemaId, ScopeEffect, ScopeModel, ScopeRef, SubtypeCond, SubtypeGate, SubtypeInfo, Symbol,
    TemplatePart, TraitArgument, TraitId, TraitImpl, TraitInfo, TypeId, TypeInfo, TypeResolution,
};
use crate::matcher::FileMatcher;
use crate::source::{
    ControlSpec as SourceControl, DefSpec as SourceDef, EnumSpec, FieldOverloads, FieldSpec,
    ImplSpec, ImplValue, MapSpec, MixinSpec, RootSpec, RuleFile, SchemaSpec, ScopesSpec, Severity,
    SourceFileResolution, SourceParser, SubtypeCond as SourceCond, TraitSpec,
    TypeResolution as SourceTypeResolution, TypeSpec,
};

/// Why lowering did not produce an IR.
#[derive(Debug)]
pub enum LowerError {
    /// The compile-time semantic checks reported errors; the IR would be
    /// ill-defined.
    Diagnostics(Vec<Diagnostic>),
}

impl LowerError {
    /// The errors that stopped lowering.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::Diagnostics(diagnostics) => diagnostics,
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Diagnostics(diagnostics) => {
                write!(
                    formatter,
                    "{} rule-source error(s) prevent lowering",
                    diagnostics.len()
                )
            }
        }
    }
}

impl std::error::Error for LowerError {}

/// Compiles merged rule sources into the runtime IR.
///
/// `sources` is `(source file name, parsed file)` in manifest order, and
/// `game` is the non-language `game.json` payload.
///
/// # Errors
///
/// Returns the error-severity diagnostics of [`compile::check`] when the
/// sources do not pass the compile-time semantic checks.
pub fn lower(sources: &[(String, RuleFile)], game: GameConfig) -> Result<RulesIr, LowerError> {
    let errors = compile::check(sources)
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(LowerError::Diagnostics(errors));
    }
    Ok(Lowering::new(sources, game).run())
}

/// Provenance of one source position.
#[derive(Clone, Debug)]
struct At {
    file: Symbol,
    pointer: String,
}

impl At {
    fn child(&self, segment: &str) -> Self {
        Self {
            file: self.file,
            pointer: format!("{}/{}", self.pointer, escape_pointer(segment)),
        }
    }

    fn index(&self, index: usize) -> Self {
        self.child(&index.to_string())
    }
}

/// Appends one JSON-pointer segment with `~0`/`~1` escaping.
fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// One declared schema, keyed by its base name.
#[derive(Clone)]
struct SchemaSource<'a> {
    spec: &'a SchemaSpec,
    formals: Vec<Symbol>,
    at: At,
}

/// The identity of one monomorphised schema instance: its declared base name
/// and its actual arguments (`None` for an argument the source left unbound).
type InstanceKey = (Symbol, Box<[Option<Symbol>]>);

/// One schema instance waiting to be expanded.
struct Job {
    schema: SchemaId,
    base: Symbol,
    formals: Vec<(Symbol, Option<Symbol>)>,
    at: At,
}

/// The formal parameters and `$key` binding in scope while lowering a field.
struct FieldContext<'c> {
    formals: &'c [(Symbol, Option<Symbol>)],
    key: Option<&'c KeyBinding>,
}

impl FieldContext<'static> {
    fn closed() -> Self {
        Self {
            formals: &[],
            key: None,
        }
    }
}

/// The `$key` binding of one generated enum-column group.
struct KeyBinding {
    /// Columns read through `$key.<column>`, in first-use order; `None` is a
    /// bare `$key` read, which stands for the matched row name.
    columns: Box<[Option<Symbol>]>,
    /// The group's values, aligned to `columns`.
    values: Box<[Option<Symbol>]>,
}

/// Where a draft field takes its key from.
#[derive(Clone, Copy)]
enum DraftKey<'d> {
    /// An exact `fields` key.
    Exact(&'d str),
    /// A `patterns` key type expression.
    Expression(&'d str),
}

/// One field specification about to be lowered.
struct FieldDraft<'d> {
    /// The schema the field belongs to, for `body: "self"` and instance gates.
    schema: SchemaId,
    spec: &'d FieldSpec,
    key: DraftKey<'d>,
    at: At,
}

/// A hashable fingerprint of a [`Field`] and its origin, so one declaration —
/// the same mixin contributing to a hundred schemas — is stored once.
///
/// Origin is part of the fingerprint, so two independently spelled but
/// structurally identical fields in different schemas keep their own
/// provenance and are *not* merged.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FieldKey {
    key: MatcherId,
    value: FieldValue,
    card: Card,
    scope: Option<ScopeEffect>,
    def: Option<DefSpec>,
    gate: Option<Gate>,
    control: Option<Control>,
    doc: Option<Symbol>,
    severity: Severity,
    deprecated: bool,
    file: Symbol,
    pointer: Symbol,
}

/// A hashable fingerprint of a [`Matcher`], for arena deduplication.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum MatcherKey {
    Scalar,
    Literal(Symbol),
    Template(Vec<TemplateKey>),
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        min: Option<u64>,
        max: Option<u64>,
    },
    Bool,
    Date,
    Loc,
    Path(Option<Symbol>),
    Ref(RefKey),
    Def {
        type_id: u32,
        subtype: Option<Symbol>,
    },
    Enum {
        id: u32,
        rows: Option<BitSet>,
    },
    Scope(Option<Symbol>),
    Link,
    Quoted(u32),
    Opaque,
    Union(Vec<u32>),
}

/// A hashable fingerprint of one [`TemplatePart`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateKey {
    Text(Symbol),
    Hole(u32),
}

/// A hashable fingerprint of a [`RefTarget`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RefKey {
    Type {
        type_id: u32,
        subtype: Option<Symbol>,
        strip_prefix: Option<Symbol>,
    },
    Trait(u32),
}

/// The lowering pass's working state.
struct Lowering<'a> {
    sources: &'a [(String, RuleFile)],
    game: GameConfig,
    strings: Interner,
    game_id: Symbol,

    schema_defs: BTreeMap<Symbol, SchemaSource<'a>>,
    mixins: BTreeMap<Symbol, (&'a MixinSpec, At)>,
    type_defs: Vec<(Symbol, &'a TypeSpec)>,
    file_defs: Vec<(Symbol, &'a crate::source::FileRule, At)>,
    scope_source: Option<(&'a ScopesSpec, At)>,

    type_ids: FxHashMap<Symbol, TypeId>,
    trait_ids: FxHashMap<Symbol, TraitId>,
    enum_ids: FxHashMap<Symbol, EnumId>,

    schemas: Vec<Schema>,
    fields: Vec<Field>,
    matchers: Vec<Matcher>,
    types: Vec<TypeInfo>,
    traits: Vec<TraitInfo>,
    enums: Vec<EnumInfo>,
    scopes: ScopeModel,
    provenance: Vec<Provenance>,
    files: Vec<FileRule>,

    instances: FxHashMap<InstanceKey, SchemaId>,
    jobs: VecDeque<Job>,
    pending_gates: Vec<(SchemaId, TypeId, Symbol)>,
    matcher_index: FxHashMap<MatcherKey, MatcherId>,
    field_index: FxHashMap<FieldKey, FieldId>,
}

impl<'a> Lowering<'a> {
    fn new(sources: &'a [(String, RuleFile)], game: GameConfig) -> Self {
        let identity = game.game_id().to_owned();
        let mut strings = Interner::default();
        let game_id = strings.intern_folded(&identity);
        Self {
            sources,
            game,
            strings,
            game_id,
            schema_defs: BTreeMap::new(),
            mixins: BTreeMap::new(),
            type_defs: Vec::new(),
            file_defs: Vec::new(),
            scope_source: None,
            type_ids: FxHashMap::default(),
            trait_ids: FxHashMap::default(),
            enum_ids: FxHashMap::default(),
            schemas: Vec::new(),
            fields: Vec::new(),
            matchers: Vec::new(),
            types: Vec::new(),
            traits: Vec::new(),
            enums: Vec::new(),
            scopes: ScopeModel::default(),
            provenance: Vec::new(),
            files: Vec::new(),
            instances: FxHashMap::default(),
            jobs: VecDeque::new(),
            pending_gates: Vec::new(),
            matcher_index: FxHashMap::default(),
            field_index: FxHashMap::default(),
        }
    }

    fn run(mut self) -> RulesIr {
        self.register_names();
        self.register_types();
        self.fill_types();
        self.fill_scopes();
        self.register_files();
        self.drain_jobs();
        while !self.pending_gates.is_empty() {
            let gates = std::mem::take(&mut self.pending_gates);
            for (schema, type_id, subtype) in gates {
                self.attach_gate(schema, type_id, subtype);
            }
            self.drain_jobs();
        }
        self.assemble()
    }

    fn assemble(mut self) -> RulesIr {
        RulesIr::new(
            self.game_id,
            std::mem::take(&mut self.files),
            std::mem::take(&mut self.schemas),
            std::mem::take(&mut self.fields),
            std::mem::take(&mut self.matchers),
            std::mem::take(&mut self.types),
            std::mem::take(&mut self.traits),
            std::mem::take(&mut self.enums),
            std::mem::take(&mut self.scopes),
            std::mem::take(&mut self.strings),
            std::mem::take(&mut self.provenance),
            self.game,
        )
    }

    // ---- registration ----------------------------------------------------

    /// Merges the named sections of every source into one namespace each.
    fn register_names(&mut self) {
        let sources = self.sources;
        for (file_name, file) in sources {
            // A source file name is data, so it keeps its spelling.
            let file_symbol = self.strings.intern_verbatim(file_name);
            for (key, spec) in &file.schemas {
                let Ok((name, formals)) = parse_schema_key(key) else {
                    continue;
                };
                let base = self.strings.intern_folded(&name);
                let formals = formals
                    .iter()
                    .map(|formal| self.strings.intern_folded(formal))
                    .collect();
                self.schema_defs.entry(base).or_insert(SchemaSource {
                    spec,
                    formals,
                    at: At {
                        file: file_symbol,
                        pointer: format!("/schemas/{}", escape_pointer(key)),
                    },
                });
            }
            for (name, spec) in &file.traits {
                let symbol = self.strings.intern_folded(name);
                if self.trait_ids.contains_key(&symbol) {
                    continue;
                }
                let id = TraitId(u32::try_from(self.traits.len()).expect("trait overflow"));
                self.trait_ids.insert(symbol, id);
                let info = self.trait_info(symbol, spec);
                self.traits.push(info);
            }
            for (name, spec) in &file.enums {
                let symbol = self.strings.intern_folded(name);
                if self.enum_ids.contains_key(&symbol) {
                    continue;
                }
                let id = EnumId(u32::try_from(self.enums.len()).expect("enum overflow"));
                self.enum_ids.insert(symbol, id);
                let info = self.enum_info(symbol, spec);
                self.enums.push(info);
            }
            for (name, spec) in &file.mixins {
                let symbol = self.strings.intern_folded(name);
                let at = At {
                    file: file_symbol,
                    pointer: format!("/mixins/{}", escape_pointer(name)),
                };
                self.mixins.entry(symbol).or_insert((spec, at));
            }
            if let Some(scopes) = &file.scopes {
                self.scope_source.get_or_insert((
                    scopes,
                    At {
                        file: file_symbol,
                        pointer: "/scopes".to_owned(),
                    },
                ));
            }
            for (name, rule) in &file.files {
                self.file_defs.push((
                    self.strings.intern_folded(name),
                    rule,
                    At {
                        file: file_symbol,
                        pointer: format!("/files/{}", escape_pointer(name)),
                    },
                ));
            }
        }
    }

    fn trait_info(&mut self, name: Symbol, spec: &TraitSpec) -> TraitInfo {
        let mut params = Vec::with_capacity(spec.params.len());
        for (param, default) in &spec.params {
            let param = self.strings.intern_folded(param);
            let default = default
                .as_deref()
                .map(|text| self.strings.intern_verbatim(text));
            params.push((param, default));
        }
        let mut bindings = Vec::with_capacity(spec.bindings.len());
        for (binding, spec) in &spec.bindings {
            let binding = self.strings.intern_folded(binding);
            let spec = self.binding(spec);
            bindings.push((binding, spec));
        }
        let requires_include = spec
            .requires
            .as_ref()
            .and_then(|requires| requires.include.as_deref())
            .map(|include| self.strings.intern_folded(include));
        let mut capabilities = Vec::new();
        for capability in spec.capabilities.iter().flatten() {
            capabilities.push(self.strings.intern_verbatim(capability));
        }
        TraitInfo {
            name,
            params,
            bindings,
            requires_include,
            capabilities: capabilities.into_boxed_slice(),
        }
    }

    fn binding(&mut self, spec: &crate::source::BindingSpec) -> Binding {
        let loc = spec
            .loc
            .as_deref()
            .map(|text| self.strings.intern_verbatim(text));
        let sprite = spec
            .sprite
            .as_deref()
            .map(|text| self.strings.intern_verbatim(text));
        Binding {
            loc,
            sprite,
            required: spec.required.unwrap_or(false),
        }
    }

    fn enum_info(&mut self, name: Symbol, spec: &EnumSpec) -> EnumInfo {
        match spec {
            EnumSpec::Members(members) => {
                let mut rows = Vec::with_capacity(members.len());
                for member in members {
                    let member = self.strings.intern_folded(member);
                    rows.push(EnumRow {
                        name: member,
                        values: Box::new([]),
                    });
                }
                EnumInfo {
                    name,
                    columns: Box::new([]),
                    rows: rows.into_boxed_slice(),
                }
            }
            EnumSpec::Table { columns, rows } => {
                let mut declared = Vec::with_capacity(columns.len());
                for (column, kind) in columns {
                    let column = self.strings.intern_folded(column);
                    declared.push(EnumColumn {
                        name: column,
                        optional: matches!(parse_column(kind), Ok((_, true))),
                    });
                }
                let order = columns.keys().collect::<Vec<_>>();
                let mut lowered = Vec::with_capacity(rows.len());
                for (row, values) in rows {
                    let row = self.strings.intern_folded(row);
                    let mut cells = Vec::with_capacity(order.len());
                    for column in &order {
                        let value = values
                            .get(column.as_str())
                            .map(|value| self.strings.intern_folded(value));
                        cells.push(value);
                    }
                    lowered.push(EnumRow {
                        name: row,
                        values: cells.into_boxed_slice(),
                    });
                }
                EnumInfo {
                    name,
                    columns: declared.into_boxed_slice(),
                    rows: lowered.into_boxed_slice(),
                }
            }
        }
    }

    /// Assigns every type id, then fills the contents that need matchers.
    fn register_types(&mut self) {
        let sources = self.sources;
        for (_file_name, file) in sources {
            for (name, spec) in &file.types {
                let symbol = self.strings.intern_folded(name);
                if self.type_ids.contains_key(&symbol) {
                    continue;
                }
                let id = TypeId(u32::try_from(self.types.len()).expect("type overflow"));
                self.type_ids.insert(symbol, id);
                self.type_defs.push((symbol, spec));
                let mut builtin = Vec::new();
                for member in spec.builtin.iter().flatten() {
                    builtin.push(self.strings.intern_folded(member));
                }
                self.types.push(TypeInfo {
                    name: symbol,
                    resolution: match spec.resolution {
                        Some(SourceTypeResolution::Replace) => TypeResolution::Replace,
                        _ => TypeResolution::Independent,
                    },
                    subtypes: Vec::new(),
                    open: spec.open.unwrap_or(false),
                    builtin: builtin.into_boxed_slice(),
                    trait_impls: Vec::new(),
                });
            }
        }
    }

    fn fill_types(&mut self) {
        let defs = self.type_defs.clone();
        for (name, spec) in defs {
            let Some(type_id) = self.type_ids.get(&name).copied() else {
                continue;
            };
            let mut subtypes = Vec::with_capacity(spec.subtypes.len());
            for (subtype_name, subtype) in &spec.subtypes {
                let name = self.strings.intern_folded(subtype_name);
                let when = subtype
                    .when
                    .as_ref()
                    .map(|condition| self.lower_condition(&FieldContext::closed(), condition));
                let trait_impls = self.lower_trait_impls(&subtype.trait_impls);
                subtypes.push(SubtypeInfo {
                    name,
                    when,
                    trait_impls,
                });
            }
            let trait_impls = self.lower_trait_impls(&spec.trait_impls);
            let info = &mut self.types[type_id.index()];
            info.subtypes = subtypes;
            info.trait_impls = trait_impls;
        }
    }

    fn lower_trait_impls(&mut self, impls: &BTreeMap<String, ImplSpec>) -> Vec<TraitImpl> {
        let mut out = Vec::with_capacity(impls.len());
        for (trait_name, spec) in impls {
            let symbol = self.strings.intern_folded(trait_name);
            let Some(trait_id) = self.trait_ids.get(&symbol).copied() else {
                continue;
            };
            let mut arguments = Vec::with_capacity(spec.0.len());
            for (argument, value) in &spec.0 {
                let argument = self.strings.intern_folded(argument);
                let value = match value {
                    ImplValue::Text(text) => TraitArgument::Text(self.strings.intern_folded(text)),
                    ImplValue::Binding(binding) => TraitArgument::Binding(self.binding(binding)),
                };
                arguments.push((argument, value));
            }
            out.push(TraitImpl {
                trait_id,
                arguments,
            });
        }
        out
    }

    fn fill_scopes(&mut self) {
        let Some((spec, at)) = self.scope_source.clone() else {
            return;
        };
        let mut types = Vec::with_capacity(spec.types.len());
        for name in &spec.types {
            types.push(self.strings.intern_folded(name));
        }
        self.scopes.types = types.into_boxed_slice();
        let mut registers = Vec::with_capacity(spec.registers.len());
        for (name, register) in &spec.registers {
            let name = self.strings.intern_folded(name);
            registers.push(RegisterInfo {
                name,
                chain: register.chain.unwrap_or(false),
            });
        }
        self.scopes.registers = registers.into_boxed_slice();
        let mut compat = Vec::with_capacity(spec.compat.len());
        for entry in &spec.compat {
            let actual = self.strings.intern_folded(&entry.actual);
            let expected = self.strings.intern_folded(&entry.expected);
            compat.push((actual, expected));
        }
        self.scopes.compat = compat.into_boxed_slice();
        let mut links = Vec::with_capacity(spec.links.len());
        for (name, link) in &spec.links {
            let Ok(parts) = expr::parse_template(name) else {
                continue;
            };
            let pattern = self.lower_template(&FieldContext::closed(), &parts, &at);
            let mut from = Vec::with_capacity(link.from.len());
            for scope in &link.from {
                from.push(self.scope_ref(scope));
            }
            let to = self.scope_ref(&link.to);
            links.push(LinkInfo {
                name: self.strings.intern_verbatim(name),
                pattern,
                from: from.into_boxed_slice(),
                to,
            });
        }
        self.scopes.links = links.into_boxed_slice();
    }

    fn scope_ref(&mut self, name: &str) -> ScopeRef {
        if name == "any" {
            ScopeRef::Any
        } else {
            ScopeRef::Type(self.strings.intern_folded(name))
        }
    }

    // ---- file rules ------------------------------------------------------

    fn register_files(&mut self) {
        let defs = self.file_defs.clone();
        let mut files = Vec::with_capacity(defs.len());
        for (name, rule, _at) in defs {
            let parser = match rule.parser.unwrap_or(SourceParser::Script) {
                SourceParser::Script => DocumentParser::Script,
                SourceParser::Localisation => DocumentParser::Localisation,
                SourceParser::Asset => DocumentParser::Asset,
                SourceParser::SyntaxOnly => DocumentParser::SyntaxOnly,
            };
            let resolution = match rule.resolution {
                SourceFileResolution::ReplaceByPath => FileResolution::ReplaceByPath,
                SourceFileResolution::Merge => FileResolution::Merge,
                SourceFileResolution::ReplaceDirectory => FileResolution::ReplaceDirectory,
            };
            let matcher = FileMatcher {
                path_prefix: match (&rule.file, rule.path.as_str()) {
                    (Some(_), _) | (None, "") => None,
                    (None, path) => Some(path.to_owned()),
                },
                path_exact: rule.file.as_ref().map(|file| {
                    if rule.path.is_empty() {
                        file.clone()
                    } else {
                        format!("{}/{}", rule.path, file)
                    }
                }),
                extensions: rule
                    .ext
                    .iter()
                    .flat_map(|ext| ext.iter().map(ToOwned::to_owned))
                    .collect(),
                path_suffix: None,
                path_exclude_prefixes: rule.exclude.clone(),
                case_sensitive: false,
            };
            let root = self.lower_root(&rule.root);
            files.push(FileRule {
                name,
                matcher,
                parser,
                resolution,
                strict: rule.strict.unwrap_or(false),
                root,
            });
        }
        let strings = &self.strings;
        files.sort_by(|left, right| strings.resolve(left.name).cmp(strings.resolve(right.name)));
        self.files = files;
    }

    fn lower_root(&mut self, root: &Option<RootSpec>) -> RootRule {
        match root {
            Some(RootSpec::Schema(name)) => {
                let reference = SchemaRef {
                    name: name.clone(),
                    args: Vec::new(),
                };
                match self.ensure_instance(&FieldContext::closed(), &reference) {
                    Some(schema) => RootRule::Schema(schema),
                    None => RootRule::Opaque,
                }
            }
            Some(RootSpec::Instance(field)) => {
                let context = FieldContext::closed();
                let body = field
                    .body
                    .as_deref()
                    .and_then(|body| self.ensure_body(&context, body, None));
                let def = field.def.as_ref().and_then(|def| self.lower_def(def, body));
                RootRule::Instance { def, body }
            }
            None => RootRule::Opaque,
        }
    }

    // ---- instances -------------------------------------------------------

    fn drain_jobs(&mut self) {
        while let Some(job) = self.jobs.pop_front() {
            self.lower_instance(&job);
        }
    }

    /// Monomorphises one schema reference, queueing the instance when new.
    fn ensure_instance(
        &mut self,
        context: &FieldContext<'_>,
        reference: &SchemaRef,
    ) -> Option<SchemaId> {
        if reference.name == "self" {
            return None;
        }
        let base = self.strings.intern_folded(&reference.name);
        let source = self.schema_defs.get(&base)?.clone();
        let mut arguments = Vec::with_capacity(source.formals.len());
        for index in 0..source.formals.len() {
            let actual = reference
                .args
                .get(index)
                .and_then(|actual| self.resolve_actual(context, actual));
            arguments.push(actual);
        }
        Some(self.instance(base, arguments))
    }

    /// The schema one `body` string names, resolving `"self"` to `current`.
    fn ensure_body(
        &mut self,
        context: &FieldContext<'_>,
        body: &str,
        current: Option<SchemaId>,
    ) -> Option<SchemaId> {
        match parse_schema_ref(body) {
            Ok(reference) if reference.name == "self" => current,
            Ok(reference) => self.ensure_instance(context, &reference),
            Err(_) => None,
        }
    }

    fn instance(&mut self, base: Symbol, arguments: Vec<Option<Symbol>>) -> SchemaId {
        let key = (base, arguments.clone().into_boxed_slice());
        if let Some(existing) = self.instances.get(&key) {
            return *existing;
        }
        // `$unbound` is interned only when a source really left an argument
        // out, so its absence is a usable invariant on a valid corpus.
        let unbound = arguments
            .iter()
            .any(Option::is_none)
            .then(|| self.strings.intern_verbatim("$unbound"));
        let id = SchemaId(u32::try_from(self.schemas.len()).expect("schema overflow"));
        let rendered = match unbound {
            Some(symbol) => arguments
                .iter()
                .map(|argument| argument.unwrap_or(symbol))
                .collect(),
            None => arguments
                .iter()
                .map(|argument| argument.expect("a bound argument"))
                .collect(),
        };
        self.schemas.push(Schema {
            name: base,
            arguments: rendered,
            ..Schema::default()
        });
        self.instances.insert(key, id);
        let source = self.schema_defs.get(&base).cloned();
        let (formals, at) = match source {
            Some(source) => (source.formals, source.at),
            None => (
                Vec::new(),
                At {
                    file: self.game_id,
                    pointer: String::new(),
                },
            ),
        };
        let formals = formals.into_iter().zip(arguments).collect();
        self.jobs.push_back(Job {
            schema: id,
            base,
            formals,
            at,
        });
        id
    }

    fn lower_instance(&mut self, job: &Job) {
        let Some(source) = self.schema_defs.get(&job.base).cloned() else {
            return;
        };
        let context = FieldContext {
            formals: &job.formals,
            key: None,
        };
        let at = job.at.clone();
        let (exact, patterns, items) = match source.spec {
            SchemaSpec::Block(block) => {
                let exact = self.lower_exact_fields(&context, job.schema, block, &at);
                let mut patterns = Vec::new();
                for (index, spec) in block.patterns.iter().enumerate() {
                    patterns.extend(self.lower_pattern(
                        &context,
                        spec,
                        &at.child("patterns").index(index),
                        job.schema,
                    ));
                }
                let items = block
                    .items
                    .as_deref()
                    .map(|items| self.lower_expr_text(&context, items));
                (exact, patterns, items)
            }
            SchemaSpec::Map { map } => {
                let patterns = self.lower_map_into(&context, map, &at.child("map"), job.schema);
                (FxHashMap::default(), patterns.to_vec(), None)
            }
            SchemaSpec::List { list } => {
                let items = self.lower_expr_text(&context, list);
                (FxHashMap::default(), Vec::new(), Some(items))
            }
        };
        let schema = &mut self.schemas[job.schema.index()];
        schema.exact = exact;
        schema.patterns = patterns.into_boxed_slice();
        schema.items = items;
    }

    /// Lowers the exact `fields` of one block, expanding its `include` mixins
    /// first so the schema's own declaration overrides a mixin's (§7.2).
    fn lower_exact_fields(
        &mut self,
        context: &FieldContext<'_>,
        schema: SchemaId,
        block: &crate::source::BlockSchema,
        at: &At,
    ) -> FxHashMap<Symbol, Box<[FieldId]>> {
        let mut merged: Vec<(String, &FieldOverloads, At)> = Vec::new();
        for mixin_name in &block.include {
            let symbol = self.strings.intern_folded(mixin_name);
            let Some((mixin, mixin_at)) = self.mixins.get(&symbol).cloned() else {
                continue;
            };
            for (key, overloads) in &mixin.fields {
                merged.push((key.clone(), overloads, mixin_at.child("fields").child(key)));
            }
        }
        for (key, overloads) in &block.fields {
            let position = merged
                .iter()
                .position(|(existing, _, _)| existing.eq_ignore_ascii_case(key));
            let entry = (key.clone(), overloads, at.child("fields").child(key));
            match position {
                Some(index) => merged[index] = entry,
                None => merged.push(entry),
            }
        }
        let mut exact: FxHashMap<Symbol, Box<[FieldId]>> = FxHashMap::default();
        for (key, overloads, key_at) in merged {
            let symbol = self.strings.intern_folded(&key);
            let list: &[FieldSpec] = match overloads {
                FieldOverloads::One(field) => std::slice::from_ref(field.as_ref()),
                FieldOverloads::Many(fields) => fields.as_slice(),
            };
            let mut ids = Vec::with_capacity(list.len());
            for (index, spec) in list.iter().enumerate() {
                let mut spec_at = key_at.clone();
                if matches!(overloads, FieldOverloads::Many(_)) {
                    spec_at = spec_at.index(index);
                }
                let draft = FieldDraft {
                    schema,
                    spec,
                    key: DraftKey::Exact(&key),
                    at: spec_at,
                };
                ids.push(self.lower_field(context, &draft, None));
            }
            exact.insert(symbol, ids.into_boxed_slice());
        }
        exact
    }

    /// Lowers one `patterns` entry, expanding an enum-keyed field into one
    /// pattern per attribute-column group (§6).
    fn lower_pattern(
        &mut self,
        context: &FieldContext<'_>,
        spec: &FieldSpec,
        at: &At,
        schema: SchemaId,
    ) -> Vec<FieldId> {
        let Some(raw_key) = spec.key.as_deref() else {
            return Vec::new();
        };
        let parsed = expr::parse(raw_key).ok();
        if let Some(parsed) = &parsed
            && let Some(enum_id) = self.enum_of_key(parsed)
        {
            let columns = self.key_columns(spec);
            if !columns.is_empty()
                && let Some(groups) = self.group_rows(enum_id, &columns)
            {
                let mut out = Vec::with_capacity(groups.len());
                for (values, rows) in groups {
                    let binding = KeyBinding {
                        columns: columns.clone().into_boxed_slice(),
                        values: values.into_boxed_slice(),
                    };
                    let grouped = FieldContext {
                        formals: context.formals,
                        key: Some(&binding),
                    };
                    let key = self.intern_matcher(Matcher::Enum {
                        id: enum_id,
                        rows: Some(rows),
                    });
                    let draft = FieldDraft {
                        schema,
                        spec,
                        key: DraftKey::Expression(raw_key),
                        at: at.clone(),
                    };
                    out.push(self.lower_field(&grouped, &draft, Some(key)));
                }
                return out;
            }
        }
        let draft = FieldDraft {
            schema,
            spec,
            key: DraftKey::Expression(raw_key),
            at: at.clone(),
        };
        vec![self.lower_field(context, &draft, None)]
    }

    /// The enum a pattern key consists of, when the whole key is one
    /// `enum<E>` alternative.
    fn enum_of_key(&self, parsed: &Expr) -> Option<EnumId> {
        let [Primary::Enum(argument)] = parsed.alternatives.as_slice() else {
            return None;
        };
        let name = first_name(argument)?;
        let symbol = self.strings.lookup_folded(&name)?;
        self.enum_ids.get(&symbol).copied()
    }

    /// The `$key` columns a field's payload reads.
    fn key_columns(&mut self, spec: &FieldSpec) -> Vec<Option<Symbol>> {
        let mut out = Vec::new();
        for column in collect_key_columns(spec) {
            let column = column.map(|column| self.strings.intern_folded(&column));
            out.push(column);
        }
        out
    }

    /// The distinct argument tuples the enum rows form under `columns`, each
    /// with the rows carrying it.
    fn group_rows(
        &self,
        enum_id: EnumId,
        columns: &[Option<Symbol>],
    ) -> Option<Vec<(Vec<Option<Symbol>>, BitSet)>> {
        let info = &self.enums[enum_id.index()];
        let mut positions = Vec::with_capacity(columns.len());
        for column in columns {
            positions.push(match column {
                None => None,
                Some(name) => Some(info.column(*name)?),
            });
        }
        let mut groups: Vec<(Vec<Option<Symbol>>, BitSet)> = Vec::new();
        for (index, row) in info.rows.iter().enumerate() {
            let values = columns
                .iter()
                .zip(&positions)
                .map(|(column, position)| match (column, position) {
                    (None, _) => Some(row.name),
                    (Some(_), Some(position)) => row.values.get(*position).copied().flatten(),
                    (Some(_), None) => None,
                })
                .collect::<Vec<_>>();
            match groups.iter_mut().find(|(existing, _)| *existing == values) {
                Some((_, rows)) => rows.insert(index),
                None => {
                    let mut rows = BitSet::new(info.rows.len());
                    rows.insert(index);
                    groups.push((values, rows));
                }
            }
        }
        Some(groups)
    }

    /// Lowers one map into a single pattern field, for both the
    /// `{"map": …}` schema shorthand and a field's `map` payload.
    fn lower_map_into(
        &mut self,
        context: &FieldContext<'_>,
        map: &MapSpec,
        at: &At,
        schema: SchemaId,
    ) -> Box<[FieldId]> {
        let spec = FieldSpec {
            key: Some(map.key.clone()),
            value: map.value.clone(),
            body: map.body.clone(),
            list: None,
            map: None,
            card: "0..*".to_owned(),
            scope: None,
            def: None,
            when: None,
            unless: None,
            control: None,
            doc: None,
            severity: None,
            deprecated: None,
            override_field: None,
        };
        self.lower_pattern(context, &spec, at, schema)
            .into_boxed_slice()
    }

    // ---- fields ----------------------------------------------------------

    fn lower_field(
        &mut self,
        context: &FieldContext<'_>,
        draft: &FieldDraft<'_>,
        key_override: Option<MatcherId>,
    ) -> FieldId {
        let spec = draft.spec;
        let key = key_override.unwrap_or_else(|| match draft.key {
            DraftKey::Exact(text) => {
                let text = self.strings.intern_verbatim(text);
                self.intern_matcher(Matcher::Literal(text))
            }
            DraftKey::Expression(raw) => self.lower_expr_text(context, raw),
        });
        let value = if let Some(raw) = spec.value.as_deref() {
            let quoted = expr::parse(raw)
                .ok()
                .and_then(|parsed| self.single_quoted(context, &parsed));
            match quoted {
                Some(schema) => FieldValue::Quoted(schema),
                None => FieldValue::Scalar(self.lower_expr_text(context, raw)),
            }
        } else if let Some(body) = spec.body.as_deref() {
            // `self` stays a self-reference so the arena needs no
            // self-referential schema edge; `child` resolves it against the
            // schema the field was matched in.
            match parse_schema_ref(body) {
                Ok(reference) if reference.name == "self" => FieldValue::SelfBlock,
                Ok(reference) => match self.ensure_instance(context, &reference) {
                    Some(schema) => FieldValue::Block(schema),
                    None => FieldValue::Scalar(self.opaque()),
                },
                Err(_) => FieldValue::Scalar(self.opaque()),
            }
        } else if let Some(list) = spec.list.as_deref() {
            let items = self.lower_expr_text(context, list);
            let inline = self.inline_schema("$list");
            self.schemas[inline.index()].items = Some(items);
            FieldValue::Block(inline)
        } else if let Some(map) = spec.map.as_ref() {
            let inline = self.inline_schema("$map");
            let patterns = self.lower_map_into(context, map, &draft.at.child("map"), inline);
            self.schemas[inline.index()].patterns = patterns;
            FieldValue::Block(inline)
        } else {
            FieldValue::Scalar(self.opaque())
        };
        // A `self` body still belongs to this schema, which is what an
        // instance body's subtype gates attach to.
        let body = match value {
            FieldValue::Block(schema) | FieldValue::Quoted(schema) => Some(schema),
            FieldValue::SelfBlock => Some(draft.schema),
            FieldValue::Scalar(_) => None,
        };
        let shorthand = self.field_shorthand(context, draft, spec);
        let def = match (spec.def.as_ref(), shorthand) {
            (Some(source), _) => self.lower_def(source, body),
            (None, Some((type_id, subtype))) => {
                self.register_gates(body, type_id);
                Some(DefSpec {
                    type_id,
                    subtype,
                    name: DefName::Key,
                    strip_prefix: None,
                    strip_suffix: None,
                })
            }
            (None, None) => None,
        };
        let scope = self.scope_effect(spec.scope.as_ref());
        let gate = match (spec.when.as_deref(), spec.unless.as_deref()) {
            (Some(when), _) => Some(Gate::When(self.strings.intern_folded(when))),
            (None, Some(unless)) => Some(Gate::Unless(self.strings.intern_folded(unless))),
            (None, None) => None,
        };
        let control = spec.control.as_ref().map(|control| self.control(control));
        let doc = spec
            .doc
            .as_deref()
            .map(|doc| self.strings.intern_verbatim(doc));
        let field = Field {
            key,
            value,
            card: card(&spec.card),
            scope,
            def,
            gate,
            control,
            doc,
            severity: spec.severity.unwrap_or(Severity::Error),
            deprecated: spec.deprecated.unwrap_or(false),
        };
        let pointer = self.strings.intern_verbatim(&draft.at.pointer);
        let fingerprint = FieldKey {
            key: field.key,
            value: field.value,
            card: field.card,
            scope: field.scope.clone(),
            def: field.def.clone(),
            gate: field.gate,
            control: field.control.clone(),
            doc: field.doc,
            severity: field.severity,
            deprecated: field.deprecated,
            file: draft.at.file,
            pointer,
        };
        if let Some(existing) = self.field_index.get(&fingerprint) {
            return *existing;
        }
        let id = FieldId(u32::try_from(self.fields.len()).expect("field overflow"));
        self.fields.push(field);
        self.provenance.push(Provenance {
            field: id,
            file: draft.at.file,
            pointer,
        });
        self.field_index.insert(fingerprint, id);
        id
    }

    fn scope_effect(&mut self, source: Option<&crate::source::ScopeEffect>) -> Option<ScopeEffect> {
        let source = source?;
        let mut scopes_in = Vec::new();
        for name in source.scope_in.iter().flatten() {
            scopes_in.push(self.strings.intern_folded(name));
        }
        let push = source
            .push
            .as_deref()
            .map(|push| self.strings.intern_folded(push));
        let mut set = Vec::new();
        for (register, target) in source.set.iter().flatten() {
            let register = self.strings.intern_folded(register);
            let target = self.strings.intern_folded(target);
            set.push((register, target));
        }
        Some(ScopeEffect {
            scopes_in: scopes_in.into_boxed_slice(),
            push,
            set: set.into_boxed_slice(),
        })
    }

    fn control(&mut self, control: &SourceControl) -> Control {
        let guard = control
            .guard
            .as_deref()
            .map(|guard| self.strings.intern_folded(guard));
        let mut chain = Vec::new();
        for link in control.chain.iter().flatten() {
            chain.push(self.strings.intern_folded(link));
        }
        let op = control
            .op
            .as_deref()
            .map(|op| self.strings.intern_folded(op));
        let on = control
            .on
            .as_deref()
            .map(|on| self.strings.intern_folded(on));
        Control {
            kind: control.kind,
            guard,
            chain: chain.into_boxed_slice(),
            op,
            on,
        }
    }

    /// The `def<T>` shorthand of a field, from its key or payload expression.
    fn field_shorthand(
        &mut self,
        context: &FieldContext<'_>,
        draft: &FieldDraft<'_>,
        spec: &FieldSpec,
    ) -> Option<(TypeId, Option<Symbol>)> {
        if let DraftKey::Expression(raw) = draft.key
            && let Ok(parsed) = expr::parse(raw)
            && let Some(shorthand) = self.def_shorthand(context, &parsed)
        {
            return Some(shorthand);
        }
        for raw in [spec.value.as_deref(), spec.list.as_deref()] {
            if let Some(raw) = raw
                && let Ok(parsed) = expr::parse(raw)
                && let Some(shorthand) = self.def_shorthand(context, &parsed)
            {
                return Some(shorthand);
            }
        }
        None
    }

    fn lower_def(&mut self, source: &SourceDef, body: Option<SchemaId>) -> Option<DefSpec> {
        let Ok((type_name, subtype)) = parse_def_type(&source.type_name) else {
            return None;
        };
        let symbol = self.strings.intern_folded(&type_name);
        let type_id = self.type_ids.get(&symbol).copied()?;
        let subtype = subtype.map(|subtype| self.strings.intern_folded(&subtype));
        self.register_gates(body, type_id);
        let name = match source.name.as_deref().map(str::trim) {
            None | Some("key") => DefName::Key,
            Some("file") => DefName::File,
            Some(other) => match other.strip_prefix("field:") {
                Some(field) => DefName::Field(self.strings.intern_folded(field.trim())),
                None => DefName::Key,
            },
        };
        let strip_prefix = source
            .strip_prefix
            .as_deref()
            .map(|prefix| self.strings.intern_folded(prefix));
        let strip_suffix = source
            .strip_suffix
            .as_deref()
            .map(|suffix| self.strings.intern_folded(suffix));
        Some(DefSpec {
            type_id,
            subtype,
            name,
            strip_prefix,
            strip_suffix,
        })
    }

    /// Records one predicate per declared subtype of `type_id` on the schema
    /// used as the instance body of a `def` (§5).
    fn register_gates(&mut self, body: Option<SchemaId>, type_id: TypeId) {
        let Some(body) = body else {
            return;
        };
        for subtype in &self.types[type_id.index()].subtypes {
            self.pending_gates.push((body, type_id, subtype.name));
        }
    }

    /// The `def<T>` shorthand of an expression, if it declares one.
    fn def_shorthand(
        &mut self,
        context: &FieldContext<'_>,
        parsed: &Expr,
    ) -> Option<(TypeId, Option<Symbol>)> {
        for alternative in &parsed.alternatives {
            let Primary::Def(argument) = alternative else {
                continue;
            };
            let segments = argument.segments()?;
            return self.lower_type_path(context, segments);
        }
        None
    }

    /// A synthetic schema standing in for an inline `map`/`list` payload. Its
    /// name is reserved (`$…`), so it can never be reached by name.
    fn inline_schema(&mut self, name: &str) -> SchemaId {
        let id = SchemaId(u32::try_from(self.schemas.len()).expect("schema overflow"));
        let name = self.strings.intern_verbatim(name);
        self.schemas.push(Schema {
            name,
            ..Schema::default()
        });
        id
    }

    /// The schema of a single `quoted<…>` alternative, when the whole value is
    /// one.
    fn single_quoted(&mut self, context: &FieldContext<'_>, parsed: &Expr) -> Option<SchemaId> {
        let mut found = None;
        for alternative in &parsed.alternatives {
            let Primary::Quoted(argument) = alternative else {
                return None;
            };
            let name = first_name(argument)?;
            let reference = SchemaRef {
                name,
                args: Vec::new(),
            };
            let schema = self.ensure_instance(context, &reference)?;
            if found.replace(schema).is_some() {
                return None;
            }
        }
        found
    }

    // ---- expressions -----------------------------------------------------

    fn opaque(&mut self) -> MatcherId {
        self.intern_matcher(Matcher::Opaque)
    }

    fn intern_matcher(&mut self, matcher: Matcher) -> MatcherId {
        let key = matcher_key(&matcher);
        if let Some(existing) = self.matcher_index.get(&key) {
            return *existing;
        }
        let id = MatcherId(u32::try_from(self.matchers.len()).expect("matcher overflow"));
        self.matchers.push(matcher);
        self.matcher_index.insert(key, id);
        id
    }

    fn lower_expr_text(&mut self, context: &FieldContext<'_>, raw: &str) -> MatcherId {
        match expr::parse(raw) {
            Ok(parsed) => self.lower_expr(context, &parsed),
            Err(_) => self.opaque(),
        }
    }

    fn lower_expr(&mut self, context: &FieldContext<'_>, parsed: &Expr) -> MatcherId {
        if parsed.alternatives.len() == 1 {
            return self.lower_primary(context, &parsed.alternatives[0]);
        }
        let mut alternatives = Vec::with_capacity(parsed.alternatives.len());
        for alternative in &parsed.alternatives {
            alternatives.push(self.lower_primary(context, alternative));
        }
        self.intern_matcher(Matcher::Union(alternatives.into_boxed_slice()))
    }

    fn lower_primary(&mut self, context: &FieldContext<'_>, primary: &Primary) -> MatcherId {
        let matcher = match primary {
            Primary::Scalar { kind, range } => match kind {
                ScalarKind::Scalar => Matcher::Scalar,
                ScalarKind::Int => Matcher::Int {
                    min: int_bound(*range, |range| range.min),
                    max: int_bound(*range, |range| range.max),
                },
                ScalarKind::Float => Matcher::Float {
                    min: float_bound(*range, |range| range.min),
                    max: float_bound(*range, |range| range.max),
                },
                ScalarKind::Bool => Matcher::Bool,
                ScalarKind::Date => Matcher::Date,
                ScalarKind::Loc => Matcher::Loc,
                ScalarKind::Link => Matcher::Link,
                ScalarKind::Opaque => Matcher::Opaque,
            },
            Primary::Ref(argument) => self.lower_ref_argument(context, argument),
            Primary::Def(argument) => match argument.segments() {
                Some(segments) => match self.lower_type_path(context, segments) {
                    Some((type_id, subtype)) => Matcher::Def { type_id, subtype },
                    None => Matcher::Opaque,
                },
                None => Matcher::Opaque,
            },
            Primary::Enum(argument) => {
                let name = first_name(argument).unwrap_or_default();
                let symbol = self.strings.intern_folded(&name);
                match self.enum_ids.get(&symbol).copied() {
                    Some(id) => Matcher::Enum { id, rows: None },
                    None => Matcher::Opaque,
                }
            }
            Primary::Scope(argument) => {
                let name = first_name(argument).unwrap_or_default();
                if name == "any" {
                    Matcher::Scope(None)
                } else {
                    Matcher::Scope(Some(self.strings.intern_folded(&name)))
                }
            }
            Primary::Quoted(argument) => {
                let name = first_name(argument).unwrap_or_default();
                let reference = SchemaRef {
                    name,
                    args: Vec::new(),
                };
                match self.ensure_instance(context, &reference) {
                    Some(schema) => Matcher::Quoted(schema),
                    None => Matcher::Opaque,
                }
            }
            Primary::Path { category } => Matcher::Path(
                category
                    .as_deref()
                    .map(|category| self.strings.intern_folded(category)),
            ),
            Primary::Literal(parts) => {
                let mut lowered = Vec::with_capacity(parts.len());
                for part in parts {
                    lowered.push(match part {
                        LiteralPart::Text(text) => {
                            TemplatePart::Text(self.strings.intern_verbatim(text))
                        }
                        LiteralPart::Hole(hole) => {
                            TemplatePart::Hole(self.lower_expr(context, hole))
                        }
                    });
                }
                match lowered.as_slice() {
                    [] => Matcher::Literal(self.strings.intern_verbatim("")),
                    [TemplatePart::Text(text)] => Matcher::Literal(*text),
                    _ => Matcher::Template(lowered.into_boxed_slice()),
                }
            }
            Primary::Param(param) => match self.resolve_param(context, param) {
                Some(symbol) => match self.type_ids.get(&symbol).copied() {
                    Some(type_id) => Matcher::Ref(RefTarget::Type {
                        type_id,
                        subtype: None,
                        strip_prefix: None,
                    }),
                    None => Matcher::Opaque,
                },
                None => Matcher::Opaque,
            },
        };
        self.intern_matcher(matcher)
    }

    fn lower_ref_argument(&mut self, context: &FieldContext<'_>, argument: &Argument) -> Matcher {
        match argument {
            Argument::Trait(name) => {
                let symbol = self.strings.intern_folded(name);
                match self.trait_ids.get(&symbol).copied() {
                    Some(id) => Matcher::Ref(RefTarget::Trait(id)),
                    None => Matcher::Opaque,
                }
            }
            Argument::Path(segments) => match self.lower_type_path(context, segments) {
                Some((type_id, subtype)) => Matcher::Ref(RefTarget::Type {
                    type_id,
                    subtype,
                    strip_prefix: None,
                }),
                None => Matcher::Opaque,
            },
            Argument::Stripped {
                segments,
                strip_prefix,
            } => {
                let strip_prefix = self.strings.intern_folded(strip_prefix);
                match self.lower_type_path(context, segments) {
                    Some((type_id, subtype)) => Matcher::Ref(RefTarget::Type {
                        type_id,
                        subtype,
                        strip_prefix: Some(strip_prefix),
                    }),
                    None => Matcher::Opaque,
                }
            }
        }
    }

    /// Resolves a dotted type path, substituting formal parameters.
    fn lower_type_path(
        &mut self,
        context: &FieldContext<'_>,
        segments: &[Segment],
    ) -> Option<(TypeId, Option<Symbol>)> {
        let mut names = Vec::with_capacity(segments.len());
        for segment in segments {
            match segment {
                Segment::Name(name) => names.push(self.strings.intern_folded(name)),
                Segment::Param(param) => names.push(self.resolve_param(context, param)?),
            }
        }
        let type_id = self.type_ids.get(names.first()?).copied()?;
        Some((type_id, names.get(1).copied()))
    }

    /// The concrete name a `$name[.column]` parameter stands for.
    fn resolve_param(&mut self, context: &FieldContext<'_>, param: &Param) -> Option<Symbol> {
        if param.name == "key" {
            let key = context.key?;
            let column = self.strings.intern_folded(param.column.as_deref()?);
            let index = key
                .columns
                .iter()
                .position(|candidate| *candidate == Some(column))?;
            return key.values.get(index).copied().flatten();
        }
        let name = self.strings.intern_folded(&param.name);
        context
            .formals
            .iter()
            .find(|(formal, _)| *formal == name)
            .and_then(|(_, value)| *value)
    }

    fn resolve_actual(&mut self, context: &FieldContext<'_>, actual: &Actual) -> Option<Symbol> {
        match actual {
            Actual::Name(name) => Some(self.strings.intern_folded(name)),
            Actual::Param { name, column } => self.resolve_param(
                context,
                &Param {
                    name: name.clone(),
                    column: column.clone(),
                },
            ),
        }
    }

    fn lower_template(
        &mut self,
        context: &FieldContext<'_>,
        parts: &[LiteralPart],
        at: &At,
    ) -> Box<[TemplatePart]> {
        let _ = at;
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            lowered.push(match part {
                LiteralPart::Text(text) => TemplatePart::Text(self.strings.intern_verbatim(text)),
                LiteralPart::Hole(hole) => TemplatePart::Hole(self.lower_expr(context, hole)),
            });
        }
        lowered.into_boxed_slice()
    }

    // ---- subtypes --------------------------------------------------------

    fn lower_condition(
        &mut self,
        context: &FieldContext<'_>,
        condition: &SourceCond,
    ) -> SubtypeCond {
        let mut entries = Vec::with_capacity(condition.0.len());
        for (field, value) in &condition.0 {
            let field = self.strings.intern_folded(field);
            let matcher = value
                .as_deref()
                .map(|raw| self.lower_expr_text(context, raw));
            entries.push((field, matcher));
        }
        SubtypeCond(entries.into_boxed_slice())
    }

    /// Records the `when` predicate of one subtype on a schema used as the
    /// instance body of that subtype's type (§5).
    fn attach_gate(&mut self, schema: SchemaId, type_id: TypeId, subtype: Symbol) {
        let existing = self.schemas[schema.index()]
            .subtype_gates
            .iter()
            .any(|gate| gate.type_id == type_id && gate.subtype == subtype);
        if existing {
            return;
        }
        let source = self.types[type_id.index()]
            .subtypes
            .iter()
            .find(|candidate| candidate.name == subtype)
            .and_then(|candidate| candidate.when.clone());
        let Some(when) = source else {
            return;
        };
        let mut gates = self.schemas[schema.index()].subtype_gates.to_vec();
        gates.push(SubtypeGate {
            type_id,
            subtype,
            when,
        });
        self.schemas[schema.index()].subtype_gates = gates.into_boxed_slice();
    }
}

/// One bound of a numeric range.
fn int_bound(
    range: Option<expr::Range>,
    bound: fn(&expr::Range) -> Option<expr::Number>,
) -> Option<i64> {
    bound(&range?).map(|number| number.0 as i64)
}

/// One bound of a numeric range, as a float.
fn float_bound(
    range: Option<expr::Range>,
    bound: fn(&expr::Range) -> Option<expr::Number>,
) -> Option<f64> {
    bound(&range?).map(|number| number.0)
}

fn card(raw: &str) -> Card {
    match parse_card(raw) {
        Ok((min, max)) => Card { min, max },
        Err(_) => Card { min: 0, max: None },
    }
}

/// A hashable fingerprint of one matcher.
fn matcher_key(matcher: &Matcher) -> MatcherKey {
    match matcher {
        Matcher::Scalar => MatcherKey::Scalar,
        Matcher::Literal(text) => MatcherKey::Literal(*text),
        Matcher::Template(parts) => MatcherKey::Template(
            parts
                .iter()
                .map(|part| match part {
                    TemplatePart::Text(text) => TemplateKey::Text(*text),
                    TemplatePart::Hole(hole) => TemplateKey::Hole(hole.index() as u32),
                })
                .collect(),
        ),
        Matcher::Int { min, max } => MatcherKey::Int {
            min: *min,
            max: *max,
        },
        Matcher::Float { min, max } => MatcherKey::Float {
            min: min.map(f64::to_bits),
            max: max.map(f64::to_bits),
        },
        Matcher::Bool => MatcherKey::Bool,
        Matcher::Date => MatcherKey::Date,
        Matcher::Loc => MatcherKey::Loc,
        Matcher::Path(category) => MatcherKey::Path(*category),
        Matcher::Ref(target) => MatcherKey::Ref(match target {
            RefTarget::Type {
                type_id,
                subtype,
                strip_prefix,
            } => RefKey::Type {
                type_id: type_id.index() as u32,
                subtype: *subtype,
                strip_prefix: *strip_prefix,
            },
            RefTarget::Trait(trait_id) => RefKey::Trait(trait_id.index() as u32),
        }),
        Matcher::Def { type_id, subtype } => MatcherKey::Def {
            type_id: type_id.index() as u32,
            subtype: *subtype,
        },
        Matcher::Enum { id, rows } => MatcherKey::Enum {
            id: id.index() as u32,
            rows: rows.clone(),
        },
        Matcher::Scope(scope) => MatcherKey::Scope(*scope),
        Matcher::Link => MatcherKey::Link,
        Matcher::Quoted(schema) => MatcherKey::Quoted(schema.index() as u32),
        Matcher::Opaque => MatcherKey::Opaque,
        Matcher::Union(alternatives) => MatcherKey::Union(
            alternatives
                .iter()
                .map(|alternative| alternative.index() as u32)
                .collect(),
        ),
    }
}

/// The `$key` columns a field's payload reads, in first-use order.
fn collect_key_columns(spec: &FieldSpec) -> Vec<Option<String>> {
    fn push(columns: &mut Vec<Option<String>>, column: Option<&str>) {
        let column = column.map(ToOwned::to_owned);
        if !columns.contains(&column) {
            columns.push(column);
        }
    }

    fn from_schema_ref(columns: &mut Vec<Option<String>>, raw: &str) {
        let Ok(reference) = parse_schema_ref(raw) else {
            return;
        };
        for argument in &reference.args {
            if let Actual::Param { name, column } = argument
                && name == "key"
            {
                push(columns, column.as_deref());
            }
        }
    }

    fn from_expr(columns: &mut Vec<Option<String>>, parsed: &Expr) {
        for alternative in &parsed.alternatives {
            match alternative {
                Primary::Param(param) if param.name == "key" => {
                    push(columns, param.column.as_deref());
                }
                Primary::Ref(argument)
                | Primary::Def(argument)
                | Primary::Enum(argument)
                | Primary::Scope(argument)
                | Primary::Quoted(argument) => {
                    for segment in argument.segments().unwrap_or_default() {
                        if let Segment::Param(param) = segment
                            && param.name == "key"
                        {
                            push(columns, param.column.as_deref());
                        }
                    }
                }
                Primary::Literal(parts) => {
                    for part in parts {
                        if let LiteralPart::Hole(hole) = part {
                            from_expr(columns, hole);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut columns = Vec::new();
    if let Some(body) = spec.body.as_deref() {
        from_schema_ref(&mut columns, body);
    }
    for raw in [spec.value.as_deref(), spec.list.as_deref()] {
        if let Some(raw) = raw
            && let Ok(parsed) = expr::parse(raw)
        {
            from_expr(&mut columns, &parsed);
        }
    }
    if let Some(map) = spec.map.as_ref() {
        if let Some(body) = map.body.as_deref() {
            from_schema_ref(&mut columns, body);
        }
        for raw in [Some(map.key.as_str()), map.value.as_deref()] {
            if let Some(raw) = raw
                && let Ok(parsed) = expr::parse(raw)
            {
                from_expr(&mut columns, &parsed);
            }
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{RefTarget, ScalarFields, Shape};
    use text::LogicalPath;

    /// A miniature of the real `events` + `on_actions` + `decisions` sources:
    /// every construct phase 3 lowers specially appears here in the shape the
    /// migrated corpus spells it.
    const SAMPLE: &str = r#"{
      "files": {
        "events":     { "path": "events", "ext": "txt", "root": "event_file" },
        "on_actions": { "path": "common/on_actions", "ext": "txt",
                        "resolution": "replace-by-path", "root": "on_actions_file" },
        "decisions":  { "path": "decisions", "ext": "txt", "root": "decision_file" },
        "localisation": { "path": "localisation", "ext": "yml", "parser": "localisation" }
      },
      "enums": {
        "on_actions": {
          "columns": { "scope": "scope_type?" },
          "rows": {
            "on_startup": { "scope": "country" },
            "on_province_religion_converted": { "scope": "province" },
            "on_battle_won_unit": { "scope": "unit" }
          }
        }
      },
      "mixins": {
        "gated": { "fields": { "potential": { "body": "trigger", "card": "0..1" } } }
      },
      "schemas": {
        "event_file": { "fields": {
          "namespace":      { "value": "scalar", "card": "0..1" },
          "country_event":  { "def": { "type": "event.country", "name": "field:id" },
                              "scope": { "set": { "root": "country", "this": "country" } },
                              "body": "event_body", "card": "0..*" },
          "province_event": { "def": { "type": "event.province", "name": "field:id" },
                              "scope": { "set": { "root": "province", "this": "province" } },
                              "body": "event_body", "card": "0..*" }
        }},
        "event_body": { "include": ["gated"], "fields": {
          "id":                  { "value": "scalar", "card": "1" },
          "is_triggered_only":   { "value": "bool", "card": "0..1" },
          "mean_time_to_happen": { "body": "mtth", "card": "0..1", "unless": "triggered" },
          "option":              { "body": "event_option", "card": "0..*" },
          "desc": [ { "value": "loc", "card": "0..*" },
                    { "body": "conditional_desc", "card": "0..*" } ]
        }},
        "event_option":     { "fields": { "name": { "value": "loc", "card": "1" } } },
        "conditional_desc": { "fields": { "trigger": { "body": "trigger", "card": "0..1" },
                                          "desc":    { "value": "loc", "card": "1" } } },
        "mtth": { "include": ["gated"], "fields": { "months": { "value": "int[0..]", "card": "0..1" } } },
        "trigger": { "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ] },
        "effect":  { "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ] },
        "on_actions_file": { "patterns": [
          { "key": "enum<on_actions>", "body": "on_action_body<$key.scope>", "card": "0..1" },
          { "key": "'on_harmonized_{scalar}'", "body": "on_action_body<country>", "card": "0..1" }
        ]},
        "on_action_body<S>": { "fields": {
          "events":        { "list": "ref<event.$S>", "card": "0..*" },
          "random_events": { "map": { "key": "int", "value": "ref<event.$S> | '0'" }, "card": "0..1" }
        }},
        "decision_file": { "fields": {
          "country_decisions": { "map": { "key": "def<decision>", "body": "decision_body" },
                                 "card": "0..1" }
        }},
        "decision_body": { "fields": {
          "potential": { "body": "trigger", "card": "1" },
          "effect":    { "body": "effect", "card": "1" },
          "major":     { "value": "'yes'", "card": "0..1" },
          "color":     { "list": "int[0..]", "card": "0..1" }
        }}
      },
      "types": {
        "event": { "resolution": "replace", "subtypes": {
          "country": {},
          "province": {},
          "triggered": { "when": { "is_triggered_only": "'yes'" } }
        }},
        "event_target": { "open": true },
        "decision": { "impl": { "Localised": {
          "name": { "loc": "$_title", "required": true } } } },
        "scripted_effect": { "impl": { "Callable": { "body": "effect" } } }
      },
      "traits": {
        "Localised": {},
        "Callable": { "params": { "body": "schema" },
                      "capabilities": ["replacement", "condition"] }
      },
      "scopes": {
        "types": ["country", "province", "unit", "trade_node"],
        "registers": { "root": {}, "this": {}, "prev": { "chain": true } },
        "links": {
          "owner": { "from": ["province", "unit"], "to": "country" },
          "event_target:{ref<event_target>}": { "from": ["any"], "to": "any" }
        },
        "compat": [{ "actual": "trade_node", "expected": "province" }]
      }
    }"#;

    fn sample() -> Vec<(String, RuleFile)> {
        vec![(
            "events.json".to_owned(),
            serde_json::from_str(SAMPLE).expect("the sample parses"),
        )]
    }

    fn lower_sample() -> RulesIr {
        let mut game = GameConfig::default();
        game.profile.game_id = "eu4".to_owned();
        lower(&sample(), game).expect("the sample lowers")
    }

    fn symbol(ir: &RulesIr, text: &str) -> Symbol {
        ir.strings().lookup_folded(text).expect("interned")
    }

    /// The `event_body` schema's subtype predicates decide the subtype set from
    /// the instance body's direct scalar fields (§5).
    struct Body(Option<&'static str>);

    impl ScalarFields for Body {
        fn scalar(&self, key: &str) -> Option<&str> {
            if key == "is_triggered_only" {
                self.0
            } else {
                None
            }
        }
    }

    #[test]
    fn def_positions_are_collected_with_provenance() {
        let ir = lower_sample();
        let event = ir.type_by_name("event").expect("the event type");
        let event_file = ir.schema_by_name("event_file").expect("event_file");

        let country_event = ir
            .lookup(event_file, "country_event", Shape::Block)
            .next()
            .expect("the country_event def position");
        let def = ir.field(country_event).def.as_ref().expect("a def");
        assert_eq!(def.type_id, event);
        assert_eq!(def.subtype, Some(symbol(&ir, "country")));
        assert_eq!(def.name, DefName::Field(symbol(&ir, "id")));

        let provenance = ir.provenance_of(country_event).expect("provenance");
        assert_eq!(ir.strings().resolve(provenance.file), "events.json");
        assert_eq!(
            ir.strings().resolve(provenance.pointer),
            "/schemas/event_file/fields/country_event"
        );

        // A `def` inside a `map` key is a definition too, reached through the
        // synthetic map schema.
        let decision = ir.type_by_name("decision").expect("the decision type");
        let decision_file = ir.schema_by_name("decision_file").expect("decision_file");
        let wrapper = ir
            .lookup(decision_file, "country_decisions", Shape::Block)
            .next()
            .expect("the map wrapper");
        let map = ir.child(wrapper, decision_file).expect("the map schema");
        let entry = ir.schema(map).patterns[0];
        let def = ir.field(entry).def.as_ref().expect("a map def");
        assert_eq!(def.type_id, decision);
        assert_eq!(def.subtype, None);
        assert_eq!(def.name, DefName::Key);
        assert!(matches!(
            ir.matcher(ir.field(entry).key),
            Matcher::Def { .. }
        ));

        // `include` expanded the mixin, and the field keeps the mixin's origin.
        let potential = ir
            .lookup(
                ir.schema_by_name("event_body").expect("event_body"),
                "potential",
                Shape::Block,
            )
            .next()
            .expect("the mixin field");
        assert_eq!(
            ir.strings()
                .resolve(ir.provenance_of(potential).expect("provenance").pointer),
            "/mixins/gated/fields/potential"
        );
        // The same mixin contributing to a second schema shares one field.
        let mtth = ir.schema_by_name("mtth").expect("mtth");
        assert_eq!(
            ir.lookup(mtth, "potential", Shape::Block).next(),
            Some(potential)
        );
    }

    #[test]
    fn subtype_predicates_decide_the_subtype_set() {
        let ir = lower_sample();
        let event = ir.type_by_name("event").expect("the event type");
        let event_body = ir.schema_by_name("event_body").expect("event_body");
        let triggered = symbol(&ir, "triggered");

        let gates = ir.schema(event_body).subtype_gates.clone();
        assert_eq!(gates.len(), 1, "only `triggered` has a `when`");
        assert_eq!(gates[0].type_id, event);
        assert_eq!(gates[0].subtype, triggered);

        let holds = ir.subtypes_of(event_body, &Body(Some("yes")));
        assert!(holds.contains(event, triggered));
        assert_eq!(holds.len(), 1);
        assert!(ir.subtypes_of(event_body, &Body(Some("no"))).is_empty());
        assert!(ir.subtypes_of(event_body, &Body(None)).is_empty());

        // The `unless: triggered` gate excludes the field exactly when the
        // subtype holds.
        let field = ir
            .lookup(event_body, "mean_time_to_happen", Shape::Block)
            .next()
            .expect("the gated field");
        assert_eq!(ir.field(field).gate, Some(Gate::Unless(triggered)));
        assert!(!ir.fields(event_body, &holds).contains(&field));
        assert!(
            ir.fields(event_body, &ir.subtypes_of(event_body, &Body(None)))
                .contains(&field)
        );
    }

    #[test]
    fn enum_column_groups_monomorphise_the_body() {
        let ir = lower_sample();
        let on_actions = ir.enum_by_name("on_actions").expect("the on_actions enum");
        let file = ir
            .schema_by_name("on_actions_file")
            .expect("on_actions_file");

        let patterns = ir.schema(file).patterns.clone();
        assert_eq!(patterns.len(), 4, "three scope groups plus one template");
        let mut scopes = Vec::new();
        for id in patterns {
            let field = ir.field(id);
            let Matcher::Enum {
                id: enum_id,
                rows: Some(rows),
            } = ir.matcher(field.key)
            else {
                continue;
            };
            assert_eq!(*enum_id, on_actions);
            assert_eq!(rows.len(), 1, "one group per distinct scope value");
            let FieldValue::Block(body) = field.value else {
                panic!("a grouped pattern has a block body");
            };
            let schema = ir.schema(body);
            assert_eq!(ir.strings().resolve(schema.name), "on_action_body");
            assert_eq!(schema.arguments.len(), 1);
            scopes.push(ir.strings().resolve(schema.arguments[0]).to_owned());
        }
        // Groups follow the first row carrying each tuple; rows sort by name.
        scopes.sort();
        assert_eq!(scopes, vec!["country", "province", "unit"]);

        // `$S` was substituted in the monomorphised body.
        let event = ir.type_by_name("event").expect("the event type");
        let country = symbol(&ir, "country");
        let body = ir
            .schema_instances("on_action_body")
            .find(|id| ir.schema(*id).arguments.as_ref() == [country])
            .expect("the country instance");
        let events = ir
            .lookup(body, "events", Shape::Block)
            .next()
            .expect("the events field");
        let list = ir.child(events, body).expect("the list schema");
        let items = ir.schema(list).items.expect("the element matcher");
        assert!(matches!(
            ir.matcher(items),
            Matcher::Ref(RefTarget::Type { type_id, subtype, .. })
                if *type_id == event && *subtype == Some(country)
        ));
        let random = ir
            .lookup(body, "random_events", Shape::Block)
            .next()
            .expect("the random_events field");
        let map = ir.child(random, body).expect("the map schema");
        let entry = ir.schema(map).patterns[0];
        let FieldValue::Scalar(value) = ir.field(entry).value else {
            panic!("a map value is scalar");
        };
        let Matcher::Union(alternatives) = ir.matcher(value) else {
            panic!("`ref<event.$S> | '0'` stays a union");
        };
        assert_eq!(alternatives.len(), 2);
        assert!(matches!(
            ir.matcher(alternatives[1]),
            Matcher::Literal(text) if ir.strings().resolve(*text) == "0"
        ));
    }

    #[test]
    fn link_patterns_stay_self_blocks_and_resolve_scopes() {
        let ir = lower_sample();
        let province = symbol(&ir, "province");
        let unit = symbol(&ir, "unit");
        let country = symbol(&ir, "country");

        let trigger = ir.schema_by_name("trigger").expect("trigger");
        let pattern = ir.schema(trigger).patterns[0];
        assert!(matches!(ir.matcher(ir.field(pattern).key), Matcher::Link));
        assert_eq!(ir.field(pattern).value, FieldValue::SelfBlock);
        assert_eq!(ir.child(pattern, trigger), Some(trigger));

        let owner = ir
            .scopes
            .link(ir.strings(), symbol(&ir, "owner"))
            .expect("owner");
        assert_eq!(
            owner.from.to_vec(),
            vec![ScopeRef::Type(province), ScopeRef::Type(unit)]
        );
        assert_eq!(owner.to, ScopeRef::Type(country));
        assert!(!owner.is_template());
        assert!(ir.scope_matches(Some(country), "owner"));
        assert!(!ir.scope_matches(Some(province), "owner"));

        let template = ir
            .scopes
            .links
            .iter()
            .find(|link| link.is_template())
            .expect("the event_target template link");
        assert_eq!(
            ir.strings().resolve(template.name),
            "event_target:{ref<event_target>}"
        );
        let [TemplatePart::Text(prefix), TemplatePart::Hole(hole)] = template.pattern.as_ref()
        else {
            panic!("the template is text + one hole");
        };
        assert_eq!(ir.strings().resolve(*prefix), "event_target:");
        assert!(matches!(
            ir.matcher(*hole),
            Matcher::Ref(RefTarget::Type { type_id, .. })
                if Some(*type_id) == ir.type_by_name("event_target")
        ));
    }

    #[test]
    fn lookup_dispatches_by_shape() {
        let ir = lower_sample();
        let event_body = ir.schema_by_name("event_body").expect("event_body");

        // `desc` carries two overloads of different shapes (§3.2).
        let scalar = ir
            .lookup(event_body, "desc", Shape::Scalar)
            .collect::<Vec<_>>();
        let block = ir
            .lookup(event_body, "desc", Shape::Block)
            .collect::<Vec<_>>();
        assert_eq!(scalar.len(), 1);
        assert_eq!(block.len(), 1);
        assert_ne!(scalar[0], block[0]);
        assert!(matches!(ir.field(scalar[0]).value, FieldValue::Scalar(_)));
        assert!(matches!(ir.field(block[0]).value, FieldValue::Block(_)));
        assert_eq!(ir.lookup(event_body, "desc", Shape::Quoted).count(), 0);
        assert_eq!(
            ir.lookup(event_body, "DESC", Shape::Scalar).count(),
            1,
            "keys are case-insensitive"
        );

        // A pattern answers only the shapes it describes.
        let trigger = ir.schema_by_name("trigger").expect("trigger");
        assert_eq!(ir.lookup(trigger, "owner", Shape::Block).count(), 1);
        assert_eq!(ir.lookup(trigger, "owner", Shape::Scalar).count(), 0);
        assert_eq!(
            ir.lookup(trigger, "owner", Shape::Block).next(),
            Some(ir.schema(trigger).patterns[0])
        );
    }

    #[test]
    fn file_rules_select_the_root_schema() {
        let ir = lower_sample();
        let events = LogicalPath::parse("events/1.txt").expect("a logical path");
        assert_eq!(
            ir.root_schema(&events),
            ir.schema_by_name("event_file"),
            "the events root is the event_file schema"
        );

        let on_actions = LogicalPath::parse("common/on_actions/x.txt").expect("a logical path");
        let (index, rule) = ir.file_rule(&on_actions).expect("an on_actions rule");
        assert_eq!(rule.resolution, FileResolution::ReplaceByPath);
        assert_eq!(
            ir.files[index].root,
            RootRule::Schema(ir.schema_by_name("on_actions_file").unwrap())
        );

        let localisation = LogicalPath::parse("localisation/x.yml").expect("a logical path");
        assert!(matches!(
            ir.root_rule(&localisation),
            Some(RootRule::Opaque)
        ));
        assert_eq!(ir.root_schema(&localisation), None);
    }

    #[test]
    fn lowering_refuses_erroring_sources() {
        let json = r#"{ "schemas": { "s": { "fields": {
          "a": { "value": "ref<missing>", "card": "0..1" } } } } }"#;
        let sources = vec![(
            "bad.json".to_owned(),
            serde_json::from_str(json).expect("parses"),
        )];
        let failure = lower(&sources, GameConfig::default()).expect_err("refuses to lower");
        assert!(failure.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == crate::compile::DiagnosticCode::UndefinedReference
        }));
    }

    /// The real first-party corpus lowers, and its key constructs survive. The
    /// test is skipped when the sources are not checked out beside the crate.
    #[test]
    fn the_first_party_corpus_lowers() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules/eu4-v2");
        let Ok(manifest) = std::fs::read_to_string(directory.join("manifest.json")) else {
            eprintln!("rules/eu4-v2 is not checked out; skipping");
            return;
        };
        let manifest: serde_json::Value = serde_json::from_str(&manifest).expect("manifest parses");
        let mut sources = Vec::new();
        for name in manifest["files"].as_array().expect("a file list") {
            let name = name.as_str().expect("a file name");
            let text = std::fs::read_to_string(directory.join(name)).expect("a listed file");
            sources.push((
                name.to_owned(),
                serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}")),
            ));
        }
        let profile: crate::profile::GameProfile = serde_json::from_str(
            &std::fs::read_to_string(directory.join("game.json")).expect("game.json"),
        )
        .expect("game.json parses");
        let ir = lower(&sources, GameConfig { profile }).expect("rules/eu4-v2 lowers");

        assert_eq!(
            ir.files.len(),
            138,
            "every `files` entry survives, including the dedicated .gfx rule"
        );
        let (_, gfx) = ir
            .file_rule(&LogicalPath::parse("interface/test.gfx").unwrap())
            .expect("interface .gfx files are classified");
        assert!(
            matches!(gfx.root, RootRule::Schema(schema) if ir.strings().resolve(ir.schema(schema).name) == "sprite_file")
        );
        assert!(ir.schemas.len() > 1_000, "{} schemas", ir.schemas.len());
        assert!(
            (5_000..12_000).contains(&ir.fields.len()),
            "{} fields, near the source's own 6,080 + shared mixin fields",
            ir.fields.len()
        );
        assert!(
            ir.matchers.len() < 5_000,
            "the matcher arena deduplicates: {} matchers",
            ir.matchers.len()
        );
        assert_eq!(
            ir.provenance.len(),
            ir.fields.len(),
            "every field has one origin"
        );
        assert!(
            ir.strings().lookup_verbatim("$unbound").is_none(),
            "no instance argument is left unbound"
        );
        assert_eq!(
            ir.type_info(ir.type_by_name("event").expect("event"))
                .subtypes
                .len(),
            4
        );
        assert_eq!(
            ir.scopes.links.len(),
            100,
            "every declared scope link survives"
        );

        // `on_actions` monomorphises into one body per distinct `scope` column
        // value, each pattern carrying only its group's rows (§6).
        let on_actions = ir.enum_by_name("on_actions").expect("the on_actions enum");
        let file = ir
            .schema_by_name("on_actions_file")
            .expect("on_actions_file");
        let mut group_rows = std::collections::BTreeMap::new();
        let mut template_patterns = 0;
        for id in ir.schema(file).patterns.iter() {
            let field = ir.field(*id);
            let Matcher::Enum {
                id,
                rows: Some(rows),
            } = ir.matcher(field.key)
            else {
                template_patterns += 1;
                continue;
            };
            assert_eq!(*id, on_actions, "the group key stays the on_actions enum");
            let FieldValue::Block(body) = field.value else {
                panic!("a grouped pattern has a block body");
            };
            let schema = ir.schema(body);
            assert_eq!(ir.strings().resolve(schema.name), "on_action_body");
            group_rows.insert(
                ir.strings().resolve(schema.arguments[0]).to_owned(),
                rows.len(),
            );
        }
        assert_eq!(
            template_patterns, 1,
            "the on_harmonized template stays one pattern"
        );
        assert_eq!(
            group_rows,
            std::collections::BTreeMap::from([
                ("country".to_owned(), 174),
                ("mercenary_company".to_owned(), 1),
                ("province".to_owned(), 81),
                ("unit".to_owned(), 2),
            ]),
            "every on_actions row lands in exactly one group"
        );
        assert_eq!(ir.schema_instances("on_action_body").count(), 4);

        // Roots and scope links survive the corpus.
        assert!(
            ir.root_schema(&LogicalPath::parse("events/1.txt").unwrap())
                .is_some()
        );
        assert!(
            ir.root_schema(&LogicalPath::parse("decisions/a.txt").unwrap())
                .is_some()
        );
        let trigger = ir.schema_by_name("trigger").expect("the trigger schema");
        assert!(
            ir.schema(trigger)
                .patterns
                .iter()
                .any(|id| { matches!(ir.matcher(ir.field(*id).key), Matcher::Link) })
        );
        assert!(ir.scopes.links.iter().any(crate::ir::LinkInfo::is_template));
    }
}
