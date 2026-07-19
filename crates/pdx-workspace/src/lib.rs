//! Workspace state and immutable snapshot boundary.
//!
//! `AnalysisHost` is the mutable owner. Queries later consume `AnalysisSnapshot` values and
//! must not depend on editor protocol types.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use pdx_eu4::{Eu4Rules, FileResolutionPolicy, ParserKind, SymbolResolutionPolicy};
use pdx_syntax::{CstKind, CstNode, Eu4FileFormat, ParsedFile, parse_eu4, parse_eu4_csv_file};
use pdx_text::{LineIndex, LogicalPath, TextRange};

/// Stable identity for a source root during one host lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceRootId(u32);

impl SourceRootId {
    /// Creates an ID. Callers that allocate IDs should keep them stable.
    #[must_use = "iterate the retained definitions"]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A source root ordered by the future overlay resolver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRoot {
    /// Stable identity.
    pub id: SourceRootId,
    /// Root kind; resolution order is implemented in a later phase.
    pub kind: SourceRootKind,
    /// Explicit filesystem path.
    pub path: PathBuf,
    /// Explicit low-to-high order among roots of the same kind.
    pub order: u32,
    /// Whether this root is allowed to own generated or edited files.
    pub writable: bool,
}

impl SourceRoot {
    /// Creates a root with an order derived from its stable ID.
    #[must_use]
    pub fn new(id: SourceRootId, kind: SourceRootKind, path: PathBuf) -> Self {
        let writable = matches!(kind, SourceRootKind::CurrentMod);
        Self { id, kind, path, order: id.get(), writable }
    }
}

/// Source-root category.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceRootKind {
    /// The local Vanilla installation/cache.
    Vanilla,
    /// An explicitly ordered dependency Mod.
    Dependency,
    /// The current Mod being edited.
    CurrentMod,
}

/// Stable identity for a discovered source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceFileId(u64);

impl SourceFileId {
    /// Creates an ID from a stable root/path hash.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A discovered file candidate in one physical source root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    /// Stable file identity.
    pub id: SourceFileId,
    /// Owning source root.
    pub root_id: SourceRootId,
    /// Physical disk path.
    pub physical_path: PathBuf,
    /// EU4 logical path relative to the root.
    pub logical_path: LogicalPath,
    /// Rules catalog category, when one matched.
    pub category_id: Option<String>,
    /// File resolution policy selected by the rules catalog.
    pub resolution: FileResolutionPolicy,
}

/// A candidate retained by overlay resolution, including shadowed definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCandidate {
    /// Logical path.
    pub logical_path: LogicalPath,
    /// Disk file identity, if this is a disk candidate.
    pub file_id: Option<SourceFileId>,
    /// Overlay document identity, if this is an in-memory candidate.
    pub document_id: Option<DocumentId>,
    /// Candidate priority; larger values win.
    pub priority: u64,
    /// File policy used to determine whether lower candidates remain active.
    pub resolution: Option<FileResolutionPolicy>,
    /// Whether this candidate is active for its logical path.
    pub active: bool,
}

/// One symbol definition retained in an index shard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Definition {
    /// Semantic kind, for example event or localisation.
    pub kind: String,
    /// Symbol name as written in source.
    pub name: String,
    /// Defining file.
    pub file_id: SourceFileId,
    /// Source range of the definition.
    pub range: TextRange,
    /// Whether this definition wins symbol resolution.
    pub active: bool,
}

/// A source reference retained for later semantic resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    /// Reference category.
    pub kind: String,
    /// Referenced name.
    pub name: String,
    /// Referencing file.
    pub file_id: SourceFileId,
    /// Source range of the reference.
    pub range: TextRange,
}

/// Atomic parse/HIR/index output for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIndexShard {
    /// File that produced this shard.
    pub file_id: SourceFileId,
    /// Definitions in source order.
    pub definitions: Vec<Definition>,
    /// References in source order.
    pub references: Vec<Reference>,
    /// Syntax error count retained as a cheap health signal.
    pub syntax_error_count: usize,
}

/// Workspace-wide symbol index made from immutable file shards.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceIndex {
    shards: BTreeMap<SourceFileId, FileIndexShard>,
    definitions: BTreeMap<(String, String), Vec<Definition>>,
    references: BTreeMap<SourceFileId, Vec<Reference>>,
}

impl WorkspaceIndex {
    /// Creates an empty index.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns all retained definitions for a kind/name, including shadowed ones.
    #[must_use]
    pub fn definitions(&self, kind: &str, name: &str) -> &[Definition] {
        self.definitions
            .get(&(kind.to_owned(), name.to_ascii_lowercase()))
            .map_or(&[], Vec::as_slice)
    }

    /// Returns the active definition for a kind/name, if one exists.
    #[must_use]
    pub fn active_definition(&self, kind: &str, name: &str) -> Option<&Definition> {
        self.definitions(kind, name).iter().find(|definition| definition.active)
    }

    /// Iterates over all retained definitions in deterministic kind/name order.
    #[must_use = "iterate the retained definitions"]
    pub fn definitions_iter(&self) -> impl Iterator<Item = &Definition> {
        self.definitions.values().flat_map(|definitions| definitions.iter())
    }

    /// Returns the shard for a file.
    #[must_use]
    pub fn shard(&self, file_id: SourceFileId) -> Option<&FileIndexShard> {
        self.shards.get(&file_id)
    }

    /// Returns all references from a file.
    #[must_use]
    pub fn references(&self, file_id: SourceFileId) -> &[Reference] {
        self.references.get(&file_id).map_or(&[], Vec::as_slice)
    }

    /// Replaces one shard as a single map operation.
    pub fn replace_shard(&mut self, shard: FileIndexShard) {
        self.shards.insert(shard.file_id, shard);
        self.rebuild_maps();
    }

    /// Removes a file shard.
    pub fn remove_shard(&mut self, file_id: SourceFileId) {
        self.shards.remove(&file_id);
        self.rebuild_maps();
    }

