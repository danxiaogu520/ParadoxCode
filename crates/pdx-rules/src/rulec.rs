//! Compiler for ParadoxCode's strict, first-party rule source format.
//!
//! This crate deliberately has no external rule-language parser or compatibility layer. Its only
//! accepted input is the versioned, manifest-driven JSON source tree owned and reviewed in this
//! repository. The manifest explicitly groups fragments by catalog, semantic context, supporting
//! tables, localisation bindings, and game profile; compilation produces one normalized logical
//! model and one canonical hash.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    FileCategory, GameProfile, RuleRecord, RuleSet, RulesError, RulesModel, SemanticModel,
    SymbolDescriptor,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

/// Current version of the developer-maintained source layout.
pub const SOURCE_FORMAT_VERSION: u32 = 9;

const SOURCE_MANIFEST: &str = "manifest.json";

/// One named source file embedded into an official runtime binary.
#[derive(Clone, Copy, Debug)]
pub struct SourceFile<'a> {
    /// Normalized path relative to the source root, using `/` separators.
    pub path: &'a str,
    /// UTF-8 JSON bytes for the source file.
    pub bytes: &'a [u8],
}

/// A complete first-party source bundle, either embedded in a runtime binary or supplied by a
/// developer-side source loader.
#[derive(Clone, Copy, Debug)]
pub struct SourceBundle<'a> {
    /// Source manifest JSON.
    pub manifest: &'a [u8],
    /// All files named by the manifest.
    pub files: &'a [SourceFile<'a>],
}

/// Identity and compatibility metadata maintained with the rule source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    /// Strict source layout version.
    pub source_format_version: u32,
    /// Game profile selected by this source tree.
    pub game_id: String,
    /// Human-readable game release supported by this rule revision.
    pub target_game_version: String,
    /// Explicit source files grouped by their normalized logical data family.
    pub files: SourceFiles,
}

/// Explicit source file lists for the first-party rule tree.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFiles {
    /// Catalog fragments containing file categories, symbol descriptors, and records.
    pub catalog: Vec<String>,
    /// Executable semantic rule fragments.
    pub semantic: Vec<String>,
    /// Type descriptor and root selector fragments.
    pub types: Vec<String>,
    /// Static enum value fragments.
    pub values: Vec<String>,
    /// Type-to-localisation binding fragments.
    pub localisation: Vec<String>,
    /// Data-only game profile fragments.
    pub profile: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogSource {
    #[serde(default)]
    file_categories: Vec<FileCategory>,
    #[serde(default)]
    symbol_descriptors: Vec<SymbolDescriptor>,
    #[serde(default)]
    records: Vec<RuleRecord>,
}

type ParsedTypeFragments = (
    BTreeMap<String, crate::TypeDescriptor>,
    BTreeMap<String, Vec<String>>,
    BTreeMap<String, BTreeMap<String, String>>,
);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalisationBindingSource {
    field: String,
    template: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    condition: Option<LocalisationBindingConditionSource>,
    #[serde(default)]
    explicit_field: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalisationBindingConditionSource {
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    key_prefix: Option<String>,
}

/// Release manifest generated from validated first-party source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    /// Rules artifact schema version.
    pub schema_version: u32,
    /// First-party source layout version.
    pub source_format_version: u32,
    /// Game profile identity.
    pub game_id: String,
    /// Supported game release.
    pub target_game_version: String,
    /// Canonical logical content hash.
    pub rule_hash: String,
    /// SHA-256 of the generated artifact bytes.
    pub artifact_sha256: String,
    /// Number of executable semantic rule alternatives.
    pub semantic_rule_count: usize,
    /// Number of file categories.
    pub file_category_count: usize,
    /// Number of symbol descriptors.
    pub symbol_descriptor_count: usize,
}

