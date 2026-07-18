//! One-time EU4 CWT bootstrap importer.
//!
//! CWT is deliberately parsed here rather than in the editor runtime. The importer preserves
//! source order, duplicate keys, directives, and documentation while lowering to scalar
//! normalized rule rows owned by pdx-eu4.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pdx_eu4::{
    CwtKeyMatcher, CwtRuleShape, CwtSemanticRule, CwtTypeDescriptor, CwtValueMatcher, Eu4Rules,
    FileCategory, FileMatcher, FileResolutionPolicy, ParserKind, RuleRecord, RulesError,
    RulesModel, SymbolDescriptor, SymbolResolutionPolicy,
};
use pdx_text::LogicalPath;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const IMPORTER_VERSION: &str = "phase12-cwt-starts-with-1";

/// A scalar or nested CWT value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CwtValue {
    /// A quoted or unquoted scalar.
    Scalar(String),
    /// A bracketed child block.
    Block(Vec<CwtNode>),
    /// A key with no value.
    Empty,
}

/// A parsed CWT node with source metadata retained for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CwtNode {
    /// Key, including bracketed selectors such as `type[<event>]`.
    pub key: String,
    /// Operator between key and value, when present.
    pub operator: Option<String>,
    /// Value shape and children.
    pub value: CwtValue,
    /// Documentation comments attached to this node.
    pub documentation: Vec<String>,
    /// ## directives attached to this node.
    pub directives: Vec<String>,
    /// Source file relative to the explicit importer root.
    pub source_file: String,
    /// One-based source line.
    pub line: usize,
    /// Stable source order within the file.
    pub order: usize,
}

/// Parses one CWT document without silently dropping unknown syntax.
pub fn parse_cwt(
    source_file: impl Into<String>,
    source: &str,
) -> Result<Vec<CwtNode>, ImportError> {
    let source_file = source_file.into();
    let tokens = lex(&source_file, source)?;
    Parser { source_file, tokens, position: 0, order: 0 }.parse_document()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Word(String),
    Quoted(String),
    Operator(String),
    Open,
    Close,
    Documentation(String),
    Directive(String),
    Newline,
    Eof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    line: usize,
}

fn lex(source_file: &str, source: &str) -> Result<Vec<Token>, ImportError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut line = 1;
    while index < chars.len() {
        match chars[index] {
            '\n' => {
                tokens.push(Token { kind: TokenKind::Newline, line });
                line += 1;
                index += 1;
            }
            '\r' => index += 1,
            character if character.is_whitespace() => index += 1,
            '#' => {
                let directive = chars.get(index + 1).is_some_and(|value| *value == '#');
                let documentation =
                    directive && chars.get(index + 2).is_some_and(|value| *value == '#');
                let prefix = if documentation {
                    3
                } else if directive {
                    2
                } else {
                    1
                };
                let start = index + prefix;
                let mut end = start;
                while end < chars.len() && chars[end] != '\n' {
                    end += 1;
                }
                let value: String = chars[start..end].iter().collect::<String>().trim().to_owned();
                if documentation {
                    tokens.push(Token { kind: TokenKind::Documentation(value), line });
                } else if directive {
                    tokens.push(Token { kind: TokenKind::Directive(value), line });
                }
                index = end;
            }
            '{' => {
                tokens.push(Token { kind: TokenKind::Open, line });
                index += 1;
            }
            '}' => {
                tokens.push(Token { kind: TokenKind::Close, line });
                index += 1;
            }
            '"' => {
                let start_line = line;
                index += 1;
                let mut value = String::new();
                let mut closed = false;
                while index < chars.len() {
                    match chars[index] {
                        '"' => {
                            index += 1;
                            closed = true;
                            break;
                        }
                        '\\' if index + 1 < chars.len() => {
                            value.push(chars[index + 1]);
                            index += 2;
                        }
                        '\n' => {
                            value.push('\n');
                            line += 1;
                            index += 1;
                        }
                        character => {
                            value.push(character);
                            index += 1;
                        }
                    }
                }
                if !closed {
                    return Err(ImportError::Parse {
                        file: source_file.to_owned(),
                        line: start_line,
                        message: "unterminated quoted scalar".to_owned(),
                    });
                }
                tokens.push(Token { kind: TokenKind::Quoted(value), line: start_line });
            }
            '<' if chars[index + 1..].iter().position(|value| *value == '>').is_some_and(
                |end| end > 0 && !chars[index + 1..index + 1 + end].contains(&' '),
            ) =>
            {
                let start = index;
                index += 1;
                while index < chars.len() && chars[index] != '>' {
                    index += 1;
                }
                if index < chars.len() {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Word(chars[start..index].iter().collect()),
                    line,
                });
            }
            '=' | '!' | '<' | '>' => {
                let start = index;
                index += 1;
                if chars.get(index).is_some_and(|value| *value == '=') {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Operator(chars[start..index].iter().collect()),
                    line,
                });
            }
            character => {
                let start_line = line;
                let start = index;
                let mut bracket_depth = 0_u32;
                while index < chars.len()
                    && !chars[index].is_whitespace()
                    && (bracket_depth > 0
                        || !matches!(chars[index], '{' | '}' | '#' | '=' | '!' | '<' | '>'))
                {
                    match chars[index] {
                        '[' => bracket_depth = bracket_depth.saturating_add(1),
                        ']' => bracket_depth = bracket_depth.saturating_sub(1),
                        _ => {}
                    }
                    index += 1;
                }
                if start == index {
                    return Err(ImportError::Parse {
                        file: source_file.to_owned(),
                        line,
                        message: format!("unsupported character {character:?}"),
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::Word(chars[start..index].iter().collect()),
                    line: start_line,
                });
            }
        }
    }
    tokens.push(Token { kind: TokenKind::Eof, line });
    Ok(tokens)
}

struct Parser {
    source_file: String,
    tokens: Vec<Token>,
    position: usize,
    order: usize,
}

impl Parser {
    fn parse_document(&mut self) -> Result<Vec<CwtNode>, ImportError> {
        self.parse_block(false)
    }

