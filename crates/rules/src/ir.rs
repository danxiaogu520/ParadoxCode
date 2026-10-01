//! The rules runtime IR (`docs/rules-redesign.md` §5).
//!
//! A [`RulesIr`] is a closed arena: every key, name, and template is an
//! interned [`Symbol`], every structure is an id-addressed entry, and all
//! expansion — mixin includes, parameterised-schema monomorphisation, and
//! enum attribute-column grouping — has already happened at compile time
//! (`crate::lower`). Consumers walk it by id and never re-derive a context
//! from a `(context, parent_path)` pair.
//!
//! Symbols come in two flavours, and the distinction is part of the contract:
//!
//! - **identity** symbols (schema, type, enum, trait, scope, register, field
//!   key, formal parameter) are interned *folded* to ASCII lowercase, because
//!   the language is case-insensitive everywhere, so a lookup needs no
//!   re-folding and two spellings cannot produce two identities;
//! - **text** symbols (literal matches, template text, documentation, binding
//!   templates) are interned verbatim, because their spelling is data.
//!
//! This module is deliberately free of any dependency on the legacy runtime
//! model: it is the shape phase 4 migrates the consumers onto and phase 5
//! keeps.

use std::fmt;

use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use text::LogicalPath;

use crate::matcher::{FileMatcher, is_eu4_date};
use crate::profile::GameProfile;
use crate::source::{ControlKind, Severity};

/// One interned string.
///
/// Compare symbols for identity; resolve them through [`Interner::resolve`].
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Symbol(u32);

impl Symbol {
    /// The dense index of this symbol in its interner, for compact side tables.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An interner for [`Symbol`]s.
///
/// One interner is shared by every string in a [`RulesIr`]; interning order is
/// deterministic because lowering walks `BTreeMap`s and ordered slices.
#[derive(Clone, Debug, Default)]
pub struct Interner {
    strings: Vec<Box<str>>,
    index: FxHashMap<Box<str>, Symbol>,
}

impl Interner {
    /// Interns `text` verbatim.
    pub fn intern_verbatim(&mut self, text: &str) -> Symbol {
        if let Some(existing) = self.index.get(text) {
            return *existing;
        }
        self.push(text.into())
    }

    /// Interns the ASCII-lowercased spelling of `text`, the identity form of
    /// every case-insensitive name in the language.
    pub fn intern_folded(&mut self, text: &str) -> Symbol {
        if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            let folded = text.to_ascii_lowercase();
            if let Some(existing) = self.index.get(folded.as_str()) {
                return *existing;
            }
            self.push(folded.into_boxed_str())
        } else {
            self.intern_verbatim(text)
        }
    }

    fn push(&mut self, text: Box<str>) -> Symbol {
        let symbol = Symbol(u32::try_from(self.strings.len()).expect("interner overflow"));
        self.index.insert(text.clone(), symbol);
        self.strings.push(text);
        symbol
    }

    /// The symbol interned from exactly this spelling, if any.
    #[must_use]
    pub fn lookup_verbatim(&self, text: &str) -> Option<Symbol> {
        self.index.get(text).copied()
    }

    /// Resolves a symbol interned folded, accepting any casing.
    #[must_use]
    pub fn lookup_folded(&self, text: &str) -> Option<Symbol> {
        if text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            let folded = text.to_ascii_lowercase();
            self.index.get(folded.as_str()).copied()
        } else {
            self.index.get(text).copied()
        }
    }

    /// The text behind a symbol.
    ///
    /// # Panics
    ///
    /// Panics when the symbol came from a different interner.
    #[must_use]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.index()]
    }

    /// The number of distinct strings interned so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// Iterates every interned symbol with its text.
    pub fn iter(&self) -> impl Iterator<Item = (Symbol, &str)> {
        self.strings
            .iter()
            .enumerate()
            .map(|(index, text)| (Symbol(index as u32), text.as_ref()))
    }
}

/// Index of a [`Schema`] in [`RulesIr::schemas`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SchemaId(pub(crate) u32);