/// Errors emitted by source loading, validation, and artifact publication.
#[derive(Debug)]
pub enum CompileError {
    /// Filesystem failure at a named path.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Strict JSON decoding failure at a named path.
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// Normalized rules runtime rejected the generated artifact.
    Rules(RulesError),
    /// Source metadata or cross-record invariants are invalid.
    Validation(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "rule source I/O error at {}: {source}",
                    path.display()
                )
            }
            Self::Json { path, source } => {
                write!(
                    formatter,
                    "rule source JSON error at {}: {source}",
                    path.display()
                )
            }
            Self::Rules(error) => write!(formatter, "generated rules are invalid: {error}"),
            Self::Validation(message) => write!(formatter, "invalid first-party rules: {message}"),
        }
    }
}

impl std::error::Error for CompileError {}

impl From<RulesError> for CompileError {
    fn from(error: RulesError) -> Self {
        Self::Rules(error)
    }
}

/// Loads and validates one complete first-party source tree.
pub fn load_source(source: &Path) -> Result<(SourceManifest, RulesModel), CompileError> {
    let manifest: SourceManifest = read_json(&source.join(SOURCE_MANIFEST))?;
    validate_source_layout(source, &manifest)?;
    let files = read_declared_files(source, &manifest.files)?;
    parse_source_files(manifest, files)
}

/// Loads and validates an embedded first-party source bundle without materializing its JSON files.
pub fn load_source_bundle(
    source: SourceBundle<'_>,
) -> Result<(SourceManifest, RulesModel), CompileError> {
    let manifest: SourceManifest =
        read_json_bytes(PathBuf::from("<embedded>/manifest.json"), source.manifest)?;
    let mut files = BTreeMap::new();
    for file in source.files {
        let path = normalize_source_path(file.path)?;
        if files.insert(path.clone(), file.bytes.to_vec()).is_some() {
            return Err(CompileError::Validation(format!(
                "duplicate embedded rule source file: {path}"
            )));
        }
    }
    validate_declared_file_set(&manifest.files, files.keys().map(String::as_str))?;
    parse_source_files(manifest, files)
}

fn parse_source_files(
    manifest: SourceManifest,
    files: BTreeMap<String, Vec<u8>>,
) -> Result<(SourceManifest, RulesModel), CompileError> {
    let catalog = parse_catalog_fragments(&manifest.files.catalog, &files)?;
    let semantic_rules = parse_semantic_fragments(&manifest.files.semantic, &files)?;
    let (type_descriptors, type_root_keys, type_root_scopes) =
        parse_type_fragments(&manifest.files.types, &files)?;
    let enum_values = parse_value_fragments(&manifest.files.values, &files)?;
    let localisation_bindings = parse_localisation_fragments(&manifest.files.localisation, &files)?;
    let profile = parse_profile_fragments(&manifest.files.profile, &files)?;
    let semantic = SemanticModel {
        rules: semantic_rules,
        enum_values,
        type_root_keys,
        type_root_scopes,
        type_descriptors,
        localisation_bindings,
    };
    validate_source_model(manifest, catalog, semantic, profile)
}

fn parse_catalog_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<CatalogSource, CompileError> {
    let mut catalog = CatalogSource::default();
    for path in paths {
        let fragment: CatalogSource = read_declared_json(path, files)?;
        catalog.file_categories.extend(fragment.file_categories);
        catalog
            .symbol_descriptors
            .extend(fragment.symbol_descriptors);
        catalog.records.extend(fragment.records);
    }
    Ok(catalog)
}

fn parse_semantic_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<crate::SemanticRule>, CompileError> {
    let mut rules = Vec::new();
    for path in paths {
        let fragment: Vec<crate::SemanticRule> = read_declared_json(path, files)?;
        rules.extend(fragment);
    }
    Ok(rules)
}

fn parse_type_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<ParsedTypeFragments, CompileError> {
    let mut descriptors = BTreeMap::new();
    let mut root_keys = BTreeMap::new();
    let mut root_scopes = BTreeMap::<String, BTreeMap<String, String>>::new();
    for path in paths {
        if path.ends_with("root-keys.json") {
            merge_unique_map(
                &mut root_keys,
                read_declared_json(path, files)?,
                "type root keys",
            )?;
        } else if path.ends_with("root-scopes.json") {
            merge_unique_map(
                &mut root_scopes,
                read_declared_json(path, files)?,
                "type root scopes",
            )?;
        } else {
            let fragment: BTreeMap<String, crate::TypeDescriptor> =
                read_declared_json(path, files)?;
            merge_unique_map(&mut descriptors, fragment, "type descriptor")?;
        }
    }
    Ok((descriptors, root_keys, root_scopes))
}