    fn parse_block(&mut self, nested: bool) -> Result<Vec<CwtNode>, ImportError> {
        let mut nodes = Vec::new();
        let mut documentation = Vec::new();
        let mut directives = Vec::new();
        loop {
            match &self.tokens[self.position].kind {
                TokenKind::Newline => self.position += 1,
                TokenKind::Documentation(value) => {
                    documentation.push(value.clone());
                    self.position += 1;
                }
                TokenKind::Directive(value) => {
                    directives.push(value.clone());
                    self.position += 1;
                }
                TokenKind::Close if nested => {
                    self.position += 1;
                    return Ok(nodes);
                }
                TokenKind::Close => return Err(self.error("unexpected closing brace")),
                TokenKind::Eof if nested => return Err(self.error("unclosed block")),
                TokenKind::Eof => return Ok(nodes),
                TokenKind::Word(_) | TokenKind::Quoted(_) => {
                    let token = self.tokens[self.position].clone();
                    let key = match token.kind {
                        TokenKind::Word(value) | TokenKind::Quoted(value) => value,
                        _ => unreachable!(),
                    };
                    self.position += 1;
                    let operator = match &self.tokens[self.position].kind {
                        TokenKind::Operator(value) => {
                            let value = Some(value.clone());
                            self.position += 1;
                            value
                        }
                        _ => None,
                    };
                    let value = match &self.tokens[self.position].kind {
                        TokenKind::Open => {
                            self.position += 1;
                            CwtValue::Block(self.parse_block(true)?)
                        }
                        TokenKind::Word(value) => {
                            let value = value.clone();
                            self.position += 1;
                            CwtValue::Scalar(value)
                        }
                        TokenKind::Quoted(value) => {
                            let value = value.clone();
                            self.position += 1;
                            CwtValue::Scalar(value)
                        }
                        TokenKind::Close
                        | TokenKind::Newline
                        | TokenKind::Eof
                        | TokenKind::Documentation(_)
                        | TokenKind::Directive(_) => CwtValue::Empty,
                        TokenKind::Operator(value) => {
                            return Err(self.error(&format!("operator without scalar: {value}")));
                        }
                    };
                    let node = CwtNode {
                        key,
                        operator,
                        value,
                        documentation: std::mem::take(&mut documentation),
                        directives: std::mem::take(&mut directives),
                        source_file: self.source_file.clone(),
                        line: token.line,
                        order: self.order,
                    };
                    self.order += 1;
                    nodes.push(node);
                }
                TokenKind::Operator(value) => {
                    return Err(self.error(&format!("operator without key: {value}")));
                }
                TokenKind::Open => return Err(self.error("unexpected opening brace")),
            }
        }
    }

    fn error(&self, message: &str) -> ImportError {
        ImportError::Parse {
            file: self.source_file.clone(),
            line: self.tokens[self.position].line,
            message: message.to_owned(),
        }
    }
}

/// Options for an explicit one-time EU4 import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportOptions {
    /// One explicit CWT directory.
    pub source: PathBuf,
    /// SQLite destination.
    pub output: PathBuf,
    /// Optional deterministic manifest output.
    pub manifest: Option<PathBuf>,
    /// Optional human-readable report output.
    pub report: Option<PathBuf>,
}

/// Per-file provenance emitted in reports and manifests.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportFileReport {
    /// Stable source path.
    pub path: String,
    /// SHA-256 of source bytes.
    pub sha256: String,
    /// Number of normalized rows lowered from this file.
    pub record_count: usize,
}

/// A successful import summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Explicit input files considered by the importer.
    pub input_count: usize,
    /// Destination selected by the caller.
    pub output: PathBuf,
    /// Canonical logical rule hash.
    pub rule_hash: String,
    /// Number of normalized rows.
    pub record_count: usize,
    /// Stable source inventory.
    pub source_files: Vec<ImportFileReport>,
    /// Non-fatal importer notes.
    pub warnings: Vec<String>,
    /// Counts of normalized construct bases found in the source corpus.
    pub construct_counts: BTreeMap<String, usize>,
    /// Counts of directive names, including legacy spellings after normalization.
    pub directive_counts: BTreeMap<String, usize>,
    /// Directive spellings that were observed but not recognized by the importer inventory.
    pub unhandled_directives: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ImportManifest {
    schema_version: u32,
    importer_version: String,
    rule_hash: String,
    files: Vec<ImportFileReport>,
}