impl SchemaId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a [`Field`] in [`RulesIr::fields`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldId(pub(crate) u32);

impl FieldId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a [`Matcher`] in [`RulesIr::matchers`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MatcherId(pub(crate) u32);

impl MatcherId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a [`TypeInfo`] in [`RulesIr::types`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TypeId(pub(crate) u32);

impl TypeId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of a [`TraitInfo`] in [`RulesIr::traits`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TraitId(pub(crate) u32);

impl TraitId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Index of an [`EnumInfo`] in [`RulesIr::enums`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EnumId(pub(crate) u32);

impl EnumId {
    /// The dense index of this id.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A compiled rule set: the whole of §5.1.
#[derive(Clone, Debug)]
pub struct RulesIr {
    /// The `game.json` identity this rule set was compiled for.
    pub game_id: Symbol,
    /// Path selection, ordered by entry name for a deterministic tie-break.
    pub files: Vec<FileRule>,
    /// Block descriptions, one entry per monomorphised instance.
    pub schemas: Vec<Schema>,
    /// Field specifications, shared by the schemas that declare them.
    pub fields: Vec<Field>,
    /// The interned, deduplicated matcher arena.
    pub matchers: Vec<Matcher>,
    /// Symbol namespaces and their traits.
    pub types: Vec<TypeInfo>,
    /// Trait definitions (parameters, bindings, capabilities).
    pub traits: Vec<TraitInfo>,
    /// Enums with their attribute columns.
    pub enums: Vec<EnumInfo>,
    /// The scope model.
    pub scopes: ScopeModel,
    /// Every string in the IR.
    pub strings: Interner,
    /// Per-field origin, sorted by [`FieldId`].
    pub provenance: Vec<Provenance>,
    /// The non-language `game.json` payload.
    pub game: GameConfig,
    lookups: Lookups,
}

/// Name → id indexes, built once at the end of lowering.
#[derive(Clone, Debug, Default)]
struct Lookups {
    schemas: FxHashMap<Symbol, SchemaId>,
    types: FxHashMap<Symbol, TypeId>,
    traits: FxHashMap<Symbol, TraitId>,
    enums: FxHashMap<Symbol, EnumId>,
}

/// One `files` entry: which documents a rule selects and how their root is
/// described.
#[derive(Clone, Debug)]
pub struct FileRule {
    /// The entry name, a stable logical identity used for diagnostics.
    pub name: Symbol,
    /// Path/extension selection.
    pub matcher: FileMatcher,
    /// The document parser.
    pub parser: DocumentParser,
    /// Definition-priority policy.
    pub resolution: FileResolution,
    /// Whether the path does not recurse into subdirectories.
    pub strict: bool,
    /// What the document root is validated against.
    pub root: RootRule,
}

/// What the root of a selected document holds.
#[derive(Clone, Debug, PartialEq)]
pub enum RootRule {
    /// The document root is a block described by this schema.
    Schema(SchemaId),
    /// The whole file is one symbol instance.
    Instance {
        /// The `def` declared at the file root.
        def: Option<DefSpec>,
        /// The instance body schema, when the root declares one.
        body: Option<SchemaId>,
    },
    /// The parser has no script structure (`localisation`, `asset`,
    /// `syntax-only`), so no root is modelled.
    Opaque,
}

/// The document parser of a [`FileRule`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentParser {
    /// Paradox script.
    Script,
    /// `localisation/*.yml`.
    Localisation,
    /// Asset manifests.
    Asset,
    /// Structure only.
    SyntaxOnly,
}

/// The definition-priority policy of a [`FileRule`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileResolution {
    /// Later paths shadow earlier ones per file path.
    ReplaceByPath,
    /// All definitions remain visible (the default).
    Merge,
    /// Later directories shadow earlier ones wholesale.
    ReplaceDirectory,
}

/// One block description: exact fields, ordered patterns, bare-value items,
/// and the instance-body subtype gates recorded for it.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    /// The declared schema name, folded. Every monomorphised instance shares
    /// it; [`Self::arguments`] tells the instances apart.
    pub name: Symbol,
    /// The actual arguments of this instance, empty for a plain schema.
    pub arguments: Box<[Symbol]>,
    /// Folded exact key → its shape overloads, in written order.
    pub exact: FxHashMap<Symbol, Box<[FieldId]>>,
    /// Non-exact keys, tried in written order.
    pub patterns: Box<[FieldId]>,
    /// The element matcher for bare values in a list block.
    pub items: Option<MatcherId>,
    /// Whether undeclared keys are allowed.
    pub open: bool,
    /// Subtype predicates to evaluate when a block lowered to this schema is
    /// used as a symbol-instance body.
    pub subtype_gates: Box<[SubtypeGate]>,
}

/// One subtype predicate attached to a schema used as an instance body.
#[derive(Clone, Debug)]
pub struct SubtypeGate {
    /// The type that owns the subtype.
    pub type_id: TypeId,
    /// The subtype granted when [`Self::when`] holds.
    pub subtype: Symbol,
    /// The predicate, evaluated against the body's direct scalar fields.
    pub when: SubtypeCond,
}

/// A conjunction of "field → expected scalar" predicates.
///
/// A `None` expectation requires the field to be *absent*; a `Some` matcher
/// requires the field to be present with a matching scalar.
#[derive(Clone, Debug, Default)]
pub struct SubtypeCond(pub Box<[(Symbol, Option<MatcherId>)]>);

/// One field specification.
#[derive(Clone, Debug)]
pub struct Field {
    /// The key matcher.
    pub key: MatcherId,
    /// What the value is.
    pub value: FieldValue,
    /// Cardinality.
    pub card: Card,
    /// Scope effect.
    pub scope: Option<ScopeEffect>,
    /// Symbol instance defined at this position.
    pub def: Option<DefSpec>,
    /// Subtype gate.
    pub gate: Option<Gate>,
    /// Control-flow primitive.
    pub control: Option<Control>,
    /// Documentation text.
    pub doc: Option<Symbol>,
    /// Diagnostic severity for violations of this field.
    pub severity: Severity,
    /// Whether the field is deprecated.
    pub deprecated: bool,
}

/// What a [`Field`]'s value is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FieldValue {
    /// A scalar described by this matcher.
    Scalar(MatcherId),
    /// A nested block described by this schema instance.
    Block(SchemaId),
    /// A nested block reusing the enclosing schema.
    SelfBlock,
    /// A quoted script parsed with this schema instance.
    Quoted(SchemaId),
}

/// The shape a field's value takes, for §3.2 dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shape {
    /// A scalar.
    Scalar,
    /// A nested block.
    Block,
    /// A quoted script.
    Quoted,
}

/// A cardinality `(min, max)`; `None` max means unbounded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Card {
    /// Minimum occurrences.
    pub min: u32,
    /// Maximum occurrences, `None` for unbounded.
    pub max: Option<u32>,
}

/// The scope effect of a field.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct ScopeEffect {
    /// Scopes the field is valid in; empty means any.
    pub scopes_in: Box<[Symbol]>,
    /// Scope entered by the nested block.
    pub push: Option<Symbol>,
    /// Registers replaced on entry, sorted by register name.
    pub set: Box<[(Symbol, Symbol)]>,
}

/// A symbol instance definition.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DefSpec {
    /// The defined type.
    pub type_id: TypeId,
    /// The subtype granted at this position, when the def names one.
    pub subtype: Option<Symbol>,
    /// How the instance name is derived.
    pub name: DefName,
    /// Affix removed from the derived instance name.
    pub strip_prefix: Option<Symbol>,
    /// Affix removed from the derived instance name.
    pub strip_suffix: Option<Symbol>,
}

/// How a [`DefSpec`] derives its instance name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefName {
    /// The scalar key itself (the default).
    Key,
    /// The value of a sibling scalar field.
    Field(Symbol),
    /// The file name without its extension.
    File,
}