fn parse_value_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<String>>, CompileError> {
    let mut values = BTreeMap::new();
    for path in paths {
        let fragment: BTreeMap<String, Vec<String>> = read_declared_json(path, files)?;
        merge_unique_map(&mut values, fragment, "enum value set")?;
    }
    Ok(values)
}

fn parse_localisation_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<crate::LocalisationBinding>, CompileError> {
    let mut source = BTreeMap::<String, Vec<LocalisationBindingSource>>::new();
    for path in paths {
        let fragment: BTreeMap<String, Vec<LocalisationBindingSource>> =
            read_declared_json(path, files)?;
        for (type_name, bindings) in fragment {
            source.entry(type_name).or_default().extend(bindings);
        }
    }
    Ok(decode_localisation_bindings(source))
}

fn parse_profile_fragments(
    paths: &[String],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<GameProfile, CompileError> {
    let mut profile = serde_json::Map::new();
    for path in paths {
        let fragment: serde_json::Value = read_declared_json(path, files)?;
        let serde_json::Value::Object(fragment) = fragment else {
            return Err(CompileError::Validation(format!(
                "profile fragment must be an object: {path}"
            )));
        };
        for (key, value) in fragment {
            if profile.insert(key.clone(), value).is_some() {
                return Err(CompileError::Validation(format!(
                    "duplicate profile field across fragments: {key}"
                )));
            }
        }
    }
    serde_json::from_value(serde_json::Value::Object(profile)).map_err(|source| {
        CompileError::Json {
            path: PathBuf::from("<profile fragments>"),
            source,
        }
    })
}

fn merge_unique_map<K, V>(
    target: &mut BTreeMap<K, V>,
    fragment: BTreeMap<K, V>,
    family: &str,
) -> Result<(), CompileError>
where
    K: Ord + fmt::Display,
{
    for (key, value) in fragment {
        let key_display = key.to_string();
        if target.insert(key, value).is_some() {
            return Err(CompileError::Validation(format!(
                "duplicate {family} identity: {key_display}"
            )));
        }
    }
    Ok(())
}

fn validate_source_model(
    manifest: SourceManifest,
    catalog: CatalogSource,
    semantic: SemanticModel,
    profile: GameProfile,
) -> Result<(SourceManifest, RulesModel), CompileError> {
    if manifest.source_format_version != SOURCE_FORMAT_VERSION {
        return Err(CompileError::Validation(format!(
            "unsupported source format version {}; expected {SOURCE_FORMAT_VERSION}",
            manifest.source_format_version
        )));
    }
    if manifest.game_id.trim().is_empty() {
        return Err(CompileError::Validation(
            "game_id must not be empty".to_owned(),
        ));
    }
    if manifest.target_game_version.trim().is_empty() {
        return Err(CompileError::Validation(
            "target_game_version must not be empty".to_owned(),
        ));
    }
    if profile.game_id.trim().is_empty() {
        return Err(CompileError::Validation(
            "profile game_id must not be empty".to_owned(),
        ));
    }
    if profile.game_id != manifest.game_id {
        return Err(CompileError::Validation(format!(
            "profile game_id {} disagrees with source manifest game_id {}",
            profile.game_id, manifest.game_id
        )));
    }
    let model = RulesModel {
        game_id: manifest.game_id.clone(),
        file_categories: catalog.file_categories,
        symbol_descriptors: catalog.symbol_descriptors,
        records: catalog.records,
        semantic,
        profile,
    };
    validate_model(&model)?;
    Ok((manifest, model))
}

fn decode_localisation_bindings(
    source: BTreeMap<String, Vec<LocalisationBindingSource>>,
) -> Vec<crate::LocalisationBinding> {
    source
        .into_iter()
        .flat_map(|(type_name, bindings)| {
            bindings
                .into_iter()
                .map(move |binding| crate::LocalisationBinding {
                    type_name: type_name.clone(),
                    field: binding.field,
                    template: binding.template,
                    required: binding.required,
                    optional: binding.optional,
                    subtype: binding.subtype,
                    condition: binding.condition.map(|condition| {
                        crate::LocalisationBindingCondition {
                            field: condition.field,
                            value: condition.value,
                            key_prefix: condition.key_prefix,
                        }
                    }),
                    explicit_field: binding.explicit_field,
                })
        })
        .collect()
}

/// Compiles source into a validated SQLite artifact and release manifest.
pub fn compile(
    source: &Path,
    output: &Path,
    manifest_output: &Path,
) -> Result<ArtifactManifest, CompileError> {
    let (source_manifest, model) = load_source(source)?;
    let rules = RuleSet::from_model(model);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|source| CompileError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let temporary = temporary_path(output);
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|source| CompileError::Io {
            path: temporary.clone(),
            source,
        })?;
    }
    rules.write_sqlite(&temporary)?;
    let loaded = RuleSet::load(&temporary)?;
    if loaded != rules {
        return Err(CompileError::Validation(
            "generated artifact does not round-trip to the source model".to_owned(),
        ));
    }
    let bytes = fs::read(&temporary).map_err(|source| CompileError::Io {
        path: temporary.clone(),
        source,
    })?;
    let artifact_manifest = ArtifactManifest {
        schema_version: loaded.schema_version(),
        source_format_version: source_manifest.source_format_version,
        game_id: source_manifest.game_id,
        target_game_version: source_manifest.target_game_version,
        rule_hash: loaded.rule_hash().to_hex(),
        artifact_sha256: Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        semantic_rule_count: loaded.model().semantic.rules.len(),
        file_category_count: loaded.model().file_categories.len(),
        symbol_descriptor_count: loaded.model().symbol_descriptors.len(),
    };
    write_json(manifest_output, &artifact_manifest)?;
    if output.exists() {
        fs::remove_file(output).map_err(|source| CompileError::Io {
            path: output.to_owned(),
            source,
        })?;
    }
    fs::rename(&temporary, output).map_err(|source| CompileError::Io {
        path: output.to_owned(),
        source,
    })?;
    Ok(artifact_manifest)
}