/// Errors returned by the CWT importer.
#[derive(Debug)]
pub enum ImportError {
    /// Filesystem failure.
    Io(std::io::Error),
    /// JSON serialization failure.
    Json(serde_json::Error),
    /// Rule artifact failure.
    Rules(RulesError),
    /// A CWT syntax failure.
    Parse { file: String, line: usize, message: String },
    /// A normalized source path was duplicated.
    DuplicatePath(String),
    /// A source path cannot be represented as a logical path.
    InvalidPath(String),
    /// CLI argument failure.
    Arguments(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "CWT import I/O error: {error}"),
            Self::Json(error) => write!(formatter, "CWT import JSON error: {error}"),
            Self::Rules(error) => write!(formatter, "CWT rules error: {error}"),
            Self::Parse { file, line, message } => {
                write!(formatter, "CWT parse error in {file}:{line}: {message}")
            }
            Self::DuplicatePath(path) => write!(formatter, "duplicate CWT logical path: {path}"),
            Self::InvalidPath(path) => write!(formatter, "invalid CWT logical path: {path}"),
            Self::Arguments(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ImportError {}
impl From<std::io::Error> for ImportError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for ImportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<RulesError> for ImportError {
    fn from(error: RulesError) -> Self {
        Self::Rules(error)
    }
}

/// Runs the explicit directory importer and atomically publishes its outputs.
pub fn import_with_options(options: &ImportOptions) -> Result<ImportReport, ImportError> {
    if !options.source.is_dir() {
        return Err(ImportError::Arguments(format!(
            "CWT source is not a directory: {}",
            options.source.display()
        )));
    }
    let sources = discover_sources(&options.source)?;
    let mut model = RulesModel::bootstrap();
    let mut source_files = Vec::new();
    let mut warnings = Vec::new();
    let mut parsed_sources = Vec::new();
    let mut order = 0_u32;
    for (relative, absolute) in sources {
        let bytes = fs::read(&absolute)?;
        let source = String::from_utf8(bytes.clone()).map_err(|_| ImportError::Parse {
            file: relative.clone(),
            line: 1,
            message: "source is not valid UTF-8".to_owned(),
        })?;
        let nodes = parse_cwt(relative.clone(), &source)?;
        parsed_sources.push((relative.clone(), nodes.clone()));
        let before = model.records.len();
        lower_nodes(&mut model, &nodes, None, &mut order);
        let record_count = model.records.len().saturating_sub(before);
        if record_count == 0 {
            warnings.push(format!("no nodes found in {relative}"));
        }
        source_files.push(ImportFileReport {
            path: relative,
            sha256: digest_hex(&bytes),
            record_count,
        });
    }
    compile_semantic_model(&mut model, &parsed_sources);
    let inventory = inventory_sources(&parsed_sources);
    for directive in &inventory.unhandled_directives {
        warnings.push(format!("unhandled CWT directive spelling: {directive}"));
    }
    add_path_categories(&mut model);
    let rules = Eu4Rules::from_model(model);
    atomic_write_rules(&rules, &options.output, &source_files)?;
    let report = ImportReport {
        input_count: source_files.len(),
        output: options.output.clone(),
        rule_hash: rules.rule_hash().to_hex(),
        record_count: rules.model().records.len(),
        source_files: source_files.clone(),
        warnings,
        construct_counts: inventory.construct_counts,
        directive_counts: inventory.directive_counts,
        unhandled_directives: inventory.unhandled_directives.into_iter().collect(),
    };
    let manifest = ImportManifest {
        schema_version: rules.schema_version(),
        importer_version: IMPORTER_VERSION.to_owned(),
        rule_hash: rules.rule_hash().to_hex(),
        files: source_files,
    };
    if let Some(path) = &options.manifest {
        atomic_write_json(path, &manifest)?;
    }
    if let Some(path) = &options.report {
        atomic_write_json(path, &report)?;
    }
    Ok(report)
}

#[derive(Default)]
struct CwtInventory {
    construct_counts: BTreeMap<String, usize>,
    directive_counts: BTreeMap<String, usize>,
    unhandled_directives: BTreeSet<String>,
}

fn inventory_sources(sources: &[(String, Vec<CwtNode>)]) -> CwtInventory {
    let mut inventory = CwtInventory::default();
    for (_, nodes) in sources {
        inventory_nodes(nodes, &mut inventory);
    }
    inventory
}

fn inventory_nodes(nodes: &[CwtNode], inventory: &mut CwtInventory) {
    for node in nodes {
        let base = node.key.split('[').next().unwrap_or(&node.key).to_ascii_lowercase();
        if is_cwt_construct(&base) {
            *inventory.construct_counts.entry(base).or_default() += 1;
        }
        for directive in &node.directives {
            let (name, handled) = inventory_directive_name(directive);
            *inventory.directive_counts.entry(name.clone()).or_default() += 1;
            if !handled {
                inventory.unhandled_directives.insert(directive.clone());
            }
        }
        if let CwtValue::Block(children) = &node.value {
            inventory_nodes(children, inventory);
        }
    }
}

fn inventory_directive_name(directive: &str) -> (String, bool) {
    let trimmed = directive.trim();
    let assignment_name = trimmed.split_once('=').map_or(trimmed, |(name, _)| name).trim();
    let first_word = assignment_name
        .trim_start_matches('#')
        .split_whitespace()
        .next()
        .unwrap_or(assignment_name);
    let normalized = first_word.to_ascii_lowercase();
    let handled = matches!(
        normalized.as_str(),
        "abbreviation"
            | "cardinality"
            | "display_name"
            | "graph_related_types"
            | "localisation"
            | "localisation_key"
            | "name_field"
            | "name_from_file"
            | "only_if_not"
            | "operator"
            | "optional"
            | "path"
            | "path_extension"
            | "path_file"
            | "path_strict"
            | "primary"
            | "push_scope"
            | "replace_scope"
            | "replace_scopes"
            | "required"
            | "scope"
            | "severity"
            | "skip_root_key"
            | "should_be_used"
            | "start_from_root"
            | "starts_with"
            | "type_key_filter"
            | "type_per_file"
            | "unique"
    );
    let has_assignment = trimmed.contains('=');
    if !handled && !has_assignment {
        return ("comment".to_owned(), true);
    }
    (normalized, handled)
}

fn is_cwt_construct(base: &str) -> bool {
    matches!(
        base,
        "types"
            | "type"
            | "subtypes"
            | "subtype"
            | "enum"
            | "complex_enum"
            | "variable_enum"
            | "alias"
            | "aliases"
            | "scope"
            | "scopes"
            | "link"
            | "scope_link"
            | "scope_links"
            | "effect"
            | "effects"
            | "trigger"
            | "triggers"
            | "modifier"
            | "modifiers"
            | "localisation"
            | "folder"
            | "folders"
    )
}

/// Compatibility entry point accepting one explicit source directory.
pub fn import_eu4(inputs: &[PathBuf], output: &Path) -> Result<ImportReport, ImportError> {
    let Some(source) = inputs.first() else {
        return Err(ImportError::Arguments(
            "at least one explicit CWT source directory is required".to_owned(),
        ));
    };
    if inputs.len() != 1 || !source.is_dir() {
        return Err(ImportError::Arguments(
            "import_eu4 requires exactly one source directory".to_owned(),
        ));
    }
    import_with_options(&ImportOptions {
        source: source.clone(),
        output: output.to_owned(),
        manifest: None,
        report: None,
    })
}

fn discover_sources(root: &Path) -> Result<Vec<(String, PathBuf)>, ImportError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut seen = BTreeSet::new();
    for (relative, _) in &files {
        if !seen.insert(relative.to_ascii_lowercase()) {
            return Err(ImportError::DuplicatePath(relative.clone()));
        }
    }
    Ok(files)
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), ImportError> {
    let mut entries = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_symlink() {
            return Err(ImportError::InvalidPath(path.display().to_string()));
        }
        if path.is_dir() {
            collect_files(root, &path, files)?;
            continue;
        }
        if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("cwt")) {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| ImportError::InvalidPath(path.display().to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let logical = LogicalPath::parse(&relative)
                .map_err(|_| ImportError::InvalidPath(relative.clone()))?;
            files.push((logical.to_string(), path));
        }
    }
    Ok(())
}