/// A field's subtype gate.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Gate {
    /// The field applies only to instances carrying this subtype.
    When(Symbol),
    /// The field applies only to instances outside this subtype.
    Unless(Symbol),
}

/// The control-flow primitive attached to a field.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Control {
    /// Which primitive this field implements.
    pub kind: ControlKind,
    /// Guard sub-block key.
    pub guard: Option<Symbol>,
    /// Sibling keys that may continue this chain.
    pub chain: Box<[Symbol]>,
    /// Logic operator for `logic` kinds.
    pub op: Option<Symbol>,
    /// Field name whose scalar values are the branch keys of a `switch`.
    pub on: Option<Symbol>,
}

/// One entry of the deduplicated matcher arena.
#[derive(Clone, Debug)]
pub enum Matcher {
    /// Any scalar.
    Scalar,
    /// One constant scalar, compared case-insensitively.
    Literal(Symbol),
    /// Literal text with `{…}` holes.
    Template(Box<[TemplatePart]>),
    /// An integer, optionally range-bounded.
    Int {
        /// Inclusive lower bound.
        min: Option<i64>,
        /// Inclusive upper bound.
        max: Option<i64>,
    },
    /// A float, optionally range-bounded.
    Float {
        /// Inclusive lower bound.
        min: Option<f64>,
        /// Inclusive upper bound.
        max: Option<f64>,
    },
    /// `yes` / `no`.
    Bool,
    /// A campaign date.
    Date,
    /// A localisation key.
    Loc,
    /// A file path, optionally of a named category.
    Path(Option<Symbol>),
    /// A symbol reference.
    Ref(RefTarget),
    /// A symbol *definition* in value position (`def<T>`, `def<T.subtype>`).
    Def {
        /// The defined type.
        type_id: TypeId,
        /// The subtype granted at this position.
        subtype: Option<Symbol>,
    },
    /// A member of an enum, restricted to `rows` when the matcher came from an
    /// attribute-column group.
    Enum {
        /// The enum.
        id: EnumId,
        /// The accepted rows, or all of them when `None`.
        rows: Option<BitSet>,
    },
    /// A scope expression of the named scope type (`None` is `any`).
    Scope(Option<Symbol>),
    /// Any scope link, register, or prefix link.
    Link,
    /// A quoted script parsed with this schema.
    Quoted(SchemaId),
    /// Unchecked text.
    Opaque,
    /// Alternatives, tried in written order.
    Union(Box<[MatcherId]>),
}

/// One piece of a template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemplatePart {
    /// Literal text.
    Text(Symbol),
    /// A `{…}` hole holding one matcher.
    Hole(MatcherId),
}

/// What a [`Matcher::Ref`] points at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefTarget {
    /// A symbol type, optionally qualified by a subtype, optionally stripping
    /// an affix from the resolved member name (the legacy template parameter's
    /// `strip_prefix`).
    Type {
        /// The referenced type.
        type_id: TypeId,
        /// The required subtype.
        subtype: Option<Symbol>,
        /// Affix removed from the resolved member name.
        strip_prefix: Option<Symbol>,
    },
    /// Any type implementing the trait.
    Trait(TraitId),
}

/// One symbol namespace.
#[derive(Clone, Debug)]
pub struct TypeInfo {
    /// The type name, folded.
    pub name: Symbol,
    /// Conflict behaviour.
    pub resolution: TypeResolution,
    /// Named subtypes, in declaration order.
    pub subtypes: Vec<SubtypeInfo>,
    /// Open world: any name may exist.
    pub open: bool,
    /// Engine-provided members.
    pub builtin: Box<[Symbol]>,
    /// Traits implemented for every instance of this type.
    pub trait_impls: Vec<TraitImpl>,
}

/// The conflict behaviour of a symbol type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeResolution {
    /// Later definitions shadow earlier ones.
    Replace,
    /// Structurally repeated keys stay independent (the default).
    Independent,
}

/// One named subtype.
#[derive(Clone, Debug)]
pub struct SubtypeInfo {
    /// The subtype name, folded.
    pub name: Symbol,
    /// Instance-body predicate granting this subtype.
    pub when: Option<SubtypeCond>,
    /// Traits implemented only for instances of this subtype.
    pub trait_impls: Vec<TraitImpl>,
}

/// One trait implementation and its arguments.
#[derive(Clone, Debug)]
pub struct TraitImpl {
    /// The implemented trait.
    pub trait_id: TraitId,
    /// Arguments, sorted by argument name.
    pub arguments: Vec<(Symbol, TraitArgument)>,
}

/// One trait-implementation argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TraitArgument {
    /// A plain argument value (`Callable`'s body schema name).
    Text(Symbol),
    /// One localisation or sprite binding contributed by the impl.
    Binding(Binding),
}

/// One trait binding: how a bound symbol's key or name is derived from an
/// instance name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    /// Localisation-key template; `$` is the instance name.
    pub loc: Option<Symbol>,
    /// Sprite-name template; `$` is the instance name.
    pub sprite: Option<Symbol>,
    /// Whether the bound symbol must resolve.
    pub required: bool,
}

/// One trait definition.
#[derive(Clone, Debug)]
pub struct TraitInfo {
    /// The trait name, folded.
    pub name: Symbol,
    /// Parameters with their defaults (`$` means the instance name), or no
    /// default.
    pub params: Vec<(Symbol, Option<Symbol>)>,
    /// Bindings the trait contributes itself; empty for `Localised`/`HasIcon`.
    pub bindings: Vec<(Symbol, Binding)>,
    /// A mixin the schemas defining an impl'ing type must include.
    pub requires_include: Option<Symbol>,
    /// Capabilities the runtime interprets.
    pub capabilities: Box<[Symbol]>,
}

/// One enum.
#[derive(Clone, Debug)]
pub struct EnumInfo {
    /// The enum name, folded.
    pub name: Symbol,
    /// Attribute columns, in declaration order.
    pub columns: Box<[EnumColumn]>,
    /// Rows, sorted by row name.
    pub rows: Box<[EnumRow]>,
}

