//! Compile-time semantic checks for rules-v2 sources
//! (`crates/rules/LANGUAGE.md` §10).
//!
//! [`check`] parses every mini-syntax string with provenance (source file +
//! JSON pointer + expression-internal column) and then runs the five
//! normative semantic checks of §10.1, plus the cross-source duplicate-name
//! rule of §1. Nothing here lowers to the runtime IR; the pass is the
//! `rulec check` payload.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::expr::{self, Expr, Param, Primary, Segment};
use crate::source::{
    EnumSpec, ExtSpec, FieldOverloads, FieldSpec, ImplSpec, ImplValue, MapSpec, MixinSpec,
    RootSpec, RuleFile, SchemaSpec, Severity, SourceParser, TraitSpec, TypeSpec,
};

/// One compile diagnostic: provenance plus the finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Error, warning, or info.
    pub severity: Severity,
    /// Which check (or parse) reported the finding.
    pub code: DiagnosticCode,
    /// Human-readable description.
    pub message: String,
    /// Source file name in the loaded rule bundle.
    pub file: String,
    /// JSON pointer of the offending value.
    pub pointer: String,
    /// 1-based column within the expression, for parse errors only.
    pub column: Option<usize>,
}

/// The closed set of `rulec` compile diagnostics (`crates/rules/LANGUAGE.md`
/// §11). These are rule-source diagnostics, distinct from the script
/// diagnostics in `crates/ide/DIAGNOSTICS.md`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticCode {
    /// A mini-syntax string failed to parse.
    Parse,
    /// A name is defined twice across sources.
    DuplicateName,
    /// Check 1: a referenced definition does not exist.
    UndefinedReference,
    /// Check 1: a definition is never referenced.
    UnusedDefinition,
    /// Check 2: an include conflict.
    IncludeConflict,
    /// Check 3: an overload or pattern is fully shadowed.
    UnreachableOverload,
    /// Check 4: a formal parameter out of position, more than one parameter
    /// level, or more than 64 instances.
    ParameterError,
    /// Check 5: a scope name or link `from` violation.
    ScopeReferenceError,
    /// D14 card syntax lint (`0..0`, fixed-length tuples, card disagreement).
    CardLint,
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Parse => "Parse",
            Self::DuplicateName => "DuplicateName",
            Self::UndefinedReference => "UndefinedReference",
            Self::UnusedDefinition => "UnusedDefinition",
            Self::IncludeConflict => "IncludeConflict",
            Self::UnreachableOverload => "UnreachableOverload",
            Self::ParameterError => "ParameterError",
            Self::ScopeReferenceError => "ScopeReferenceError",
            Self::CardLint => "CardLint",
        })
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let column = self
            .column
            .map_or_else(String::new, |column| format!(":{column}"));
        write!(
            formatter,
            "{}{}{}: {} {}: {}",
            self.file, self.pointer, column, self.severity, self.code, self.message
        )
    }
}

/// Runs parsing and the six semantic checks over merged rule sources.
///
/// `sources` is `(source file name, parsed file)` in normalized relative-path order. The
/// returned diagnostics are sorted by `(file, pointer, code)` so runs are
/// deterministic.
pub fn check(sources: &[(String, RuleFile)]) -> Vec<Diagnostic> {
    let mut checker = Checker::new(sources);
    checker.collect();
    checker.check_block_forms();
    checker.check_include_conflicts();
    checker.check_unreachable_overloads();
    checker.check_parameters();
    checker.check_scopes();
    checker.check_queries();
    checker.check_card_disagreement();
    checker.check_undefined_references();
    checker.check_unused_definitions();
    if !checker.queries.is_empty()
        && !checker
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        checker
            .diagnostics
            .extend(crate::lower::check_queries(sources));
    }
    let Checker {
        mut diagnostics, ..
    } = checker;
    diagnostics.sort_by(|left, right| {
        (&left.file, &left.pointer, left.code, &left.message).cmp(&(
            &right.file,
            &right.pointer,
            right.code,
            &right.message,
        ))
    });
    diagnostics
}

/// One definition's provenance.
#[derive(Clone, Debug)]
struct At {
    file: String,
    pointer: String,
}

impl At {
    fn child(&self, segment: &str) -> Self {
        Self {
            file: self.file.clone(),
            pointer: join_pointer(&self.pointer, segment),
        }
    }

    fn index(&self, index: usize) -> Self {
        self.child(&index.to_string())
    }
}

/// Appends one JSON-pointer segment with `~0`/`~1` escaping.
fn join_pointer(base: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{base}/{escaped}")
}

/// A schema reference spelled as a `body` string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SchemaRef {
    /// Schema name, or `"self"` for the enclosing schema.
    pub(crate) name: String,
    /// Actual arguments at the call site.
    pub(crate) args: Vec<Actual>,
}

/// One actual argument of a schema call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Actual {
    /// A concrete name.
    Name(String),
    /// `$name`: a formal parameter.
    Param {
        /// The formal parameter name.
        name: String,
    },
}

/// Where a definition lives and what it is called.
#[derive(Clone)]
struct Named<'a, T> {
    value: &'a T,
    at: At,
}

/// One schema, normalised over the full and shorthand forms.
#[derive(Clone)]
struct SchemaDef<'a> {
    formals: Vec<String>,
    at: At,
    include: &'a [String],
    fields: &'a BTreeMap<String, FieldOverloads>,
    patterns: &'a [FieldSpec],
}

/// The kind of name a reference points at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefKind {
    Schema,
    Mixin,
    Type,
    Enum,
    Trait,
}

/// One name reference collected during the walk.
#[derive(Clone, Debug)]
struct Reference {
    kind: RefKind,
    name: String,
    /// Subtype qualifier of a type reference (`ref<event.country>`).
    subtype: Option<String>,
    /// Schema whose fields produced the reference, when any: schema
    /// reachability (check 1) walks only schema-owned references.
    owner: Option<String>,
    at: At,
}

/// One parameterised-schema call site.
#[derive(Clone, Debug)]
struct CallSite {
    callee: String,
    args: Vec<Actual>,
    caller: String,
}

/// One use of a formal parameter (`$name`).
#[derive(Clone, Debug)]
struct ParamUse {
    name: String,
    at: At,
    /// Formals of the enclosing parameterised schema.
    formals: Vec<String>,
    what: &'static str,
}

/// Context retained for dependent selectors until all schemas and mixins exist.
#[derive(Clone)]
enum QueryOwner {
    Schema(String),
    Mixin(String),
}

impl QueryOwner {
    fn name(&self) -> &str {
        match self {
            Self::Schema(name) | Self::Mixin(name) => name,
        }
    }
}

#[derive(Clone)]
struct QueryUse {
    query: expr::FieldQuery,
    at: At,
    owner: Option<QueryOwner>,
    siblings_allowed: bool,
}

/// The collected world the checks reason over.
struct Checker<'a> {
    sources: &'a [(String, RuleFile)],
    diagnostics: Vec<Diagnostic>,
    schemas: BTreeMap<String, SchemaDef<'a>>,
    mixins: BTreeMap<String, Named<'a, MixinSpec>>,
    types: BTreeMap<String, Named<'a, TypeSpec>>,
    traits: BTreeMap<String, Named<'a, TraitSpec>>,
    enums: BTreeMap<String, Named<'a, EnumSpec>>,
    scope_types: BTreeSet<String>,
    registers: BTreeSet<String>,
    references: Vec<Reference>,
    calls: Vec<CallSite>,
    params: Vec<ParamUse>,
    queries: Vec<QueryUse>,
    /// Files `root` schema names, for schema reachability.
    root_schemas: Vec<String>,
}

impl<'a> Checker<'a> {
    fn new(sources: &'a [(String, RuleFile)]) -> Self {
        Self {
            sources,
            diagnostics: Vec::new(),
            schemas: BTreeMap::new(),
            mixins: BTreeMap::new(),
            types: BTreeMap::new(),
            traits: BTreeMap::new(),
            enums: BTreeMap::new(),
            scope_types: BTreeSet::new(),
            registers: BTreeSet::new(),
            references: Vec::new(),
            calls: Vec::new(),
            params: Vec::new(),
            queries: Vec::new(),
            root_schemas: Vec::new(),
        }
    }

    /// Parses one type expression, recording parse errors (including the
    /// union-shape rule) and collecting parameter uses.
    fn expr(&mut self, at: &At, raw: &str, ctx: &ParamCtx) -> Option<Expr> {
        match expr::parse(raw) {
            Ok(parsed) => {
                for param in expr_params(&parsed) {
                    self.params.push(ParamUse {
                        name: param.name,
                        at: at.clone(),
                        formals: ctx.formals.to_vec(),

                        what: "a type expression",
                    });
                }
                self.collect_queries(at, &parsed, ctx);
                Some(parsed)
            }
            Err(failure) => {
                report_parse(
                    &mut self.diagnostics,
                    at,
                    Some(failure.column),
                    failure.message,
                );
                None
            }
        }
    }