fn lower_nodes(model: &mut RulesModel, nodes: &[CwtNode], parent: Option<&str>, order: &mut u32) {
    for (index, node) in nodes.iter().enumerate() {
        let logical_id = format!("{}:{}:{}", node.source_file, parent.unwrap_or("root"), index);
        let base = node.key.split('[').next().unwrap_or(&node.key).to_ascii_lowercase();
        let table = match base.as_str() {
            "types" | "type" => "types",
            "subtypes" | "subtype" => "subtypes",
            "enum" | "complex_enum" | "variable_enum" => "enums",
            "alias" => "aliases",
            "scope" | "scopes" => "scopes",
            "link" | "scope_link" | "scope_links" => "links",
            "effect" | "effects" => "effects",
            "trigger" | "triggers" => "triggers",
            "modifier" | "modifiers" => "modifiers",
            "localisation" => "localisation",
            "folder" | "folders" => "folders",
            _ => "cwt_nodes",
        };
        let mut fields = BTreeMap::new();
        fields.insert("key".to_owned(), node.key.clone());
        fields.insert(
            "shape".to_owned(),
            match node.value {
                CwtValue::Scalar(_) => "scalar",
                CwtValue::Block(_) => "block",
                CwtValue::Empty => "empty",
            }
            .to_owned(),
        );
        if let Some(operator) = &node.operator {
            fields.insert("operator".to_owned(), operator.clone());
        }
        if let CwtValue::Scalar(value) = &node.value {
            fields.insert("value".to_owned(), value.clone());
        }
        if !node.documentation.is_empty() {
            fields.insert("documentation".to_owned(), node.documentation.join("\n"));
        }
        if !node.directives.is_empty() {
            fields.insert("directives".to_owned(), node.directives.join("\n"));
        }
        fields.insert("source_file".to_owned(), node.source_file.clone());
        fields.insert("line".to_owned(), node.line.to_string());
        if let CwtValue::Block(children) = &node.value {
            fields.insert("child_count".to_owned(), children.len().to_string());
        }
        model.records.push(RuleRecord {
            table: table.to_owned(),
            logical_id: logical_id.clone(),
            source_order: *order,
            fields,
        });
        *order = order.saturating_add(1);
        add_symbol_descriptor(model, &node.key);
        if let CwtValue::Block(children) = &node.value {
            lower_nodes(model, children, Some(&logical_id), order);
        }
    }
}

fn add_symbol_descriptor(model: &mut RulesModel, key: &str) {
    let base = key.split('[').next().unwrap_or(key).to_ascii_lowercase();
    let kind = key.split_once('[').and_then(|(_, rest)| rest.strip_suffix(']')).unwrap_or(
        match base.as_str() {
            "effect" | "effects" => "scripted_effect",
            "trigger" | "triggers" => "scripted_trigger",
            "localisation" => "localisation",
            _ => base.as_str(),
        },
    );
    if !matches!(
        base.as_str(),
        "type"
            | "types"
            | "subtype"
            | "subtypes"
            | "alias"
            | "enum"
            | "complex_enum"
            | "scope"
            | "link"
            | "effect"
            | "trigger"
            | "localisation"
    ) {
        return;
    }
    if !model.symbol_descriptors.iter().any(|descriptor| descriptor.kind_id == kind) {
        model.symbol_descriptors.push(SymbolDescriptor {
            kind_id: kind.to_owned(),
            resolution: SymbolResolutionPolicy::ReplaceBySymbol,
            case_sensitive: false,
        });
    }
}

fn compile_semantic_model(model: &mut RulesModel, sources: &[(String, Vec<CwtNode>)]) {
    for (source_file, nodes) in sources {
        compile_semantic_nodes(model, source_file, nodes, None, None, None);
    }
    model.cwt.rules.sort_by(|left, right| left.id.cmp(&right.id));
    for values in model.cwt.enum_values.values_mut() {
        values.sort();
        values.dedup();
    }
}

fn compile_semantic_nodes(
    model: &mut RulesModel,
    source_file: &str,
    nodes: &[CwtNode],
    context: Option<&str>,
    parent_path: Option<&[String]>,
    alternative_id: Option<&str>,
) {
    for node in nodes {
        let base = node.key.split('[').next().unwrap_or(&node.key).to_ascii_lowercase();
        if base == "enums" {
            compile_enum_container(model, &node.key, &node.value);
            continue;
        }
        if context.is_none() && base == "links" {
            compile_scope_links(model, source_file, node);
            continue;
        }
        if base == "alias" {
            if let Some((namespace, alias_name)) = parse_bracket_pair(&node.key) {
                compile_alias_rule(model, source_file, node, &namespace, &alias_name, &[]);
            }
            continue;
        }
        if let Some(context) = context {
            // A bare CWT token inside a block is a value clause, not a literal script key.
            // CWTools lowers it to LeafValueRule (for example `int` in `color = { int int int }`).
            if node.operator.is_none() && matches!(node.value, CwtValue::Empty) {
                compile_leaf_value_rule(
                    model,
                    source_file,
                    node,
                    context,
                    parent_path.unwrap_or(&[]),
                    alternative_id,
                );
                continue;
            }
            // A nested alias block is an ordinary rule container. Metadata such as `subtype`
            // and `localisation` is intentionally not promoted to a script key at this stage.
            if base == "subtype" {
                if let CwtValue::Block(children) = &node.value {
                    compile_semantic_nodes(
                        model,
                        source_file,
                        children,
                        Some(context),
                        parent_path,
                        alternative_id,
                    );
                }
            } else if base == "alias_name" || cwt_type_metadata(&base) {
                // Alias-name clauses and type metadata are selectors for CWTools itself, not
                // literal script keys.
                if base == "alias_name"
                    && let CwtValue::Block(children) = &node.value
                {
                    compile_semantic_nodes(
                        model,
                        source_file,
                        children,
                        Some(context),
                        parent_path,
                        alternative_id,
                    );
                }
            } else {
                compile_rule_node(
                    model,
                    source_file,
                    node,
                    context,
                    parent_path.unwrap_or(&[]),
                    alternative_id,
                );
            }
            continue;
        }
        if base == "type" {
            if let Some(type_name) = bracket_value(&node.key) {
                let type_context = format!("type:{type_name}");
                if let CwtValue::Block(children) = &node.value {
                    collect_type_root_keys(model, &type_name, children);
                    collect_type_descriptor(model, &type_name, children);
                    compile_semantic_nodes(
                        model,
                        source_file,
                        children,
                        Some(&type_context),
                        Some(&[]),
                        None,
                    );
                }
            }
            continue;
        }
        if base == "types" {
            if let CwtValue::Block(children) = &node.value {
                compile_semantic_nodes(model, source_file, children, None, None, None);
            }
            continue;
        }
        // Root rule blocks (`event = { ... }`, etc.) are retained under a stable context. This
        // lets the runtime validate their direct children without guessing a global key catalog.
        if let CwtValue::Block(children) = &node.value {
            let root_context = format!("root:{}", node.key);
            compile_semantic_nodes(
                model,
                source_file,
                children,
                Some(&root_context),
                Some(&[]),
                None,
            );
        }
    }
}