    fn resolve_priorities(&mut self, priorities: &BTreeMap<SourceFileId, u64>, rules: &Eu4Rules) {
        for values in self.definitions.values_mut() {
            let policy = rules
                .model()
                .symbol_descriptors
                .iter()
                .find(|descriptor| {
                    descriptor.kind_id
                        == values.first().map_or("", |definition| definition.kind.as_str())
                })
                .map_or(SymbolResolutionPolicy::ReplaceBySymbol, |descriptor| {
                    descriptor.resolution
                });
            let winner = values
                .iter()
                .enumerate()
                .max_by_key(|(_, definition)| {
                    priorities.get(&definition.file_id).copied().unwrap_or(0)
                })
                .map(|(index, _)| index);
            for (index, definition) in values.iter_mut().enumerate() {
                definition.active = match policy {
                    SymbolResolutionPolicy::Merge | SymbolResolutionPolicy::Unique => true,
                    SymbolResolutionPolicy::ReplaceBySymbol => Some(index) == winner,
                };
            }
            values.sort_by_key(|definition| (!definition.active, definition.file_id));
        }
    }

    fn rebuild_maps(&mut self) {
        self.definitions.clear();
        self.references.clear();
        for shard in self.shards.values() {
            for definition in &shard.definitions {
                self.definitions
                    .entry((definition.kind.clone(), definition.name.to_ascii_lowercase()))
                    .or_default()
                    .push(definition.clone());
            }
            self.references.insert(shard.file_id, shard.references.clone());
        }
        for values in self.definitions.values_mut() {
            values.sort_by_key(|definition| (!definition.active, definition.file_id));
        }
    }
}

/// Vanilla cache metadata. Loading is explicit; a host never refreshes it implicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VanillaIndexCacheMetadata {
    /// Cache format version.
    pub schema_version: u32,
    /// Rules hash used to create the cache.
    pub rule_hash: String,
    /// Source identity supplied by the caller.
    pub source_identity: String,
}

/// Explicitly managed Vanilla index cache seam.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VanillaIndexCache {
    metadata: Option<VanillaIndexCacheMetadata>,
    index: WorkspaceIndex,
}

impl VanillaIndexCache {
    /// Creates an unconfigured cache. It never scans or refreshes by itself.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns cache metadata when a caller has explicitly installed a snapshot.
    #[must_use]
    pub fn metadata(&self) -> Option<&VanillaIndexCacheMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the cached immutable index.
    #[must_use]
    pub fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Installs a newly rebuilt cache. This is the only refresh operation.
    pub fn refresh(&mut self, metadata: VanillaIndexCacheMetadata, index: WorkspaceIndex) {
        self.metadata = Some(metadata);
        self.index = index;
    }
}

/// Stable identity for an editor document during one server lifetime.
///
/// The value is the client URI rather than a filesystem path. This keeps the identity stable for
/// unsaved and non-file documents while leaving URI/path conversion to `pdx-lsp`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DocumentId(String);

impl DocumentId {
    /// Creates a document identity from its client URI.
    #[must_use]
    pub fn new(uri: impl Into<String>) -> Self {
        Self(uri.into())
    }

    /// Returns the client URI.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The current candidate for a document.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DocumentSource {
    /// Unsaved editor text is currently overriding the backing candidate.
    Overlay,
    /// Text was recovered from the backing filesystem candidate after close.
    Disk,
}

/// A document candidate exposed by an immutable workspace snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSnapshot {
    id: DocumentId,
    version: Option<i64>,
    text: String,
    line_index: LineIndex,
    source: DocumentSource,
    path: Option<PathBuf>,
}

impl DocumentSnapshot {
    /// Returns the document identity.
    #[must_use]
    pub fn id(&self) -> &DocumentId {
        &self.id
    }

    /// Returns the editor version, or `None` for a disk candidate.
    #[must_use]
    pub const fn version(&self) -> Option<i64> {
        self.version
    }

    /// Returns the lossless document text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the UTF-8/UTF-16 line index for this text.
    #[must_use]
    pub const fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// Returns whether this candidate is an overlay or disk text.
    #[must_use]
    pub const fn source(&self) -> DocumentSource {
        self.source
    }

    /// Returns the backing filesystem path, when this URI is a file URI.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }
}

/// One editor-neutral document change. Ranges use UTF-8 byte offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextChange {
    /// A byte range to replace, or `None` for a full-document replacement.
    pub range: Option<TextRange>,
    /// Replacement text.
    pub text: String,
}

impl TextChange {
    /// Creates a full-document replacement.
    #[must_use]
    pub fn full(text: impl Into<String>) -> Self {
        Self { range: None, text: text.into() }
    }

    /// Creates a ranged replacement.
    #[must_use]
    pub fn ranged(range: TextRange, text: impl Into<String>) -> Self {
        Self { range: Some(range), text: text.into() }
    }
}

/// A change applied by the event loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceChange {
    /// Replace the configured source roots.
    SetSourceRoots(Vec<SourceRoot>),
    /// Replace the explicit workspace root.
    SetWorkspaceRoot(Option<PathBuf>),
}

/// Errors raised while applying an editor document event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    /// An open notification was received for a document that is already open.
    AlreadyOpen(DocumentId),
    /// A change or close notification targeted no open overlay.
    NotOpen(DocumentId),
    /// A change version was not newer than the current version.
    StaleVersion { document: DocumentId, current: i64, received: i64 },
    /// A change range was not on UTF-8 boundaries or exceeded the current text.
    InvalidRange { document: DocumentId, range: TextRange },
}

impl fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyOpen(document) => {
                write!(formatter, "document is already open: {}", document.as_str())
            }
            Self::NotOpen(document) => {
                write!(formatter, "document is not open: {}", document.as_str())
            }
            Self::StaleVersion { document, current, received } => write!(
                formatter,
                "stale document version for {}: current {}, received {}",
                document.as_str(),
                current,
                received
            ),
            Self::InvalidRange { document, range } => write!(
                formatter,
                "invalid UTF-8 document range {}..{} for {}",
                range.start(),
                range.end(),
                document.as_str()
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Errors raised while materializing source roots and index shards.
#[derive(Debug)]
pub enum WorkspaceError {
    /// Filesystem discovery or read failure.
    Io(std::io::Error),
    /// A root-relative path escaped its logical root.
    InvalidLogicalPath(PathBuf),
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "workspace I/O error: {error}"),
            Self::InvalidLogicalPath(path) => {
                write!(formatter, "invalid workspace logical path: {}", path.display())
            }
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn collect_disk_files(
    root: &std::path::Path,
    current: &std::path::Path,
    output: &mut Vec<(LogicalPath, PathBuf)>,
) -> Result<(), WorkspaceError> {
    let mut entries = fs::read_dir(current)
        .map_err(WorkspaceError::Io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(WorkspaceError::Io)?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_disk_files(root, &path, output)?;
            continue;
        }
        if path.is_symlink() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        let logical = LogicalPath::parse(&relative)
            .map_err(|_| WorkspaceError::InvalidLogicalPath(path.clone()))?;
        output.push((logical, path));
    }
    Ok(())
}