impl EnumInfo {
    /// The index of a column by name.
    #[must_use]
    pub fn column(&self, name: Symbol) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }

    /// The index of a row by name.
    #[must_use]
    pub fn row(&self, name: Symbol) -> Option<usize> {
        self.rows.iter().position(|row| row.name == name)
    }

    /// The rows carrying `value` in `column`.
    #[must_use]
    pub fn rows_matching(&self, column: usize, value: Option<Symbol>) -> BitSet {
        let mut set = BitSet::new(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            if row.values.get(column).copied().flatten() == value {
                set.insert(index);
            }
        }
        set
    }
}

/// One attribute column of an enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnumColumn {
    /// The column name, folded.
    pub name: Symbol,
    /// Whether rows may omit the column.
    pub optional: bool,
}

/// One row of an enum, with its column values aligned to
/// [`EnumInfo::columns`].
#[derive(Clone, Debug)]
pub struct EnumRow {
    /// The row name, folded.
    pub name: Symbol,
    /// One value per column, `None` when the row omits an optional column.
    pub values: Box<[Option<Symbol>]>,
}

/// The scope model.
#[derive(Clone, Debug, Default)]
pub struct ScopeModel {
    /// Scope type names; `any` is not listed.
    pub types: Box<[Symbol]>,
    /// Registers.
    pub registers: Box<[RegisterInfo]>,
    /// Links, including templates.
    pub links: Box<[LinkInfo]>,
    /// Compatibility overrides.
    pub compat: Box<[(Symbol, Symbol)]>,
}

impl ScopeModel {
    /// Whether `name` is a declared scope type or the reserved `any`.
    #[must_use]
    pub fn is_known_scope(&self, strings: &Interner, name: Symbol) -> bool {
        strings.resolve(name) == "any" || self.types.contains(&name)
    }

    /// The first link whose template matches `key` exactly, when the key is a
    /// plain (hole-free) link name.
    #[must_use]
    pub fn link(&self, strings: &Interner, key: Symbol) -> Option<&LinkInfo> {
        let spelling = strings.resolve(key);
        self.links.iter().find(|link| {
            link.pattern.len() == 1
                && match link.pattern[0] {
                    TemplatePart::Text(text) => {
                        strings.resolve(text).eq_ignore_ascii_case(spelling)
                    }
                    TemplatePart::Hole(_) => false,
                }
        })
    }
}

/// One scope register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegisterInfo {
    /// The register name, folded.
    pub name: Symbol,
    /// Whether the register chains (`prev_prev`, `fromfrom`, …).
    pub chain: bool,
}

/// One scope link.
#[derive(Clone, Debug)]
pub struct LinkInfo {
    /// The declared key text (template text), verbatim.
    pub name: Symbol,
    /// The key template.
    pub pattern: Box<[TemplatePart]>,
    /// Scopes the link may start from.
    pub from: Box<[ScopeRef]>,
    /// The scope the link lands in.
    pub to: ScopeRef,
}

impl LinkInfo {
    /// Whether the link key carries holes.
    #[must_use]
    pub fn is_template(&self) -> bool {
        self.pattern
            .iter()
            .any(|part| matches!(part, TemplatePart::Hole(_)))
    }
}

/// One end of a scope link: a concrete scope type, or `any`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeRef {
    /// The reserved `any`.
    Any,
    /// A declared scope type.
    Type(Symbol),
}

impl ScopeRef {
    /// The named type, when this end is not `any`.
    #[must_use]
    pub const fn type_name(self) -> Option<Symbol> {
        match self {
            Self::Any => None,
            Self::Type(name) => Some(name),
        }
    }
}

/// Where one compiled field came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provenance {
    /// The field.
    pub field: FieldId,
    /// The source file name as listed by the manifest.
    pub file: Symbol,
    /// The JSON pointer of the field specification.
    pub pointer: Symbol,
}

/// The non-language part of `game.json`.
///
/// Rule structure moved into the language (schemas, types, enums, scopes);
/// what remains is installation and presentation configuration, which stays
/// the [`GameProfile`] the composition root already selects.
#[derive(Clone, Debug, Default)]
pub struct GameConfig {
    /// Scan layout, source encoding, fallback keys, and hover cards.
    pub profile: GameProfile,
}

impl GameConfig {
    /// The game identity this configuration selects.
    #[must_use]
    pub fn game_id(&self) -> &str {
        &self.profile.game_id
    }

    /// Keys that are always considered known.
    #[must_use]
    pub fn fallback_keys(&self) -> &[String] {
        &self.profile.fallback_keys
    }
}

/// A subset of an enum's rows, or of an enum's members.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BitSet(Box<[u64]>);

impl BitSet {
    /// An empty set able to hold `len` members.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(64)].into_boxed_slice())
    }

    /// Adds `index`.
    pub fn insert(&mut self, index: usize) {
        let word = index / 64;
        if word >= self.0.len() {
            let mut words = self.0.to_vec();
            words.resize(word + 1, 0);
            self.0 = words.into_boxed_slice();
        }
        self.0[word] |= 1 << (index % 64);
    }

    /// Whether `index` is a member.
    #[must_use]
    pub fn contains(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word & (1 << (index % 64)) != 0)
    }

    /// The number of members.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.iter().map(|word| word.count_ones() as usize).sum()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }

    /// Iterates the members in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(word, bits)| {
            (0..64)
                .filter(move |bit| bits & (1 << bit) != 0)
                .map(move |bit| word * 64 + bit)
        })
    }
}

/// The direct scalar fields of one symbol instance, as the subtype predicates
/// of §5 see them.
///
/// Implementations must compare `key` case-insensitively: the IR passes the
/// folded spelling the language made canonical.
pub trait ScalarFields {
    /// The scalar value of the direct child field `key`, if the field is
    /// present and scalar.
    fn scalar(&self, key: &str) -> Option<&str>;
}