fn collect_type_descriptor(model: &mut RulesModel, type_name: &str, nodes: &[CwtNode]) {
    let descriptor = model.cwt.type_descriptors.entry(type_name.to_owned()).or_insert_with(|| {
        CwtTypeDescriptor { name: type_name.to_owned(), ..CwtTypeDescriptor::default() }
    });
    for node in nodes {
        let base = node.key.split('[').next().unwrap_or(&node.key).to_ascii_lowercase();
        let scalar = match &node.value {
            CwtValue::Scalar(value) => Some(value.as_str()),
            _ => None,
        };
        match base.as_str() {
            "path" => descriptor.path = scalar.map(str::to_owned),
            "path_file" => descriptor.path_file = scalar.map(str::to_owned),
            "path_extension" => descriptor.path_extension = scalar.map(str::to_owned),
            "path_strict" => descriptor.path_strict = scalar.is_some_and(parse_cwt_bool),
            "type_per_file" => descriptor.type_per_file = scalar.is_some_and(parse_cwt_bool),
            "skip_root_key" => {
                let path = if let Some(value) = scalar {
                    vec![value.to_owned()]
                } else if let CwtValue::Block(children) = &node.value {
                    children.iter().map(|child| child.key.clone()).collect()
                } else {
                    Vec::new()
                };
                if !path.is_empty()
                    && !descriptor.skip_root_paths.iter().any(|known| known == &path)
                {
                    descriptor.skip_root_paths.push(path);
                }
            }
            "name_field" => descriptor.name_field = scalar.map(str::to_owned),
            "name_from_file" => descriptor.name_from_file = scalar.is_some_and(parse_cwt_bool),
            "starts_with" => descriptor.starts_with = scalar.map(str::to_owned),
            _ => {}
        }
        if base == "subtype" {
            if let Some(prefix) = node
                .directives
                .iter()
                .find_map(|directive| cwt_directive_value(directive, "starts_with"))
            {
                descriptor.starts_with = Some(prefix);
            }
        }
    }
}

fn cwt_directive_value(directive: &str, name: &str) -> Option<String> {
    let (key, value) = directive.split_once('=')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
}

fn parse_cwt_bool(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "yes" | "true")
}

fn compile_scope_links(model: &mut RulesModel, source_file: &str, node: &CwtNode) {
    let CwtValue::Block(links) = &node.value else { return };
    for link in links {
        let CwtValue::Block(fields) = &link.value else { continue };
        let input_scopes = fields
            .iter()
            .find(|field| field.key.eq_ignore_ascii_case("input_scopes"))
            .and_then(|field| match &field.value {
                CwtValue::Block(scopes) => {
                    Some(scopes.iter().map(|scope| scope.key.clone()).collect())
                }
                CwtValue::Scalar(scope) if !scope.eq_ignore_ascii_case("any") => {
                    Some(vec![scope.clone()])
                }
                _ => None,
            })
            .unwrap_or_default();
        let output_scope = fields
            .iter()
            .find(|field| field.key.eq_ignore_ascii_case("output_scope"))
            .and_then(|field| match &field.value {
                CwtValue::Scalar(scope) if !scope.is_empty() => Some(scope.clone()),
                _ => None,
            });
        let Some(output_scope) = output_scope else { continue };
        for context in ["effect", "trigger"] {
            model.cwt.rules.push(CwtSemanticRule {
                id: format!("{source_file}:{}:scope-link:{context}:{}", link.order, link.key),
                context: context.to_owned(),
                parent_path: Vec::new(),
                key: CwtKeyMatcher::Exact(link.key.clone()),
                operator: None,
                value: CwtValueMatcher::AnyScalar,
                shape: CwtRuleShape::Node,
                child_context: Some(context.to_owned()),
                alternative_id: None,
                severity: None,
                required: false,
                documentation: Vec::new(),
                allowed_scopes: input_scopes.clone(),
                push_scope: Some(output_scope.clone()),
                replace_scope: Vec::new(),
                min_occurs: None,
                strict_min: true,
                max_occurs: Some(1),
                source_file: source_file.to_owned(),
                line: u32::try_from(link.line).unwrap_or(u32::MAX),
            });
        }
    }
}

fn cwt_type_metadata(base: &str) -> bool {
    matches!(
        base,
        "name_field"
            | "name_from_file"
            | "type_per_file"
            | "skip_root_key"
            | "path"
            | "path_strict"
            | "path_file"
            | "path_extension"
            | "localisation"
            | "display_name"
            | "abbreviation"
            | "push_scope"
            | "replace_scope"
            | "only_if_not"
            | "starts_with"
            | "start_from_root"
            | "unique"
            | "should_be_used"
            | "graph_related_types"
    )
}

fn collect_type_root_keys(model: &mut RulesModel, type_name: &str, nodes: &[CwtNode]) {
    for node in nodes {
        if node.key.split('[').next().is_some_and(|base| base.eq_ignore_ascii_case("subtype")) {
            let mut type_key_filter = None;
            for directive in &node.directives {
                let Some((key, value)) = directive.split_once('=') else { continue };
                if key.trim().eq_ignore_ascii_case("type_key_filter") {
                    let roots = parse_scope_words(value.trim());
                    model
                        .cwt
                        .type_root_keys
                        .entry(type_name.to_owned())
                        .or_default()
                        .extend(roots.iter().cloned());
                    type_key_filter = roots.first().cloned();
                }
            }
            if let Some(root) = type_key_filter
                && let Some(scope) = scope_metadata(&node.directives).1
            {
                model
                    .cwt
                    .type_root_scopes
                    .entry(type_name.to_owned())
                    .or_default()
                    .insert(root, scope);
            }
        }
        if let CwtValue::Block(children) = &node.value {
            collect_type_root_keys(model, type_name, children);
        }
    }
}

fn compile_enum_container(model: &mut RulesModel, key: &str, value: &CwtValue) {
    let CwtValue::Block(children) = value else { return };
    for enum_node in children {
        let Some(name) = bracket_value(&enum_node.key) else { continue };
        let CwtValue::Block(values) = &enum_node.value else { continue };
        let entry = model.cwt.enum_values.entry(name).or_default();
        for value in values {
            entry.push(value.key.clone());
        }
        let _ = key;
    }
}

fn compile_alias_rule(
    model: &mut RulesModel,
    source_file: &str,
    node: &CwtNode,
    namespace: &str,
    alias_name: &str,
    parent_path: &[String],
) {
    let key = alias_key_matcher(alias_name);
    let (value, shape) = match &node.value {
        CwtValue::Scalar(value) => (value_matcher(value), CwtRuleShape::Leaf),
        CwtValue::Block(children) => (
            CwtValueMatcher::AnyScalar,
            if has_bare_values(children) { CwtRuleShape::ValueClause } else { CwtRuleShape::Node },
        ),
        CwtValue::Empty => (CwtValueMatcher::AnyScalar, CwtRuleShape::Leaf),
    };
    let (allowed_scopes, push_scope, replace_scope) = scope_metadata(&node.directives);
    let child_context = child_alias_context(&node.value);
    let severity = cwt_severity(&node.directives);
    let required = cwt_required(&node.directives);
    let (min_occurs, strict_min, max_occurs) = cardinality_bounds(&node.directives);
    let id = format!("{source_file}:{}:alias:{namespace}:{alias_name}", node.order);
    model.cwt.rules.push(CwtSemanticRule {
        id: id.clone(),
        context: namespace.to_owned(),
        parent_path: parent_path.to_vec(),
        key,
        operator: node.operator.clone(),
        value,
        shape,
        child_context,
        alternative_id: Some(id.clone()),
        severity,
        required,
        documentation: node.documentation.clone(),
        allowed_scopes,
        push_scope,
        replace_scope,
        min_occurs,
        strict_min,
        max_occurs,
        source_file: source_file.to_owned(),
        line: u32::try_from(node.line).unwrap_or(u32::MAX),
    });
    if let CwtValue::Block(children) = &node.value {
        let mut child_path = parent_path.to_vec();
        child_path.push(alias_name.to_owned());
        compile_semantic_nodes(
            model,
            source_file,
            children,
            Some(namespace),
            Some(&child_path),
            Some(&id),
        );
    }
}