fn stable_file_id(root: SourceRootId, logical: &LogicalPath, salt: u64) -> u64 {
    let mut value = 0xcbf29ce484222325_u64 ^ u64::from(root.get());
    for byte in logical.as_str().bytes() {
        value = (value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    value ^ salt.wrapping_mul(0x9e3779b97f4a7c15)
}

fn root_priority(root: &SourceRoot) -> u64 {
    match root.kind {
        SourceRootKind::Vanilla => 0,
        SourceRootKind::Dependency => 1_000 + u64::from(root.order),
        SourceRootKind::CurrentMod => 10_000 + u64::from(root.order),
    }
}

fn build_shard(file: &SourceFile, source: &str, rules: &Eu4Rules) -> FileIndexShard {
    let Some(category) = rules.classify(&file.logical_path) else {
        return FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: 0,
        };
    };
    match &category.parser {
        ParserKind::PdxScript => {
            let parsed = parse_eu4(Eu4FileFormat::PdxScript, source);
            shard_from_parsed(file, &parsed, category.id.as_str(), rules)
        }
        ParserKind::Localisation => {
            let parsed = parse_eu4(Eu4FileFormat::Localisation, source);
            shard_from_parsed(file, &parsed, category.id.as_str(), rules)
        }
        ParserKind::Csv(dialect) => {
            let dialect = match dialect {
                pdx_eu4::CsvDialect::Comma => pdx_syntax::csv::CsvDialect::Comma,
                pdx_eu4::CsvDialect::Tab => pdx_syntax::csv::CsvDialect::Tab,
                pdx_eu4::CsvDialect::Semicolon => pdx_syntax::csv::CsvDialect::Semicolon,
            };
            let parsed = parse_eu4_csv_file(source, dialect);
            let definitions =
                if file.logical_path.as_str().eq_ignore_ascii_case("map/definition.csv") {
                    parsed
                        .parse()
                        .records
                        .iter()
                        .flat_map(|record| record.cells.first())
                        .filter_map(|cell| {
                            let name = parsed.source()[usize::try_from(cell.value_range.start())
                                .ok()?
                                ..usize::try_from(cell.value_range.end()).ok()?]
                                .trim()
                                .to_owned();
                            name.parse::<u32>().ok().map(|_| Definition {
                                kind: "province_id".to_owned(),
                                name,
                                file_id: file.id,
                                range: cell.value_range,
                                active: true,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            FileIndexShard {
                file_id: file.id,
                definitions,
                references: Vec::new(),
                syntax_error_count: parsed.errors().len(),
            }
        }
        ParserKind::Asset | ParserKind::SyntaxOnly => FileIndexShard {
            file_id: file.id,
            definitions: Vec::new(),
            references: Vec::new(),
            syntax_error_count: 0,
        },
    }
}

fn shard_from_parsed(
    file: &SourceFile,
    parsed: &ParsedFile,
    category_id: &str,
    rules: &Eu4Rules,
) -> FileIndexShard {
    let mut definitions = Vec::new();
    let mut references = Vec::new();
    collect_semantics(
        file,
        parsed,
        parsed.root(),
        category_id,
        false,
        true,
        &mut definitions,
        &mut references,
    );
    collect_scripted_effect_params(file, parsed, &mut definitions);
    collect_eu4_dynamic_members(file, parsed.root(), parsed, &mut definitions, None);
    collect_cwt_type_members(file, parsed, rules, &mut definitions);
    FileIndexShard {
        file_id: file.id,
        definitions,
        references,
        syntax_error_count: parsed.errors().len(),
    }
}

/// Collects workspace members declared by CWT `type[...]` definitions.
///
/// CWTools builds these members from the parsed workspace, rather than treating a type's name as
/// a literal root key. For example, `type[mission]` with `skip_root_key = any` exposes every child
/// of every root clause in `missions/*.txt` as a `<mission>` member. Keeping this in the workspace
/// shard makes CWT key/value matching, completion, and hover see the same dynamic names.
fn collect_cwt_type_members(
    file: &SourceFile,
    parsed: &ParsedFile,
    rules: &Eu4Rules,
    definitions: &mut Vec<Definition>,
) {
    for descriptor in rules.model().cwt.type_descriptors.values() {
        if !cwt_type_path_matches(descriptor, &file.logical_path) {
            continue;
        }

        if descriptor.type_per_file {
            let Some(file_name) = file.logical_path.as_str().rsplit('/').next() else {
                continue;
            };
            let name = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
            if !name.is_empty() {
                definitions.push(Definition {
                    kind: descriptor.name.clone(),
                    name: name.to_owned(),
                    file_id: file.id,
                    range: parsed.root().range(),
                    active: true,
                });
            }
            continue;
        }

        if descriptor.skip_root_paths.is_empty() {
            for child in parsed.root().children() {
                if child.kind() == CstKind::Property {
                    collect_cwt_type_definition(file, parsed, descriptor, child, definitions);
                }
            }
        } else {
            for root in parsed.root().children() {
                if root.kind() != CstKind::Property {
                    continue;
                }
                for skip_path in &descriptor.skip_root_paths {
                    collect_cwt_skip_root_path(
                        file,
                        parsed,
                        descriptor,
                        root,
                        skip_path,
                        definitions,
                    );
                }
            }
        }
    }
}

fn collect_cwt_skip_root_path(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_eu4::CwtTypeDescriptor,
    node: &CstNode,
    path: &[String],
    definitions: &mut Vec<Definition>,
) {
    let Some(head) = path.first() else {
        collect_cwt_block_children(file, parsed, descriptor, node, definitions);
        return;
    };
    let node_key = cwt_property_key(node, parsed).unwrap_or_default();
    if !head.eq_ignore_ascii_case("any") && !head.eq_ignore_ascii_case(&node_key) {
        return;
    }
    if path.len() == 1 {
        collect_cwt_block_children(file, parsed, descriptor, node, definitions);
        return;
    }
    for child in cwt_block_properties(node) {
        collect_cwt_skip_root_path(file, parsed, descriptor, child, &path[1..], definitions);
    }
}

fn collect_cwt_block_children(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_eu4::CwtTypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    for child in cwt_block_properties(node) {
        collect_cwt_type_definition(file, parsed, descriptor, child, definitions);
    }
}

fn collect_cwt_type_definition(
    file: &SourceFile,
    parsed: &ParsedFile,
    descriptor: &pdx_eu4::CwtTypeDescriptor,
    node: &CstNode,
    definitions: &mut Vec<Definition>,
) {
    let Some(key) = cwt_property_key(node, parsed) else { return };
    if !cwt_type_key_matches(descriptor, &key) {
        return;
    }
    let Some(name) = descriptor
        .name_field
        .as_deref()
        .and_then(|field| find_property(node, field, parsed))
        .or_else(|| Some(key))
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    definitions.push(Definition {
        kind: descriptor.name.clone(),
        name,
        file_id: file.id,
        range: node.range(),
        active: true,
    });
}

fn cwt_type_key_matches(descriptor: &pdx_eu4::CwtTypeDescriptor, key: &str) -> bool {
    descriptor.type_key_filter.as_ref().is_none_or(|(values, negate)| {
        (values.iter().any(|value| value.eq_ignore_ascii_case(key))) != *negate
    })
}

fn cwt_block_properties(node: &CstNode) -> impl Iterator<Item = &CstNode> {
    node.children().iter().flat_map(|child| {
        if child.kind() != CstKind::Value {
            return Vec::new();
        }
        child
            .children()
            .iter()
            .filter(|block| block.kind() == CstKind::Block)
            .flat_map(|block| {
                block.children().iter().filter(|child| child.kind() == CstKind::Property)
            })
            .collect::<Vec<_>>()
    })
}

fn cwt_property_key(node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    node.children()
        .iter()
        .find(|child| child.kind() == CstKind::Key)
        .and_then(|child| parsed.text(child.range()))
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn cwt_type_path_matches(
    descriptor: &pdx_eu4::CwtTypeDescriptor,
    logical_path: &LogicalPath,
) -> bool {
    let path = logical_path.as_str().replace('\\', "/").to_ascii_lowercase();
    let (directory, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
    if let Some(prefix) = descriptor.path.as_deref() {
        let prefix =
            prefix.trim_matches('/').strip_prefix("game/").unwrap_or(prefix.trim_matches('/'));
        let prefix = prefix.to_ascii_lowercase();
        let matches = if descriptor.path_strict {
            directory == prefix
        } else {
            directory == prefix || directory.starts_with(&format!("{prefix}/"))
        };
        if !matches {
            return false;
        }
    }
    if let Some(expected_file) = descriptor.path_file.as_deref()
        && !file_name.eq_ignore_ascii_case(expected_file)
    {
        return false;
    }
    if let Some(expected_extension) = descriptor.path_extension.as_deref() {
        let expected_extension = expected_extension.trim_start_matches('.');
        let actual_extension = file_name.rsplit_once('.').map_or("", |(_, extension)| extension);
        if !actual_extension.eq_ignore_ascii_case(expected_extension) {
            return false;
        }
    }
    true
}

fn collect_eu4_dynamic_members(
    file: &SourceFile,
    node: &CstNode,
    parsed: &ParsedFile,
    definitions: &mut Vec<Definition>,
    parent_key: Option<&str>,
) {
    if node.kind() == CstKind::Property {
        let key = node
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Key)
            .and_then(|child| parsed.text(child.range()))
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let kind = dynamic_member_kind(&key, parent_key);
        if let Some(kind) = kind
            && let Some((name, range)) = direct_property_scalar(node, parsed)
            && !name.is_empty()
        {
            definitions.push(Definition {
                kind: kind.to_owned(),
                name,
                file_id: file.id,
                range,
                active: true,
            });
        }
        for child in node.children() {
            collect_eu4_dynamic_members(file, child, parsed, definitions, Some(&key));
        }
        return;
    }
    for child in node.children() {
        collect_eu4_dynamic_members(file, child, parsed, definitions, parent_key);
    }
}

fn dynamic_member_kind(key: &str, parent_key: Option<&str>) -> Option<&'static str> {
    Some(match key {
        "set_country_flag" => "country_flag",
        "set_global_flag" => "global_flag",
        "set_province_flag" => "province_flag",
        "set_ruler_flag" => "ruler_flag",
        "set_heir_flag" => "heir_flag",
        "set_consort_flag" => "consort_flag",
        "save_event_target_as" => "event_target",
        "save_global_event_target_as" => "global_event_target",
        "set_saved_name" => "saved_name",
        "which"
            if matches!(
                parent_key,
                Some("set_variable")
                    | Some("change_variable")
                    | Some("new_variable")
                    | Some("new_variables")
            ) =>
        {
            "variable"
        }
        _ => return None,
    })
}

fn direct_property_scalar(node: &CstNode, parsed: &ParsedFile) -> Option<(String, TextRange)> {
    for child in node.children() {
        if matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString) {
            return parsed
                .text(child.range())
                .map(|value| (value.trim_matches('"').trim().to_owned(), child.range()));
        }
        if child.kind() == CstKind::Value {
            if let Some(value) = child
                .children()
                .iter()
                .find(|value| matches!(value.kind(), CstKind::BareValue | CstKind::QuotedString))
            {
                return parsed
                    .text(value.range())
                    .map(|text| (text.trim_matches('"').trim().to_owned(), value.range()));
            }
        }
    }
    None
}

fn collect_scripted_effect_params(
    file: &SourceFile,
    parsed: &ParsedFile,
    definitions: &mut Vec<Definition>,
) {
    let path = file.logical_path.as_str().to_ascii_lowercase();
    if !(path.starts_with("common/scripted_effects/")
        || path.starts_with("common/scripted_triggers/"))
    {
        return;
    }
    for token in parsed.tokens().iter().filter(|token| token.kind() == pdx_syntax::TokenKind::Bare)
    {
        let Some(raw) = parsed.text(token.range()) else { continue };
        let mut opening = None;
        for (offset, character) in raw.char_indices() {
            if character != '$' {
                continue;
            }
            if let Some(start) = opening.take() {
                if start + 1 < offset {
                    let name = raw[start + 1..offset].to_owned();
                    let token_start = usize::try_from(token.range().start()).unwrap_or(0);
                    let start =
                        u32::try_from(token_start.saturating_add(start)).unwrap_or(u32::MAX);
                    let end = u32::try_from(
                        token_start.saturating_add(offset.saturating_add(character.len_utf8())),
                    )
                    .unwrap_or(u32::MAX);
                    let range = TextRange::new(start, end).unwrap_or(token.range());
                    definitions.push(Definition {
                        kind: "scripted_effect_param".to_owned(),
                        name: name.clone(),
                        file_id: file.id,
                        range,
                        active: true,
                    });
                    definitions.push(Definition {
                        kind: "scripted_effect_param_dollar".to_owned(),
                        name: format!("${name}$"),
                        file_id: file.id,
                        range,
                        active: true,
                    });
                }
            } else {
                opening = Some(offset);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_semantics(
    file: &SourceFile,
    parsed: &ParsedFile,
    node: &CstNode,
    category_id: &str,
    inside_key: bool,
    top_level: bool,
    definitions: &mut Vec<Definition>,
    references: &mut Vec<Reference>,
) {
    match node.kind() {
        CstKind::LocalisationEntry => {
            if let Some(key) =
                node.children().iter().find(|child| child.kind() == CstKind::LocalisationKey)
            {
                if let Some(name) = parsed.text(key.range()) {
                    definitions.push(Definition {
                        kind: "localisation".to_owned(),
                        name: name.trim().to_owned(),
                        file_id: file.id,
                        range: node.range(),
                        active: true,
                    });
                }
            }
        }
        CstKind::Property => {
            let key = node
                .children()
                .iter()
                .find(|child| child.kind() == CstKind::Key)
                .and_then(|child| parsed.text(child.range()))
                .map(str::trim)
                .map(str::to_owned);
            if let Some(key) = key {
                if top_level {
                    if let Some(kind) =
                        definition_kind(file.logical_path.as_str(), &key, node, parsed)
                    {
                        let name = event_name(node, parsed).unwrap_or_else(|| key.clone());
                        definitions.push(Definition {
                            kind,
                            name,
                            file_id: file.id,
                            range: node.range(),
                            active: true,
                        });
                        if file
                            .logical_path
                            .as_str()
                            .to_ascii_lowercase()
                            .contains("common/government_reforms/")
                            && find_property(node, "legacy_government", parsed)
                                .is_some_and(|value| value.eq_ignore_ascii_case("yes"))
                            && find_property(node, "legacy_equivalent", parsed).is_none()
                        {
                            definitions.push(Definition {
                                kind: "hardcoded_legacy_government".to_owned(),
                                name: key.clone(),
                                file_id: file.id,
                                range: node.range(),
                                active: true,
                            });
                        }
                    }
                    if file
                        .logical_path
                        .as_str()
                        .to_ascii_lowercase()
                        .contains("common/country_tags")
                        && key.eq_ignore_ascii_case("countries")
                    {
                        for child in node.children() {
                            if child.kind() != CstKind::Value {
                                continue;
                            }
                            for block_child in child.children() {
                                if block_child.kind() != CstKind::Block {
                                    continue;
                                }
                                for country in block_child.children() {
                                    if country.kind() != CstKind::Property {
                                        continue;
                                    }
                                    let Some(country_key) = country
                                        .children()
                                        .iter()
                                        .find(|child| child.kind() == CstKind::Key)
                                        .and_then(|child| parsed.text(child.range()))
                                        .map(str::trim)
                                    else {
                                        continue;
                                    };
                                    definitions.push(Definition {
                                        kind: "country_tag".to_owned(),
                                        name: country_key.to_owned(),
                                        file_id: file.id,
                                        range: country.range(),
                                        active: true,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        CstKind::BareValue if !inside_key => {
            if let Some(name) =
                parsed.text(node.range()).map(str::trim).filter(|value| !value.is_empty())
            {
                references.push(Reference {
                    kind: category_id.to_owned(),
                    name: name.to_owned(),
                    file_id: file.id,
                    range: node.range(),
                });
            }
        }
        _ => {}
    }
    for child in node.children() {
        collect_semantics(
            file,
            parsed,
            child,
            category_id,
            inside_key || node.kind() == CstKind::Key,
            top_level && node.kind() == CstKind::Document,
            definitions,
            references,
        );
    }
}

fn definition_kind(path: &str, key: &str, node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    let path = path.to_ascii_lowercase();
    if path.contains("scripted_effect") {
        return Some("scripted_effect".to_owned());
    }
    if path.contains("scripted_trigger") {
        return Some("scripted_trigger".to_owned());
    }
    if path.contains("events/") || key.ends_with("_event") {
        return Some("event".to_owned());
    }
    if node.children().iter().any(|child| child.kind() == CstKind::Value)
        && (key == "country_event" || key == "province_event")
    {
        return Some("event".to_owned());
    }
    if let Some(kind) = eu4_dynamic_definition_kind(&path) {
        return Some(kind.to_owned());
    }
    let _ = parsed;
    None
}

fn eu4_dynamic_definition_kind(path: &str) -> Option<&'static str> {
    let path = path.trim_end_matches('/');
    if path.contains("common/country_tags") {
        return None;
    }
    let directory = path.rsplit_once('/').map_or(path, |(directory, _)| directory);
    Some(match directory {
        "common/cultures" => "culture",
        "common/religions" => "religion",
        "common/tradenodes" => "trade_node",
        "common/colonial_regions" => "colonial_region",
        "common/estates" => "estate",
        "common/ideas" => "idea_group",
        "common/governments" => "government",
        "common/government_reforms" => "government_reform",
        "common/subject_types" => "subject_type",
        "common/technologies" => "technology",
        "common/buildings" => "building",
        "common/units" => "unit_type",
        "common/mercenary_companies" => "mercenary_company",
        "common/trade_companies" => "trade_company",
        "common/advisortypes" => "advisor_type",
        "common/leader_personalities" => "leader_personality",
        "common/ruler_personalities" => "ruler_personality",
        "common/event_modifiers" => "event_modifier",
        "common/static_modifiers" => "static_modifier",
        "common/timed_modifiers" => "timed_modifier",
        "common/triggered_modifiers" => "triggered_modifier",
        "common/subject_type_upgrades" => "subject_type_upgrade",
        "common/peace_treaties" => "peace_treaty",
        "common/casus_belli" | "common/cb_types" => "casus_belli",
        "common/wargoal_types" => "wargoal_type",
        "common/institutions" => "institution",
        "common/great_projects" => "great_project",
        "common/estate_privileges" => "estate_privilege",
        "common/estate_agendas" => "estate_agenda",
        "common/diplomatic_actions" | "common/new_diplomatic_actions" => "diplomatic_action",
        "common/disasters" => "disaster",
        "common/rebel_types" => "rebel_type",
        "common/insults" => "insult",
        "common/opinion_modifiers" => "opinion_modifier",
        "common/tradegoods" => "tradegood",
        _ => return None,
    })
}

fn event_name(node: &CstNode, parsed: &ParsedFile) -> Option<String> {
    for child in node.children() {
        if child.kind() != CstKind::Value {
            continue;
        }
        if let Some(id) = find_property(child, "id", parsed) {
            return Some(id);
        }
    }
    None
}

fn find_property(node: &CstNode, wanted: &str, parsed: &ParsedFile) -> Option<String> {
    if node.kind() == CstKind::Property {
        let key = node
            .children()
            .iter()
            .find(|child| child.kind() == CstKind::Key)
            .and_then(|child| parsed.text(child.range()))
            .map(str::trim);
        if key == Some(wanted) {
            for child in node.children() {
                if matches!(child.kind(), CstKind::BareValue | CstKind::QuotedString) {
                    return parsed
                        .text(child.range())
                        .map(|value| value.trim_matches('"').trim().to_owned());
                }
                if child.kind() == CstKind::Value {
                    if let Some(value) = child.children().iter().find(|value| {
                        matches!(value.kind(), CstKind::BareValue | CstKind::QuotedString)
                    }) {
                        return parsed
                            .text(value.range())
                            .map(|value| value.trim_matches('"').trim().to_owned());
                    }
                }
            }
        }
    }
    node.children().iter().find_map(|child| find_property(child, wanted, parsed))
}

/// Mutable owner of workspace state.
#[derive(Clone, Debug)]
pub struct AnalysisHost {
    revision: u64,
    rules: Arc<Eu4Rules>,
    roots: Vec<SourceRoot>,
    workspace_root: Option<PathBuf>,
    documents: BTreeMap<DocumentId, DocumentSnapshot>,
    source_files: BTreeMap<SourceFileId, SourceFile>,
    disk_text: BTreeMap<SourceFileId, String>,
    index: WorkspaceIndex,
}

impl AnalysisHost {
    /// Creates an empty host with the bootstrap EU4 rule identity.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Eu4Rules::empty())
    }

    /// Creates an empty host around an immutable rule database.
    #[must_use]
    pub fn new(rules: Eu4Rules) -> Self {
        Self {
            revision: 0,
            rules: Arc::new(rules),
            roots: Vec::new(),
            workspace_root: None,
            documents: BTreeMap::new(),
            source_files: BTreeMap::new(),
            disk_text: BTreeMap::new(),
            index: WorkspaceIndex::empty(),
        }
    }

    /// Applies one event-loop change and advances the snapshot revision.
    pub fn apply_change(&mut self, change: WorkspaceChange) {
        match change {
            WorkspaceChange::SetSourceRoots(roots) => self.roots = roots,
            WorkspaceChange::SetWorkspaceRoot(root) => self.workspace_root = root,
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// Scans all configured roots in stable order and atomically refreshes source files and shards.
    pub fn refresh_source_roots(&mut self) -> Result<(), WorkspaceError> {
        let mut files = BTreeMap::new();
        let mut texts = BTreeMap::new();
        let mut next_index = 0_u64;
        for root in &self.roots {
            let mut paths = Vec::new();
            collect_disk_files(&root.path, &root.path, &mut paths)?;
            paths.sort_by(|left, right| left.0.cmp(&right.0));
            for (logical, physical) in paths {
                let id = SourceFileId::new(stable_file_id(root.id, &logical, next_index));
                next_index = next_index.saturating_add(1);
                let Some(category) = self.rules.classify(&logical) else { continue };
                let text = fs::read_to_string(&physical).map_err(WorkspaceError::Io)?;
                let source_file = SourceFile {
                    id,
                    root_id: root.id,
                    physical_path: physical,
                    logical_path: logical,
                    category_id: Some(category.id.clone()),
                    resolution: category.resolution,
                };
                files.insert(id, source_file);
                texts.insert(id, text);
            }
        }
        let mut index = WorkspaceIndex::empty();
        for (id, file) in &files {
            if let Some(text) = texts.get(id) {
                let shard = build_shard(file, text, self.rules.as_ref());
                index.replace_shard(shard);
            }
        }
        let priorities = files
            .values()
            .filter_map(|file| {
                self.roots
                    .iter()
                    .find(|root| root.id == file.root_id)
                    .map(|root| (file.id, root_priority(root)))
            })
            .collect::<BTreeMap<_, _>>();
        index.resolve_priorities(&priorities, self.rules.as_ref());
        self.source_files = files;
        self.disk_text = texts;
        self.index = index;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Returns a mutable workspace index for targeted shard replacement.
    pub fn replace_index_shard(&mut self, shard: FileIndexShard) {
        self.index.replace_shard(shard);
        self.revision = self.revision.saturating_add(1);
    }

    /// Opens a document overlay with a complete initial text.
    pub fn open_document(
        &mut self,
        id: DocumentId,
        version: i64,
        text: String,
        path: Option<PathBuf>,
    ) -> Result<(), DocumentError> {
        if self
            .documents
            .get(&id)
            .is_some_and(|document| document.source == DocumentSource::Overlay)
        {
            return Err(DocumentError::AlreadyOpen(id));
        }
        let line_index = LineIndex::new(&text);
        self.documents.insert(
            id.clone(),
            DocumentSnapshot {
                id,
                version: Some(version),
                text,
                line_index,
                source: DocumentSource::Overlay,
                path,
            },
        );
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Applies all changes from one `didChange` notification atomically.
    pub fn apply_document_changes(
        &mut self,
        id: &DocumentId,
        version: i64,
        changes: &[TextChange],
    ) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let current_version = current.version.unwrap_or(version);
        if version <= current_version {
            return Err(DocumentError::StaleVersion {
                document: id.clone(),
                current: current_version,
                received: version,
            });
        }

        let mut text = current.text.clone();
        for change in changes {
            if let Some(range) = change.range {
                let start = usize::try_from(range.start()).ok();
                let end = usize::try_from(range.end()).ok();
                let valid = start
                    .zip(end)
                    .is_some_and(|(start, end)| start <= end && text.get(start..end).is_some());
                if !valid {
                    return Err(DocumentError::InvalidRange { document: id.clone(), range });
                }
                if let (Some(start), Some(end)) = (start, end) {
                    text.replace_range(start..end, &change.text);
                }
            } else {
                text = change.text.clone();
            }
        }

        let path = current.path.clone();
        self.documents.insert(
            id.clone(),
            DocumentSnapshot {
                id: id.clone(),
                version: Some(version),
                line_index: LineIndex::new(&text),
                text,
                source: DocumentSource::Overlay,
                path,
            },
        );
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Closes an overlay and restores its current disk candidate when available.
    pub fn close_document(&mut self, id: &DocumentId) -> Result<(), DocumentError> {
        let Some(current) = self.documents.get(id) else {
            return Err(DocumentError::NotOpen(id.clone()));
        };
        if current.source != DocumentSource::Overlay {
            return Err(DocumentError::NotOpen(id.clone()));
        }
        let path = current.path.clone();
        self.documents.remove(id);
        if let Some(path) = path {
            if let Ok(text) = fs::read_to_string(&path) {
                self.documents.insert(
                    id.clone(),
                    DocumentSnapshot {
                        id: id.clone(),
                        version: None,
                        line_index: LineIndex::new(&text),
                        text,
                        source: DocumentSource::Disk,
                        path: Some(path),
                    },
                );
            }
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    /// Captures an immutable query view.
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        AnalysisSnapshot {
            revision: self.revision,
            rules: Arc::clone(&self.rules),
            roots: Arc::from(self.roots.clone()),
            workspace_root: self.workspace_root.clone(),
            documents: Arc::new(self.documents.clone()),
            source_files: Arc::new(self.source_files.clone()),
            disk_text: Arc::new(self.disk_text.clone()),
            index: Arc::new(self.index.clone()),
        }
    }
}

/// Immutable workspace view used by analysis queries.
#[derive(Clone, Debug)]
pub struct AnalysisSnapshot {
    revision: u64,
    rules: Arc<Eu4Rules>,
    roots: Arc<[SourceRoot]>,
    workspace_root: Option<PathBuf>,
    documents: Arc<BTreeMap<DocumentId, DocumentSnapshot>>,
    source_files: Arc<BTreeMap<SourceFileId, SourceFile>>,
    disk_text: Arc<BTreeMap<SourceFileId, String>>,
    index: Arc<WorkspaceIndex>,
}

impl AnalysisSnapshot {
    /// Returns the monotonic revision captured by this snapshot.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the immutable EU4 rules used for this snapshot.
    #[must_use]
    pub fn rules(&self) -> &Eu4Rules {
        &self.rules
    }

    /// Returns source roots in configured order.
    #[must_use]
    pub fn source_roots(&self) -> &[SourceRoot] {
        &self.roots
    }

    /// Returns the explicit workspace root, if configured.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_root.as_deref()
    }

    /// Returns all current document candidates keyed by stable document identity.
    #[must_use]
    pub fn documents(&self) -> &BTreeMap<DocumentId, DocumentSnapshot> {
        &self.documents
    }

    /// Returns one current document candidate.
    #[must_use]
    pub fn document(&self, id: &DocumentId) -> Option<&DocumentSnapshot> {
        self.documents.get(id)
    }

    /// Returns all discovered source files.
    #[must_use]
    pub fn source_files(&self) -> &BTreeMap<SourceFileId, SourceFile> {
        &self.source_files
    }

    /// Returns the immutable file/symbol index.
    #[must_use]
    pub fn index(&self) -> &WorkspaceIndex {
        &self.index
    }

    /// Resolves one logical path, retaining lower-priority candidates as shadowed entries.
    #[must_use]
    pub fn resolve(&self, logical_path: &LogicalPath) -> Vec<ResolvedCandidate> {
        let mut candidates = self
            .source_files
            .values()
            .filter(|file| &file.logical_path == logical_path)
            .map(|file| {
                let priority =
                    self.roots.iter().find(|root| root.id == file.root_id).map_or(0, root_priority);
                ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: Some(file.id),
                    document_id: None,
                    priority,
                    resolution: Some(file.resolution),
                    active: false,
                }
            })
            .collect::<Vec<_>>();
        for document in self.documents.values() {
            let Some(path) = document.path() else { continue };
            let Some(root) = self.roots.iter().find(|root| path.starts_with(&root.path)) else {
                continue;
            };
            let Ok(relative) = path
                .strip_prefix(&root.path)
                .map(|value| LogicalPath::parse(&value.to_string_lossy()))
            else {
                continue;
            };
            if relative.as_ref().is_ok_and(|value| value == logical_path) {
                candidates.push(ResolvedCandidate {
                    logical_path: logical_path.clone(),
                    file_id: None,
                    document_id: Some(document.id().clone()),
                    priority: 20_000,
                    resolution: None,
                    active: false,
                });
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.priority));
        let overlay_present = candidates.iter().any(|candidate| candidate.document_id.is_some());
        if overlay_present {
            if let Some(first) = candidates.first_mut() {
                first.active = true;
            }
        } else if candidates
            .first()
            .is_some_and(|candidate| candidate.resolution == Some(FileResolutionPolicy::Merge))
        {
            for candidate in &mut candidates {
                candidate.active = true;
            }
        } else if let Some(first) = candidates.first_mut() {
            first.active = true;
        }
        candidates
    }

    /// Returns the current text for a disk file, if it was scanned.
    #[must_use]
    pub fn source_text(&self, file_id: SourceFileId) -> Option<&str> {
        self.disk_text.get(&file_id).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        AnalysisHost, DocumentId, DocumentSource, SourceRoot, SourceRootId, SourceRootKind,
        TextChange,
    };
    use pdx_eu4::Eu4Rules;
    use pdx_text::{LogicalPath, TextRange};

    #[test]
    fn stale_document_versions_are_rejected_atomically() {
        let mut host = AnalysisHost::new(Eu4Rules::empty());
        let id = DocumentId::new("file:///tmp/example.txt");
        host.open_document(id.clone(), 1, "a😀z".to_owned(), None).expect("open should succeed");
        let range = TextRange::new(1, 5).expect("emoji range");
        let error = host
            .apply_document_changes(&id, 1, &[TextChange::ranged(range, "x")])
            .expect_err("same version must be rejected");
        assert!(matches!(error, super::DocumentError::StaleVersion { .. }));
        assert_eq!(host.snapshot().document(&id).expect("document exists").text(), "a😀z");
        host.apply_document_changes(&id, 2, &[TextChange::ranged(range, "x")])
            .expect("new version should succeed");
        assert_eq!(host.snapshot().document(&id).expect("document exists").text(), "axz");
    }

    #[test]
    fn close_restores_the_backing_disk_candidate() {
        let path = std::env::temp_dir().join(format!("pdx-workspace-{}.txt", std::process::id()));
        fs::write(&path, "disk").expect("write fixture");
        let mut host = AnalysisHost::new(Eu4Rules::empty());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
            SourceRootId::new(1),
            SourceRootKind::CurrentMod,
            path.parent().expect("temp parent").to_owned(),
        )]));
        let id = DocumentId::new("file:///tmp/pdx-workspace.txt");
        host.open_document(id.clone(), 1, "overlay".to_owned(), Some(path.clone()))
            .expect("open should succeed");
        host.close_document(&id).expect("close should succeed");
        let snapshot = host.snapshot();
        let document = snapshot.document(&id).expect("disk candidate exists");
        assert_eq!(document.source(), DocumentSource::Disk);
        assert_eq!(document.version(), None);
        assert_eq!(document.text(), "disk");
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn roots_overlay_and_shards_preserve_shadowed_semantic_definitions() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-workspace-phase4-{nonce}"));
        let vanilla = root.join("vanilla");
        let dependency = root.join("dependency");
        let current = root.join("current");
        for directory in [
            vanilla.join("common/events"),
            dependency.join("common/events"),
            dependency.join("common/scripted_effects"),
            current.join("common/events"),
            current.join("common/scripted_triggers"),
            current.join("localisation"),
        ] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        fs::write(vanilla.join("common/events/foo.txt"), "country_event = { id = foo.1 }\n")
            .expect("vanilla event");
        fs::write(dependency.join("common/events/foo.txt"), "country_event = { id = foo.1 }\n")
            .expect("dependency event");
        fs::write(
            dependency.join("common/scripted_effects/effects.txt"),
            "heal_army = { add_manpower = 1 }\n",
        )
        .expect("effect");
        let current_event = current.join("common/events/foo.txt");
        fs::write(&current_event, "country_event = { id = foo.1 }\n").expect("current event");
        fs::write(
            current.join("common/scripted_triggers/triggers.txt"),
            "is_ready = { always = yes }\n",
        )
        .expect("trigger");
        fs::write(
            current.join("localisation/test_l_english.yml"),
            "l_english:\n foo_name:0 \"Foo\"\n",
        )
        .expect("localisation");

        let mut host = AnalysisHost::new(Eu4Rules::bootstrap());
        host.apply_change(super::WorkspaceChange::SetSourceRoots(vec![
            SourceRoot {
                id: SourceRootId::new(1),
                kind: SourceRootKind::Vanilla,
                path: vanilla,
                order: 0,
                writable: false,
            },
            SourceRoot {
                id: SourceRootId::new(2),
                kind: SourceRootKind::Dependency,
                path: dependency,
                order: 0,
                writable: false,
            },
            SourceRoot {
                id: SourceRootId::new(3),
                kind: SourceRootKind::CurrentMod,
                path: current.clone(),
                order: 0,
                writable: true,
            },
        ]));
        host.refresh_source_roots().expect("scan roots");
        let snapshot = host.snapshot();
        let event_definitions = snapshot.index().definitions("event", "foo.1");
        assert_eq!(event_definitions.len(), 3);
        assert_eq!(
            snapshot.index().active_definition("event", "foo.1").expect("active event").file_id,
            event_definitions[0].file_id
        );
        assert_eq!(snapshot.index().definitions("scripted_effect", "heal_army").len(), 1);
        assert_eq!(snapshot.index().definitions("scripted_trigger", "is_ready").len(), 1);
        assert_eq!(snapshot.index().definitions("localisation", "foo_name").len(), 1);

        let logical = LogicalPath::new("common/events/foo.txt");
        assert_eq!(
            snapshot.resolve(&logical).iter().filter(|candidate| candidate.active).count(),
            1
        );
        host.open_document(
            DocumentId::new("file:///current/foo.txt"),
            1,
            "country_event = { id = foo.1 }\n".to_owned(),
            Some(current_event.clone()),
        )
        .expect("overlay");
        let overlay_snapshot = host.snapshot();
        let resolved = overlay_snapshot.resolve(&logical);
        assert!(resolved.first().and_then(|candidate| candidate.document_id.as_ref()).is_some());
        assert!(resolved.first().is_some_and(|candidate| candidate.active));
        fs::remove_dir_all(root).expect("cleanup");
    }
}