/// Workspace facts the IR cannot decide alone.
///
/// A `when` predicate or a `ref<…>` matcher may name a symbol that only the
/// workspace index knows, so the IR asks for it instead of guessing.
pub trait SymbolFacts {
    /// Whether `name` is a known member of `type_id`.
    fn type_member(&self, type_id: TypeId, name: &str) -> bool {
        let _ = (type_id, name);
        false
    }

    /// Whether this instance carries a particular declared subtype.
    /// A missing subtype fact is conservative: membership alone grants none.
    fn type_subtype_member(&self, type_id: TypeId, subtype: Symbol, name: &str) -> bool {
        let _ = (type_id, subtype, name);
        false
    }

    /// Whether `name` is a known instance of some type implementing `trait_id`.
    fn trait_impl_member(&self, trait_id: TraitId, name: &str) -> bool {
        let _ = (trait_id, name);
        false
    }
}

/// Facts that know no workspace symbols.
///
/// Symbol-typed matchers then match nothing, which under-grants subtypes
/// instead of over-granting them; consumers with a workspace index pass their
/// own [`SymbolFacts`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSymbolFacts;

impl SymbolFacts for NoSymbolFacts {}

/// The subtypes holding for one instance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubtypeSet {
    entries: Vec<(TypeId, Symbol)>,
}

impl SubtypeSet {
    /// Adds one subtype, ignoring duplicates.
    pub fn insert(&mut self, type_id: TypeId, subtype: Symbol) {
        if !self.entries.contains(&(type_id, subtype)) {
            self.entries.push((type_id, subtype));
        }
    }

    /// Whether this exact `(type, subtype)` entry holds.
    #[must_use]
    pub fn contains(&self, type_id: TypeId, subtype: Symbol) -> bool {
        self.entries.contains(&(type_id, subtype))
    }

    /// Whether any type grants `subtype`.
    #[must_use]
    pub fn contains_name(&self, subtype: Symbol) -> bool {
        self.entries.iter().any(|(_, name)| *name == subtype)
    }

    /// Whether no subtype holds.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The number of holding subtypes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterates the holding `(type, subtype)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (TypeId, Symbol)> + '_ {
        self.entries.iter().copied()
    }
}

/// The fields a `(schema, key, shape)` lookup can match.
///
/// Exact-key overloads of the requested shape come first in written order,
/// then the schema's patterns in written order (§3.2).
pub struct Candidates<'ir> {
    ir: &'ir RulesIr,
    exact: std::slice::Iter<'ir, FieldId>,
    patterns: std::slice::Iter<'ir, FieldId>,
    shape: Shape,
}

impl Iterator for Candidates<'_> {
    type Item = FieldId;

    fn next(&mut self) -> Option<FieldId> {
        for id in self.exact.by_ref() {
            if self.ir.shape(*id) == Some(self.shape) {
                return Some(*id);
            }
        }
        for id in self.patterns.by_ref() {
            if self.ir.shape(*id) == Some(self.shape) {
                return Some(*id);
            }
        }
        None
    }
}

impl fmt::Debug for Candidates<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Candidates").finish_non_exhaustive()
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_debug(hasher: &mut Sha256, value: &impl fmt::Debug) {
    hash_bytes(hasher, format!("{value:?}").as_bytes());
}