fn compile_rule_node(
    model: &mut RulesModel,
    source_file: &str,
    node: &CwtNode,
    context: &str,
    parent_path: &[String],
    alternative_id: Option<&str>,
) {
    let key = key_matcher(&node.key);
    let (value, shape) = match &node.value {
        CwtValue::Scalar(value) => (value_matcher(value), CwtRuleShape::Leaf),
        CwtValue::Block(children) => (
            CwtValueMatcher::AnyScalar,
            if has_bare_values(children) { CwtRuleShape::ValueClause } else { CwtRuleShape::Node },
        ),
        CwtValue::Empty => (CwtValueMatcher::AnyScalar, CwtRuleShape::Leaf),
    };
    let (allowed_scopes, push_scope, replace_scope) = scope_metadata(&node.directives);
    let child_context = child_alias_context(&node.value);
    let severity = cwt_severity(&node.directives);
    let required = cwt_required(&node.directives);
    let (min_occurs, strict_min, max_occurs) = cardinality_bounds(&node.directives);
    let id = format!("{source_file}:{}:rule:{context}:{}", node.order, node.key);
    model.cwt.rules.push(CwtSemanticRule {
        id,
        context: context.to_owned(),
        parent_path: parent_path.to_vec(),
        key,
        operator: node.operator.clone(),
        value,
        shape,
        child_context,
        alternative_id: alternative_id.map(str::to_owned),
        severity,
        required,
        documentation: node.documentation.clone(),
        allowed_scopes,
        push_scope,
        replace_scope,
        min_occurs,
        strict_min,
        max_occurs,
        source_file: source_file.to_owned(),
        line: u32::try_from(node.line).unwrap_or(u32::MAX),
    });
    if let CwtValue::Block(children) = &node.value {
        let mut child_path = parent_path.to_vec();
        child_path.push(node.key.clone());
        compile_semantic_nodes(
            model,
            source_file,
            children,
            Some(context),
            Some(&child_path),
            alternative_id,
        );
    }
}

fn has_bare_values(children: &[CwtNode]) -> bool {
    children.iter().any(|child| child.operator.is_none() && matches!(child.value, CwtValue::Empty))
}

fn compile_leaf_value_rule(
    model: &mut RulesModel,
    source_file: &str,
    node: &CwtNode,
    context: &str,
    parent_path: &[String],
    alternative_id: Option<&str>,
) {
    let (allowed_scopes, push_scope, replace_scope) = scope_metadata(&node.directives);
    let severity = cwt_severity(&node.directives);
    let required = cwt_required(&node.directives);
    let (min_occurs, strict_min, max_occurs) = cardinality_bounds(&node.directives);
    model.cwt.rules.push(CwtSemanticRule {
        id: format!("{source_file}:{}:leaf-value:{context}:{}", node.order, node.key),
        context: context.to_owned(),
        parent_path: parent_path.to_vec(),
        key: CwtKeyMatcher::AnyScalar,
        operator: None,
        value: value_matcher(&node.key),
        shape: CwtRuleShape::LeafValue,
        child_context: None,
        alternative_id: alternative_id.map(str::to_owned),
        severity,
        required,
        documentation: node.documentation.clone(),
        allowed_scopes,
        push_scope,
        replace_scope,
        min_occurs,
        strict_min,
        max_occurs,
        source_file: source_file.to_owned(),
        line: u32::try_from(node.line).unwrap_or(u32::MAX),
    });
}

fn parse_bracket_pair(key: &str) -> Option<(String, String)> {
    let value = key.strip_prefix("alias[")?.strip_suffix(']')?;
    let (namespace, name) = value.split_once(':')?;
    Some((namespace.to_owned(), name.to_owned()))
}

fn child_alias_context(value: &CwtValue) -> Option<String> {
    let CwtValue::Block(children) = value else { return None };
    children.iter().find_map(|child| {
        let base = child.key.split('[').next()?.to_ascii_lowercase();
        (base == "alias_name").then(|| bracket_value(&child.key)).flatten()
    })
}

fn bracket_value(key: &str) -> Option<String> {
    key.split_once('[').and_then(|(_, value)| value.strip_suffix(']')).map(str::to_owned)
}

fn alias_key_matcher(name: &str) -> CwtKeyMatcher {
    if let Some(type_name) = name.strip_prefix('<').and_then(|value| value.strip_suffix('>')) {
        CwtKeyMatcher::Type(type_name.to_owned())
    } else if let Some(enum_name) =
        name.strip_prefix("enum[").and_then(|value| value.strip_suffix(']'))
    {
        CwtKeyMatcher::Enum(enum_name.to_owned())
    } else {
        CwtKeyMatcher::Exact(name.to_owned())
    }
}

fn key_matcher(key: &str) -> CwtKeyMatcher {
    if key == "scalar" {
        CwtKeyMatcher::AnyScalar
    } else if let Some(type_name) = key.strip_prefix('<').and_then(|value| value.strip_suffix('>'))
    {
        CwtKeyMatcher::Type(type_name.to_owned())
    } else if let Some(enum_name) =
        key.strip_prefix("enum[").and_then(|value| value.strip_suffix(']'))
    {
        CwtKeyMatcher::Enum(enum_name.to_owned())
    } else if let Some(name) =
        key.strip_prefix("value_set[").and_then(|value| value.strip_suffix(']'))
    {
        CwtKeyMatcher::Dynamic(name.to_owned())
    } else {
        CwtKeyMatcher::Exact(key.to_owned())
    }
}