fn validate_model(model: &RulesModel) -> Result<(), CompileError> {
    if !model.profile.game_id.trim().is_empty() && model.profile.game_id != model.game_id {
        return Err(CompileError::Validation(format!(
            "profile game_id {} disagrees with rules game_id {}",
            model.profile.game_id, model.game_id
        )));
    }
    unique_nonempty(
        model.file_categories.iter().map(|item| item.id.as_str()),
        "file category",
    )?;
    unique_nonempty(
        model
            .symbol_descriptors
            .iter()
            .map(|item| item.kind_id.as_str()),
        "symbol descriptor",
    )?;
    unique_nonempty(
        model.semantic.rules.iter().map(|item| item.id.as_str()),
        "semantic rule",
    )?;
    for rule in &model.semantic.rules {
        if rule.context.trim().is_empty() {
            return Err(CompileError::Validation(format!(
                "semantic rule {} has an empty context",
                rule.id
            )));
        }
        if rule
            .severity
            .is_some_and(|severity| !(1..=3).contains(&severity))
        {
            return Err(CompileError::Validation(format!(
                "semantic rule {} has invalid severity",
                rule.id
            )));
        }
        if rule
            .min_occurs
            .zip(rule.max_occurs)
            .is_some_and(|(min, max)| min > max)
        {
            return Err(CompileError::Validation(format!(
                "semantic rule {} has minimum cardinality greater than maximum",
                rule.id
            )));
        }
        if rule.required && rule.min_occurs.is_some_and(|minimum| minimum == 0) {
            return Err(CompileError::Validation(format!(
                "semantic rule {} marks a zero-minimum field as required",
                rule.id
            )));
        }
        if rule.required && rule.max_occurs.is_some_and(|maximum| maximum == 0) {
            return Err(CompileError::Validation(format!(
                "semantic rule {} marks a zero-maximum field as required",
                rule.id
            )));
        }
        if matches!(rule.shape, crate::RuleShape::QuotedScript) {
            if rule
                .child_context
                .as_deref()
                .is_none_or(|context| context.trim().is_empty())
            {
                return Err(CompileError::Validation(format!(
                    "semantic rule {} has quoted Script without a child context",
                    rule.id
                )));
            }
            if !matches!(
                rule.value,
                crate::ValueMatcher::AnyScalar | crate::ValueMatcher::Opaque(_)
            ) {
                return Err(CompileError::Validation(format!(
                    "semantic rule {} has quoted Script with a non-opaque scalar matcher",
                    rule.id
                )));
            }
        }
        if rule.context.eq_ignore_ascii_case("trigger")
            && matches!(rule.shape, crate::RuleShape::Node)
            && matches!(&rule.key, crate::KeyMatcher::Exact(key)
                if ["area", "region", "continent"].iter().any(|name| key.eq_ignore_ascii_case(name)))
        {
            return Err(CompileError::Validation(format!(
                "semantic rule {} models a province collection predicate as a trigger node",
                rule.id
            )));
        }
    }
    for (identity, descriptor) in &model.semantic.type_descriptors {
        if identity != &descriptor.name {
            return Err(CompileError::Validation(format!(
                "type descriptor key {identity} disagrees with embedded name {}",
                descriptor.name
            )));
        }
        if let Some(scripted_macro) = &descriptor.scripted_macro {
            if scripted_macro.body_context.trim().is_empty() {
                return Err(CompileError::Validation(format!(
                    "type descriptor {identity} has an empty scripted macro body context"
                )));
            }
            if scripted_macro.macro_enabled && !scripted_macro.usage.is_nonempty() {
                return Err(CompileError::Validation(format!(
                    "type descriptor {identity} enables scripted macros without a usage capability"
                )));
            }
        }
    }
    let mut binding_ids = BTreeSet::new();
    for binding in &model.semantic.localisation_bindings {
        if binding.type_name.trim().is_empty() || binding.field.trim().is_empty() {
            return Err(CompileError::Validation(
                "localisation binding type and field must not be empty".to_owned(),
            ));
        }
        if !model
            .semantic
            .type_descriptors
            .contains_key(&binding.type_name)
        {
            return Err(CompileError::Validation(format!(
                "localisation binding {}.{} refers to unknown type {}",
                binding.type_name, binding.field, binding.type_name
            )));
        }
        if binding.required && binding.optional {
            return Err(CompileError::Validation(format!(
                "localisation binding {}.{} cannot be both required and optional",
                binding.type_name, binding.field
            )));
        }
        if binding
            .subtype
            .as_deref()
            .is_some_and(|subtype| subtype.trim().is_empty())
        {
            return Err(CompileError::Validation(format!(
                "localisation binding {}.{} has an empty subtype",
                binding.type_name, binding.field
            )));
        }
        if let Some(condition) = &binding.condition {
            if condition
                .field
                .as_deref()
                .is_some_and(|field| field.trim().is_empty())
                || condition
                    .key_prefix
                    .as_deref()
                    .is_some_and(|prefix| prefix.trim().is_empty())
            {
                return Err(CompileError::Validation(format!(
                    "localisation binding {}.{} condition contains an empty selector",
                    binding.type_name, binding.field
                )));
            }
            let has_field = condition
                .field
                .as_deref()
                .is_some_and(|field| !field.trim().is_empty());
            let has_key_prefix = condition
                .key_prefix
                .as_deref()
                .is_some_and(|prefix| !prefix.trim().is_empty());
            if has_field == has_key_prefix {
                return Err(CompileError::Validation(format!(
                    "localisation binding {}.{} condition must select one field or key_prefix",
                    binding.type_name, binding.field
                )));
            }
            if condition.value.is_some() && !has_field {
                return Err(CompileError::Validation(format!(
                    "localisation binding {}.{} condition value requires field",
                    binding.type_name, binding.field
                )));
            }
        }
        if binding.explicit_field.is_some() != binding.template.is_none() {
            return Err(CompileError::Validation(format!(
                "localisation binding {}.{} must use either template or explicit_field",
                binding.type_name, binding.field
            )));
        }
        if let Some(template) = binding.template.as_deref()
            && (template.matches('$').count() != 1 || template.trim().is_empty())
        {
            return Err(CompileError::Validation(format!(
                "localisation binding {}.{} template must contain exactly one `$`",
                binding.type_name, binding.field
            )));
        }
        let identity = format!(
            "{}\u{1f}{}\u{1f}{}",
            binding.type_name,
            binding.subtype.as_deref().unwrap_or_default(),
            binding.field
        );
        if !binding_ids.insert(identity) {
            return Err(CompileError::Validation(format!(
                "duplicate localisation binding {}.{}",
                binding.type_name, binding.field
            )));
        }
    }
    Ok(())
}