impl RulesIr {
    /// Returns a stable SHA-256 fingerprint of the compiled rules-v2 content.
    ///
    /// Arena order is preserved where it is semantically observable. The
    /// schema exact-key lookup map is sorted by resolved key because its hash
    /// iteration order is an implementation detail. The digest includes the
    /// interned text table and game profile, so an empty IR and any populated
    /// IR have distinct identities and strings cannot alias by symbol index.
    /// Arena fields remain publicly mutable for compiler tooling, so this
    /// method computes the digest from the current content on each call.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"paradoxcode/rules-v2-ir/v1\0");
        hash_debug(&mut hasher, &self.game_id);
        for (symbol, text) in self.strings.iter() {
            hash_debug(&mut hasher, &(symbol, text));
        }
        hash_debug(&mut hasher, &self.files);
        hash_debug(&mut hasher, &self.fields);
        hash_debug(&mut hasher, &self.matchers);
        hash_debug(&mut hasher, &self.types);
        hash_debug(&mut hasher, &self.traits);
        hash_debug(&mut hasher, &self.enums);
        hash_debug(&mut hasher, &self.scopes);
        hash_debug(&mut hasher, &self.provenance);
        for schema in &self.schemas {
            hash_debug(&mut hasher, &schema.name);
            hash_debug(&mut hasher, &schema.arguments);
            let mut exact = schema
                .exact
                .iter()
                .map(|(key, fields)| (self.strings.resolve(*key), fields))
                .collect::<Vec<_>>();
            exact.sort_by(|left, right| left.0.cmp(right.0));
            hash_debug(&mut hasher, &exact);
            hash_debug(&mut hasher, &schema.patterns);
            hash_debug(&mut hasher, &schema.items);
            hash_debug(&mut hasher, &schema.open);
            hash_debug(&mut hasher, &schema.subtype_gates);
        }
        let profile =
            serde_json::to_vec(&self.game.profile).expect("game profiles are serializable");
        hash_bytes(&mut hasher, &profile);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Assembles a rule set from already-lowered parts and builds its name
    /// indexes.
    ///
    /// `crate::lower` is the only caller: the parts are validated while they
    /// are lowered, not here, so an arbitrary set of parts would produce an
    /// arena whose ids do not mean what they claim.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        game_id: Symbol,
        files: Vec<FileRule>,
        schemas: Vec<Schema>,
        fields: Vec<Field>,
        matchers: Vec<Matcher>,
        types: Vec<TypeInfo>,
        traits: Vec<TraitInfo>,
        enums: Vec<EnumInfo>,
        scopes: ScopeModel,
        strings: Interner,
        mut provenance: Vec<Provenance>,
        game: GameConfig,
    ) -> Self {
        provenance.sort_by_key(|entry| entry.field);
        // A declared schema name may have several monomorphised instances; the
        // name index keeps the first, then prefers the argument-free instance,
        // which is the one a name (rather than a call site) means.
        let mut schema_lookup: FxHashMap<Symbol, SchemaId> = FxHashMap::default();
        for (index, schema) in schemas.iter().enumerate() {
            schema_lookup
                .entry(schema.name)
                .or_insert(SchemaId(index as u32));
        }
        for (index, schema) in schemas.iter().enumerate() {
            if schema.arguments.is_empty() {
                schema_lookup.insert(schema.name, SchemaId(index as u32));
            }
        }
        let lookups = Lookups {
            schemas: schema_lookup,
            types: types
                .iter()
                .enumerate()
                .map(|(index, info)| (info.name, TypeId(index as u32)))
                .collect(),
            traits: traits
                .iter()
                .enumerate()
                .map(|(index, info)| (info.name, TraitId(index as u32)))
                .collect(),
            enums: enums
                .iter()
                .enumerate()
                .map(|(index, info)| (info.name, EnumId(index as u32)))
                .collect(),
        };
        Self {
            game_id,
            files,
            schemas,
            fields,
            matchers,
            types,
            traits,
            enums,
            scopes,
            strings,
            provenance,
            game,
            lookups,
        }
    }

    /// An empty rule set: the state a host starts in before a bundle is
    /// installed. Every arena is empty, so a lookup finds nothing rather than
    /// panicking.
    #[must_use]
    pub fn empty() -> Self {
        let mut strings = Interner::default();
        let game_id = strings.intern_folded("");
        Self::new(
            game_id,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ScopeModel::default(),
            strings,
            Vec::new(),
            GameConfig::default(),
        )
    }

    /// The game identity this rule set was compiled for.
    #[must_use]
    pub fn game_id(&self) -> &str {
        self.strings.resolve(self.game_id)
    }

    /// The interner behind every symbol in this rule set.
    #[must_use]
    pub const fn strings(&self) -> &Interner {
        &self.strings
    }

    /// One schema by id.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn schema(&self, id: SchemaId) -> &Schema {
        &self.schemas[id.index()]
    }

    /// One field by id.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn field(&self, id: FieldId) -> &Field {
        &self.fields[id.index()]
    }

    /// One matcher by id.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn matcher(&self, id: MatcherId) -> &Matcher {
        &self.matchers[id.index()]
    }

    /// The schema declared under `name`, if any.
    ///
    /// Matches case-insensitively and prefers the argument-free instance of a
    /// parameterised schema; use [`Self::schema_instances`] to reach every
    /// monomorphised instance.
    #[must_use]
    pub fn schema_by_name(&self, name: &str) -> Option<SchemaId> {
        let symbol = self.strings.lookup_folded(name)?;
        self.lookups.schemas.get(&symbol).copied()
    }

    /// Every monomorphised instance declared under `name`, in creation order.
    pub fn schema_instances(&self, name: &str) -> impl Iterator<Item = SchemaId> + '_ {
        let symbol = self.strings.lookup_folded(name);
        self.schemas
            .iter()
            .enumerate()
            .filter(move |(_, schema)| Some(schema.name) == symbol)
            .map(|(index, _)| SchemaId(index as u32))
    }

    /// The type declared under `name`, if any.
    #[must_use]
    pub fn type_by_name(&self, name: &str) -> Option<TypeId> {
        self.strings
            .lookup_folded(name)
            .and_then(|symbol| self.lookups.types.get(&symbol).copied())
    }

    /// The trait declared under `name`, if any.
    #[must_use]
    pub fn trait_by_name(&self, name: &str) -> Option<TraitId> {
        self.strings
            .lookup_folded(name)
            .and_then(|symbol| self.lookups.traits.get(&symbol).copied())
    }

    /// The enum declared under `name`, if any.
    #[must_use]
    pub fn enum_by_name(&self, name: &str) -> Option<EnumId> {
        self.strings
            .lookup_folded(name)
            .and_then(|symbol| self.lookups.enums.get(&symbol).copied())
    }

    /// One type's information.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn type_info(&self, id: TypeId) -> &TypeInfo {
        &self.types[id.index()]
    }

    /// One trait's information.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn trait_info(&self, id: TraitId) -> &TraitInfo {
        &self.traits[id.index()]
    }

    /// One enum's information.
    ///
    /// # Panics
    ///
    /// Panics when the id does not belong to this rule set.
    #[must_use]
    pub fn enum_info(&self, id: EnumId) -> &EnumInfo {
        &self.enums[id.index()]
    }

    /// The origin of one compiled field, when it has one.
    #[must_use]
    pub fn provenance_of(&self, field: FieldId) -> Option<&Provenance> {
        self.provenance
            .binary_search_by_key(&field, |entry| entry.field)
            .ok()
            .map(|index| &self.provenance[index])
    }

    /// The file rule selecting `path`, if any, with the rule's index.
    ///
    /// The most specific matcher wins; equally specific matchers resolve to the
    /// last rule in entry-name order, mirroring the legacy catalog scan.
    #[must_use]
    pub fn file_rule(&self, path: &LogicalPath) -> Option<(usize, &FileRule)> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.matcher.matches(path))
            .max_by_key(|(_, rule)| rule.matcher.specificity())
    }

    /// The root schema of the document at `path`, for roots that are schemas.
    #[must_use]
    pub fn root_schema(&self, path: &LogicalPath) -> Option<SchemaId> {
        match self.file_rule(path)?.1.root {
            RootRule::Schema(schema) => Some(schema),
            RootRule::Instance { .. } | RootRule::Opaque => None,
        }
    }

    /// What the document root at `path` holds.
    #[must_use]
    pub fn root_rule(&self, path: &LogicalPath) -> Option<&RootRule> {
        Some(&self.file_rule(path)?.1.root)
    }

    /// The shape of a field's value, for §3.2 dispatch.
    ///
    /// A union of quoted-script branches is still quoted script; a union that
    /// mixes shapes has no single shape and is reported as `None`.
    #[must_use]
    pub fn shape(&self, field: FieldId) -> Option<Shape> {
        match self.field(field).value {
            FieldValue::Scalar(matcher) => self.matcher_shape(matcher),
            FieldValue::Block(_) | FieldValue::SelfBlock => Some(Shape::Block),
            FieldValue::Quoted(_) => Some(Shape::Quoted),
        }
    }

    fn matcher_shape(&self, matcher: MatcherId) -> Option<Shape> {
        match self.matcher(matcher) {
            Matcher::Quoted(_) => Some(Shape::Quoted),
            Matcher::Union(alternatives) => {
                let mut shape = None;
                for alternative in alternatives {
                    let candidate = self.matcher_shape(*alternative)?;
                    match shape {
                        None => shape = Some(candidate),
                        Some(existing) if existing == candidate => {}
                        Some(_) => return None,
                    }
                }
                shape.or(Some(Shape::Scalar))
            }
            _ => Some(Shape::Scalar),
        }
    }

    /// Looks up a key in a schema: exact overloads of `shape` in written
    /// order, then the schema's patterns in written order.
    #[must_use]
    pub fn lookup(&self, schema: SchemaId, key: &str, shape: Shape) -> Candidates<'_> {
        let schema = self.schema(schema);
        let exact = self
            .strings
            .lookup_folded(key)
            .and_then(|symbol| schema.exact.get(&symbol))
            .map_or(&[][..], |fields| fields.as_ref());
        Candidates {
            ir: self,
            exact: exact.iter(),
            patterns: schema.patterns.iter(),
            shape,
        }
    }

    /// The schema a block-valued field lowers to, resolving `self` against the
    /// schema the field was matched in.
    #[must_use]
    pub fn child(&self, field: FieldId, current: SchemaId) -> Option<SchemaId> {
        match self.field(field).value {
            FieldValue::Block(schema) | FieldValue::Quoted(schema) => Some(schema),
            FieldValue::SelfBlock => Some(current),
            FieldValue::Scalar(_) => None,
        }
    }

    /// The fields of a schema valid under `subtypes`, deterministically
    /// ordered by [`FieldId`] — the completion surface.
    #[must_use]
    pub fn fields(&self, schema: SchemaId, subtypes: &SubtypeSet) -> Vec<FieldId> {
        let schema = self.schema(schema);
        let mut ids = Vec::with_capacity(schema.exact.len() + schema.patterns.len());
        for overloads in schema.exact.values() {
            ids.extend(overloads.iter().copied());
        }
        ids.extend(schema.patterns.iter().copied());
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|id| self.gate_holds(self.field(*id).gate, subtypes));
        ids
    }

    /// Whether a field's subtype gate holds under `subtypes`.
    #[must_use]
    pub fn gate_holds(&self, gate: Option<Gate>, subtypes: &SubtypeSet) -> bool {
        match gate {
            None => true,
            Some(Gate::When(subtype)) => subtypes.contains_name(subtype),
            Some(Gate::Unless(subtype)) => !subtypes.contains_name(subtype),
        }
    }

    /// The subtypes holding for an instance body, using no workspace facts.
    #[must_use]
    pub fn subtypes_of(&self, schema: SchemaId, body: &impl ScalarFields) -> SubtypeSet {
        self.subtypes_of_with(schema, body, &NoSymbolFacts)
    }

    /// The subtypes holding for an instance body, resolving symbol matchers
    /// through `facts`.
    #[must_use]
    pub fn subtypes_of_with(
        &self,
        schema: SchemaId,
        body: &impl ScalarFields,
        facts: &impl SymbolFacts,
    ) -> SubtypeSet {
        let mut set = SubtypeSet::default();
        for gate in self.schema(schema).subtype_gates.iter() {
            if self.condition_holds(&gate.when, body, facts) {
                set.insert(gate.type_id, gate.subtype);
            }
        }
        set
    }

    /// Whether a subtype predicate holds against an instance body.
    #[must_use]
    pub fn condition_holds(
        &self,
        condition: &SubtypeCond,
        body: &impl ScalarFields,
        facts: &impl SymbolFacts,
    ) -> bool {
        condition.0.iter().all(|(field, expected)| {
            let present = body.scalar(self.strings.resolve(*field));
            match (present, expected) {
                (None, None) => true,
                (None, Some(_)) | (Some(_), None) => false,
                (Some(value), Some(matcher)) => self.scalar_matches(*matcher, value, facts),
            }
        })
    }

    /// Whether a scalar value matches a matcher.
    ///
    /// Decidable from the IR alone except for symbol-typed matchers, which
    /// delegate to `facts`.
    #[must_use]
    pub fn scalar_matches(
        &self,
        matcher: MatcherId,
        value: &str,
        facts: &impl SymbolFacts,
    ) -> bool {
        match self.matcher(matcher) {
            Matcher::Scalar
            | Matcher::Opaque
            | Matcher::Loc
            | Matcher::Path(_)
            | Matcher::Quoted(_)
            | Matcher::Link => !value.is_empty(),
            Matcher::Literal(text) => self.strings.resolve(*text).eq_ignore_ascii_case(value),
            Matcher::Template(parts) => self.template_matches(parts, value, facts),
            Matcher::Int { min, max } => value.parse::<i64>().is_ok_and(|parsed| {
                min.is_none_or(|bound| parsed >= bound) && max.is_none_or(|bound| parsed <= bound)
            }),
            Matcher::Float { min, max } => value.parse::<f64>().is_ok_and(|parsed| {
                min.is_none_or(|bound| parsed >= bound) && max.is_none_or(|bound| parsed <= bound)
            }),
            Matcher::Bool => {
                let folded = value.to_ascii_lowercase();
                folded == "yes" || folded == "no"
            }
            Matcher::Date => is_eu4_date(value),
            Matcher::Ref(target) => match target {
                RefTarget::Type {
                    type_id,
                    subtype,
                    strip_prefix,
                } => {
                    let restored;
                    let name = if let Some(prefix) = strip_prefix {
                        restored = format!("{}{value}", self.strings.resolve(*prefix));
                        restored.as_str()
                    } else {
                        value
                    };
                    subtype.map_or_else(
                        || facts.type_member(*type_id, name),
                        |subtype| facts.type_subtype_member(*type_id, subtype, name),
                    )
                }
                RefTarget::Trait(trait_id) => facts.trait_impl_member(*trait_id, value),
            },
            Matcher::Def { type_id, .. } => facts.type_member(*type_id, value),
            Matcher::Enum { id, rows } => self.enum_contains(*id, rows.as_ref(), value),
            Matcher::Scope(scope) => self.scope_matches(scope.as_ref().copied(), value),
            Matcher::Union(alternatives) => alternatives
                .iter()
                .any(|alternative| self.scalar_matches(*alternative, value, facts)),
        }
    }

    /// Whether `value` is a member of an enum, restricted to `rows` when the
    /// matcher came from an attribute-column group.
    #[must_use]
    pub fn enum_contains(&self, id: EnumId, rows: Option<&BitSet>, value: &str) -> bool {
        let info = self.enum_info(id);
        let Some(symbol) = self.strings.lookup_folded(value) else {
            return false;
        };
        let Some(index) = info.row(symbol) else {
            return false;
        };
        rows.is_none_or(|rows| rows.contains(index))
    }

    /// Whether an actual scope satisfies an expected scope, including declared overrides.
    #[must_use]
    pub fn scopes_compatible(&self, actual: Symbol, expected: Symbol) -> bool {
        actual == expected
            || self.strings.resolve(actual) == "any"
            || self.strings.resolve(expected) == "any"
            || self.scopes.compat.contains(&(actual, expected))
    }

    /// Whether a scope expression matches the named scope type.
    ///
    /// Decidable from the IR when the value is the scope type itself or a
    /// plain link name (through the scope model). A register or a template
    /// link has no statically known scope, so it does not match — the same
    /// under-granting rule as [`SymbolFacts`].
    #[must_use]
    pub fn scope_matches(&self, expected: Option<Symbol>, value: &str) -> bool {
        let Some(symbol) = self.strings.lookup_folded(value) else {
            return false;
        };
        if expected.is_none() {
            return self.scopes.types.contains(&symbol)
                || self.scopes.link(&self.strings, symbol).is_some();
        }
        if expected.is_some_and(|expected| self.scopes_compatible(symbol, expected)) {
            return true;
        }
        if let Some(link) = self.scopes.link(&self.strings, symbol) {
            return match link.to {
                ScopeRef::Any => true,
                ScopeRef::Type(actual) => {
                    expected.is_some_and(|expected| self.scopes_compatible(actual, expected))
                }
            };
        }
        false
    }

    fn template_matches(
        &self,
        parts: &[TemplatePart],
        value: &str,
        facts: &impl SymbolFacts,
    ) -> bool {
        let Some((first, rest)) = parts.split_first() else {
            return value.is_empty();
        };
        match first {
            TemplatePart::Text(text) => {
                let text = self.strings.resolve(*text);
                value.len() >= text.len()
                    && value.is_char_boundary(text.len())
                    && value[..text.len()].eq_ignore_ascii_case(text)
                    && self.template_matches(rest, &value[text.len()..], facts)
            }
            TemplatePart::Hole(hole) => value
                .char_indices()
                .map(|(index, _)| index)
                .skip(1)
                .chain(std::iter::once(value.len()))
                .any(|end| {
                    self.scalar_matches(*hole, &value[..end], facts)
                        && self.template_matches(rest, &value[end..], facts)
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_folds_identity_and_keeps_text() {
        let mut interner = Interner::default();
        let folded = interner.intern_folded("Event");
        let verbatim = interner.intern_verbatim("Event");
        assert_eq!(interner.resolve(folded), "event");
        assert_eq!(interner.resolve(verbatim), "Event");
        assert_ne!(folded, verbatim, "identity and text symbols are distinct");
        assert_eq!(interner.lookup_folded("EVENT"), Some(folded));
        assert_eq!(interner.lookup_verbatim("Event"), Some(verbatim));
        assert_eq!(interner.lookup_folded("Event"), Some(folded));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn bit_sets_track_members() {
        let mut set = BitSet::new(3);
        assert!(set.is_empty());
        set.insert(0);
        set.insert(70);
        assert!(set.contains(0));
        assert!(!set.contains(1));
        assert!(set.contains(70));
        assert_eq!(set.len(), 2);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec![0, 70]);
    }

    #[test]
    fn fingerprint_is_stable_and_distinguishes_empty_from_compiled_ir() {
        let empty = RulesIr::empty();
        assert_eq!(empty.fingerprint(), RulesIr::empty().fingerprint());

        let source = r#"{"files":{"sample":{"path":"common/sample","root":"sample"}},"schemas":{"sample":{"fields":{"alpha":{"value":"scalar","card":"0..1"},"omega":{"value":"scalar","card":"0..1"}}}}}"#;
        let files = vec![(
            "sample.json".to_owned(),
            serde_json::from_str(source).expect("rule source parses"),
        )];
        let compiled = crate::lower::lower(&files, GameConfig::default()).expect("lowers");
        assert_ne!(empty.fingerprint(), compiled.fingerprint());
        assert_eq!(compiled.fingerprint(), compiled.clone().fingerprint());

        let mut reordered = compiled.clone();
        let schema = &mut reordered.schemas[0];
        let mut entries = schema
            .exact
            .iter()
            .map(|(key, fields)| (*key, fields.clone()))
            .collect::<Vec<_>>();
        entries.reverse();
        schema.exact = FxHashMap::default();
        schema.exact.extend(entries);
        assert_eq!(compiled.fingerprint(), reordered.fingerprint());
    }
}
