//! Compile-time semantic checks for rules-v2 sources
//! (`docs/rules-language.md` §10).
//!
//! [`check`] parses every mini-syntax string with provenance (source file +
//! JSON pointer + expression-internal column) and then runs the eight
//! normative semantic checks of §10.1, plus the cross-source duplicate-name
//! rule of §1. Nothing here lowers to the runtime IR; the pass is the
//! `rulec check` payload.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::expr::{self, Expr, Param, Primary, Segment};
use crate::source::{
    EnumSpec, ExtSpec, FieldOverloads, FieldSpec, MapSpec, MixinSpec, RootSpec, RuleFile,
    SchemaSpec, Severity, SourceParser, TraitSpec, TypeSpec,
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
    /// Source file name as listed by the manifest.
    pub file: String,
    /// JSON pointer of the offending value.
    pub pointer: String,
    /// 1-based column within the expression, for parse errors only.
    pub column: Option<usize>,
}

/// The closed set of `rulec` compile diagnostics (`docs/rules-language.md`
/// §11). These are rule-source diagnostics, distinct from the script
/// diagnostics in `docs/diagnostics.md`.
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
    /// Check 5: a `when`-read field carries `when`/`unless`.
    SubtypeWhenDependency,
    /// Check 6: a scope name or link `from` violation.
    ScopeReferenceError,
    /// Check 7: a trait `requires` does not hold.
    UnsatisfiedTraitRequirement,
    /// Check 8: a type impls one trait twice.
    DuplicateTraitImpl,
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
            Self::SubtypeWhenDependency => "SubtypeWhenDependency",
            Self::ScopeReferenceError => "ScopeReferenceError",
            Self::UnsatisfiedTraitRequirement => "UnsatisfiedTraitRequirement",
            Self::DuplicateTraitImpl => "DuplicateTraitImpl",
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

/// Runs parsing and the eight semantic checks over merged rule sources.
///
/// `sources` is `(source file name, parsed file)` in manifest order. The
/// returned diagnostics are sorted by `(file, pointer, code)` so runs are
/// deterministic.
pub fn check(sources: &[(String, RuleFile)]) -> Vec<Diagnostic> {
    let mut checker = Checker::new(sources);
    checker.collect();
    checker.check_include_conflicts();
    checker.check_unreachable_overloads();
    checker.check_parameters();
    checker.check_when_dependencies();
    checker.check_scopes();
    checker.check_traits();
    checker.check_subtype_gates();
    checker.check_card_disagreement();
    checker.check_undefined_references();
    checker.check_unused_definitions();
    let Checker { mut diagnostics, .. } = checker;
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
struct SchemaRef {
    /// Schema name, or `"self"` for the enclosing schema.
    name: String,
    /// Actual arguments at the call site.
    args: Vec<Actual>,
}

/// One actual argument of a schema call.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Actual {
    /// A concrete name.
    Name(String),
    /// `$name` or `$name.column`.
    Param { name: String, column: Option<String> },
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
    /// Enum name of the site's `map`/`pattern` key expression, when the key
    /// is `enum<E>`; it gives `$key` its domain.
    key_enum: Option<String>,
}

/// One use of a formal parameter (`$name[.column]`).
#[derive(Clone, Debug)]
struct ParamUse {
    name: String,
    at: At,
    /// Formals of the enclosing parameterised schema.
    formals: Vec<String>,
    /// Whether the use sits under a `map`/`pattern` (where `$key` binds).
    in_map: bool,
    what: &'static str,
}