fn validate_source_layout(source: &Path, manifest: &SourceManifest) -> Result<(), CompileError> {
    let mut actual = BTreeSet::from([SOURCE_MANIFEST.to_owned()]);
    collect_source_files(source, source, &mut actual)?;
    let mut expected = BTreeSet::from([SOURCE_MANIFEST.to_owned()]);
    expected.extend(validate_declared_paths(&manifest.files)?);
    if let Some(unexpected) = actual.difference(&expected).next() {
        return Err(CompileError::Validation(format!(
            "unknown rule source file: {unexpected}"
        )));
    }
    if let Some(missing) = expected.difference(&actual).next() {
        return Err(CompileError::Validation(format!(
            "declared rule source file is missing: {missing}"
        )));
    }
    Ok(())
}

fn collect_source_files(
    root: &Path,
    directory: &Path,
    output: &mut BTreeSet<String>,
) -> Result<(), CompileError> {
    let entries = fs::read_dir(directory).map_err(|source| CompileError::Io {
        path: directory.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CompileError::Io {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| CompileError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_source_files(root, &path, output)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(CompileError::Validation(format!(
                "rule source entry must be a regular file or directory: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| CompileError::Validation("rule source path escaped root".to_owned()))?
            .to_str()
            .ok_or_else(|| {
                CompileError::Validation(format!(
                    "rule source filename is not UTF-8: {}",
                    path.display()
                ))
            })?
            .replace('\\', "/");
        output.insert(relative);
    }
    Ok(())
}

fn validate_declared_paths(files: &SourceFiles) -> Result<BTreeSet<String>, CompileError> {
    let mut paths = BTreeSet::new();
    for (family, entries) in [
        ("catalog", &files.catalog),
        ("semantic", &files.semantic),
        ("types", &files.types),
        ("values", &files.values),
        ("localisation", &files.localisation),
        ("profile", &files.profile),
    ] {
        for entry in entries {
            let path = normalize_source_path(entry)?;
            let expected_prefix = format!("{family}/");
            if !path.starts_with(&expected_prefix) {
                return Err(CompileError::Validation(format!(
                    "{family} source file is outside its source directory: {path}"
                )));
            }
            if !paths.insert(path.clone()) {
                return Err(CompileError::Validation(format!(
                    "duplicate declared rule source file: {path}"
                )));
            }
        }
    }
    if paths.is_empty() {
        return Err(CompileError::Validation(
            "rule source manifest declares no input files".to_owned(),
        ));
    }
    Ok(paths)
}

fn validate_declared_file_set<'a>(
    files: &SourceFiles,
    actual: impl Iterator<Item = &'a str>,
) -> Result<(), CompileError> {
    let expected = validate_declared_paths(files)?;
    let actual = actual
        .map(normalize_source_path)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if let Some(unexpected) = actual.difference(&expected).next() {
        return Err(CompileError::Validation(format!(
            "embedded rule source contains an undeclared file: {unexpected}"
        )));
    }
    if let Some(missing) = expected.difference(&actual).next() {
        return Err(CompileError::Validation(format!(
            "embedded rule source is missing a declared file: {missing}"
        )));
    }
    Ok(())
}

fn normalize_source_path(path: &str) -> Result<String, CompileError> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains('\0') {
        return Err(CompileError::Validation(format!(
            "invalid rule source path: {path}"
        )));
    }
    let components = path.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
        || !path.ends_with(".json")
    {
        return Err(CompileError::Validation(format!(
            "invalid rule source path: {path}"
        )));
    }
    if components.first() == Some(&"manifest.json") {
        return Err(CompileError::Validation(
            "manifest.json must not be listed as a rule input".to_owned(),
        ));
    }
    Ok(path.to_owned())
}

fn read_declared_files(
    source: &Path,
    manifest: &SourceFiles,
) -> Result<BTreeMap<String, Vec<u8>>, CompileError> {
    let paths = validate_declared_paths(manifest)?;
    let mut files = BTreeMap::new();
    for path in paths {
        let full = source.join(&path);
        let bytes = fs::read(&full).map_err(|source| CompileError::Io { path: full, source })?;
        files.insert(path, bytes);
    }
    Ok(files)
}

fn read_declared_json<T: DeserializeOwned>(
    path: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<T, CompileError> {
    let bytes = files.get(path).ok_or_else(|| {
        CompileError::Validation(format!("declared rule source file is missing: {path}"))
    })?;
    read_json_bytes(PathBuf::from(path), bytes)
}

fn unique_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
    family: &str,
) -> Result<(), CompileError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(CompileError::Validation(format!(
                "{family} identity must not be empty"
            )));
        }
        if !seen.insert(value) {
            return Err(CompileError::Validation(format!(
                "duplicate {family} identity: {value}"
            )));
        }
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, CompileError> {
    let bytes = fs::read(path).map_err(|source| CompileError::Io {
        path: path.to_owned(),
        source,
    })?;
    read_json_bytes(path.to_owned(), &bytes)
}

fn read_json_bytes<T: DeserializeOwned>(path: PathBuf, bytes: &[u8]) -> Result<T, CompileError> {
    serde_json::from_slice(bytes).map_err(|source| CompileError::Json { path, source })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CompileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CompileError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| CompileError::Json {
        path: path.to_owned(),
        source,
    })?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|source| CompileError::Io {
        path: path.to_owned(),
        source,
    })
}

