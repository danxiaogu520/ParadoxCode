use crate::matcher::{KeyMatcher, ValueMatcher};
use crate::model::RulesModel;
use crate::runtime::RulesError;
use sha2::{Digest, Sha256};
use std::fmt;
/// A stable digest of canonical rule content.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct RuleHash([u8; 32]);

impl RuleHash {
    /// Returns the all-zero placeholder hash used by the empty Phase 0 database.
    #[must_use]
    pub const fn empty() -> Self {
        Self([0; 32])
    }

    /// Returns the raw digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Creates a hash from raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the lower-case hexadecimal form stored in manifests and diagnostics.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Parses a lower-case or upper-case hexadecimal SHA-256 digest.
    pub fn from_hex(value: &str) -> Result<Self, RulesError> {
        if value.len() != 64 {
            return Err(RulesError::InvalidHash(value.to_owned()));
        }
        let mut bytes = [0_u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| RulesError::InvalidHash(value.to_owned()))?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Debug for RuleHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("RuleHash").field(&self.0).finish()
    }
}
pub(crate) fn canonical_hash(model: &RulesModel) -> RuleHash {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"paradoxcode/rules/v7\0");
    put_str(&mut bytes, &model.game_id);
    let mut categories = model.file_categories.clone();
    categories.sort_by(|left, right| left.id.cmp(&right.id));
    put_len(&mut bytes, categories.len());
    for category in categories {
        put_str(&mut bytes, &category.id);
        put_str(&mut bytes, &category.parser.as_str());
        put_str(&mut bytes, category.resolution.as_str());
        put_opt_str(&mut bytes, category.matcher.path_prefix.as_deref());
        put_opt_str(&mut bytes, category.matcher.path_exact.as_deref());
        put_len(&mut bytes, category.matcher.extensions.len());
        for extension in category.matcher.extensions {
            put_str(&mut bytes, &extension);
        }
        put_opt_str(&mut bytes, category.matcher.path_suffix.as_deref());
        put_len(&mut bytes, category.matcher.path_exclude_prefixes.len());
        for prefix in category.matcher.path_exclude_prefixes {
            put_str(&mut bytes, &prefix);
        }
        bytes.push(u8::from(category.matcher.case_sensitive));
    }
    let mut descriptors = model.symbol_descriptors.clone();
    descriptors.sort_by(|left, right| left.kind_id.cmp(&right.kind_id));
    put_len(&mut bytes, descriptors.len());
    for descriptor in descriptors {
        put_str(&mut bytes, &descriptor.kind_id);
        put_str(&mut bytes, descriptor.resolution.as_str());
        bytes.push(u8::from(descriptor.case_sensitive));
    }
    let mut records = model.records.clone();
    records.sort_by(|left, right| {
        (&left.table, &left.logical_id, left.source_order).cmp(&(
            &right.table,
            &right.logical_id,
            right.source_order,
        ))
    });
    put_len(&mut bytes, records.len());
    for record in records {
        put_str(&mut bytes, &record.table);
        put_str(&mut bytes, &record.logical_id);
        bytes.extend_from_slice(&record.source_order.to_le_bytes());
        put_len(&mut bytes, record.fields.len());
        for (key, value) in record.fields {
            put_str(&mut bytes, &key);
            put_str(&mut bytes, &value);
        }
    }
    let mut semantic_rules = model.semantic.rules.clone();
    semantic_rules.sort_by(|left, right| left.id.cmp(&right.id));
    put_len(&mut bytes, semantic_rules.len());
    for rule in semantic_rules {
        put_str(&mut bytes, &rule.id);
        put_str(&mut bytes, &rule.context);
        put_len(&mut bytes, rule.parent_path.len());
        for parent in rule.parent_path {
            put_str(&mut bytes, &parent);
        }
        put_semantic_key(&mut bytes, &rule.key);
        put_opt_str(&mut bytes, rule.operator.as_deref());
        put_semantic_value(&mut bytes, &rule.value);
        put_str(&mut bytes, rule.shape.as_str());
        put_opt_str(&mut bytes, rule.child_context.as_deref());
        put_opt_str(&mut bytes, rule.alternative_id.as_deref());
        match rule.severity {
            Some(severity) => {
                bytes.push(1);
                bytes.push(severity);
            }
            None => bytes.push(0),
        }
        bytes.push(u8::from(rule.required));
        bytes.push(u8::from(rule.deprecated));
        put_len(&mut bytes, rule.documentation.len());
        for documentation in &rule.documentation {
            put_str(&mut bytes, documentation);
        }
        match rule.min_occurs {
            Some(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => bytes.push(0),
        }
        let mut allowed_scopes = rule.allowed_scopes.clone();
        allowed_scopes.sort();
        put_len(&mut bytes, allowed_scopes.len());
        for scope in allowed_scopes {
            put_str(&mut bytes, &scope);
        }
        put_opt_str(&mut bytes, rule.push_scope.as_deref());
        let mut replace_scope = rule.replace_scope.clone();
        replace_scope.sort();
        put_len(&mut bytes, replace_scope.len());
        for (register, scope) in replace_scope {
            put_str(&mut bytes, &register);
            put_str(&mut bytes, &scope);
        }
        match rule.max_occurs {
            Some(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => bytes.push(0),
        }
        bytes.push(u8::from(rule.strict_min));
        put_str(&mut bytes, &rule.source_file);
        bytes.extend_from_slice(&rule.line.to_le_bytes());
    }
    let mut enum_names = model
        .semantic
        .enum_values
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    enum_names.sort();
    put_len(&mut bytes, enum_names.len());
    for name in enum_names {
        put_str(&mut bytes, &name);
        let mut values = model
            .semantic
            .enum_values
            .get(&name)
            .cloned()
            .unwrap_or_default();
        values.sort();
        put_len(&mut bytes, values.len());
        for value in values {
            put_str(&mut bytes, &value);
        }
    }
    let mut type_names = model
        .semantic
        .type_root_keys
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    type_names.sort();
    put_len(&mut bytes, type_names.len());
    for name in type_names {
        put_str(&mut bytes, &name);
        let mut roots = model
            .semantic
            .type_root_keys
            .get(&name)
            .cloned()
            .unwrap_or_default();
        roots.sort();
        put_len(&mut bytes, roots.len());
        for root in roots {
            put_str(&mut bytes, &root);
        }
    }
    let mut scoped_types = model
        .semantic
        .type_root_scopes
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    scoped_types.sort();
    put_len(&mut bytes, scoped_types.len());
    for type_name in scoped_types {
        put_str(&mut bytes, &type_name);
        let scopes = model
            .semantic
            .type_root_scopes
            .get(&type_name)
            .cloned()
            .unwrap_or_default();
        put_len(&mut bytes, scopes.len());
        for (root, scope) in scopes {
            put_str(&mut bytes, &root);
            put_str(&mut bytes, &scope);
        }
    }
    let mut descriptor_names = model
        .semantic
        .type_descriptors
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    descriptor_names.sort();
    put_len(&mut bytes, descriptor_names.len());
    for name in descriptor_names {
        let descriptor = model
            .semantic
            .type_descriptors
            .get(&name)
            .expect("type descriptor");
        put_str(&mut bytes, &descriptor.name);
        put_opt_str(&mut bytes, descriptor.path.as_deref());
        put_opt_str(&mut bytes, descriptor.path_file.as_deref());
        put_opt_str(&mut bytes, descriptor.path_extension.as_deref());
        bytes.push(u8::from(descriptor.path_strict));
        bytes.push(u8::from(descriptor.type_per_file));
        let mut skip_root_paths = descriptor.skip_root_paths.clone();
        skip_root_paths.sort();
        put_len(&mut bytes, skip_root_paths.len());
        for path in skip_root_paths {
            put_len(&mut bytes, path.len());
            for key in path {
                put_str(&mut bytes, &key);
            }
        }
        put_opt_str(&mut bytes, descriptor.name_field.as_deref());
        bytes.push(u8::from(descriptor.name_from_file));
        put_opt_str(&mut bytes, descriptor.starts_with.as_deref());
        match &descriptor.type_key_filter {
            Some((values, negate)) => {
                bytes.push(1);
                bytes.push(u8::from(*negate));
                let mut values = values.clone();
                values.sort();
                put_len(&mut bytes, values.len());
                for value in values {
                    put_str(&mut bytes, &value);
                }
            }
            None => bytes.push(0),
        }
        match &descriptor.scripted_macro {
            Some(scripted_macro) => {
                bytes.push(1);
                put_str(&mut bytes, &scripted_macro.body_context);
                bytes.push(u8::from(scripted_macro.macro_enabled));
                bytes.push(u8::from(scripted_macro.usage.replacement));
                bytes.push(u8::from(scripted_macro.usage.condition));
                bytes.push(u8::from(scripted_macro.usage.dynamic_key));
                bytes.push(u8::from(scripted_macro.usage.opaque_text));
            }
            None => bytes.push(0),
        }
    }
    let mut localisation_bindings = model.semantic.localisation_bindings.clone();
    localisation_bindings.sort_by(|left, right| {
        (
            left.type_name.as_str(),
            left.subtype.as_deref().unwrap_or_default(),
            left.field.as_str(),
            left.template.as_deref().unwrap_or_default(),
        )
            .cmp(&(
                right.type_name.as_str(),
                right.subtype.as_deref().unwrap_or_default(),
                right.field.as_str(),
                right.template.as_deref().unwrap_or_default(),
            ))
    });
    put_len(&mut bytes, localisation_bindings.len());
    for binding in localisation_bindings {
        put_str(&mut bytes, &binding.type_name);
        put_str(&mut bytes, &binding.field);
        put_opt_str(&mut bytes, binding.template.as_deref());
        bytes.push(u8::from(binding.required));
        bytes.push(u8::from(binding.optional));
        put_opt_str(&mut bytes, binding.subtype.as_deref());
        if let Some(condition) = &binding.condition {
            put_opt_str(&mut bytes, condition.field.as_deref());
            put_opt_str(&mut bytes, condition.value.as_deref());
            put_opt_str(&mut bytes, condition.key_prefix.as_deref());
        } else {
            put_opt_str(&mut bytes, None);
            put_opt_str(&mut bytes, None);
            put_opt_str(&mut bytes, None);
        }
        put_opt_str(&mut bytes, binding.explicit_field.as_deref());
    }
    let digest = Sha256::digest(bytes);
    let mut result = [0_u8; 32];
    result.copy_from_slice(&digest);
    RuleHash(result)
}

fn put_len(bytes: &mut Vec<u8>, length: usize) {
    bytes.extend_from_slice(&u64::try_from(length).unwrap_or(u64::MAX).to_le_bytes());
}
fn put_str(bytes: &mut Vec<u8>, value: &str) {
    put_len(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}
fn put_opt_str(bytes: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            bytes.push(1);
            put_str(bytes, value);
        }
        None => bytes.push(0),
    }
}