    fn collect_queries(&mut self, at: &At, parsed: &Expr, ctx: &ParamCtx) {
        for alternative in &parsed.alternatives {
            match alternative {
                Primary::Query(query) => {
                    self.references.push(Reference {
                        kind: RefKind::Schema,
                        name: query.schema.clone(),
                        subtype: None,
                        owner: ctx
                            .query_owner
                            .as_ref()
                            .map(|owner| owner.name().to_owned()),
                        at: at.clone(),
                    });
                    self.queries.push(QueryUse {
                        query: query.clone(),
                        at: at.clone(),
                        owner: ctx.query_owner.clone(),
                        siblings_allowed: ctx.siblings_allowed,
                    });
                }
                Primary::Literal(parts) => {
                    for part in parts {
                        if let expr::LiteralPart::Hole(hole) = part {
                            self.collect_queries(at, hole, ctx);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Validate query-specific names and dependent selectors after collection.
    fn check_queries(&mut self) {
        for usage in &self.queries {
            if let Some(scope) = &usage.query.scope_accepts
                && scope != "any"
                && !self.scope_types.contains(scope)
            {
                report(
                    &mut self.diagnostics,
                    &usage.at,
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("query scope `{scope}` is not declared"),
                );
            }
            if self
                .schemas
                .get(&usage.query.schema)
                .is_some_and(|schema| !schema.formals.is_empty())
            {
                report(
                    &mut self.diagnostics,
                    &usage.at,
                    DiagnosticCode::ParameterError,
                    Severity::Error,
                    format!(
                        "query schema `{}` must be non-parameterized",
                        usage.query.schema
                    ),
                );
            }
            let Some(expr::QuerySelector::Sibling(sibling)) = &usage.query.selector else {
                continue;
            };
            if !usage.siblings_allowed || usage.owner.is_none() {
                report_parse(
                    &mut self.diagnostics,
                    &usage.at,
                    None,
                    "a sibling selector requires a schema field or items context".into(),
                );
                continue;
            }
            let declared = match &usage.owner {
                Some(QueryOwner::Schema(name)) => self.schema_has_field(name, sibling),
                Some(QueryOwner::Mixin(name)) => {
                    let owners = self
                        .schemas
                        .iter()
                        .filter(|(_, schema)| schema.include.contains(name));
                    let mut any_owner = false;
                    let mut all_declared = true;
                    for (schema, _) in owners {
                        any_owner = true;
                        all_declared &= self.schema_has_field(schema, sibling);
                    }
                    if any_owner {
                        all_declared
                    } else {
                        self.mixins.get(name).is_some_and(|mixin| {
                            mixin
                                .value
                                .fields
                                .keys()
                                .any(|key| key.eq_ignore_ascii_case(sibling))
                        })
                    }
                }
                None => false,
            };
            if !declared {
                report(
                    &mut self.diagnostics,
                    &usage.at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!("sibling selector names undeclared exact field `{sibling}`"),
                );
            }
        }
    }

    fn schema_has_field(&self, schema: &str, field: &str) -> bool {
        self.schemas.get(schema).is_some_and(|schema| {
            schema
                .fields
                .keys()
                .any(|key| key.eq_ignore_ascii_case(field))
                || schema.include.iter().any(|name| {
                    self.mixins.get(name).is_some_and(|mixin| {
                        mixin
                            .value
                            .fields
                            .keys()
                            .any(|key| key.eq_ignore_ascii_case(field))
                    })
                })
        })
    }

    /// Parses one schema reference (`body` string).
    fn schema_ref(&mut self, at: &At, raw: &str, ctx: &ParamCtx) -> Option<SchemaRef> {
        match parse_schema_ref(raw) {
            Ok(reference) => {
                for argument in &reference.args {
                    if let Actual::Param { name, .. } = argument {
                        self.params.push(ParamUse {
                            name: name.clone(),
                            at: at.clone(),
                            formals: ctx.formals.to_vec(),

                            what: "a schema argument",
                        });
                    }
                }
                Some(reference)
            }
            Err(failure) => {
                report_parse(&mut self.diagnostics, at, None, failure);
                None
            }
        }
    }

    /// Scans a non-expression string for `$name` parameters, which are only
    /// legal in type expressions and schema arguments.
    fn forbid_params(&mut self, at: &At, raw: &str, what: &str) {
        if let Some(name) = first_param(raw) {
            report(
                &mut self.diagnostics,
                at,
                DiagnosticCode::ParameterError,
                Severity::Error,
                format!("formal parameter `${name}` is not allowed in {what}"),
            );
        }
    }

    /// D14 card lints: `0..0` disables rather than bounds, `N..N` is a
    /// fixed-length tuple better written as `list` plus the arity, and two
    /// overloads of one key disagreeing on the card deserve a look.
    fn lint_card(&mut self, at: &At, raw: &str) {
        let Ok((min, max)) = parse_card(raw) else {
            return;
        };
        if max == Some(0) {
            report(
                &mut self.diagnostics,
                at,
                DiagnosticCode::CardLint,
                Severity::Warning,
                format!("card `{raw}` disables the field; write nothing instead"),
            );
        } else if max == Some(min) && min > 1 {
            report(
                &mut self.diagnostics,
                at,
                DiagnosticCode::CardLint,
                Severity::Info,
                format!(
                    "card `{raw}` is a fixed-length tuple; prefer `\"list\"` plus card `\"{min}\"`"
                ),
            );
        }
    }

    /// D14: two overloads of one key with different cards are worth a look.
    fn check_card_disagreement(&mut self) {
        let sources = self.sources;
        let mut diagnostics = Vec::new();
        for (file_name, file) in sources {
            for (schema_name, schema) in &file.schemas {
                let at = At {
                    file: file_name.clone(),
                    pointer: join_pointer("/schemas", schema_name),
                };
                let SchemaSpec::Block(block) = schema else {
                    continue;
                };
                lint_field_cards(&mut diagnostics, &at.child("fields"), &block.fields);
            }
            for (mixin_name, mixin) in &file.mixins {
                let at = At {
                    file: file_name.clone(),
                    pointer: join_pointer("/mixins", mixin_name),
                };
                lint_field_cards(&mut diagnostics, &at.child("fields"), &mixin.fields);
            }
        }
        self.diagnostics.extend(diagnostics);
    }

    /// Records the trait references of one `impl` map and validates each
    /// binding argument: exactly one of `loc` / `sprite`, matching the trait
    /// (a `Localised` binding is a localisation key, a `HasIcon` binding is a
    /// sprite).
    fn collect_trait_impls(&mut self, at: &At, impls: &'a BTreeMap<String, ImplSpec>) {
        for (trait_name, spec) in impls {
            let impl_at = at.child("impl").child(trait_name);
            self.references.push(Reference {
                kind: RefKind::Trait,
                name: trait_name.clone(),
                subtype: None,
                owner: None,
                at: impl_at.clone(),
            });
            for (binding_name, value) in &spec.0 {
                let binding_at = impl_at.child(binding_name);
                let ImplValue::Binding(binding) = value else {
                    if let ImplValue::Text(text) = value {
                        self.forbid_params(&binding_at, text, "a trait argument");
                    }
                    continue;
                };
                if binding.loc.is_some() == binding.sprite.is_some() {
                    report(
                        &mut self.diagnostics,
                        &binding_at,
                        DiagnosticCode::Parse,
                        Severity::Error,
                        format!(
                            "trait binding `{binding_name}` must declare exactly one of \
                             `loc` / `sprite`"
                        ),
                    );
                }
                if trait_name == "Localised" && binding.sprite.is_some() {
                    report(
                        &mut self.diagnostics,
                        &binding_at,
                        DiagnosticCode::Parse,
                        Severity::Error,
                        format!(
                            "trait binding `{binding_name}` is a `Localised` binding; spell it \
                             with `loc`"
                        ),
                    );
                }
                if trait_name == "HasIcon" && binding.loc.is_some() {
                    report(
                        &mut self.diagnostics,
                        &binding_at,
                        DiagnosticCode::Parse,
                        Severity::Error,
                        format!(
                            "trait binding `{binding_name}` is a `HasIcon` binding; spell it \
                             with `sprite`"
                        ),
                    );
                }
                // `loc` / `sprite` are instance-name templates (`$` is the
                // placeholder), not type expressions, so `$name` is legal.
            }
        }
    }

    fn collect(&mut self) {
        self.collect_scopes();
        for (file_name, file) in self.sources {
            self.collect_file(file_name, file);
        }
    }

    fn collect_scopes(&mut self) {
        let mut link_names = BTreeSet::new();
        let mut register_names = BTreeSet::new();
        for (file_name, file) in self.sources {
            let Some(scopes) = file.scopes.as_ref() else {
                continue;
            };
            let base = At {
                file: file_name.clone(),
                pointer: "/scopes".to_owned(),
            };
            for (index, name) in scopes.types.iter().enumerate() {
                let at = base.child("types").index(index);
                self.forbid_params(&at, name, "a scope type name");
                if name == "any" {
                    report(
                        &mut self.diagnostics,
                        &at,
                        DiagnosticCode::ScopeReferenceError,
                        Severity::Error,
                        "`any` is reserved and must not be declared as a scope type".to_owned(),
                    );
                }
                if !self.scope_types.insert(name.clone()) {
                    report(
                        &mut self.diagnostics,
                        &at,
                        DiagnosticCode::DuplicateName,
                        Severity::Error,
                        format!("scope type `{name}` is declared more than once"),
                    );
                }
            }
            for (name, register) in &scopes.registers {
                let at = base.child("registers").child(name);
                self.forbid_params(&at, name, "a register name");
                if !register_names.insert(name.clone()) {
                    report(
                        &mut self.diagnostics,
                        &at,
                        DiagnosticCode::DuplicateName,
                        Severity::Error,
                        format!("register `{name}` is declared more than once"),
                    );
                }
                self.registers.insert(name.clone());
                if register.chain == Some(true)
                    && matches!(
                        register.role,
                        crate::source::RegisterRole::Root | crate::source::RegisterRole::Current
                    )
                {
                    report(
                        &mut self.diagnostics,
                        &at.child("chain"),
                        DiagnosticCode::ScopeReferenceError,
                        Severity::Error,
                        "only previous/from register roles can chain".to_owned(),
                    );
                }
            }
            for (name, link) in &scopes.links {
                let at = base.child("links").child(name);
                self.forbid_params(&at, name, "a link name");
                if !link_names.insert(name.clone()) {
                    report(
                        &mut self.diagnostics,
                        &at,
                        DiagnosticCode::DuplicateName,
                        Severity::Error,
                        format!("link `{name}` is declared more than once"),
                    );
                }
                match expr::parse_template(name) {
                    Ok(parts) => {
                        for part in parts {
                            if let expr::LiteralPart::Hole(hole) = part {
                                self.collect_queries(&at, &hole, &ParamCtx::closed());
                            }
                        }
                    }
                    Err(failure) => report_parse(
                        &mut self.diagnostics,
                        &at,
                        Some(failure.column),
                        failure.message,
                    ),
                }
                if link.from.is_empty() {
                    report(
                        &mut self.diagnostics,
                        &at,
                        DiagnosticCode::ScopeReferenceError,
                        Severity::Error,
                        format!("link `{name}` has an empty `from` list"),
                    );
                }
                for scope in &link.from {
                    self.forbid_params(&at, scope, "a link `from` entry");
                }
                self.forbid_params(&at, &link.to, "a link `to` entry");
            }
            for (index, compat) in scopes.compat.iter().enumerate() {
                let at = base.child("compat").index(index);
                self.forbid_params(&at, &compat.actual, "a compat entry");
                self.forbid_params(&at, &compat.expected, "a compat entry");
            }
        }
    }

    fn collect_file(&mut self, file_name: &str, file: &'a RuleFile) {
        for (entry_name, rule) in &file.files {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/files", entry_name),
            };
            self.forbid_params(&at, &rule.path, "a files path");
            if let Some(ext) = &rule.ext {
                let ext_at = at.child("ext");
                for (index, value) in ext.iter().enumerate() {
                    let spec_at = match ext {
                        ExtSpec::One(_) => ext_at.clone(),
                        ExtSpec::Many(_) => ext_at.index(index),
                    };
                    self.forbid_params(&spec_at, value, "a files extension");
                }
            }
            if let Some(name) = &rule.file {
                self.forbid_params(&at.child("file"), name, "a files file name");
            }
            for (index, prefix) in rule.exclude.iter().enumerate() {
                self.forbid_params(
                    &at.child("exclude").index(index),
                    prefix,
                    "a files exclusion",
                );
            }
            // D14: a `script` entry without a root would silently validate
            // nothing, which the no-guessing rule forbids; `localisation` and
            // `asset` files have no script structure to declare.
            if rule.parser.unwrap_or(SourceParser::Script) == SourceParser::Script
                && rule.root.is_none()
            {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "a `script` file entry must declare `root`".to_owned(),
                );
            }
            match rule.root.as_ref() {
                Some(RootSpec::Schema(name)) => {
                    self.references.push(Reference {
                        kind: RefKind::Schema,
                        name: name.clone(),
                        subtype: None,
                        owner: None,
                        at: at.child("root"),
                    });
                    self.root_schemas.push(name.clone());
                }
                Some(RootSpec::Instance(field)) => {
                    self.collect_field_payload(&at.child("root"), "", &ParamCtx::closed(), field);
                }
                None => {}
            }
        }

        for (key, spec) in &file.schemas {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/schemas", key),
            };
            let (name, formals) = match parse_schema_key(key) {
                Ok(parsed) => parsed,
                Err(failure) => {
                    report_parse(&mut self.diagnostics, &at, None, failure);
                    continue;
                }
            };
            for formal in &formals {
                self.forbid_params(&at, formal, "a formal parameter list");
            }
            let formals_for_fields = formals.clone();
            let def = SchemaDef {
                formals,
                at: at.clone(),
                include: &[],
                fields: &EMPTY_FIELDS,
                patterns: &[],
            };
            if self.schemas.contains_key(&name) {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::DuplicateName,
                    Severity::Error,
                    format!("schema `{name}` is defined more than once"),
                );
            }
            match spec {
                SchemaSpec::Block(block) => {
                    let def = SchemaDef {
                        include: &block.include,
                        fields: &block.fields,
                        patterns: &block.patterns,
                        ..def
                    };
                    self.schemas.insert(name.clone(), def);
                    for mixin in &block.include {
                        self.references.push(Reference {
                            kind: RefKind::Mixin,
                            name: mixin.clone(),
                            subtype: None,
                            owner: Some(name.clone()),
                            at: at.child("include"),
                        });
                    }
                    self.collect_schema_fields(&at, &name, &formals_for_fields, block);
                }
                SchemaSpec::Map { map } => {
                    self.schemas.insert(name.clone(), def);
                    let ctx = ParamCtx::schema(&[], &name, false);
                    let owner = Some(name.as_str());
                    if let Some(parsed) = self.expr(&at.child("map").child("key"), &map.key, &ctx) {
                        self.collect_refs_from_expr(&at.child("map").child("key"), &parsed, owner);
                    }
                    if let Some(value) = &map.value
                        && let Some(parsed) =
                            self.expr(&at.child("map").child("value"), value, &ctx)
                    {
                        self.collect_refs_from_expr(
                            &at.child("map").child("value"),
                            &parsed,
                            owner,
                        );
                    }
                    if let Some(body) = &map.body {
                        self.collect_body_ref(&at.child("map").child("body"), body, &ctx, owner);
                    }
                }
                SchemaSpec::List { list } => {
                    self.schemas.insert(name.clone(), def);
                    if let Some(parsed) = self.expr(
                        &at.child("list"),
                        list,
                        &ParamCtx::schema(&[], &name, false),
                    ) {
                        self.collect_refs_from_expr(&at.child("list"), &parsed, Some(&name));
                    }
                }
            }
        }

        for (name, mixin) in &file.mixins {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/mixins", name),
            };
            self.forbid_params(&at, name, "a mixin name");
            if self.mixins.contains_key(name) {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::DuplicateName,
                    Severity::Error,
                    format!("mixin `{name}` is defined more than once"),
                );
            }
            self.mixins.insert(
                name.clone(),
                Named {
                    value: mixin,
                    at: at.clone(),
                },
            );
            self.collect_field_map(
                &at,
                name,
                &ParamCtx {
                    formals: &[],
                    query_owner: Some(QueryOwner::Mixin(name.clone())),
                    siblings_allowed: true,
                },
                &mixin.fields,
            );
        }

        for (name, spec) in &file.types {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/types", name),
            };
            self.forbid_params(&at, name, "a type name");
            if self.types.contains_key(name) {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::DuplicateName,
                    Severity::Error,
                    format!("type `{name}` is defined more than once"),
                );
            }
            self.types.insert(
                name.clone(),
                Named {
                    value: spec,
                    at: at.clone(),
                },
            );
            for builtin in spec.builtin.iter().flatten() {
                self.forbid_params(&at.child("builtin"), builtin, "a builtin member");
            }
            self.collect_trait_impls(&at, &spec.trait_impls);
            for subtype_name in spec.subtypes.keys() {
                self.forbid_params(
                    &at.child("subtypes").child(subtype_name),
                    subtype_name,
                    "a subtype name",
                );
            }
        }

        for (name, spec) in &file.traits {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/traits", name),
            };
            self.forbid_params(&at, name, "a trait name");
            if self.traits.contains_key(name) {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::DuplicateName,
                    Severity::Error,
                    format!("trait `{name}` is defined more than once"),
                );
            }
            self.traits.insert(
                name.clone(),
                Named {
                    value: spec,
                    at: at.clone(),
                },
            );
        }

        for (name, spec) in &file.enums {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/enums", name),
            };
            self.forbid_params(&at, name, "an enum name");
            if self.enums.contains_key(name) {
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::DuplicateName,
                    Severity::Error,
                    format!("enum `{name}` is defined more than once"),
                );
            }
            self.enums.insert(
                name.clone(),
                Named {
                    value: spec,
                    at: at.clone(),
                },
            );
            match spec {
                EnumSpec::Members(members) => {
                    for (index, member) in members.iter().enumerate() {
                        self.forbid_params(&at.index(index), member, "an enum member");
                    }
                }
            }
        }
    }

    /// Collects the fields and patterns of one block schema.
    fn check_block_forms(&mut self) {
        for (file, source) in self.sources {
            for (name, schema) in &source.schemas {
                if let SchemaSpec::Block(block) = schema {
                    let at = At {
                        file: file.clone(),
                        pointer: join_pointer("/schemas", name),
                    };
                    self.check_block_form_specs(&at, block);
                }
            }
        }
    }

    fn check_block_form_specs(&mut self, at: &At, block: &crate::source::BlockSchema) {
        for (index, form) in block.forms.iter().enumerate() {
            let at = at.child("forms").index(index);
            if form.fields.is_empty() && form.patterns.is_empty() {
                report_parse(
                    &mut self.diagnostics,
                    &at,
                    None,
                    "a block form must constrain at least one field or pattern".into(),
                );
            }
            for (key, card) in &form.fields {
                let at = at.child("fields").child(key);
                let exists = block
                    .fields
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(key))
                    || block.include.iter().any(|name| {
                        self.mixins.get(name).is_some_and(|mixin| {
                            mixin
                                .value
                                .fields
                                .keys()
                                .any(|name| name.eq_ignore_ascii_case(key))
                        })
                    });
                if !exists {
                    report_parse(
                        &mut self.diagnostics,
                        &at,
                        None,
                        format!("block form names undeclared field `{key}`"),
                    );
                }
                if let Err(message) = parse_card(card) {
                    report_parse(&mut self.diagnostics, &at, None, message);
                }
            }
            for (index, card) in &form.patterns {
                let at = at.child("patterns").child(index);
                if index
                    .parse::<usize>()
                    .ok()
                    .is_none_or(|index| index >= block.patterns.len())
                {
                    report_parse(
                        &mut self.diagnostics,
                        &at,
                        None,
                        format!("block form names undeclared pattern {index}"),
                    );
                }
                if let Err(message) = parse_card(card) {
                    report_parse(&mut self.diagnostics, &at, None, message);
                }
            }
        }
    }

    fn collect_schema_fields(
        &mut self,
        at: &At,
        name: &str,
        formals: &[String],
        block: &crate::source::BlockSchema,
    ) {
        let ctx = ParamCtx::schema(formals, name, true);
        self.collect_field_map(at, name, &ctx, &block.fields);
        for (index, pattern) in block.patterns.iter().enumerate() {
            let pattern_at = at.child("patterns").index(index);
            if pattern.key.is_none() {
                report(
                    &mut self.diagnostics,
                    &pattern_at,
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "a pattern must declare a `key` type expression".to_owned(),
                );
            }
            self.collect_field_payload(&pattern_at, name, &ctx, pattern);
        }
        if let Some(items) = &block.items
            && let Some(parsed) = self.expr(&at.child("items"), items, &ctx)
        {
            self.collect_refs_from_expr(&at.child("items"), &parsed, Some(name));
        }
    }

    fn collect_field_map(
        &mut self,
        at: &At,
        schema: &str,
        ctx: &ParamCtx,
        fields: &BTreeMap<String, FieldOverloads>,
    ) {
        for (key, overloads) in fields {
            self.forbid_params(&at.child("fields").child(key), key, "an exact field key");
            let (list, indexed) = match overloads {
                FieldOverloads::One(field) => (std::slice::from_ref(field.as_ref()), false),
                FieldOverloads::Many(fields) => (fields.as_slice(), true),
            };
            for (index, field) in list.iter().enumerate() {
                let mut field_at = at.child("fields").child(key);
                if indexed {
                    field_at = field_at.index(index);
                }
                self.collect_field_payload(&field_at, schema, ctx, field);
            }
        }
    }

    /// Collects one field specification's mini-syntax, references, calls,
    /// params, and def positions.
    fn collect_field_payload(&mut self, at: &At, schema: &str, ctx: &ParamCtx, field: &FieldSpec) {
        let owner = (!schema.is_empty()).then_some(schema);
        if let Some(control) = &field.control {
            if let Some(selector_schema) = &control.selector_schema {
                if selector_schema == "self" {
                    report(
                        &mut self.diagnostics,
                        &at.child("control").child("selector_schema"),
                        DiagnosticCode::Parse,
                        Severity::Error,
                        "selector_schema must name a schema explicitly".to_owned(),
                    );
                }
                self.collect_body_ref(
                    &at.child("control").child("selector_schema"),
                    selector_schema,
                    ctx,
                    owner,
                );
            }
            if control.kind == crate::source::ControlKind::Switch
                && (control.on.is_none() || control.selector_schema.is_none())
            {
                report(
                    &mut self.diagnostics,
                    &at.child("control"),
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "switch control requires on and selector_schema".to_owned(),
                );
            } else if control.kind != crate::source::ControlKind::Switch
                && control.selector_schema.is_some()
            {
                report(
                    &mut self.diagnostics,
                    &at.child("control"),
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "selector_schema is only valid on switch control".to_owned(),
                );
            }
        }
        if field
            .control
            .as_ref()
            .is_some_and(|control| control.kind == crate::source::ControlKind::Constant)
            && field.value.as_deref() != Some("bool")
        {
            report(
                &mut self.diagnostics,
                &at.child("control"),
                DiagnosticCode::Parse,
                Severity::Error,
                "a constant predicate must have a scalar bool value".to_owned(),
            );
        }
        if let Some(key) = &field.key
            && let Some(parsed) = self.expr(&at.child("key"), key, ctx)
        {
            self.collect_refs_from_expr(&at.child("key"), &parsed, owner);
        }
        let mut payload_count = 0usize;
        if let Some(value) = &field.value {
            payload_count += 1;
            if let Some(parsed) = self.expr(&at.child("value"), value, ctx) {
                self.collect_refs_from_expr(&at.child("value"), &parsed, owner);
            }
        }
        if let Some(body) = &field.body {
            payload_count += 1;
            self.collect_body_ref(&at.child("body"), body, ctx, owner);
        }
        if let Some(list) = &field.list {
            payload_count += 1;
            if let Some(parsed) = self.expr(&at.child("list"), list, &ctx.without_siblings()) {
                self.collect_refs_from_expr(&at.child("list"), &parsed, owner);
            }
        }
        if let Some(map) = &field.map {
            payload_count += 1;
            let map_at = at.child("map");
            let map_ctx = ctx.without_siblings();
            if let Some(parsed) = self.expr(&map_at.child("key"), &map.key, &map_ctx) {
                self.collect_refs_from_expr(&map_at.child("key"), &parsed, owner);
            }
            if let Some(value) = &map.value
                && let Some(parsed) = self.expr(&map_at.child("value"), value, &map_ctx)
            {
                self.collect_refs_from_expr(&map_at.child("value"), &parsed, owner);
            }
            if let Some(body) = &map.body {
                self.collect_body_ref(&map_at.child("body"), body, &map_ctx, owner);
            }
            if map.value.is_some() == map.body.is_some() {
                report(
                    &mut self.diagnostics,
                    &map_at,
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "a map must declare exactly one of `value` / `body`".to_owned(),
                );
            }
        }
        if payload_count > 1 {
            report(
                &mut self.diagnostics,
                at,
                DiagnosticCode::Parse,
                Severity::Error,
                "a field must declare exactly one of `value` / `body` / `list` / `map`".to_owned(),
            );
        }
        self.forbid_params(&at.child("card"), &field.card, "a card");
        if let Err(failure) = parse_card(&field.card) {
            report_parse(&mut self.diagnostics, &at.child("card"), None, failure);
        } else {
            self.lint_card(&at.child("card"), &field.card);
        }
        if let Some(scope) = &field.scope {
            let scope_at = at.child("scope");
            if let Some(scopes) = &scope.scope_in {
                for (index, name) in scopes.iter().enumerate() {
                    self.forbid_params(&scope_at.child("in").index(index), name, "a scope effect");
                }
            }
            if let Some(push) = &scope.push {
                self.forbid_params(&scope_at.child("push"), push, "a scope effect");
            }
            for (register, target) in &scope.set.clone().unwrap_or_default() {
                self.forbid_params(
                    &scope_at.child("set").child(register),
                    register,
                    "a scope effect",
                );
                self.forbid_params(
                    &scope_at.child("set").child(register),
                    target,
                    "a scope effect",
                );
            }
        }
        if let Some(def) = &field.def {
            let def_at = at.child("def");
            self.forbid_params(&def_at.child("type"), &def.type_name, "a def type");
            match parse_def_type(&def.type_name) {
                Ok((type_name, subtype)) => {
                    self.references.push(Reference {
                        kind: RefKind::Type,
                        name: type_name.clone(),
                        subtype,
                        owner: owner.map(ToOwned::to_owned),
                        at: def_at.child("type"),
                    });
                }
                Err(failure) => {
                    report_parse(&mut self.diagnostics, &def_at.child("type"), None, failure)
                }
            }
            if let Some(name) = &def.name {
                self.forbid_params(&def_at.child("name"), name, "a def name source");
                if let Err(failure) = parse_def_name(name) {
                    report_parse(&mut self.diagnostics, &def_at.child("name"), None, failure);
                }
            }
        }
    }

    /// Collects the reference and call site of one `body` string.
    fn collect_body_ref(&mut self, at: &At, raw: &str, ctx: &ParamCtx, owner: Option<&str>) {
        let Some(reference) = self.schema_ref(at, raw, ctx) else {
            return;
        };
        if reference.name != "self" {
            self.references.push(Reference {
                kind: RefKind::Schema,
                name: reference.name.clone(),
                subtype: None,
                owner: owner.map(ToOwned::to_owned),
                at: at.clone(),
            });
        }
        if !reference.args.is_empty() {
            self.calls.push(CallSite {
                callee: reference.name.clone(),
                args: reference.args.clone(),
                caller: owner.unwrap_or_default().to_owned(),
            });
        }
    }

    /// Collects the type/enum/schema references of one parsed expression.
    fn collect_refs_from_expr(&mut self, at: &At, parsed: &Expr, owner: Option<&str>) {
        for alternative in &parsed.alternatives {
            match alternative {
                Primary::Ref(argument) | Primary::Def(argument) => match argument {
                    expr::Argument::Path(segments) | expr::Argument::Stripped { segments, .. } => {
                        let names = segments
                            .iter()
                            .filter_map(|segment| match segment {
                                Segment::Name(name) => Some(name.clone()),
                                Segment::Param(_) => None,
                            })
                            .collect::<Vec<_>>();
                        if let Some(type_name) = names.first() {
                            self.references.push(Reference {
                                kind: RefKind::Type,
                                name: type_name.clone(),
                                subtype: names.get(1).cloned(),
                                owner: owner.map(ToOwned::to_owned),
                                at: at.clone(),
                            });
                        }
                    }
                },
                Primary::Enum(argument) => {
                    if let Some(name) = first_name(argument) {
                        self.references.push(Reference {
                            kind: RefKind::Enum,
                            name,
                            subtype: None,
                            owner: owner.map(ToOwned::to_owned),
                            at: at.clone(),
                        });
                    }
                }
                Primary::Literal(parts) => {
                    for part in parts {
                        if let expr::LiteralPart::Hole(hole) = part {
                            self.collect_refs_from_expr(at, hole, owner);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Check 2: include conflicts.
    fn check_include_conflicts(&mut self) {
        let schema_names = self.schemas.keys().cloned().collect::<Vec<_>>();
        for name in schema_names {
            let Some(schema) = self.schemas.get(&name) else {
                continue;
            };
            if schema.include.is_empty() {
                continue;
            }
            let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for mixin_name in schema.include {
                if let Some(mixin) = self.mixins.get(mixin_name) {
                    for key in mixin.value.fields.keys() {
                        owners
                            .entry(key.clone())
                            .or_default()
                            .push(mixin_name.clone());
                    }
                }
            }
            for key in schema.fields.keys() {
                owners.entry(key.clone()).or_default().push(String::new());
            }
            for (key, sources) in owners {
                let mixins_only = sources.iter().filter(|source| !source.is_empty()).count();
                let body = sources.iter().any(|source| source.is_empty());
                if mixins_only > 1 {
                    report(
                        &mut self.diagnostics,
                        &schema.at.child("include"),
                        DiagnosticCode::IncludeConflict,
                        Severity::Error,
                        format!("field `{key}` is declared by {} mixins", mixins_only),
                    );
                    continue;
                }
                if mixins_only == 1 && body && !self.body_overrides(schema, &key) {
                    report(
                        &mut self.diagnostics,
                        &schema.at.child("fields").child(&key),
                        DiagnosticCode::IncludeConflict,
                        Severity::Error,
                        format!(
                            "field `{key}` is declared by both a mixin and the schema; \
                             mark the schema field `\"override\": true` to take over"
                        ),
                    );
                }
            }
        }
    }

    fn body_overrides(&self, schema: &SchemaDef<'_>, key: &str) -> bool {
        schema
            .fields
            .get(key)
            .is_some_and(|overloads| match overloads {
                FieldOverloads::One(field) => field.override_field == Some(true),
                FieldOverloads::Many(fields) => fields
                    .iter()
                    .any(|field| field.override_field == Some(true)),
            })
    }

    /// Check 3: unreachable overloads and covered patterns.
    fn check_unreachable_overloads(&mut self) {
        let schema_names = self.schemas.keys().cloned().collect::<Vec<_>>();
        for name in schema_names {
            let Some(schema) = self.schemas.get(&name) else {
                continue;
            };
            for (key, overloads) in schema.fields {
                let list: &[FieldSpec] = match overloads {
                    FieldOverloads::One(field) => std::slice::from_ref(field.as_ref()),
                    FieldOverloads::Many(fields) => fields.as_slice(),
                };
                for later in 1..list.len() {
                    for earlier in 0..later {
                        if field_structure_eq(&list[earlier], &list[later]) {
                            report(
                                &mut self.diagnostics,
                                &schema.at.child("fields").child(key).index(later),
                                DiagnosticCode::UnreachableOverload,
                                Severity::Error,
                                format!(
                                    "overload {later} of `{key}` is fully shadowed by overload {earlier}"
                                ),
                            );
                            break;
                        }
                    }
                }
            }
            for later in 1..schema.patterns.len() {
                for earlier in 0..later {
                    if field_structure_eq(&schema.patterns[earlier], &schema.patterns[later]) {
                        report(
                            &mut self.diagnostics,
                            &schema.at.child("patterns").index(later),
                            DiagnosticCode::UnreachableOverload,
                            Severity::Error,
                            format!("pattern {later} is fully covered by pattern {earlier}"),
                        );
                        break;
                    }
                }
            }
        }
    }

    /// Check 4: parameter position, one level, and the instantiation cap.
    fn check_parameters(&mut self) {
        for use_site in &self.params {
            let known = use_site
                .formals
                .iter()
                .any(|formal| formal == &use_site.name);
            if !known {
                let reason = format!(
                    "parameter `${}` is not a formal of the enclosing schema ({})",
                    use_site.name, use_site.what
                );
                report(
                    &mut self.diagnostics,
                    &use_site.at,
                    DiagnosticCode::ParameterError,
                    Severity::Error,
                    reason,
                );
            }
        }
        self.check_instantiation_cap();
    }

    /// Check 4c: the 64-instance cap, statically approximated. Each call
    /// site contributes the product of its argument domain sizes; concrete
    /// names count 1, and a forwarded formal counts the caller's instances. Sites with
    /// unresolvable domains cannot prove an excess and are not counted.
    fn check_instantiation_cap(&mut self) {
        let mut counts: BTreeMap<String, u64> = self
            .schemas
            .iter()
            .filter(|(_, schema)| !schema.formals.is_empty())
            .map(|(name, _)| (name.clone(), 0u64))
            .collect();
        for _ in 0..32 {
            // Each round recomputes every callee as the sum over its call
            // sites from the previous round (a fixpoint, not an accumulation).
            let mut next: BTreeMap<String, u64> =
                counts.keys().map(|name| (name.clone(), 0u64)).collect();
            let mut concrete_calls = BTreeSet::new();
            for call in &self.calls {
                if !counts.contains_key(&call.callee) {
                    continue;
                }
                // Repeated concrete tuples lower to the same arena instance,
                // regardless of how many fields call them.
                if let Some(arguments) = call
                    .args
                    .iter()
                    .map(|argument| match argument {
                        Actual::Name(name) => Some(name.as_str()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    && !concrete_calls.insert((call.callee.as_str(), arguments))
                {
                    continue;
                }
                let Some(product) = self.call_domain_product(call, &counts) else {
                    continue;
                };
                let entry = next.entry(call.callee.clone()).or_insert(0);
                *entry = (*entry + product).min(INSTANTIATION_CAP);
            }
            if next == counts {
                break;
            }
            counts = next;
        }
        for (name, count) in counts {
            if count > 64 {
                let Some(schema) = self.schemas.get(&name) else {
                    continue;
                };
                let at = schema.at.clone();
                report(
                    &mut self.diagnostics,
                    &at,
                    DiagnosticCode::ParameterError,
                    Severity::Error,
                    format!("schema `{name}` has {count} instances, over the limit of 64"),
                );
            }
        }
    }

    fn call_domain_product(&self, call: &CallSite, counts: &BTreeMap<String, u64>) -> Option<u64> {
        let mut product = 1u64;
        for argument in &call.args {
            let domain = match argument {
                Actual::Name(_) => 1u64,
                Actual::Param { name, .. } => {
                    // A forwarded formal instantiates once per caller instance.
                    let caller = self.schemas.get(&call.caller)?;
                    if !caller.formals.iter().any(|formal| formal == name) {
                        return None;
                    }
                    *counts.get(&call.caller)?
                }
            };
            product = product.saturating_mul(domain.max(1)).min(INSTANTIATION_CAP);
        }
        Some(product)
    }

    /// Check 5: scope names and link `from` lists.
    fn check_scopes(&mut self) {
        let declared = self.scope_types.clone();
        let scope_names = |name: &str| name == "any" || declared.contains(name);
        for (file_name, file) in self.sources {
            for (entry_name, rule) in &file.files {
                if let Some(RootSpec::Instance(field)) = rule.root.as_ref() {
                    let at = At {
                        file: file_name.clone(),
                        pointer: join_pointer("/files", entry_name),
                    };
                    self.check_scope_effect(&at.child("root"), field, &scope_names);
                }
            }
            for (key, spec) in &file.schemas {
                let at = At {
                    file: file_name.clone(),
                    pointer: join_pointer("/schemas", key),
                };
                match spec {
                    SchemaSpec::Block(block) => {
                        self.check_scope_fields(&at, &block.fields, &block.patterns, &scope_names);
                    }
                    SchemaSpec::Map { .. } | SchemaSpec::List { .. } => {}
                }
            }
            for (name, mixin) in &file.mixins {
                let at = At {
                    file: file_name.clone(),
                    pointer: join_pointer("/mixins", name),
                };
                self.check_scope_fields(&at, &mixin.fields, &[], &scope_names);
            }
            if let Some(scopes) = &file.scopes {
                let at = At {
                    file: file_name.clone(),
                    pointer: "/scopes".to_owned(),
                };
                for (name, link) in &scopes.links {
                    let link_at = at.child("links").child(name);
                    for scope in &link.from {
                        if !scope_names(scope) {
                            report(
                                &mut self.diagnostics,
                                &link_at,
                                DiagnosticCode::ScopeReferenceError,
                                Severity::Error,
                                format!("link `{name}` has undeclared `from` scope `{scope}`"),
                            );
                        }
                    }
                    if !scope_names(&link.to) {
                        report(
                            &mut self.diagnostics,
                            &link_at,
                            DiagnosticCode::ScopeReferenceError,
                            Severity::Error,
                            format!("link `{name}` has undeclared `to` scope `{}`", link.to),
                        );
                    }
                }
                for (index, compat) in scopes.compat.iter().enumerate() {
                    let compat_at = at.child("compat").index(index);
                    for name in [&compat.actual, &compat.expected] {
                        if !scope_names(name) {
                            report(
                                &mut self.diagnostics,
                                &compat_at,
                                DiagnosticCode::ScopeReferenceError,
                                Severity::Error,
                                format!("compat names undeclared scope `{name}`"),
                            );
                        }
                    }
                }
            }
        }
    }

    fn check_scope_fields(
        &mut self,
        at: &At,
        fields: &BTreeMap<String, FieldOverloads>,
        patterns: &[FieldSpec],
        scope_names: &impl Fn(&str) -> bool,
    ) {
        for (key, overloads) in fields {
            let list: &[FieldSpec] = match overloads {
                FieldOverloads::One(field) => std::slice::from_ref(field.as_ref()),
                FieldOverloads::Many(fields) => fields.as_slice(),
            };
            for field in list {
                self.check_scope_effect(&at.child("fields").child(key), field, scope_names);
            }
        }
        for (index, pattern) in patterns.iter().enumerate() {
            self.check_scope_effect(&at.child("patterns").index(index), pattern, scope_names);
        }
    }

    fn check_scope_effect(
        &mut self,
        at: &At,
        field: &FieldSpec,
        scope_names: &impl Fn(&str) -> bool,
    ) {
        let Some(scope) = &field.scope else {
            return;
        };
        let scope_at = at.child("scope");
        for (index, name) in scope.scope_in.iter().flatten().enumerate() {
            if first_param(name).is_some() {
                continue;
            }
            if !scope_names(name) {
                report(
                    &mut self.diagnostics,
                    &scope_at.child("in").index(index),
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("`scope.in` names undeclared scope `{name}`"),
                );
            }
        }
        if let Some(push) = &scope.push
            && first_param(push).is_none()
            && !scope_names(push)
        {
            report(
                &mut self.diagnostics,
                &scope_at.child("push"),
                DiagnosticCode::ScopeReferenceError,
                Severity::Error,
                format!("`scope.push` names undeclared scope `{push}`"),
            );
        }
        for (register, target) in scope.set.iter().flatten() {
            if first_param(register).is_some() || first_param(target).is_some() {
                continue;
            }
            if !self.registers.contains(register) {
                report(
                    &mut self.diagnostics,
                    &scope_at.child("set").child(register),
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("`scope.set` names undeclared register `{register}`"),
                );
            }
            if !scope_names(target) {
                report(
                    &mut self.diagnostics,
                    &scope_at.child("set").child(register),
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("`scope.set` assigns undeclared scope `{target}`"),
                );
            }
        }
    }

    /// Check 1 (errors): every collected reference must resolve.
    fn check_undefined_references(&mut self) {
        for reference in &self.references {
            let missing = match reference.kind {
                RefKind::Schema => !self.schemas.contains_key(&reference.name),
                RefKind::Mixin => !self.mixins.contains_key(&reference.name),
                RefKind::Type => !self.types.contains_key(&reference.name),
                RefKind::Enum => !self.enums.contains_key(&reference.name),
                RefKind::Trait => !self.traits.contains_key(&reference.name),
            };
            if missing {
                report(
                    &mut self.diagnostics,
                    &reference.at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!(
                        "{} `{}` is not defined",
                        reference.kind.label(),
                        reference.name
                    ),
                );
                continue;
            }
            if let Some(subtype) = &reference.subtype
                && let Some(type_def) = self.types.get(&reference.name)
                && !type_def.value.subtypes.contains_key(subtype)
            {
                report(
                    &mut self.diagnostics,
                    &reference.at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!(
                        "subtype `{subtype}` is not defined on type `{}`",
                        reference.name
                    ),
                );
            }
        }
    }

    /// Check 1 (warnings): definitions never referenced.
    fn check_unused_definitions(&mut self) {
        let mut used_schemas = BTreeSet::new();
        let mut queue = self.root_schemas.clone();
        queue.extend(
            self.references
                .iter()
                .filter(|reference| reference.kind == RefKind::Schema && reference.owner.is_none())
                .map(|reference| reference.name.clone()),
        );
        while let Some(name) = queue.pop() {
            if !used_schemas.insert(name.clone()) {
                continue;
            }
            let includes = self
                .schemas
                .get(&name)
                .map(|schema| schema.include.to_vec())
                .unwrap_or_default();
            queue.extend(
                self.references
                    .iter()
                    .filter(|reference| {
                        reference.kind == RefKind::Schema
                            && reference.owner.as_deref().is_some_and(|owner| {
                                owner == name || includes.iter().any(|include| include == owner)
                            })
                    })
                    .map(|reference| reference.name.clone()),
            );
        }
        let mut used_mixins = BTreeSet::new();
        for schema in self.schemas.values() {
            used_mixins.extend(schema.include.iter().cloned());
        }

        let used_types = self
            .references
            .iter()
            .filter(|reference| reference.kind == RefKind::Type)
            .map(|reference| reference.name.clone())
            .collect::<BTreeSet<_>>();
        let used_enums = self
            .references
            .iter()
            .filter(|reference| reference.kind == RefKind::Enum)
            .map(|reference| reference.name.clone())
            .collect::<BTreeSet<_>>();
        let used_traits = self
            .references
            .iter()
            .filter(|reference| reference.kind == RefKind::Trait)
            .map(|reference| reference.name.clone())
            .collect::<BTreeSet<_>>();

        for (name, schema) in &self.schemas {
            if !used_schemas.contains(name) {
                report(
                    &mut self.diagnostics,
                    &schema.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("schema `{name}` is never referenced"),
                );
            }
        }
        for (name, mixin) in &self.mixins {
            if !used_mixins.contains(name) {
                report(
                    &mut self.diagnostics,
                    &mixin.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("mixin `{name}` is never included"),
                );
            }
        }
        for (name, type_def) in &self.types {
            if !used_types.contains(name) {
                report(
                    &mut self.diagnostics,
                    &type_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("type `{name}` is never referenced"),
                );
            }
        }
        for (name, enum_def) in &self.enums {
            if !used_enums.contains(name) {
                report(
                    &mut self.diagnostics,
                    &enum_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("enum `{name}` is never referenced"),
                );
            }
        }
        for (name, trait_def) in &self.traits {
            if !used_traits.contains(name) {
                report(
                    &mut self.diagnostics,
                    &trait_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("trait `{name}` is never implemented"),
                );
            }
        }
    }
}

/// Parameter scope of one walk position.
struct ParamCtx<'a> {
    formals: &'a [String],
    query_owner: Option<QueryOwner>,
    siblings_allowed: bool,
}

impl<'a> ParamCtx<'a> {
    fn schema(formals: &'a [String], name: &str, siblings_allowed: bool) -> Self {
        Self {
            formals,
            query_owner: Some(QueryOwner::Schema(name.to_owned())),
            siblings_allowed,
        }
    }

    fn without_siblings(&self) -> Self {
        Self {
            formals: self.formals,
            query_owner: self.query_owner.clone(),
            siblings_allowed: false,
        }
    }
}

impl ParamCtx<'static> {
    fn closed() -> Self {
        Self {
            formals: &[],
            query_owner: None,
            siblings_allowed: false,
        }
    }
}

static EMPTY_FIELDS: BTreeMap<String, FieldOverloads> = BTreeMap::new();

/// Saturation point and limit of the parameterised-schema instance count
/// (`crates/rules/LANGUAGE.md` §10.1, check 4): counts saturate one above the
/// limit so "over 64" stays provable under saturating arithmetic.
const INSTANTIATION_CAP: u64 = 65;

impl RefKind {
    fn label(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Mixin => "mixin",
            Self::Type => "type",
            Self::Enum => "enum",
            Self::Trait => "trait",
        }
    }
}

/// The first named segment of a constructor argument.
pub(crate) fn first_name(argument: &expr::Argument) -> Option<String> {
    match argument {
        expr::Argument::Path(segments) | expr::Argument::Stripped { segments, .. } => {
            segments.iter().find_map(|segment| match segment {
                Segment::Name(name) => Some(name.clone()),
                Segment::Param(_) => None,
            })
        }
    }
}

/// All formal parameters appearing in a parsed expression.
fn expr_params(parsed: &Expr) -> Vec<Param> {
    fn walk(parsed: &Expr, out: &mut Vec<Param>) {
        for alternative in &parsed.alternatives {
            match alternative {
                Primary::Param(param) => out.push(param.clone()),
                Primary::Ref(argument)
                | Primary::Def(argument)
                | Primary::Enum(argument)
                | Primary::Scope(argument) => {
                    if let Some(segments) = argument.segments() {
                        for segment in segments {
                            if let Segment::Param(param) = segment {
                                out.push(param.clone());
                            }
                        }
                    }
                }
                Primary::Literal(parts) => {
                    for part in parts {
                        if let expr::LiteralPart::Hole(hole) = part {
                            walk(hole, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(parsed, &mut out);
    out
}

/// The first `$name` spelled in a raw string, if any.
fn first_param(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'$' {
            continue;
        }
        let next = bytes.get(index + 1)?;
        if next.is_ascii_alphabetic() || *next == b'_' {
            let rest = &raw[index + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            return Some(rest[..end].to_owned());
        }
    }
    None
}

/// Parses `Name`, `Name<F1,F2>`.
pub(crate) fn parse_schema_key(raw: &str) -> Result<(String, Vec<String>), String> {
    let raw = raw.trim();
    let Some(open) = raw.find('<') else {
        if is_ident(raw) {
            return Ok((raw.to_owned(), Vec::new()));
        }
        return Err(format!("`{raw}` is not a schema name"));
    };
    if !raw.ends_with('>') {
        return Err(format!("schema name `{raw}` is missing its closing `>`"));
    }
    let name = raw[..open].trim();
    if !is_ident(name) {
        return Err(format!("`{name}` is not a schema name"));
    }
    let mut formals = Vec::new();
    for formal in raw[open + 1..raw.len() - 1].split(',') {
        let formal = formal.trim();
        if !is_ident(formal) {
            return Err(format!("`{formal}` is not a formal parameter name"));
        }
        formals.push(formal.to_owned());
    }
    if formals.is_empty() {
        return Err(format!("schema `{name}` has an empty parameter list"));
    }
    Ok((name.to_owned(), formals))
}

/// Parses `self`, `Name`, or `Name<arg, arg>`.
pub(crate) fn parse_schema_ref(raw: &str) -> Result<SchemaRef, String> {
    let raw = raw.trim();
    if raw == "self" {
        return Ok(SchemaRef {
            name: "self".to_owned(),
            args: Vec::new(),
        });
    }
    let Some(open) = raw.find('<') else {
        if is_ident(raw) {
            return Ok(SchemaRef {
                name: raw.to_owned(),
                args: Vec::new(),
            });
        }
        return Err(format!("`{raw}` is not a schema reference"));
    };
    if !raw.ends_with('>') {
        return Err(format!(
            "schema reference `{raw}` is missing its closing `>`"
        ));
    }
    let name = raw[..open].trim();
    if !is_ident(name) {
        return Err(format!("`{name}` is not a schema name"));
    }
    let mut args = Vec::new();
    for argument in raw[open + 1..raw.len() - 1].split(',') {
        let argument = argument.trim();
        if argument.is_empty() {
            return Err(format!("schema `{name}` has an empty argument"));
        }
        if let Some(parameter) = argument.strip_prefix('$') {
            if !is_ident(parameter) {
                return Err(format!("`{argument}` is not a schema argument"));
            }
            args.push(Actual::Param {
                name: parameter.to_owned(),
            });
        } else if is_ident(argument) {
            args.push(Actual::Name(argument.to_owned()));
        } else {
            return Err(format!(
                "`{argument}` is not a schema argument (only names and `$parameters` qualify)"
            ));
        }
    }
    Ok(SchemaRef {
        name: name.to_owned(),
        args,
    })
}

/// Parses `1`, `0..1`, `1..*`, `2..5`.
pub(crate) fn parse_card(raw: &str) -> Result<(u32, Option<u32>), String> {
    let raw = raw.trim();
    let (lower, upper) = raw.split_once("..").unwrap_or((raw, raw));
    let lower = lower.trim();
    let upper = upper.trim();
    let min = lower
        .parse::<u32>()
        .map_err(|_| format!("`{raw}` is not a card"))?;
    let max = if upper == "*" {
        None
    } else {
        Some(
            upper
                .parse::<u32>()
                .map_err(|_| format!("`{raw}` is not a card"))?,
        )
    };
    if max.is_some_and(|max| max < min) {
        return Err(format!(
            "card `{raw}` has a lower bound above its upper bound"
        ));
    }
    Ok((min, max))
}

/// Parses `key`, `field:<name>`, `file`.
pub(crate) fn parse_def_name(raw: &str) -> Result<(), String> {
    let raw = raw.trim();
    if raw == "key" || raw == "file" {
        return Ok(());
    }
    if let Some(field) = raw.strip_prefix("field:")
        && is_ident(field.trim())
    {
        return Ok(());
    }
    Err(format!("`{raw}` is not a def name source"))
}

/// Parses `T` or `T.subtype`.
pub(crate) fn parse_def_type(raw: &str) -> Result<(String, Option<String>), String> {
    let raw = raw.trim();
    match raw.split_once('.') {
        Some((type_name, subtype)) if is_ident(type_name.trim()) && is_ident(subtype.trim()) => {
            Ok((type_name.trim().to_owned(), Some(subtype.trim().to_owned())))
        }
        None if is_ident(raw) => Ok((raw.to_owned(), None)),
        _ => Err(format!("`{raw}` is not a def type")),
    }
}

fn is_ident(raw: &str) -> bool {
    let mut chars = raw.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_')
}

/// Structural equality of two field specifications, ignoring presentation
/// fields (`doc`/`severity`/`deprecated`/`override`). Expressions compare as
/// parsed syntax, so `'a|b'` and `'a | b'` are identical.
fn field_structure_eq(left: &FieldSpec, right: &FieldSpec) -> bool {
    fn expr_eq(left: &Option<String>, right: &Option<String>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left == right
                    || match (expr::parse(left), expr::parse(right)) {
                        (Ok(left), Ok(right)) => left == right,
                        _ => false,
                    }
            }
            _ => false,
        }
    }
    fn map_eq(left: &Option<MapSpec>, right: &Option<MapSpec>) -> bool {
        match (left, right) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                expr_eq(&Some(left.key.clone()), &Some(right.key.clone()))
                    && expr_eq(&left.value, &right.value)
                    && left.body == right.body
            }
            _ => false,
        }
    }
    expr_eq(&left.key, &right.key)
        && expr_eq(&left.value, &right.value)
        && left.body == right.body
        && expr_eq(&left.list, &right.list)
        && map_eq(&left.map, &right.map)
        && left.card == right.card
        && left.scope == right.scope
        && left.def == right.def
        && left.control == right.control
}

/// Records one semantic diagnostic.
fn report(
    diagnostics: &mut Vec<Diagnostic>,
    at: &At,
    code: DiagnosticCode,
    severity: Severity,
    message: String,
) {
    diagnostics.push(Diagnostic {
        severity,
        code,
        message,
        file: at.file.clone(),
        pointer: at.pointer.clone(),
        column: None,
    });
}

/// Records one mini-syntax parse error.
fn report_parse(
    diagnostics: &mut Vec<Diagnostic>,
    at: &At,
    column: Option<usize>,
    message: String,
) {
    diagnostics.push(Diagnostic {
        severity: Severity::Error,
        code: DiagnosticCode::Parse,
        message,
        file: at.file.clone(),
        pointer: at.pointer.clone(),
        column,
    });
}

/// D14 card lint for one field map: shape overloads of one key must agree on
/// the cardinality upper bound, or one of them is almost certainly wrong.
///
/// Only the upper bound is compared: the legacy corpus has 576 overload pairs
/// that differ solely in the leaf-versus-node default (`0..1` versus `1`),
/// which is a migration artefact rather than a finding.
fn lint_field_cards(
    diagnostics: &mut Vec<Diagnostic>,
    at: &At,
    fields: &BTreeMap<String, FieldOverloads>,
) {
    for (key, overloads) in fields {
        let FieldOverloads::Many(specs) = overloads else {
            continue;
        };
        let maxima: BTreeSet<Option<u32>> = specs
            .iter()
            .filter_map(|spec| parse_card(&spec.card).ok().map(|(_, max)| max))
            .collect();
        if maxima.len() > 1 {
            report(
                diagnostics,
                &at.child(key),
                DiagnosticCode::CardLint,
                Severity::Info,
                format!(
                    "overloads of `{key}` disagree on the `card` upper bound ({}); \
                     check the arity",
                    specs
                        .iter()
                        .map(|spec| spec.card.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples of `crates/rules/LANGUAGE.md` folded into one fully
    /// referenced document: it must produce zero diagnostics.
    const CLEAN: &str = r#"{
  "files": {
    "events": {
      "path": "events",
      "ext": "txt",
      "root": "events_file"
    },
    "scripted_effects": {
      "path": "common/scripted_effects",
      "ext": "txt",
      "root": "scripted_effects_file"
    },
    "on_actions": {
      "path": "common/on_actions",
      "ext": "txt",
      "root": "on_actions_file"
    }
  },
  "schemas": {
    "events_file": {
      "fields": {
        "country_event": {
          "def": {
            "type": "event.country",
            "name": "field:id"
          },
          "scope": {
            "set": {
              "root": "country",
              "this": "country"
            }
          },
          "body": "event_body",
          "card": "0..*"
        }
      }
    },
    "event_body": {
      "include": [
        "gated"
      ],
      "fields": {
        "id": {
          "value": "scalar",
          "card": "1"
        },
        "title": {
          "value": "loc",
          "card": "0..1"
        },
        "is_triggered_only": {
          "value": "bool",
          "card": "0..1"
        },
        "mean_time_to_happen": {
          "body": "mtth",
          "card": "0..1"
        },
        "option": {
          "body": "event_option",
          "card": "0..*"
        },
        "picture": {
          "value": "ref<sprite> | enum<pictures>",
          "card": "0..*"
        }
      }
    },
    "event_option": {
      "fields": {
        "name": {
          "value": "loc",
          "card": "0..1"
        },
        "ai_chance": {
          "body": "mtth",
          "card": "0..1"
        }
      }
    },
    "mtth": {
      "fields": {
        "days": {
          "value": "int[0..]",
          "card": "0..1"
        },
        "factor": {
          "value": "float",
          "card": "0..1"
        }
      }
    },
    "scripted_effects_file": {
      "map": {
        "key": "def<scripted_effect>",
        "body": "effect"
      }
    },
    "effect": {
      "fields": {
        "add_prestige": {
          "value": "int",
          "card": "0..*"
        },
        "if": {
          "body": "self",
          "card": "0..*",
          "control": {
            "kind": "branch",
            "guard": "limit",
            "chain": [
              "else"
            ]
          }
        },
        "else": {
          "body": "self",
          "card": "0..*",
          "control": {
            "kind": "branch_continue"
          }
        },
        "limit": {
          "body": "trigger",
          "card": "0..1",
          "control": {
            "kind": "guard"
          }
        },
        "hidden_effect": {
          "body": "self",
          "card": "0..*",
          "control": {
            "kind": "transparent"
          }
        }
      },
      "patterns": [
        {
          "key": "link",
          "body": "self",
          "card": "0..*"
        }
      ]
    },
    "trigger": {
      "fields": {
        "always": {
          "value": "bool",
          "card": "0..1"
        },
        "has_country_modifier": {
          "value": "ref<event_modifier>",
          "card": "0..1"
        }
      },
      "patterns": [
        {
          "key": "link",
          "body": "self",
          "card": "0..*"
        }
      ]
    },
    "on_actions_file": {
      "patterns": [
        {"key": "enum<on_actions_country>", "body": "on_action_body<country>", "card": "0..*"},
        {"key": "enum<on_actions_province>", "body": "on_action_body<province>", "card": "0..*"}
      ]
    },
    "on_action_body<S>": {
      "fields": {
        "events": {
          "list": "ref<event.$S>",
          "card": "0..*"
        }
      }
    }
  },
  "mixins": {
    "gated": {
      "fields": {
        "potential": {
          "body": "trigger",
          "card": "0..1"
        }
      }
    }
  },
  "types": {
    "event": {
      "resolution": "replace",
      "subtypes": {
        "country": {},
        "province": {},
        "triggered": {}
      }
    },
    "scripted_effect": {
      "resolution": "replace",
      "impl": {
        "Template": {
          "body": "effect"
        }
      }
    },
    "event_modifier": {},
    "sprite": {}
  },
  "traits": {
    "Template": {}
  },
  "enums": {
    "pictures": [
      "one",
      "two"
    ],
    "on_actions_country": [
      "on_startup"
    ],
    "on_actions_province": [
      "on_province_religion_converted"
    ]
  },
  "scopes": {
    "types": [
      "country",
      "province"
    ],
    "registers": {
      "root": {
        "role": "root"
      },
      "this": {
        "role": "current"
      },
      "prev": {
        "role": "previous",
        "chain": true
      },
      "from": {
        "role": "from",
        "chain": true
      }
    },
    "links": {
      "owner": {
        "from": [
          "province"
        ],
        "to": "country"
      },
      "capital": {
        "from": [
          "country"
        ],
        "to": "province"
      }
    }
  }
}"#;

    fn run(sources: &[(&str, &str)]) -> Vec<Diagnostic> {
        let parsed = sources
            .iter()
            .map(|(name, json)| {
                (
                    (*name).to_owned(),
                    serde_json::from_str(&fill_cards(json)).expect("source parses"),
                )
            })
            .collect::<Vec<_>>();
        check(&parsed)
    }

    /// Fixtures predate the D14 mandatory `card`, so the corpus-neutral
    /// `0..1` is filled in mechanically and each fixture only spells what it
    /// is about. `missing_card_is_a_parse_error` covers the requirement.
    fn fill_cards(json: &str) -> String {
        let mut value: serde_json::Value = serde_json::from_str(json).expect("fixture is JSON");
        fill_cards_in(&mut value);
        serde_json::to_string(&value).expect("fixture serializes")
    }

    fn fill_cards_in(value: &mut serde_json::Value) {
        let serde_json::Value::Object(document) = value else {
            return;
        };
        for (key, entry) in document.iter_mut() {
            match key.as_str() {
                "schemas" => {
                    if let serde_json::Value::Object(schemas) = entry {
                        for schema in schemas.values_mut() {
                            fill_block_cards(schema);
                        }
                    }
                }
                "mixins" => {
                    if let serde_json::Value::Object(mixins) = entry {
                        for mixin in mixins.values_mut() {
                            if let Some(fields) = mixin.get_mut("fields") {
                                fill_field_cards(fields);
                            }
                        }
                    }
                }
                "files" => {
                    if let serde_json::Value::Object(files) = entry {
                        for rule in files.values_mut() {
                            if let Some(root) = rule.get_mut("root") {
                                fill_spec_cards(root);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// At schema position `{"map": …}` / `{"list": …}` are the shorthands;
    /// anything else is a block.
    fn fill_block_cards(schema: &mut serde_json::Value) {
        let serde_json::Value::Object(block) = schema else {
            return;
        };
        if block.len() == 1 && (block.contains_key("map") || block.contains_key("list")) {
            return;
        }
        if let Some(fields) = block.get_mut("fields") {
            fill_field_cards(fields);
        }
        if let Some(serde_json::Value::Array(patterns)) = block.get_mut("patterns") {
            for pattern in patterns {
                fill_spec_cards(pattern);
            }
        }
    }

    fn fill_field_cards(fields: &mut serde_json::Value) {
        let serde_json::Value::Object(fields) = fields else {
            return;
        };
        for overloads in fields.values_mut() {
            match overloads {
                serde_json::Value::Array(specs) => {
                    for spec in specs {
                        fill_spec_cards(spec);
                    }
                }
                spec => fill_spec_cards(spec),
            }
        }
    }

    fn fill_spec_cards(spec: &mut serde_json::Value) {
        let serde_json::Value::Object(spec) = spec else {
            return;
        };
        let is_field_spec = ["value", "body", "list", "map"]
            .iter()
            .any(|key| spec.contains_key(*key));
        if is_field_spec && !spec.contains_key("card") {
            spec.insert(
                "card".to_owned(),
                serde_json::Value::String("0..1".to_owned()),
            );
        }
    }

    fn errors(diagnostics: &[Diagnostic]) -> Vec<DiagnosticCode> {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .map(|diagnostic| diagnostic.code)
            .collect()
    }

    #[test]
    fn clean_document_produces_no_diagnostics() {
        assert_eq!(run(&[("core.json", CLEAN)]), Vec::new());
    }

    #[test]
    fn register_chaining_depends_on_role_instead_of_spelling() {
        for role in ["root", "current", "previous", "from"] {
            let source = format!(
                r#"{{"scopes":{{"registers":{{"arbitrary":{{"role":"{role}","chain":true}}}}}}}}"#
            );
            let diagnostics = run(&[("registers.json", &source)]);
            assert_eq!(
                errors(&diagnostics),
                if matches!(role, "root" | "current") {
                    vec![DiagnosticCode::ScopeReferenceError]
                } else {
                    vec![]
                },
                "{role}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn switch_control_requires_an_explicit_resolvable_selector_schema() {
        for (control, expected) in [
            (
                r#"{"kind":"switch","on":"selector","selector_schema":"predicates"}"#,
                None,
            ),
            (
                r#"{"kind":"switch","on":"selector"}"#,
                Some(DiagnosticCode::Parse),
            ),
            (
                r#"{"kind":"switch","selector_schema":"predicates"}"#,
                Some(DiagnosticCode::Parse),
            ),
            (
                r#"{"kind":"switch","on":"selector","selector_schema":"missing"}"#,
                Some(DiagnosticCode::UndefinedReference),
            ),
            (
                r#"{"kind":"switch","on":"selector","selector_schema":"self"}"#,
                Some(DiagnosticCode::Parse),
            ),
            (
                r#"{"kind":"transparent","selector_schema":"predicates"}"#,
                Some(DiagnosticCode::Parse),
            ),
        ] {
            let source = serde_json::json!({
                "files": {"f": {"path": "events", "root": "effect"}},
                "schemas": {
                    "predicates": {"fields": {"fixed": {"value": "bool"}}},
                    "effect": {"fields": {"dispatch": {
                        "body": "self",
                        "control": serde_json::from_str::<serde_json::Value>(control).unwrap()
                    }}}
                }
            })
            .to_string();
            let diagnostics = run(&[("control.json", &source)]);
            assert_eq!(
                errors(&diagnostics),
                expected.into_iter().collect::<Vec<_>>(),
                "{control}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn constant_control_requires_a_scalar_boolean_predicate() {
        let valid = run(&[(
            "constant.json",
            r#"{"schemas":{"predicate":{"fields":{
            "fixed":{"value":"bool","control":{"kind":"constant"}}
        }}}}"#,
        )]);
        assert!(errors(&valid).is_empty(), "{valid:?}");
        for payload in [r#""value":"int""#, r#""body":"self""#] {
            let source = format!(
                r#"{{"schemas":{{"predicate":{{"fields":{{"fixed":{{{payload},"control":{{"kind":"constant"}}}}}}}}}}}}"#
            );
            let invalid = run(&[("constant.json", &source)]);
            assert!(
                invalid
                    .iter()
                    .any(|item| item.severity == Severity::Error
                        && item.pointer.ends_with("/control")),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn block_forms_reject_missing_targets_invalid_bounds_and_empty_branches() {
        let diagnostics = run(&[(
            "forms.json",
            r#"{
            "files": {"test": {"path":"test", "root":"s"}},
            "schemas": {"s": {
                "fields": {"x": {"value":"int", "card":"0..1"}},
                "patterns": [{"key":"scalar", "value":"int", "card":"0..1"}],
                "forms": [
                    {"fields":{"missing":"1"}},
                    {"patterns":{"1":"1"}},
                    {"fields":{"x":"2..1"}},
                    {}
                ]
            }}
        }"#,
        )]);
        let errors = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 4, "{diagnostics:#?}");
        assert!(
            errors
                .iter()
                .all(|diagnostic| diagnostic.code == DiagnosticCode::Parse
                    && diagnostic.pointer.starts_with("/schemas/s/forms/"))
        );
    }

    #[test]
    fn block_forms_resolve_case_insensitive_included_fields() {
        let diagnostics = run(&[(
            "forms.json",
            r#"{
            "files": {"test": {"path":"test", "root":"s"}},
            "schemas": {"s": {"include":["shared"], "forms":[{"fields":{"X":"1"}}]}},
            "mixins": {"shared": {"fields":{"x":{"value":"int","card":"0..1"}}}}
        }"#,
        )]);
        assert!(errors(&diagnostics).is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn parse_errors_report_expression_columns() {
        let diagnostics = run(&[(
            "core.json",
            r#"{ "schemas": { "s": { "fields": { "a": { "value": "int[1.5..2]" } } } } }"#,
        )]);
        assert_eq!(errors(&diagnostics), vec![DiagnosticCode::Parse]);
        let parse = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == DiagnosticCode::Parse)
            .expect("parse diagnostic");
        assert_eq!(parse.column, Some(5), "{diagnostics:?}");
        assert_eq!(parse.pointer, "/schemas/s/fields/a/value");
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let diagnostics = run(&[
            (
                "one.json",
                r#"{ "schemas": { "s": { "fields": { "a": { "value": "bool" } } } } }"#,
            ),
            (
                "two.json",
                r#"{ "schemas": { "s": { "fields": { "b": { "value": "bool" } } } } }"#,
            ),
        ]);
        assert_eq!(errors(&diagnostics), vec![DiagnosticCode::DuplicateName]);
    }

    #[test]
    fn undefined_references_are_errors() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "events_file" } },
              "schemas": {
                "events_file": { "include": ["missing_mixin"], "fields": {
                  "a": { "value": "ref<missing_type> | enum<missing_enum>" },
                  "b": { "body": "missing_schema" },
                  "c": { "def": { "type": "known.missing_subtype" }, "value": "scalar" }
                }},
                "known": { "fields": { "z": { "value": "bool" } } }
              },
              "types": { "known": {"impl":{"MissingTrait":{}}} }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UndefinedReference,
            ],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn unused_definitions_warn() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "root_schema" } },
              "schemas": {
                "root_schema": { "fields": { "a": { "value": "bool" } } },
                "orphan": { "fields": { "b": { "value": "bool" } } }
              },
              "mixins": { "unused_mixin": { "fields": { "c": { "value": "bool" } } } },
              "types": { "unused_type": {} },
              "enums": { "unused_enum": ["x"] },
              "traits": { "UnusedTrait": {} }
            }"#,
        )]);
        let warnings = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            warnings,
            vec![
                "enum `unused_enum` is never referenced",
                "mixin `unused_mixin` is never included",
                "schema `orphan` is never referenced",
                "trait `UnusedTrait` is never implemented",
                "type `unused_type` is never referenced",
            ],
            "{diagnostics:?}"
        );
        assert_eq!(errors(&diagnostics), Vec::new());
    }

    #[test]
    fn include_conflicts_require_explicit_override() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "mixins": {
                "one":   { "fields": { "shared": { "value": "bool" } } },
                "two":   { "fields": { "shared": { "value": "bool" } } },
                "other": { "fields": { "taken": { "value": "bool" } } }
              },
              "schemas": {
                "bad":  { "include": ["one", "two", "other"],
                          "fields": { "taken": { "value": "bool" } } },
                "good": { "include": ["other"], "fields": {
                  "taken": { "value": "bool", "override": true }
                }}
              }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::IncludeConflict,
                DiagnosticCode::IncludeConflict,
            ],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn unreachable_overloads_and_covered_patterns_are_errors() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "schemas": {
                "s": {
                  "fields": {
                    "a": [ { "value": "loc" }, { "value": "loc" }, { "value": "bool" } ]
                  },
                  "patterns": [
                    { "key": "'x'", "value": "loc" },
                    { "key": "'x'", "value": "loc" },
                    { "key": "'y'", "value": "loc" }
                  ]
                }
              }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::UnreachableOverload,
                DiagnosticCode::UnreachableOverload,
            ],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn parameter_positions_and_names_are_checked() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "schemas": {
                "free": {
                  "fields": {
                    "a": { "value": "$key" },
                    "b": { "value": "scalar", "scope": { "push": "$S" } }
                  }
                },
                "box<S>": {
                  "fields": { "c": { "value": "ref<$Other>" } }
                }
              }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::ParameterError,
                DiagnosticCode::ParameterError,
                DiagnosticCode::ParameterError,
            ],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn instantiation_cap_is_enforced() {
        let fields = (0..65)
            .map(|index| format!("\"f{index}\": {{\"body\": \"boxed<m{index}>\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            r#"{{"files":{{"big":{{"path":"x","ext":"txt","root":"big_file"}}}},"schemas":{{"big_file":{{"fields":{{{fields}}}}},"boxed<S>":{{"fields":{{"x":{{"value":"scalar"}}}}}}}}}}"#
        );
        let diagnostics = run(&[("core.json", &source)]);
        assert_eq!(
            errors(&diagnostics),
            vec![DiagnosticCode::ParameterError],
            "{diagnostics:?}"
        );
        assert!(
            diagnostics[0].message.contains("64"),
            "{:?}",
            diagnostics[0]
        );
    }

    #[test]
    fn repeated_concrete_calls_share_one_instance_for_the_cap() {
        let fields = (0..100)
            .map(|index| format!("\"f{index}\": {{\"body\": \"boxed<scalar>\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!(
            r#"{{"files":{{"test":{{"path":"x","ext":"txt","root":"root"}}}},"schemas":{{"root":{{"fields":{{{fields}}}}},"boxed<S>":{{"fields":{{"value":{{"value":"$S"}}}}}}}}}}"#
        );
        let diagnostics = run(&[("core.json", &source)]);
        assert!(errors(&diagnostics).is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn scope_names_and_link_from_lists_are_checked() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
  "schemas": {
    "s": {
      "fields": {
        "a": {
          "value": "bool",
          "scope": {
            "in": [
              "nowhere"
            ],
            "push": "any",
            "set": {
              "ghost": "country",
              "root": "nowhere"
            }
          }
        }
      }
    }
  },
  "scopes": {
    "types": [
      "country",
      "any",
      "country"
    ],
    "registers": {
      "root": {
        "role": "root"
      }
    },
    "links": {
      "empty": {
        "from": [],
        "to": "country"
      },
      "stray": {
        "from": [
          "nowhere"
        ],
        "to": "country"
      }
    },
    "compat": [
      {
        "actual": "nowhere",
        "expected": "country"
      }
    ]
  }
}"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::ScopeReferenceError,
                DiagnosticCode::DuplicateName,
            ],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn diagnostics_are_sorted_deterministically() {
        let first = run(&[("core.json", CLEAN)]);
        let second = run(&[("core.json", CLEAN)]);
        assert_eq!(first, second);
    }

    /// D14: a `script` file entry without `root` validates nothing, so it is
    /// rejected instead of silently accepted.
    #[test]
    fn script_files_must_declare_a_root() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": {
                "rooted":   { "path": "a", "ext": "txt", "root": "s" },
                "rootless": { "path": "b", "ext": "txt" },
                "localisation": { "path": "localisation", "ext": "yml", "parser": "localisation" }
              },
              "schemas": { "s": { "fields": { "k": { "value": "bool" } } } }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![DiagnosticCode::Parse],
            "{diagnostics:?}"
        );
        assert!(
            diagnostics[0].pointer.ends_with("rootless"),
            "{diagnostics:?}"
        );
    }

    /// D14: `map` needs exactly one value shape, and a trait binding is
    /// either a localisation key or a sprite — never neither, never both.
    #[test]
    fn maps_and_bindings_declare_exactly_one_shape() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "s" } },
              "schemas": { "s": { "fields": {
                "both":  { "map": { "key": "scalar", "value": "bool", "body": "s" } },
                "neither": { "map": { "key": "scalar" } }
              }}},
              "traits": {"T": {}},
              "types": {"test": {"impl": {"T": {
                "both": {"loc": "$", "sprite": "$"},
                "neither": {}
              }}}}
            }"#,
        )]);
        // Two `map` payloads and two type implementation bindings are each malformed.
        assert_eq!(
            errors(&diagnostics),
            vec![
                DiagnosticCode::Parse,
                DiagnosticCode::Parse,
                DiagnosticCode::Parse,
                DiagnosticCode::Parse
            ],
            "{diagnostics:?}"
        );
    }

    /// D14 card lint: `0..0` disables a field, `N..N` is a tuple, and
    /// overloads of one key must agree on the cardinality.
    #[test]
    fn card_lints_flag_disabling_tuples_and_disagreement() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "s" } },
              "schemas": { "s": { "fields": {
                "disabled": { "value": "bool", "card": "0..0" },
                "tuple":    { "value": "scalar", "card": "3..3" },
                "mixed": [
                  { "value": "scalar", "card": "1" },
                  { "body": "s", "card": "0..*" }
                ]
              }}}
            }"#,
        )]);
        let card_lints: Vec<&Diagnostic> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == DiagnosticCode::CardLint)
            .collect();
        assert_eq!(card_lints.len(), 3, "{diagnostics:?}");
        assert_eq!(
            card_lints
                .iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Warning)
                .count(),
            1,
            "{diagnostics:?}"
        );
    }
    #[test]
    fn queries_resolve_schema_scope_and_reachability() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "scopes": { "types": ["country"] },
            "files": { "test": { "path": "test", "ext": "txt", "root": "root" } },
            "schemas": {
                "root": { "fields": { "selected": { "value": "keysof<source,scope_accepts=country,shape=scalar,value_kind_any=(int|float|bool),capability=exportable>" } } },
                "source": { "fields": { "value": { "value": "int" } } }
            }
        }"#,
        )]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        let diagnostics = run(&[(
            "query.json",
            r#"{
            "schemas": { "root": { "items": "keysof<missing,scope_accepts=unknown>" } }
        }"#,
        )]);
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.code
            == DiagnosticCode::UndefinedReference
            && diagnostic.message.contains("missing")));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic.code
            == DiagnosticCode::ScopeReferenceError
            && diagnostic.message.contains("unknown")));
    }

    #[test]
    fn queries_validate_dependent_siblings_in_fields_patterns_and_items() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "files": { "test": { "path": "test", "ext": "txt", "root": "root" } },
            "schemas": {
                "root": {
                    "include": ["selector"],
                    "fields": { "value": { "value": "valuesof<source,key=sibling<on_trigger>>" } },
                    "patterns": [{ "key": "valuesof<source,key=sibling<on_trigger>>", "value": "scalar" }],
                    "items": "valuesof<source,key=sibling<ON_TRIGGER>>"
                },
                "source": { "fields": { "value": { "value": "int" } } }
            },
            "mixins": { "selector": { "fields": { "on_trigger": { "value": "keysof<source>" } } } }
        }"#,
        )]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn queries_reject_undeclared_or_out_of_context_siblings() {
        for schema in [
            r#"{ "fields": { "value": { "value": "valuesof<source,key=sibling<missing>>" } } }"#,
            r#"{ "items": "valuesof<source,key=sibling<missing>>" }"#,
            r#"{ "list": "valuesof<source,key=sibling<missing>>" }"#,
            r#"{ "map": { "key": "valuesof<source,key=sibling<missing>>", "value": "scalar" } }"#,
            r#"{ "fields": { "on_trigger": { "value": "scalar" }, "value": { "list": "valuesof<source,key=sibling<on_trigger>>" } } }"#,
            r#"{ "fields": { "on_trigger": { "value": "scalar" }, "value": { "map": { "key": "valuesof<source,key=sibling<on_trigger>>", "value": "scalar" } } } }"#,
        ] {
            let json = format!(
                r#"{{ "schemas": {{ "root": {schema}, "source": {{ "fields": {{ "value": {{ "value": "int" }} }} }} }} }}"#
            );
            let diagnostics = run(&[("query.json", &json)]);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == Severity::Error
                        && diagnostic.message.contains("sibling")),
                "{schema}: {diagnostics:?}"
            );
        }
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "files": { "test": { "path": "test", "ext": "txt", "root": { "value": "valuesof<source,key=sibling<missing>>" } } },
            "schemas": { "source": { "fields": { "value": { "value": "int" } } } }
        }"#,
        )]);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error
                    && diagnostic.message.contains("sibling")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn queries_validate_mixin_siblings_in_each_including_schema() {
        let json = r#"{
            "schemas": {
                "good": { "include": ["dependent"], "fields": { "selector": { "value": "scalar" } } },
                "bad": { "include": ["dependent"] },
                "source": { "fields": { "value": { "value": "int" } } }
            },
            "mixins": { "dependent": { "fields": { "value": { "value": "valuesof<source,key=sibling<selector>>" } } } }
        }"#;
        let diagnostics = run(&[("query.json", json)]);
        assert!(
            diagnostics.iter().any(|diagnostic| diagnostic.code
                == DiagnosticCode::UndefinedReference
                && diagnostic.message.contains("selector")),
            "{diagnostics:?}"
        );
        let valid = json.replace("\"bad\": { \"include\": [\"dependent\"] },", "");
        let diagnostics = run(&[("query.json", &valid)]);
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == Severity::Error),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn queries_in_template_holes_are_validated() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "schemas": { "root": { "fields": { "value": { "value": "'prefix_{keysof<missing>}'" } } } }
        }"#,
        )]);
        assert!(
            diagnostics.iter().any(|diagnostic| diagnostic.code
                == DiagnosticCode::UndefinedReference
                && diagnostic.message.contains("missing")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn queries_allow_non_dependent_self_schema_projection() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "files": { "test": { "path": "test", "ext": "txt", "root": "root" } },
            "schemas": { "root": { "fields": { "value": { "value": "keysof<root>" }, "nested": { "body": "self" } } } }
        }"#,
        )]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
    #[test]
    fn queries_reject_value_projection_dependency_cycles() {
        for schemas in [
            r#"{ "root": { "fields": { "value": { "value": "valuesof<root,key=value>" } } } }"#,
            r#"{
                "root": { "fields": { "value": { "value": "valuesof<other,key=value>" } } },
                "other": { "fields": { "value": { "value": "valuesof<root,key=value>" } } }
            }"#,
            r#"{ "root": { "patterns": [{ "key": "keysof<root>", "value": "scalar" }] } }"#,
        ] {
            let json = format!(r#"{{ "schemas": {schemas} }}"#);
            let diagnostics = run(&[("query.json", &json)]);
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == Severity::Error
                        && diagnostic.message.contains("cycl")),
                "{schemas}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn queries_do_not_treat_unselected_values_as_dependencies() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "files": { "test": { "path": "test", "ext": "txt", "root": "root" } },
            "schemas": { "root": { "fields": {
                "selected": { "value": "int" },
                "value": { "value": "valuesof<root,key=selected>" },
                "nested": { "body": "self" }
            } } }
        }"#,
        )]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
    #[test]
    fn queries_do_not_depend_on_filtered_out_key_matchers() {
        let diagnostics = run(&[(
            "query.json",
            r#"{
            "files": { "test": { "path": "test", "ext": "txt", "root": "root" } },
            "schemas": { "root": {
                "fields": { "value": { "value": "int" } },
                "patterns": [{ "key": "keysof<root,shape=scalar>", "body": "self" }]
            } }
        }"#,
        )]);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}