fn temporary_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rules.pdxrules");
    output.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        KeyMatcher, RuleShape, ScriptedMacroDescriptor, ScriptedMacroUsage, SemanticRule,
        TypeDescriptor, ValueMatcher,
    };

    #[test]
    fn validation_rejects_duplicate_rule_ids_and_invalid_cardinality() {
        let rule = SemanticRule {
            id: "duplicate".to_owned(),
            context: "root".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("key".to_owned()),
            operator: None,
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::Leaf,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            deprecated: false,
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: Some(2),
            strict_min: true,
            max_occurs: Some(1),
            source_file: "semantic-rules.json".to_owned(),
            line: 1,
        };
        let mut model = RulesModel {
            game_id: "eu4".to_owned(),
            ..RulesModel::default()
        };
        model.semantic.rules = vec![rule.clone(), rule];
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn validation_requires_quoted_script_context_and_opaque_value() {
        let mut rule = SemanticRule {
            id: "quoted".to_owned(),
            context: "effect".to_owned(),
            parent_path: Vec::new(),
            key: KeyMatcher::Exact("effect".to_owned()),
            operator: Some("=".to_owned()),
            value: ValueMatcher::AnyScalar,
            shape: RuleShape::QuotedScript,
            child_context: None,
            alternative_id: None,
            severity: None,
            required: false,
            deprecated: false,
            documentation: Vec::new(),
            allowed_scopes: Vec::new(),
            push_scope: None,
            replace_scope: Vec::new(),
            min_occurs: None,
            strict_min: true,
            max_occurs: None,
            source_file: "semantic-rules.json".to_owned(),
            line: 1,
        };
        let mut model = RulesModel {
            game_id: "eu4".to_owned(),
            ..RulesModel::default()
        };
        model.semantic.rules.push(rule.clone());
        assert!(
            validate_model(&model)
                .expect_err("quoted Script needs child context")
                .to_string()
                .contains("without a child context")
        );

        rule.child_context = Some("effect".to_owned());
        rule.value = ValueMatcher::Bool;
        model.semantic.rules = vec![rule];
        assert!(
            validate_model(&model)
                .expect_err("quoted Script needs opaque matcher")
                .to_string()
                .contains("non-opaque scalar matcher")
        );
    }

    #[test]
    fn validation_rejects_enabled_scripted_macro_without_usage() {
        let mut model = RulesModel {
            game_id: "eu4".to_owned(),
            ..RulesModel::default()
        };
        model.semantic.type_descriptors.insert(
            "scripted_effect".to_owned(),
            TypeDescriptor {
                name: "scripted_effect".to_owned(),
                scripted_macro: Some(ScriptedMacroDescriptor {
                    body_context: "effect".to_owned(),
                    macro_enabled: true,
                    usage: ScriptedMacroUsage::default(),
                }),
                ..TypeDescriptor::default()
            },
        );
        let error = validate_model(&model).expect_err("empty macro usage must be rejected");
        assert!(error.to_string().contains("usage capability"));
    }

    #[test]
    fn committed_source_compiles_with_release_shape() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let expected: ArtifactManifest =
            read_json(&root.join("rules/manifest.json")).expect("committed manifest");
        let (_, source_model) = load_source(&root.join("rules/eu4")).expect("source model");
        assert_eq!(source_model.file_categories.len(), 121);
        assert_eq!(source_model.symbol_descriptors.len(), 2667);
        assert_eq!(source_model.records.len(), 13_719);
        assert_eq!(source_model.semantic.rules.len(), 8_525);
        assert_eq!(source_model.semantic.enum_values.len(), 72);
        assert_eq!(source_model.semantic.type_root_keys.len(), 5);
        assert_eq!(source_model.semantic.type_root_scopes.len(), 1);
        assert_eq!(source_model.semantic.type_descriptors.len(), 148);
        assert_eq!(source_model.semantic.localisation_bindings.len(), 187);
        assert_eq!(source_model.profile.scan_roots.len(), 123);
        let quoted_script_rules = source_model
            .semantic
            .rules
            .iter()
            .filter(|rule| matches!(rule.shape, RuleShape::QuotedScript))
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            quoted_script_rules,
            [
                "missions/missions:23:quoted-script:root:mission_series:trigger",
                "missions/missions:25:quoted-script:root:mission_series:effect",
            ],
            "first-party quoted Script rules must describe fixed schemas, not workspace macros"
        );
        let scripted_effect = source_model
            .semantic
            .type_descriptors
            .get("scripted_effect")
            .and_then(|descriptor| descriptor.scripted_macro.as_ref())
            .expect("scripted effect macro descriptor");
        assert_eq!(scripted_effect.body_context, "effect");
        assert!(scripted_effect.macro_enabled);
        assert!(scripted_effect.usage.is_nonempty());
        let scripted_trigger = source_model
            .semantic
            .type_descriptors
            .get("scripted_trigger")
            .and_then(|descriptor| descriptor.scripted_macro.as_ref())
            .expect("scripted trigger macro descriptor");
        assert_eq!(scripted_trigger.body_context, "trigger");
        assert!(scripted_trigger.macro_enabled);
        assert!(scripted_trigger.usage.is_nonempty());
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("pdx-bake-test-{nonce}"));
        fs::create_dir_all(&directory).expect("temporary directory");
        let actual = compile(
            &root.join("rules/eu4"),
            &directory.join("eu4.pdxrules"),
            &directory.join("manifest.json"),
        )
        .expect("compile committed first-party source");
        assert_eq!(actual, expected);
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