/// One symbol-defining position (`def`).
#[derive(Clone, Debug)]
struct DefSite {
    type_name: String,
    /// Instance-body schema name, when the position carries a block body.
    body: Option<String>,
    at: At,
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
    defs: Vec<DefSite>,
    /// Files `root` schema names, for schema reachability.
    root_schemas: Vec<String>,
    /// `when` conditions per type: (type name, subtype name, field, at).
    when_reads: Vec<(String, String, String, At)>,
    /// Field-level `when`/`unless` gates: (body schema, subtype, at).
    subtype_gates: Vec<(String, String, At)>,
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
            defs: Vec::new(),
            root_schemas: Vec::new(),
            when_reads: Vec::new(),
            subtype_gates: Vec::new(),
        }
    }

    /// Parses one type expression, recording parse errors (including the
    /// union-shape rule) and collecting parameter uses.
    fn expr(&mut self, at: &At, raw: &str, ctx: &ParamCtx) -> Option<Expr> {
        match expr::parse(raw) {
            Ok(parsed) => {
                if let Err(mixed) = union_shape(&parsed) {
                    report_parse(&mut self.diagnostics, 
                        at,
                        None,
                        format!(
                            "union mixes {mixed} branches; all branches must share one shape \
                             (express shape variation with field-array overloads)"
                        ),
                    );
                }
                for param in expr_params(&parsed) {
                    self.params.push(ParamUse {
                        name: param.name,
                        at: at.clone(),
                        formals: ctx.formals.to_vec(),
                        in_map: ctx.in_map,
                        what: "a type expression",
                    });
                }
                Some(parsed)
            }
            Err(failure) => {
                report_parse(&mut self.diagnostics, at, Some(failure.column), failure.message);
                None
            }
        }
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
                            in_map: ctx.in_map,
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
            report(&mut self.diagnostics, 
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
                    report(&mut self.diagnostics, 
                        &at,
                        DiagnosticCode::ScopeReferenceError,
                        Severity::Error,
                        "`any` is reserved and must not be declared as a scope type".to_owned(),
                    );
                }
                if !self.scope_types.insert(name.clone()) {
                    report(&mut self.diagnostics, 
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
                    report(&mut self.diagnostics, 
                        &at,
                        DiagnosticCode::DuplicateName,
                        Severity::Error,
                        format!("register `{name}` is declared more than once"),
                    );
                }
                self.registers.insert(name.clone());
                let _ = register;
            }
            for (name, link) in &scopes.links {
                let at = base.child("links").child(name);
                self.forbid_params(&at, name, "a link name");
                if !link_names.insert(name.clone()) {
                    report(&mut self.diagnostics, 
                        &at,
                        DiagnosticCode::DuplicateName,
                        Severity::Error,
                        format!("link `{name}` is declared more than once"),
                    );
                }
                match expr::parse_template(name) {
                    Ok(_) => {}
                    Err(failure) => report_parse(&mut self.diagnostics, &at, Some(failure.column), failure.message),
                }
                if link.from.is_empty() {
                    report(&mut self.diagnostics, 
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
                self.forbid_params(&at.child("exclude").index(index), prefix, "a files exclusion");
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
                    self.collect_field_payload(&at.child("root"), "", &[], false, None, field);
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
                report(&mut self.diagnostics, 
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
                    let ctx = ParamCtx {
                        formals: &[],
                        in_map: true,
                    };
                    let owner = Some(name.as_str());
                    if let Some(parsed) = self.expr(&at.child("map").child("key"), &map.key, &ctx) {
                        self.collect_refs_from_expr(&at.child("map").child("key"), &parsed, owner);
                        self.collect_def_shorthand(
                            &at.child("map").child("key"),
                            &parsed,
                            map.body.as_deref(),
                        );
                    }
                    if let Some(value) = &map.value
                        && let Some(parsed) = self.expr(&at.child("map").child("value"), value, &ctx)
                    {
                        self.collect_refs_from_expr(&at.child("map").child("value"), &parsed, owner);
                    }
                    if let Some(body) = &map.body {
                        self.collect_body_ref(
                            &at.child("map").child("body"),
                            body,
                            &ctx,
                            Some(map.key.as_str()),
                            owner,
                        );
                    }
                }
                SchemaSpec::List { list } => {
                    self.schemas.insert(name.clone(), def);
                    self.expr(&at.child("list"), list, &ParamCtx::closed());
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
                report(&mut self.diagnostics, 
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
            self.collect_field_map(&at, name, &[], false, None, &mixin.fields);
        }

        for (name, spec) in &file.types {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/types", name),
            };
            self.forbid_params(&at, name, "a type name");
            if self.types.contains_key(name) {
                report(&mut self.diagnostics, 
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
            for trait_name in spec.trait_impls.keys() {
                self.references.push(Reference {
                    kind: RefKind::Trait,
                    name: trait_name.clone(),
                    subtype: None,
                    owner: None,
                    at: at.child("impl"),
                });
            }
            for (subtype_name, subtype) in &spec.subtypes {
                let subtype_at = at.child("subtypes").child(subtype_name);
                self.forbid_params(&subtype_at, subtype_name, "a subtype name");
                for trait_name in subtype.trait_impls.keys() {
                    self.references.push(Reference {
                        kind: RefKind::Trait,
                        name: trait_name.clone(),
                        subtype: None,
                        owner: None,
                        at: subtype_at.child("impl"),
                    });
                }
                if let Some(when) = &subtype.when {
                    for (field, value) in &when.0 {
                        self.forbid_params(&subtype_at.child("when"), field, "a when field name");
                        let value_at = subtype_at.child("when").child(field);
                        if let Some(value) = value
                            && let Some(parsed) =
                                self.expr(&value_at, value, &ParamCtx::closed())
                        {
                            self.collect_refs_from_expr(&value_at, &parsed, None);
                        }
                        self.when_reads.push((
                            name.clone(),
                            subtype_name.clone(),
                            field.clone(),
                            value_at,
                        ));
                    }
                }
            }
        }

        for (name, spec) in &file.traits {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/traits", name),
            };
            self.forbid_params(&at, name, "a trait name");
            if self.traits.contains_key(name) {
                report(&mut self.diagnostics, 
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
            if let Some(requires) = &spec.requires
                && let Some(include) = &requires.include {
                    self.references.push(Reference {
                        kind: RefKind::Mixin,
                        name: include.clone(),
                        subtype: None,
                        owner: None,
                        at: at.child("requires").child("include"),
                    });
                }
            for (binding_name, binding) in &spec.bindings {
                let binding_at = at.child("bindings").child(binding_name);
                if binding.loc.is_some() == binding.sprite.is_some() {
                    report(&mut self.diagnostics, 
                        &binding_at,
                        DiagnosticCode::Parse,
                        Severity::Error,
                        format!(
                            "trait binding `{binding_name}` must declare exactly one of \
                             `loc` / `sprite`"
                        ),
                    );
                }
                for (key, value) in [
                    ("loc", binding.loc.as_deref()),
                    ("sprite", binding.sprite.as_deref()),
                ] {
                    if let Some(value) = value {
                        self.forbid_params(&binding_at.child(key), value, "a binding template");
                    }
                }
            }
        }

        for (name, spec) in &file.enums {
            let at = At {
                file: file_name.to_owned(),
                pointer: join_pointer("/enums", name),
            };
            self.forbid_params(&at, name, "an enum name");
            if self.enums.contains_key(name) {
                report(&mut self.diagnostics, 
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
                EnumSpec::Table { columns, rows } => {
                    for (column, kind) in columns {
                        self.forbid_params(&at.child("columns").child(column), column, "a column name");
                        match parse_column(kind) {
                            Ok((parsed, optional)) => {
                                if parsed != "scope_type" {
                                    report_parse(&mut self.diagnostics, 
                                        &at.child("columns").child(column),
                                        None,
                                        format!("unknown column kind `{parsed}` (expected `scope_type`)"),
                                    );
                                }
                                let _ = optional;
                            }
                            Err(failure) => report_parse(&mut self.diagnostics, 
                                &at.child("columns").child(column),
                                None,
                                failure,
                            ),
                        }
                    }
                    for (row, values) in rows {
                        self.forbid_params(&at.child("rows").child(row), row, "an enum row name");
                        for (column, value) in values {
                            self.forbid_params(
                                &at.child("rows").child(row).child(column),
                                value,
                                "an enum row value",
                            );
                        }
                    }
                }
            }
        }
    }

    /// Collects the fields and patterns of one block schema.
    fn collect_schema_fields(
        &mut self,
        at: &At,
        name: &str,
        formals: &[String],
        block: &crate::source::BlockSchema,
    ) {
        let _ = name;
        self.collect_field_map(at, name, formals, false, None, &block.fields);
        for (index, pattern) in block.patterns.iter().enumerate() {
            let pattern_at = at.child("patterns").index(index);
            let ctx = ParamCtx {
                formals,
                in_map: true,
            };
            if let Some(key) = &pattern.key {
                if let Some(parsed) = self.expr(&pattern_at.child("key"), key, &ctx) {
                    self.collect_refs_from_expr(&pattern_at.child("key"), &parsed, Some(name));
                    self.collect_def_shorthand(
                        &pattern_at.child("key"),
                        &parsed,
                        pattern.body.as_deref(),
                    );
                }
            } else {
                report(&mut self.diagnostics, 
                    &pattern_at,
                    DiagnosticCode::Parse,
                    Severity::Error,
                    "a pattern must declare a `key` type expression".to_owned(),
                );
            }
            self.collect_field_payload(&pattern_at, name, formals, true, pattern.key.as_deref(), pattern);
        }
        if let Some(items) = &block.items
            && let Some(parsed) = self.expr(&at.child("items"), items, &ParamCtx::closed())
        {
            self.collect_refs_from_expr(&at.child("items"), &parsed, Some(name));
        }
    }

    fn collect_field_map(
        &mut self,
        at: &At,
        schema: &str,
        formals: &[String],
        in_map: bool,
        key_expr: Option<&str>,
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
                self.collect_field_payload(&field_at, schema, formals, in_map, key_expr, field);
            }
        }
    }

    /// Collects one field specification's mini-syntax, references, calls,
    /// params, and def positions.
    fn collect_field_payload(
        &mut self,
        at: &At,
        schema: &str,
        formals: &[String],
        in_map: bool,
        key_expr: Option<&str>,
        field: &FieldSpec,
    ) {
        let ctx = ParamCtx { formals, in_map };
        let owner = (!schema.is_empty()).then_some(schema);
        if let Some(key) = &field.key
            && let Some(parsed) = self.expr(&at.child("key"), key, &ctx) {
                self.collect_enum_ref(&at.child("key"), &parsed, owner);
            }
        let mut payload_count = 0usize;
        if let Some(value) = &field.value {
            payload_count += 1;
            if let Some(parsed) = self.expr(&at.child("value"), value, &ctx) {
                self.collect_refs_from_expr(&at.child("value"), &parsed, owner);
                self.collect_def_shorthand(&at.child("value"), &parsed, field.body.as_deref());
            }
        }
        if let Some(body) = &field.body {
            payload_count += 1;
            self.collect_body_ref(&at.child("body"), body, &ctx, key_expr, owner);
        }
        if let Some(list) = &field.list {
            payload_count += 1;
            if let Some(parsed) = self.expr(&at.child("list"), list, &ctx) {
                self.collect_refs_from_expr(&at.child("list"), &parsed, owner);
                self.collect_def_shorthand(&at.child("list"), &parsed, field.body.as_deref());
            }
        }
        if let Some(map) = &field.map {
            payload_count += 1;
            let map_at = at.child("map");
            let map_ctx = ParamCtx {
                formals,
                in_map: true,
            };
            if let Some(parsed) = self.expr(&map_at.child("key"), &map.key, &map_ctx) {
                self.collect_refs_from_expr(&map_at.child("key"), &parsed, owner);
                self.collect_def_shorthand(&map_at.child("key"), &parsed, map.body.as_deref());
            }
            if let Some(value) = &map.value
                && let Some(parsed) = self.expr(&map_at.child("value"), value, &map_ctx)
            {
                self.collect_refs_from_expr(&map_at.child("value"), &parsed, owner);
            }
            if let Some(body) = &map.body {
                self.collect_body_ref(&map_at.child("body"), body, &map_ctx, Some(&map.key), owner);
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
            report(&mut self.diagnostics, 
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
                self.forbid_params(&scope_at.child("set").child(register), register, "a scope effect");
                self.forbid_params(&scope_at.child("set").child(register), target, "a scope effect");
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
                    self.defs.push(DefSite {
                        type_name,
                        body: field.body.as_ref().map(|body| {
                            parse_schema_ref(body)
                                .map(|reference| reference.name)
                                .unwrap_or_default()
                        }),
                        at: def_at.clone(),
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
        for (cond, label) in [(&field.when, "when"), (&field.unless, "unless")] {
            if let Some(subtype) = cond {
                self.forbid_params(&at.child(label), subtype, "a subtype gate");
                self.subtype_gates.push((schema.to_owned(), subtype.clone(), at.child(label)));
            }
        }
    }

    /// Collects the reference and call site of one `body` string.
    fn collect_body_ref(
        &mut self,
        at: &At,
        raw: &str,
        ctx: &ParamCtx,
        key_expr: Option<&str>,
        owner: Option<&str>,
    ) {
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
                key_enum: key_expr.and_then(enum_name),
            });
        }
    }


    /// Collects `def<T>` shorthand positions of one expression (§4.2): the
    /// instance body is the enclosing field's `body`, when there is one.
    fn collect_def_shorthand(&mut self, at: &At, parsed: &Expr, body: Option<&str>) {
        for alternative in &parsed.alternatives {
            if let Primary::Def(argument) = alternative
                && let expr::Argument::Path(segments) = argument
                && let Some(Segment::Name(type_name)) = segments.first()
            {
                let subtype = match segments.get(1) {
                    Some(Segment::Name(subtype)) => Some(subtype.clone()),
                    _ => None,
                };
                self.defs.push(DefSite {
                    type_name: type_name.clone(),
                    body: body.map(ToOwned::to_owned),
                    at: at.clone(),
                });
                let _ = subtype;
            }
        }
    }

    /// Collects the type/enum/schema/trait references of one parsed expression.
    fn collect_refs_from_expr(&mut self, at: &At, parsed: &Expr, owner: Option<&str>) {
        for alternative in &parsed.alternatives {
            match alternative {
                Primary::Ref(argument) | Primary::Def(argument) => match argument {
                    expr::Argument::Path(segments) => {
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
                    expr::Argument::Trait(name) => {
                        self.references.push(Reference {
                            kind: RefKind::Trait,
                            name: name.clone(),
                            subtype: None,
                            owner: owner.map(ToOwned::to_owned),
                            at: at.clone(),
                        });
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
                Primary::Quoted(argument) => {
                    if let Some(name) = first_name(argument) {
                        self.references.push(Reference {
                            kind: RefKind::Schema,
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

    /// Collects the enum reference of a `map`/`pattern` key expression, if any.
    fn collect_enum_ref(&mut self, at: &At, parsed: &Expr, owner: Option<&str>) {
        for alternative in &parsed.alternatives {
            if let Primary::Enum(argument) = alternative
                && let Some(name) = first_name(argument)
            {
                self.references.push(Reference {
                    kind: RefKind::Enum,
                    name,
                    subtype: None,
                    owner: owner.map(ToOwned::to_owned),
                    at: at.clone(),
                });
            }
        }
    }

    // ---- checks ----------------------------------------------------------

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
                        owners.entry(key.clone()).or_default().push(mixin_name.clone());
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
                    report(&mut self.diagnostics, 
                        &schema.at.child("include"),
                        DiagnosticCode::IncludeConflict,
                        Severity::Error,
                        format!("field `{key}` is declared by {} mixins", mixins_only),
                    );
                    continue;
                }
                if mixins_only == 1 && body && !self.body_overrides(schema, &key) {
                    report(&mut self.diagnostics, 
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
        schema.fields.get(key).is_some_and(|overloads| match overloads {
            FieldOverloads::One(field) => field.override_field == Some(true),
            FieldOverloads::Many(fields) => fields.iter().any(|field| field.override_field == Some(true),
            ),
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
                            report(&mut self.diagnostics, 
                                &schema
                                    .at
                                    .child("fields")
                                    .child(key)
                                    .index(later),
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
                        report(&mut self.diagnostics, 
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
            let known = use_site.formals.iter().any(|formal| formal == &use_site.name);
            let key_binding = use_site.name == "key" && use_site.in_map;
            if !known && !key_binding {
                let reason = if use_site.name == "key" {
                    "`$key` binds only under a `map` or `pattern`".to_owned()
                } else {
                    format!(
                        "parameter `${}` is not a formal of the enclosing schema ({})",
                        use_site.name, use_site.what
                    )
                };
                report(&mut self.diagnostics, 
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
    /// names count 1, `$key[.column]` counts the distinct values of the key
    /// enum, and a forwarded formal counts the caller's instances. Sites with
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
            for call in &self.calls {
                if !counts.contains_key(&call.callee) {
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
                report(&mut self.diagnostics, 
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
                Actual::Param { name, column } if name == "key" => {
                    let enum_name = call.key_enum.as_ref()?;
                    let enum_members = self.enums.get(enum_name)?;
                    match enum_members.value {
                        EnumSpec::Members(members) => {
                            if column.is_some() {
                                return None;
                            }
                            members.len() as u64
                        }
                        EnumSpec::Table { columns, rows } => match column {
                            Some(column) => {
                                if !columns.contains_key(column) {
                                    return None;
                                }
                                rows.values()
                                    .filter_map(|values| values.get(column))
                                    .collect::<BTreeSet<_>>()
                                    .len() as u64
                            }
                            None => rows.len() as u64,
                        },
                    }
                }
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

    /// Check 5: `when` dependencies, plus `when` field names under check 1.
    fn check_when_dependencies(&mut self) {
        // Instance-body fields per type, from the `def` positions.
        let mut body_fields: BTreeMap<String, BTreeMap<String, FieldSite>> = BTreeMap::new();
        for def_site in &self.defs {
            let Some(body_name) = def_site.body.as_deref() else {
                continue;
            };
            if body_name == "self" {
                continue;
            }
            let Some(schema) = self.schemas.get(body_name) else {
                continue;
            };
            let fields = self.resolved_fields(schema);
            body_fields
                .entry(def_site.type_name.clone())
                .or_default()
                .extend(fields);
        }
        for (type_name, subtype, field_name, at) in &self.when_reads {
            let Some(fields) = body_fields.get(type_name) else {
                continue;
            };
            let Some(site) = fields.get(field_name) else {
                report(&mut self.diagnostics, 
                    at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!(
                        "`when` of subtype `{subtype}` reads field `{field_name}`, \
                         which no def body of type `{type_name}` declares"
                    ),
                );
                continue;
            };
            if site.conditional {
                report(&mut self.diagnostics, 
                    at,
                    DiagnosticCode::SubtypeWhenDependency,
                    Severity::Error,
                    format!(
                        "`when` of subtype `{subtype}` reads field `{field_name}`, \
                         which itself carries `when`/`unless`"
                    ),
                );
            }
        }
    }

    /// All exact fields of a schema after mixin expansion, with their
    /// conditional flag.
    fn resolved_fields(&self, schema: &SchemaDef<'_>) -> BTreeMap<String, FieldSite> {
        let mut fields = BTreeMap::new();
        for (key, overloads) in schema.fields {
            let list: &[FieldSpec] = match overloads {
                FieldOverloads::One(field) => std::slice::from_ref(field.as_ref()),
                FieldOverloads::Many(fields) => fields.as_slice(),
            };
            fields.insert(
                key.clone(),
                FieldSite {
                    conditional: list
                        .iter()
                        .any(|field| field.when.is_some() || field.unless.is_some()),
                },
            );
        }
        for mixin_name in schema.include {
            if let Some(mixin) = self.mixins.get(mixin_name) {
                for (key, overloads) in &mixin.value.fields {
                    let list: &[FieldSpec] = match overloads {
                        FieldOverloads::One(field) => std::slice::from_ref(field.as_ref()),
                        FieldOverloads::Many(fields) => fields.as_slice(),
                    };
                    fields.entry(key.clone()).or_insert(FieldSite {
                        conditional: list
                            .iter()
                            .any(|field| field.when.is_some() || field.unless.is_some()),
                    });
                }
            }
        }
        fields
    }

    /// Check 6: scope names and link `from` lists.
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
                            report(&mut self.diagnostics, 
                                &link_at,
                                DiagnosticCode::ScopeReferenceError,
                                Severity::Error,
                                format!("link `{name}` has undeclared `from` scope `{scope}`"),
                            );
                        }
                    }
                    if !scope_names(&link.to) {
                        report(&mut self.diagnostics, 
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
                            report(&mut self.diagnostics, 
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
                report(&mut self.diagnostics, 
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
            report(&mut self.diagnostics, 
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
                report(&mut self.diagnostics, 
                    &scope_at.child("set").child(register),
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("`scope.set` names undeclared register `{register}`"),
                );
            }
            if !scope_names(target) {
                report(&mut self.diagnostics, 
                    &scope_at.child("set").child(register),
                    DiagnosticCode::ScopeReferenceError,
                    Severity::Error,
                    format!("`scope.set` assigns undeclared scope `{target}`"),
                );
            }
        }
    }

    /// Checks 7 and 8: trait requirements and duplicate impls.
    fn check_traits(&mut self) {
        for (type_name, type_def) in &self.types {
            let mut impl_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut impl_ats: BTreeMap<String, At> = BTreeMap::new();
            for trait_name in type_def.value.trait_impls.keys() {
                *impl_counts.entry(trait_name.clone()).or_default() += 1;
                impl_ats
                    .entry(trait_name.clone())
                    .or_insert_with(|| type_def.at.child("impl"));
            }
            for (subtype_name, subtype) in &type_def.value.subtypes {
                for trait_name in subtype.trait_impls.keys() {
                    *impl_counts.entry(trait_name.clone()).or_default() += 1;
                    impl_ats.entry(trait_name.clone()).or_insert_with(|| {
                        type_def
                            .at
                            .child("subtypes")
                            .child(subtype_name)
                            .child("impl")
                    });
                }
            }
            for (trait_name, count) in impl_counts {
                if count > 1 {
                    report(&mut self.diagnostics, 
                        &impl_ats[&trait_name],
                        DiagnosticCode::DuplicateTraitImpl,
                        Severity::Error,
                        format!("type `{type_name}` impls trait `{trait_name}` {count} times"),
                    );
                }
                let Some(required) = self
                    .traits
                    .get(&trait_name)
                    .and_then(|trait_def| trait_def.value.requires.as_ref())
                    .and_then(|requires| requires.include.clone())
                else {
                    continue;
                };
                for def_site in self.defs.iter().filter(|site| site.type_name == *type_name) {
                    let Some(body) = def_site.body.as_deref() else {
                        continue;
                    };
                    if body == "self" {
                        continue;
                    }
                    let Some(schema) = self.schemas.get(body) else {
                        continue;
                    };
                    if !schema.include.iter().any(|include| include == &required) {
                        report(
                            &mut self.diagnostics,
                            &def_site.at,
                            DiagnosticCode::UnsatisfiedTraitRequirement,
                            Severity::Error,
                            format!(
                                "type `{type_name}` impls `{trait_name}`, which requires \
                                 include `{required}` on its def body schema `{body}`"
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Field-level `when`/`unless` gates name subtypes of the types whose
    /// def positions use the gated field's schema (check 1).
    fn check_subtype_gates(&mut self) {
        for (schema_name, subtype, at) in &self.subtype_gates {
            let def_types = self
                .defs
                .iter()
                .filter(|site| site.body.as_deref() == Some(schema_name.as_str()))
                .map(|site| site.type_name.clone())
                .collect::<Vec<_>>();
            if def_types.is_empty() {
                continue;
            }
            for type_name in def_types {
                let defined = self
                    .types
                    .get(&type_name)
                    .is_some_and(|type_def| type_def.value.subtypes.contains_key(subtype));
                if !defined {
                    report(
                        &mut self.diagnostics,
                        at,
                        DiagnosticCode::UndefinedReference,
                        Severity::Error,
                        format!(
                            "subtype gate `{subtype}` is not defined on type `{type_name}`                              (the def type of schema `{schema_name}`)"
                        ),
                    );
                }
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
                report(&mut self.diagnostics, 
                    &reference.at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!("{} `{}` is not defined", reference.kind.label(), reference.name),
                );
                continue;
            }
            if let Some(subtype) = &reference.subtype
                && let Some(type_def) = self.types.get(&reference.name)
                && !type_def.value.subtypes.contains_key(subtype)
            {
                report(&mut self.diagnostics, 
                    &reference.at,
                    DiagnosticCode::UndefinedReference,
                    Severity::Error,
                    format!("subtype `{subtype}` is not defined on type `{}`", reference.name),
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
        for trait_def in self.traits.values() {
            if let Some(include) = trait_def
                .value
                .requires
                .as_ref()
                .and_then(|requires| requires.include.clone())
            {
                used_mixins.insert(include);
            }
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
                report(&mut self.diagnostics, 
                    &schema.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("schema `{name}` is never referenced"),
                );
            }
        }
        for (name, mixin) in &self.mixins {
            if !used_mixins.contains(name) {
                report(&mut self.diagnostics, 
                    &mixin.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("mixin `{name}` is never included"),
                );
            }
        }
        for (name, type_def) in &self.types {
            if !used_types.contains(name) {
                report(&mut self.diagnostics, 
                    &type_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("type `{name}` is never referenced"),
                );
            }
        }
        for (name, enum_def) in &self.enums {
            if !used_enums.contains(name) {
                report(&mut self.diagnostics, 
                    &enum_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("enum `{name}` is never referenced"),
                );
            }
        }
        for (name, trait_def) in &self.traits {
            if !used_traits.contains(name) {
                report(&mut self.diagnostics, 
                    &trait_def.at,
                    DiagnosticCode::UnusedDefinition,
                    Severity::Warning,
                    format!("trait `{name}` is never implemented"),
                );
            }
        }
    }
}

/// One resolved exact field, for the `when` dependency check.
struct FieldSite {
    conditional: bool,
}

/// Parameter scope of one walk position.
struct ParamCtx<'a> {
    formals: &'a [String],
    in_map: bool,
}

impl ParamCtx<'static> {
    fn closed() -> Self {
        Self {
            formals: &[],
            in_map: false,
        }
    }
}

static EMPTY_FIELDS: BTreeMap<String, FieldOverloads> = BTreeMap::new();

/// Saturation point and limit of the parameterised-schema instance count
/// (`docs/rules-language.md` §10.1, check 4): counts saturate one above the
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
fn first_name(argument: &expr::Argument) -> Option<String> {
    match argument {
        expr::Argument::Path(segments) => segments.iter().find_map(|segment| match segment {
            Segment::Name(name) => Some(name.clone()),
            Segment::Param(_) => None,
        }),
        expr::Argument::Trait(name) => Some(name.clone()),
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
                | Primary::Scope(argument)
                | Primary::Quoted(argument) => {
                    if let expr::Argument::Path(segments) = argument {
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

/// The shape mismatch of a mixed union, if any.
fn union_shape(parsed: &Expr) -> Result<(), &'static str> {
    let mut quoted = false;
    let mut scalar = false;
    for alternative in &parsed.alternatives {
        match alternative {
            Primary::Quoted(_) => quoted = true,
            _ => scalar = true,
        }
    }
    if quoted && scalar {
        Err("scalar and quoted-script")
    } else {
        Ok(())
    }
}

/// The `enum<E>` name of a map/pattern key expression, when it is one.
fn enum_name(raw: &str) -> Option<String> {
    let parsed = expr::parse(raw).ok()?;
    for alternative in &parsed.alternatives {
        if let Primary::Enum(argument) = alternative {
            return first_name(argument);
        }
    }
    None
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
fn parse_schema_key(raw: &str) -> Result<(String, Vec<String>), String> {
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
fn parse_schema_ref(raw: &str) -> Result<SchemaRef, String> {
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
        return Err(format!("schema reference `{raw}` is missing its closing `>`"));
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
            let (name, column) = match parameter.split_once('.') {
                Some((name, column)) => (name, Some(column)),
                None => (parameter, None),
            };
            if !is_ident(name) || column.is_some_and(|column| !is_ident(column)) {
                return Err(format!("`{argument}` is not a schema argument"));
            }
            args.push(Actual::Param {
                name: name.to_owned(),
                column: column.map(ToOwned::to_owned),
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
fn parse_card(raw: &str) -> Result<(u32, Option<u32>), String> {
    let raw = raw.trim();
    let (lower, upper) = raw.split_once("..").unwrap_or((raw, raw));
    let lower = lower.trim();
    let upper = upper.trim();
    let min = lower.parse::<u32>().map_err(|_| format!("`{raw}` is not a card"))?;
    let max = if upper == "*" {
        None
    } else {
        Some(upper.parse::<u32>().map_err(|_| format!("`{raw}` is not a card"))?)
    };
    if max.is_some_and(|max| max < min) {
        return Err(format!("card `{raw}` has a lower bound above its upper bound"));
    }
    Ok((min, max))
}

/// Parses `key`, `field:<name>`, `file`.
fn parse_def_name(raw: &str) -> Result<(), String> {
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
fn parse_def_type(raw: &str) -> Result<(String, Option<String>), String> {
    let raw = raw.trim();
    match raw.split_once('.') {
        Some((type_name, subtype)) if is_ident(type_name.trim()) && is_ident(subtype.trim()) => {
            Ok((type_name.trim().to_owned(), Some(subtype.trim().to_owned())))
        }
        None if is_ident(raw) => Ok((raw.to_owned(), None)),
        _ => Err(format!("`{raw}` is not a def type")),
    }
}

/// Parses `scope_type` or `scope_type?`.
fn parse_column(raw: &str) -> Result<(String, bool), String> {
    let raw = raw.trim();
    let (kind, optional) = match raw.strip_suffix('?') {
        Some(kind) => (kind.trim(), true),
        None => (raw, false),
    };
    if is_ident(kind) {
        Ok((kind.to_owned(), optional))
    } else {
        Err(format!("`{raw}` is not a column declaration"))
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
        && left.when == right.when
        && left.unless == right.unless
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
fn report_parse(diagnostics: &mut Vec<Diagnostic>, at: &At, column: Option<usize>, message: String) {
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

    /// The examples of `docs/rules-language.md` folded into one fully
    /// referenced document: it must produce zero diagnostics.
    const CLEAN: &str = r#"{
      "files": {
        "events":           { "path": "events", "ext": "txt", "root": "events_file" },
        "scripted_effects": { "path": "common/scripted_effects", "ext": "txt", "root": "scripted_effects_file" },
        "on_actions":       { "path": "common/on_actions", "ext": "txt", "root": "on_actions_file" }
      },
      "schemas": {
        "events_file": { "fields": {
          "country_event": {
            "def": { "type": "event.country", "name": "field:id" },
            "scope": { "set": { "root": "country", "this": "country" } },
            "body": "event_body", "card": "0..*"
          }
        }},
        "event_body": {
          "include": ["gated"],
          "fields": {
            "id":                 { "value": "scalar", "card": "1" },
            "title":              { "value": "loc", "card": "0..1" },
            "is_triggered_only":  { "value": "bool", "card": "0..1" },
            "mean_time_to_happen": { "body": "mtth", "card": "0..1", "unless": "triggered" },
            "option":             { "body": "event_option", "card": "0..*" },
            "picture":            { "value": "ref<sprite> | enum<pictures>", "card": "0..*" }
          }
        },
        "event_option": { "fields": {
          "name":     { "value": "loc", "card": "0..1" },
          "ai_chance": { "body": "mtth", "card": "0..1" }
        }},
        "mtth": { "fields": {
          "days":   { "value": "int[0..]", "card": "0..1" },
          "factor": { "value": "float", "card": "0..1" }
        }},
        "scripted_effects_file": { "map": { "key": "def<scripted_effect>", "body": "effect" } },
        "effect": {
          "fields": {
            "add_prestige":  { "value": "int", "card": "0..*" },
            "if":            { "body": "self", "card": "0..*", "control": { "kind": "branch", "guard": "limit", "chain": ["else"] } },
            "else":          { "body": "self", "card": "0..*", "control": { "kind": "branch_continue" } },
            "limit":         { "body": "trigger", "card": "0..1", "control": { "kind": "guard" } },
            "hidden_effect": { "body": "self", "card": "0..*", "control": { "kind": "transparent" } }
          },
          "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ]
        },
        "trigger": {
          "fields": {
            "always":              { "value": "bool", "card": "0..1" },
            "has_country_modifier": { "value": "ref<event_modifier>", "card": "0..1" }
          },
          "patterns": [ { "key": "link", "body": "self", "card": "0..*" } ]
        },
        "on_actions_file": { "map": { "key": "enum<on_actions>", "body": "on_action_body<$key.scope>" } },
        "on_action_body<S>": { "fields": {
          "events": { "list": "ref<event.$S>", "card": "0..*" }
        }}
      },
      "mixins": {
        "gated": { "fields": { "potential": { "body": "trigger", "card": "0..1" } } }
      },
      "types": {
        "event": {
          "resolution": "replace",
          "subtypes": {
            "country":  {},
            "province": {},
            "triggered": { "when": { "is_triggered_only": "'yes'" } }
          }
        },
        "scripted_effect": {
          "resolution": "replace",
          "impl": { "Callable": { "body": "effect" } }
        },
        "event_modifier": {},
        "sprite": {}
      },
      "traits": {
        "Callable": { "params": { "body": "schema" }, "capabilities": ["replacement"] }
      },
      "enums": {
        "pictures": ["one", "two"],
        "on_actions": {
          "columns": { "scope": "scope_type" },
          "rows": {
            "on_startup":                     { "scope": "country" },
            "on_province_religion_converted": { "scope": "province" }
          }
        }
      },
      "scopes": {
        "types":     ["country", "province"],
        "registers": { "root": {}, "this": {}, "prev": { "chain": true }, "from": { "chain": true } },
        "links": {
          "owner":   { "from": ["province"], "to": "country" },
          "capital": { "from": ["country"],  "to": "province" }
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
            ("one.json", r#"{ "schemas": { "s": { "fields": { "a": { "value": "bool" } } } } }"#),
            ("two.json", r#"{ "schemas": { "s": { "fields": { "b": { "value": "bool" } } } } }"#),
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
                  "a": { "value": "ref<missing_type> | enum<missing_enum> | ref<impl MissingTrait>" },
                  "b": { "body": "missing_schema" },
                  "c": { "def": { "type": "known.missing_subtype" }, "value": "scalar" }
                }},
                "known": { "fields": { "z": { "value": "bool" } } }
              },
              "types": { "known": {} }
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
                    "a": { "value": "$key.scope" },
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
        let members = (0..65)
            .map(|index| format!("\"m{index}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!(
            r#"{{
              "files": {{ "big": {{ "path": "x", "ext": "txt", "root": "big_file" }} }},
              "schemas": {{
                "big_file": {{ "map": {{ "key": "enum<big>", "body": "boxed<$key>" }} }},
                "boxed<S>": {{ "fields": {{ "x": {{ "value": "scalar" }} }} }}
              }},
              "enums": {{ "big": [ {members} ] }}
            }}"#
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
    fn when_dependencies_are_enforced() {
        let gated = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "events_file" } },
              "schemas": {
                "events_file": { "fields": {
                  "country_event": { "def": { "type": "event", "name": "field:id" }, "body": "event_body" }
                }},
                "event_body": { "fields": {
                  "id": { "value": "scalar" },
                  "is_triggered_only": { "value": "bool", "unless": "triggered" }
                }}
              },
              "types": { "event": { "subtypes": {
                "triggered": { "when": { "is_triggered_only": "'yes'" } }
              }}}
            }"#,
        )]);
        assert_eq!(
            errors(&gated),
            vec![DiagnosticCode::SubtypeWhenDependency],
            "{gated:?}"
        );

        let missing = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "events_file" } },
              "schemas": {
                "events_file": { "fields": {
                  "country_event": { "def": { "type": "event", "name": "field:id" }, "body": "event_body" }
                }},
                "event_body": { "fields": { "id": { "value": "scalar" } } }
              },
              "types": { "event": { "subtypes": {
                "triggered": { "when": { "not_a_field": "'yes'" } }
              }}}
            }"#,
        )]);
        assert_eq!(
            errors(&missing),
            vec![DiagnosticCode::UndefinedReference],
            "{missing:?}"
        );
    }

    #[test]
    fn subtype_gates_must_name_defined_subtypes() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "events_file" } },
              "schemas": {
                "events_file": { "fields": {
                  "country_event": { "def": { "type": "event", "name": "field:id" }, "body": "event_body" }
                }},
                "event_body": { "fields": {
                  "id": { "value": "scalar" },
                  "x": { "value": "bool", "unless": "ghost" }
                }}
              },
              "types": { "event": { "subtypes": { "triggered": {} } } }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![DiagnosticCode::UndefinedReference],
            "{diagnostics:?}"
        );
    }

    #[test]
    fn scope_names_and_link_from_lists_are_checked() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "schemas": {
                "s": { "fields": {
                  "a": { "value": "bool", "scope": { "in": ["nowhere"], "push": "any",
                          "set": { "ghost": "country", "root": "nowhere" } } }
                }}
              },
              "scopes": {
                "types": ["country", "any", "country"],
                "registers": { "root": {} },
                "links": {
                  "empty":  { "from": [], "to": "country" },
                  "stray":  { "from": ["nowhere"], "to": "country" }
                },
                "compat": [ { "actual": "nowhere", "expected": "country" } ]
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
    fn trait_requirements_hold_at_def_positions() {
        let source = |include: &str| {
            format!(
                r#"{{
                  "files": {{ "b": {{ "path": "b", "ext": "txt", "root": "buildings_file" }} }},
                  "schemas": {{
                    "buildings_file": {{ "map": {{ "key": "def<building>", "body": "building_body" }} }},
                    "building_body": {{ "include": [ "{include}" ], "fields": {{
                      "cost": {{ "value": "int" }}
                    }} }}
                  }},
                  "mixins": {{ "modifier_block": {{ "fields": {{ "modifier": {{ "value": "bool" }} }} }} }},
                  "types": {{ "building": {{ "impl": {{ "ModifierSource": {{}} }} }} }},
                  "traits": {{ "ModifierSource": {{ "requires": {{ "include": "modifier_block" }} }} }}
                }}"#
            )
        };
        let satisfied = run(&[("core.json", &source("modifier_block"))]);
        assert_eq!(errors(&satisfied), Vec::new(), "{satisfied:?}");

        let unsatisfied = run(&[("core.json", &source("other_block"))]);
        assert_eq!(
            errors(&unsatisfied),
            vec![
                DiagnosticCode::UndefinedReference,
                DiagnosticCode::UnsatisfiedTraitRequirement,
            ],
            "{unsatisfied:?}"
        );
    }

    #[test]
    fn duplicate_trait_impls_are_errors() {
        let diagnostics = run(&[(
            "core.json",
            r#"{
              "files": { "e": { "path": "e", "ext": "txt", "root": "events_file" } },
              "schemas": {
                "events_file": { "fields": {
                  "country_event": { "def": { "type": "event", "name": "field:id" }, "body": "event_body" }
                }},
                "event_body": { "fields": { "id": { "value": "scalar" } } }
              },
              "types": { "event": {
                "impl": { "Localised": {} },
                "subtypes": { "country": { "impl": { "Localised": {} } } }
              }},
              "traits": { "Localised": {} }
            }"#,
        )]);
        assert_eq!(
            errors(&diagnostics),
            vec![DiagnosticCode::DuplicateTraitImpl],
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
              "traits": {
                "T": { "bindings": {
                  "both": { "loc": "{x}", "sprite": "{x}" },
                  "neither": {}
                }}
              }
            }"#,
        )]);
        // Two `map` payloads and two trait bindings are each malformed.
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
}