fn value_matcher(value: &str) -> CwtValueMatcher {
    if value == "scalar" {
        return CwtValueMatcher::AnyScalar;
    }
    if value == "bool" {
        return CwtValueMatcher::Bool;
    }
    if value == "int" {
        return CwtValueMatcher::Int { min: None, max: None };
    }
    if value == "float" {
        return CwtValueMatcher::Float { min: None, max: None };
    }
    if value == "localisation" || value == "localization" {
        return CwtValueMatcher::Localisation;
    }
    if value == "filepath" {
        return CwtValueMatcher::Filepath;
    }
    if let Some(name) = value.strip_prefix('<').and_then(|value| value.strip_suffix('>')) {
        return CwtValueMatcher::Type(name.to_owned());
    }
    if let Some(name) = value.strip_prefix("enum[").and_then(|value| value.strip_suffix(']')) {
        return CwtValueMatcher::Enum(name.to_owned());
    }
    if let Some(name) = value.strip_prefix("scope[").and_then(|value| value.strip_suffix(']')) {
        return CwtValueMatcher::Scope(Some(name.to_owned()));
    }
    if let Some((kind, range)) = value.split_once('[')
        && let Some(range) = range.strip_suffix(']')
        && matches!(kind, "int" | "float")
    {
        if kind == "int" {
            let (min, max) = range
                .split_once("..")
                .map_or((None, None), |(min, max)| (parse_int_bound(min), parse_int_bound(max)));
            return CwtValueMatcher::Int { min, max };
        }
        let (min, max) = range
            .split_once("..")
            .map_or((None, None), |(min, max)| (parse_float_bound(min), parse_float_bound(max)));
        return CwtValueMatcher::Float { min, max };
    }
    if value == "scope_field" {
        return CwtValueMatcher::Dynamic("scope_field".to_owned());
    }
    if value == "value" {
        return CwtValueMatcher::Dynamic("value".to_owned());
    }
    if let Some(name) = value.strip_prefix("value[").and_then(|value| value.strip_suffix(']')) {
        return CwtValueMatcher::Dynamic(name.to_owned());
    }
    if let Some(name) = value.strip_prefix("value_set[").and_then(|value| value.strip_suffix(']')) {
        return CwtValueMatcher::DynamicSet(name.to_owned());
    }
    if matches!(value, "alias_match_left") || value.starts_with("alias_") {
        return CwtValueMatcher::Opaque(value.to_owned());
    }
    CwtValueMatcher::Exact(value.to_owned())
}

fn parse_int_bound(value: &str) -> Option<i64> {
    if value.is_empty() || value == "inf" || value == "-inf" {
        return None;
    }
    value.parse::<i64>().ok()
}

fn parse_float_bound(value: &str) -> Option<String> {
    if value.is_empty() || value == "inf" || value == "-inf" {
        return None;
    }
    value.parse::<f64>().ok().map(|value| value.to_string())
}

fn cardinality_bounds(directives: &[String]) -> (Option<u32>, bool, Option<u32>) {
    let Some(value) = directives.iter().find_map(|directive| {
        let (key, value) = directive.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("cardinality") { Some(value.trim()) } else { None }
    }) else {
        return if cwt_required(directives) {
            (Some(1), true, Some(1))
        } else if cwt_optional(directives) {
            (Some(0), true, Some(1))
        } else {
            (Some(1), true, Some(1))
        };
    };
    if value.eq_ignore_ascii_case("many") {
        return (Some(0), true, None);
    }
    if let Some((min, max)) = value.split_once("..") {
        let min_text = min.trim();
        let min = min_text.trim_start_matches('~').parse::<u32>().ok();
        let max = if max.trim().eq_ignore_ascii_case("inf") {
            None
        } else {
            max.trim().parse::<u32>().ok()
        };
        return (
            if cwt_required(directives) { Some(1) } else { min },
            !min_text.starts_with('~'),
            max,
        );
    }
    let exact = value.parse::<u32>().ok();
    (if cwt_required(directives) { Some(1) } else { exact }, true, exact)
}

fn scope_metadata(directives: &[String]) -> (Vec<String>, Option<String>, Vec<(String, String)>) {
    let allowed_scopes = directives
        .iter()
        .find_map(|directive| directive_value(directive, "scope"))
        .filter(|value| !value.eq_ignore_ascii_case("any"))
        .map(|value| parse_scope_words(&value))
        .unwrap_or_default();
    let push_scope = directives
        .iter()
        .find_map(|directive| directive_value(directive, "push_scope"))
        .and_then(|value| parse_scope_words(&value).into_iter().next());
    let replace_scope = directives
        .iter()
        .find_map(|directive| {
            directive_value(directive, "replace_scope")
                .or_else(|| directive_value(directive, "replace_scopes"))
        })
        .map(|value| parse_scope_pairs(&value))
        .unwrap_or_default();
    (allowed_scopes, push_scope, replace_scope)
}

fn cwt_severity(directives: &[String]) -> Option<u8> {
    directives.iter().find_map(|directive| {
        let (key, value) = directive.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("severity") {
            return None;
        }
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(1),
            "warning" | "warn" => Some(2),
            "info" | "information" => Some(3),
            _ => None,
        }
    })
}

fn cwt_required(directives: &[String]) -> bool {
    directives.iter().any(|directive| {
        let name = directive.split_once('=').map_or(directive.as_str(), |(name, _)| name);
        name.trim().eq_ignore_ascii_case("required")
    })
}

fn cwt_optional(directives: &[String]) -> bool {
    directives.iter().any(|directive| {
        let name = directive.split_once('=').map_or(directive.as_str(), |(name, _)| name);
        name.trim().eq_ignore_ascii_case("optional")
    })
}

fn directive_value(directive: &str, wanted: &str) -> Option<String> {
    let (key, value) = directive.split_once('=')?;
    key.trim().eq_ignore_ascii_case(wanted).then(|| value.trim().to_owned())
}

fn parse_scope_words(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split_whitespace()
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_scope_pairs(value: &str) -> Vec<(String, String)> {
    let words = parse_scope_words(value);
    let mut pairs = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let key = words[index].trim_end_matches('=').to_owned();
        index += 1;
        if words.get(index).is_some_and(|value| value == "=") {
            index += 1;
        }
        let Some(value) = words.get(index) else { break };
        pairs.push((key, value.to_owned()));
        index += 1;
    }
    pairs
}

fn add_path_categories(model: &mut RulesModel) {
    let prefixes: BTreeSet<String> = model
        .records
        .iter()
        .filter(|record| {
            record.fields.get("key").is_some_and(|key| key.eq_ignore_ascii_case("path"))
        })
        .filter_map(|record| record.fields.get("value").map(|value| normalize_cwt_path(value)))
        .collect();
    for prefix in prefixes {
        let id = format!("cwt-path-{}", prefix.replace('/', "-"));
        if !model.file_categories.iter().any(|category| category.id == id) {
            model.file_categories.push(FileCategory {
                id,
                parser: ParserKind::PdxScript,
                resolution: FileResolutionPolicy::Merge,
                matcher: FileMatcher {
                    path_prefix: Some(prefix),
                    extensions: vec!["txt".to_owned()],
                    path_suffix: None,
                    case_sensitive: false,
                },
            });
        }
    }
}