fn put_semantic_key(bytes: &mut Vec<u8>, matcher: &KeyMatcher) {
    match matcher {
        KeyMatcher::Exact(value) => {
            put_str(bytes, "exact");
            put_str(bytes, value);
        }
        KeyMatcher::Type(value) => {
            put_str(bytes, "type");
            put_str(bytes, value);
        }
        KeyMatcher::Enum(value) => {
            put_str(bytes, "enum");
            put_str(bytes, value);
        }
        KeyMatcher::AnyScalar => put_str(bytes, "any"),
        KeyMatcher::Date => put_str(bytes, "date"),
        KeyMatcher::Dynamic(value) => {
            put_str(bytes, "dynamic");
            put_str(bytes, value);
        }
    }
}

fn put_semantic_value(bytes: &mut Vec<u8>, matcher: &ValueMatcher) {
    match matcher {
        ValueMatcher::AnyScalar => put_str(bytes, "any"),
        ValueMatcher::Exact(value) => {
            put_str(bytes, "exact");
            put_str(bytes, value);
        }
        ValueMatcher::Bool => put_str(bytes, "bool"),
        ValueMatcher::Int { min, max } => {
            put_str(bytes, "int");
            put_opt_str(bytes, min.map(|value| value.to_string()).as_deref());
            put_opt_str(bytes, max.map(|value| value.to_string()).as_deref());
        }
        ValueMatcher::Float { min, max } => {
            put_str(bytes, "float");
            put_opt_str(bytes, min.as_deref());
            put_opt_str(bytes, max.as_deref());
        }
        ValueMatcher::Date => put_str(bytes, "date"),
        ValueMatcher::Type(value) => {
            put_str(bytes, "type");
            put_str(bytes, value);
        }
        ValueMatcher::Enum(value) => {
            put_str(bytes, "enum");
            put_str(bytes, value);
        }
        ValueMatcher::Scope(value) => {
            put_str(bytes, "scope");
            put_opt_str(bytes, value.as_deref());
        }
        ValueMatcher::Localisation => put_str(bytes, "localisation"),
        ValueMatcher::Filepath => put_str(bytes, "filepath"),
        ValueMatcher::Dynamic(value) => {
            put_str(bytes, "dynamic");
            put_str(bytes, value);
        }
        ValueMatcher::DynamicSet(value) => {
            put_str(bytes, "dynamic-set");
            put_str(bytes, value);
        }
        ValueMatcher::Opaque(value) => {
            put_str(bytes, "opaque");
            put_str(bytes, value);
        }
    }
}