fn normalize_cwt_path(value: &str) -> String {
    let value = value.trim().trim_matches('/');
    value.strip_prefix("game/").unwrap_or(value).to_owned()
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn atomic_write_rules(
    rules: &Eu4Rules,
    output: &Path,
    source_files: &[ImportFileReport],
) -> Result<(), ImportError> {
    let temp = temp_path(output);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    rules.write_sqlite(&temp)?;
    let mut connection = Connection::open(&temp).map_err(RulesError::from)?;
    let transaction = connection.transaction().map_err(RulesError::from)?;
    for source in source_files {
        transaction
            .execute(
                "INSERT INTO import_provenance(source_path, source_sha256, importer_version) VALUES (?1, ?2, ?3)",
                params![source.path, source.sha256, IMPORTER_VERSION],
            )
            .map_err(RulesError::from)?;
    }
    transaction.commit().map_err(RulesError::from)?;
    fs::rename(&temp, output).map_err(|error| {
        let _ = fs::remove_file(&temp);
        ImportError::Io(error)
    })
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ImportError> {
    let temp = temp_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        ImportError::Io(error)
    })
}

fn temp_path(path: &Path) -> PathBuf {
    let nonce =
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_nanos());
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("pdxrules");
    path.with_file_name(format!(".{name}.tmp-{}-{nonce}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{CwtValue, ImportOptions, cardinality_bounds, import_with_options, parse_cwt};
    use pdx_eu4::CwtRuleShape;

    #[test]
    fn parser_preserves_bracketed_keys_directives_docs_and_duplicates() {
        let nodes = parse_cwt(
            "fixture.cwt",
            "## cardinality = many\n### An event type\ntype[event] = {\n  value = 1\n  value = 2\n}\n",
        )
        .expect("parse");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].key, "type[event]");
        assert_eq!(nodes[0].directives, vec!["cardinality = many"]);
        assert_eq!(nodes[0].documentation, vec!["An event type"]);
        let CwtValue::Block(children) = &nodes[0].value else { panic!("block") };
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn cardinality_preserves_lower_and_upper_bounds() {
        assert_eq!(
            cardinality_bounds(&["cardinality = 1..4".to_owned()]),
            (Some(1), true, Some(4))
        );
        assert_eq!(
            cardinality_bounds(&["cardinality = ~1..4".to_owned()]),
            (Some(1), false, Some(4))
        );
        assert_eq!(
            cardinality_bounds(&["cardinality = 3..3".to_owned()]),
            (Some(3), true, Some(3))
        );
        assert_eq!(cardinality_bounds(&["cardinality = many".to_owned()]), (Some(0), true, None));
        assert_eq!(cardinality_bounds(&[]), (Some(1), true, Some(1)));
        assert_eq!(cardinality_bounds(&["required".to_owned()]), (Some(1), true, Some(1)));
    }

    #[test]
    fn importer_preserves_rule_operator_documentation_and_required() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-cwt-ir-{nonce}"));
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(
            root.join("rules.cwt"),
            "## required\n### Must be present\nalias[effect:test_rule] == bool\n",
        )
        .expect("source");
        let output = root.join("eu4.pdxrules");
        import_with_options(&ImportOptions {
            source: root.clone(),
            output: output.clone(),
            manifest: None,
            report: None,
        })
        .expect("import");
        let rules = pdx_eu4::Eu4Rules::load(&output).expect("load");
        let rule = rules
            .model()
            .cwt
            .rules
            .iter()
            .find(|rule| rule.id.contains("test_rule"))
            .expect("semantic rule");
        assert_eq!(rule.operator.as_deref(), Some("=="));
        assert!(rule.required);
        assert_eq!(rule.min_occurs, Some(1));
        assert_eq!(rule.documentation, vec!["Must be present"]);
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn importer_writes_and_reloads_artifact() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("pdx-cwt-{nonce}"));
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join("types.cwt"), "types = { type[event] = { path = \"events\" } }\n")
            .expect("source");
        let output = root.join("eu4.pdxrules");
        let report = import_with_options(&ImportOptions {
            source: root.clone(),
            output: output.clone(),
            manifest: None,
            report: None,
        })
        .expect("import");
        assert_eq!(report.input_count, 1);
        assert!(!report.rule_hash.is_empty());
        assert_eq!(report.construct_counts.get("types"), Some(&1));
        assert!(report.unhandled_directives.is_empty());
        assert!(pdx_eu4::Eu4Rules::load(&output).is_ok());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn pinned_eu4_cwt_corpus_has_a_stable_semantic_inventory() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let output = std::env::temp_dir().join(format!("pdx-cwt-corpus-{nonce}.pdxrules"));
        let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../reference/cwtools-eu4-config");
        let report = import_with_options(&ImportOptions {
            source,
            output: output.clone(),
            manifest: None,
            report: None,
        })
        .expect("import pinned corpus");
        assert_eq!(report.input_count, 73);
        assert!(report.unhandled_directives.is_empty());
        assert_eq!(report.construct_counts.get("alias"), Some(&2910));
        assert_eq!(report.directive_counts.get("cardinality"), Some(&3506));
        assert_eq!(
            report.rule_hash,
            "1818e5fe1fd4b0f4c5ba0759c33351779a7ca4669de7d02bc0f9634dc2aaff35"
        );
        let rules = pdx_eu4::Eu4Rules::load(&output).expect("load pinned corpus");
        assert!(rules.model().cwt.rules.iter().any(|rule| rule.alternative_id.is_some()));
        assert!(rules.model().cwt.rules.iter().any(|rule| rule.shape == CwtRuleShape::ValueClause));
        assert!(rules.model().cwt.rules.iter().any(|rule| rule.shape == CwtRuleShape::LeafValue));
        assert_eq!(
            rules
                .model()
                .cwt
                .type_descriptors
                .get("event")
                .and_then(|descriptor| descriptor.path.as_deref()),
            Some("game/events")
        );
        assert_eq!(
            rules
                .model()
                .cwt
                .type_descriptors
                .get("on_action")
                .and_then(|descriptor| descriptor.starts_with.as_deref()),
            Some("on_harmonized_")
        );
        std::fs::remove_file(output).expect("cleanup");
    }
}
