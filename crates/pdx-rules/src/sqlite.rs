use crate::CURRENT_SCHEMA_VERSION;
use crate::GameProfile;
use crate::matcher::{FileMatcher, KeyMatcher, ValueMatcher};
use crate::model::{
    FileCategory, FileResolutionPolicy, LocalisationBinding, LocalisationBindingCondition,
    ParserKind, RuleRecord, RuleShape, RulesModel, ScriptedMacroDescriptor, ScriptedMacroUsage,
    SemanticModel, SemanticRule, SymbolDescriptor, SymbolResolutionPolicy, TypeDescriptor,
    TypeRootScope,
};
use crate::runtime::{RuleSet, RulesError};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use std::path::Path;
fn schema(connection: &Connection) -> Result<(), RulesError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS file_categories (
            id TEXT PRIMARY KEY NOT NULL, parser TEXT NOT NULL, resolution TEXT NOT NULL,
            path_prefix TEXT, path_exact TEXT, extensions TEXT NOT NULL, path_suffix TEXT,
            path_exclude_prefixes TEXT NOT NULL DEFAULT '', case_sensitive INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS symbol_descriptors (
            kind_id TEXT PRIMARY KEY NOT NULL, resolution TEXT NOT NULL, case_sensitive INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rule_records (
            table_name TEXT NOT NULL, logical_id TEXT NOT NULL, source_order INTEGER NOT NULL,
            PRIMARY KEY (table_name, logical_id)
        );
        CREATE TABLE IF NOT EXISTS rule_fields (
            table_name TEXT NOT NULL, logical_id TEXT NOT NULL, field_name TEXT NOT NULL, field_value TEXT NOT NULL,
            PRIMARY KEY (table_name, logical_id, field_name),
            FOREIGN KEY (table_name, logical_id) REFERENCES rule_records(table_name, logical_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS semantic_rules (
            id TEXT PRIMARY KEY NOT NULL,
            context TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            key_kind TEXT NOT NULL,
            key_value TEXT,
            operator TEXT,
            value_kind TEXT NOT NULL,
            value_arg TEXT,
            value_min TEXT,
            value_max TEXT,
            shape TEXT NOT NULL,
            child_context TEXT,
            alternative_id TEXT,
            severity INTEGER,
            required INTEGER NOT NULL DEFAULT 0,
            deprecated INTEGER NOT NULL DEFAULT 0,
            documentation TEXT NOT NULL DEFAULT '',
            allowed_scopes TEXT NOT NULL DEFAULT '',
            push_scope TEXT,
            replace_scope TEXT NOT NULL DEFAULT '',
            min_occurs INTEGER,
            strict_min INTEGER NOT NULL DEFAULT 1,
            max_occurs INTEGER,
            source_file TEXT NOT NULL,
            line INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS enum_values (
            enum_name TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (enum_name, value)
        );
        CREATE TABLE IF NOT EXISTS type_root_keys (
            type_name TEXT NOT NULL,
            root_key TEXT NOT NULL,
            PRIMARY KEY (type_name, root_key)
        );
        CREATE TABLE IF NOT EXISTS type_root_scopes (
            type_name TEXT NOT NULL,
            root_key TEXT NOT NULL,
            scope TEXT NOT NULL,
            this_scope TEXT,
            from_scope TEXT,
            documentation TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (type_name, root_key)
        );
        CREATE TABLE IF NOT EXISTS type_descriptors (
            type_name TEXT PRIMARY KEY NOT NULL,
            path TEXT,
            path_file TEXT,
            path_extension TEXT,
            path_strict INTEGER NOT NULL DEFAULT 0,
            type_per_file INTEGER NOT NULL DEFAULT 0,
            skip_root_keys TEXT NOT NULL DEFAULT '',
            name_field TEXT,
            name_from_file INTEGER NOT NULL DEFAULT 0,
            starts_with TEXT,
            type_key_filter TEXT NOT NULL DEFAULT '',
            type_key_filter_negate INTEGER NOT NULL DEFAULT 0,
            root_entries TEXT,
            body_context TEXT,
            scripted_macro_body_context TEXT,
            scripted_macro_enabled INTEGER NOT NULL DEFAULT 0,
            scripted_macro_replacement INTEGER NOT NULL DEFAULT 0,
            scripted_macro_condition INTEGER NOT NULL DEFAULT 0,
            scripted_macro_dynamic_key INTEGER NOT NULL DEFAULT 0,
            scripted_macro_opaque_text INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS localisation_bindings (
            type_name TEXT NOT NULL,
            field TEXT NOT NULL,
            template TEXT,
            required INTEGER NOT NULL DEFAULT 0,
            optional INTEGER NOT NULL DEFAULT 0,
            subtype TEXT,
            condition_field TEXT,
            condition_value TEXT,
            condition_key_prefix TEXT,
            explicit_field TEXT,
            PRIMARY KEY(type_name, field, subtype)
        );"
    )?;
    ensure_file_category_columns(connection)?;
    ensure_semantic_columns(connection)?;
    ensure_type_root_scope_columns(connection)?;
    Ok(())
}

fn ensure_file_category_columns(connection: &Connection) -> Result<(), RulesError> {
    for (name, definition) in [
        ("path_exact", "TEXT"),
        ("path_exclude_prefixes", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('file_categories') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE file_categories ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn ensure_semantic_columns(connection: &Connection) -> Result<(), RulesError> {
    // `CREATE TABLE IF NOT EXISTS` never adds columns to an artifact written by an older
    // importer, so the write path upgrades those artifacts in place before repopulating
    // them. The load path still rejects any schema_version it does not recognize, which
    // keeps the migration code below an internal detail of artifact regeneration.
    for (name, definition) in [
        ("child_context", "TEXT"),
        ("alternative_id", "TEXT"),
        ("severity", "INTEGER"),
        ("operator", "TEXT"),
        ("required", "INTEGER NOT NULL DEFAULT 0"),
        ("deprecated", "INTEGER NOT NULL DEFAULT 0"),
        ("documentation", "TEXT NOT NULL DEFAULT ''"),
        ("allowed_scopes", "TEXT NOT NULL DEFAULT ''"),
        ("push_scope", "TEXT"),
        ("replace_scope", "TEXT NOT NULL DEFAULT ''"),
        ("min_occurs", "INTEGER"),
        ("strict_min", "INTEGER NOT NULL DEFAULT 1"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('semantic_rules') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE semantic_rules ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    for (name, definition) in [
        ("type_key_filter", "TEXT NOT NULL DEFAULT ''"),
        ("type_key_filter_negate", "INTEGER NOT NULL DEFAULT 0"),
        ("root_entries", "TEXT"),
        ("scripted_macro_body_context", "TEXT"),
        ("scripted_macro_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ("scripted_macro_replacement", "INTEGER NOT NULL DEFAULT 0"),
        ("scripted_macro_condition", "INTEGER NOT NULL DEFAULT 0"),
        ("scripted_macro_dynamic_key", "INTEGER NOT NULL DEFAULT 0"),
        ("scripted_macro_opaque_text", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('type_descriptors') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE type_descriptors ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn ensure_type_root_scope_columns(connection: &Connection) -> Result<(), RulesError> {
    for (name, definition) in [
        ("this_scope", "TEXT"),
        ("from_scope", "TEXT"),
        ("documentation", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('type_root_scopes') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            connection.execute(
                &format!("ALTER TABLE type_root_scopes ADD COLUMN {name} {definition}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn write(path: &Path, rules: &RuleSet) -> Result<(), RulesError> {
    let mut connection = Connection::open(path)?;
    write_connection(&mut connection, rules)
}

pub(crate) fn load(path: &Path) -> Result<RuleSet, RulesError> {
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let version = metadata(&connection, "schema_version")?
        .ok_or_else(|| RulesError::MissingMetadata("schema_version".to_owned()))?
        .parse::<u32>()
        .map_err(|_| RulesError::SchemaVersion(0))?;
    if version != CURRENT_SCHEMA_VERSION {
        return Err(RulesError::SchemaVersion(version));
    }
    let stored = metadata(&connection, "rule_hash")?
        .ok_or_else(|| RulesError::MissingMetadata("rule_hash".to_owned()))?;
    let game_id = metadata(&connection, "game_id")?
        .ok_or_else(|| RulesError::MissingMetadata("game_id".to_owned()))?;
    let profile_json = metadata(&connection, "profile_json")?
        .ok_or_else(|| RulesError::MissingMetadata("profile_json".to_owned()))?;
    let profile: GameProfile = serde_json::from_str(&profile_json)
        .map_err(|error| RulesError::Source(format!("invalid persisted game profile: {error}")))?;
    if !profile.game_id.is_empty() && profile.game_id != game_id {
        return Err(RulesError::GameMismatch {
            expected: game_id,
            actual: profile.game_id,
        });
    }
    let mut model = read_model(&connection)?;
    model.game_id = game_id;
    model.profile = profile;
    let mut rules = RuleSet::from_model(model);
    rules.schema_version = version;
    let computed = rules.rule_hash.to_hex();
    if stored != computed {
        return Err(RulesError::HashMismatch { stored, computed });
    }
    Ok(rules)
}

fn write_connection(connection: &mut Connection, rules: &RuleSet) -> Result<(), RulesError> {
    schema(connection)?;
    let transaction = connection.transaction()?;
    transaction.execute_batch("DELETE FROM metadata; DELETE FROM file_categories; DELETE FROM symbol_descriptors; DELETE FROM enum_values; DELETE FROM type_root_keys; DELETE FROM type_root_scopes; DELETE FROM type_descriptors; DELETE FROM localisation_bindings; DELETE FROM semantic_rules; DELETE FROM rule_fields; DELETE FROM rule_records;")?;
    transaction.execute(
        "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1), ('rule_hash', ?2), ('game_id', ?3), ('profile_json', ?4)",
        params![
            rules.schema_version.to_string(),
            rules.rule_hash.to_hex(),
            rules.game_id(),
            serde_json::to_string(rules.profile())
                .map_err(|error| RulesError::Source(format!("serialize game profile: {error}")))?,
        ],
    )?;
    for category in &rules.model.file_categories {
        transaction.execute("INSERT INTO file_categories(id, parser, resolution, path_prefix, path_exact, extensions, path_suffix, path_exclude_prefixes, case_sensitive) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", params![category.id, category.parser.as_str(), category.resolution.as_str(), category.matcher.path_prefix, category.matcher.path_exact, category.matcher.extensions.join("\u{1f}"), category.matcher.path_suffix, category.matcher.path_exclude_prefixes.join("\u{1e}"), i64::from(category.matcher.case_sensitive)])?;
    }
    for descriptor in &rules.model.symbol_descriptors {
        transaction.execute("INSERT INTO symbol_descriptors(kind_id, resolution, case_sensitive) VALUES (?1, ?2, ?3)", params![descriptor.kind_id, descriptor.resolution.as_str(), i64::from(descriptor.case_sensitive)])?;
    }
    for record in &rules.model.records {
        transaction.execute(
            "INSERT INTO rule_records(table_name, logical_id, source_order) VALUES (?1, ?2, ?3)",
            params![record.table, record.logical_id, record.source_order],
        )?;
        for (field_name, field_value) in &record.fields {
            transaction.execute("INSERT INTO rule_fields(table_name, logical_id, field_name, field_value) VALUES (?1, ?2, ?3, ?4)", params![record.table, record.logical_id, field_name, field_value])?;
        }
    }
    for rule in &rules.model.semantic.rules {
        let (key_kind, key_value) = semantic_key_columns(&rule.key);
        let (value_kind, value_arg, value_min, value_max) = semantic_value_columns(&rule.value);
        transaction.execute(
            "INSERT INTO semantic_rules(id, context, parent_path, key_kind, key_value, operator, value_kind, value_arg, value_min, value_max, shape, child_context, alternative_id, severity, required, documentation, allowed_scopes, push_scope, replace_scope, min_occurs, strict_min, max_occurs, source_file, line, deprecated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                rule.id,
                rule.context,
                rule.parent_path.join("\u{1f}"),
                key_kind,
                key_value,
                rule.operator,
                value_kind,
                value_arg,
                value_min,
                value_max,
                rule.shape.as_str(),
                rule.child_context,
                rule.alternative_id,
                rule.severity,
                i64::from(rule.required),
                rule.documentation.join("\u{1f}"),
                rule.allowed_scopes.join("\u{1f}"),
                rule.push_scope,
                encode_replace_scope(&rule.replace_scope),
                rule.min_occurs,
                i64::from(rule.strict_min),
                rule.max_occurs,
                rule.source_file,
                rule.line,
                i64::from(rule.deprecated),
            ],
        )?;
    }
    for (name, values) in &rules.model.semantic.enum_values {
        for value in values {
            transaction.execute(
                "INSERT INTO enum_values(enum_name, value) VALUES (?1, ?2)",
                params![name, value],
            )?;
        }
    }
    for (type_name, roots) in &rules.model.semantic.type_root_keys {
        for root in roots {
            transaction.execute(
                "INSERT INTO type_root_keys(type_name, root_key) VALUES (?1, ?2)",
                params![type_name, root],
            )?;
        }
    }
    for (type_name, scopes) in &rules.model.semantic.type_root_scopes {
        for (root, scope) in scopes {
            transaction.execute(
                "INSERT INTO type_root_scopes(type_name, root_key, scope, this_scope, from_scope, documentation) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    type_name,
                    root,
                    scope.root,
                    scope.this,
                    scope.from,
                    scope.documentation.join("\u{1f}"),
                ],
            )?;
        }
    }
    for (type_name, descriptor) in &rules.model.semantic.type_descriptors {
        let scripted_macro = descriptor.scripted_macro.as_ref();
        transaction.execute(
            "INSERT INTO type_descriptors(type_name, path, path_file, path_extension, path_strict, type_per_file, skip_root_keys, name_field, name_from_file, starts_with, type_key_filter, type_key_filter_negate, root_entries, body_context, scripted_macro_body_context, scripted_macro_enabled, scripted_macro_replacement, scripted_macro_condition, scripted_macro_dynamic_key, scripted_macro_opaque_text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                type_name,
                descriptor.path,
                descriptor.path_file,
                descriptor.path_extension,
                i64::from(descriptor.path_strict),
                i64::from(descriptor.type_per_file),
                descriptor
                    .skip_root_paths
                    .iter()
                    .map(|path| path.join("\u{1e}"))
                    .collect::<Vec<_>>()
                    .join("\u{1f}"),
                descriptor.name_field,
                i64::from(descriptor.name_from_file),
                descriptor.starts_with,
                descriptor
                    .type_key_filter
                    .as_ref()
                    .map_or_else(String::new, |(values, _)| values.join("\u{1f}")),
                i64::from(
                    descriptor.type_key_filter.as_ref().is_some_and(|(_, negate)| *negate),
                ),
                descriptor.root_entries,
                descriptor.body_context,
                scripted_macro.map(|value| value.body_context.as_str()),
                i64::from(scripted_macro.is_some_and(|value| value.macro_enabled)),
                i64::from(scripted_macro.is_some_and(|value| value.usage.replacement)),
                i64::from(scripted_macro.is_some_and(|value| value.usage.condition)),
                i64::from(scripted_macro.is_some_and(|value| value.usage.dynamic_key)),
                i64::from(scripted_macro.is_some_and(|value| value.usage.opaque_text)),
            ],
        )?;
    }
    for binding in &rules.model.semantic.localisation_bindings {
        transaction.execute(
            "INSERT INTO localisation_bindings(type_name, field, template, required, optional, subtype, condition_field, condition_value, condition_key_prefix, explicit_field) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                binding.type_name,
                binding.field,
                binding.template,
                i64::from(binding.required),
                i64::from(binding.optional),
                binding.subtype,
                binding.condition.as_ref().and_then(|condition| condition.field.as_deref()),
                binding.condition.as_ref().and_then(|condition| condition.value.as_deref()),
                binding.condition.as_ref().and_then(|condition| condition.key_prefix.as_deref()),
                binding.explicit_field,
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn semantic_key_columns(matcher: &KeyMatcher) -> (&'static str, Option<&str>) {
    match matcher {
        KeyMatcher::Exact(value) => ("exact", Some(value)),
        KeyMatcher::Type(value) => ("type", Some(value)),
        KeyMatcher::Enum(value) => ("enum", Some(value)),
        KeyMatcher::AnyScalar => ("any", None),
        KeyMatcher::Date => ("date", None),
        KeyMatcher::Dynamic(value) => ("dynamic", Some(value)),
    }
}

fn semantic_value_columns(
    matcher: &ValueMatcher,
) -> (&'static str, Option<&str>, Option<String>, Option<String>) {
    match matcher {
        ValueMatcher::AnyScalar => ("any", None, None, None),
        ValueMatcher::Exact(value) => ("exact", Some(value), None, None),
        ValueMatcher::Bool => ("bool", None, None, None),
        ValueMatcher::Int { min, max } => (
            "int",
            None,
            min.map(|value| value.to_string()),
            max.map(|value| value.to_string()),
        ),
        ValueMatcher::Float { min, max } => ("float", None, min.clone(), max.clone()),
        ValueMatcher::Date => ("date", None, None, None),
        ValueMatcher::Type(value) => ("type", Some(value), None, None),
        ValueMatcher::Enum(value) => ("enum", Some(value), None, None),
        ValueMatcher::Scope(value) => ("scope", value.as_deref(), None, None),
        ValueMatcher::Localisation => ("localisation", None, None, None),
        ValueMatcher::Filepath => ("filepath", None, None, None),
        ValueMatcher::Dynamic(value) => ("dynamic", Some(value), None, None),
        ValueMatcher::DynamicSet(value) => ("dynamic-set", Some(value), None, None),
        ValueMatcher::Opaque(value) => ("opaque", Some(value), None, None),
    }
}

fn encode_replace_scope(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(register, scope)| format!("{register}={scope}"))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn decode_replace_scope(value: Option<&str>) -> Vec<(String, String)> {
    value
        .unwrap_or_default()
        .split('\u{1e}')
        .filter_map(|pair| pair.split_once('='))
        .map(|(register, scope)| (register.to_owned(), scope.to_owned()))
        .collect()
}

fn metadata(connection: &Connection, key: &str) -> Result<Option<String>, RulesError> {
    connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn read_model(connection: &Connection) -> Result<RulesModel, RulesError> {
    let mut categories = Vec::new();
    let mut statement = connection.prepare("SELECT id, parser, resolution, path_prefix, path_exact, extensions, path_suffix, path_exclude_prefixes, case_sensitive FROM file_categories ORDER BY id")?;
    let rows = statement.query_map([], |row| -> rusqlite::Result<FileCategory> {
        let extensions: String = row.get(5)?;
        let parser = ParserKind::parse(&row.get::<_, String>(1)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let resolution = FileResolutionPolicy::parse(&row.get::<_, String>(2)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(FileCategory {
            id: row.get(0)?,
            parser,
            resolution,
            matcher: FileMatcher {
                path_prefix: row.get(3)?,
                path_exact: row.get(4)?,
                extensions: if extensions.is_empty() {
                    Vec::new()
                } else {
                    extensions.split('\u{1f}').map(str::to_owned).collect()
                },
                path_suffix: row.get(6)?,
                path_exclude_prefixes: {
                    let encoded: String = row.get(7)?;
                    if encoded.is_empty() {
                        Vec::new()
                    } else {
                        encoded.split('\u{1e}').map(str::to_owned).collect()
                    }
                },
                case_sensitive: row.get::<_, i64>(8)? != 0,
            },
        })
    })?;
    for row in rows {
        categories.push(row?);
    }
    let mut descriptors = Vec::new();
    let mut statement = connection.prepare(
        "SELECT kind_id, resolution, case_sensitive FROM symbol_descriptors ORDER BY kind_id",
    )?;
    let rows = statement.query_map([], |row| -> rusqlite::Result<SymbolDescriptor> {
        let resolution = SymbolResolutionPolicy::parse(&row.get::<_, String>(1)?)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(SymbolDescriptor {
            kind_id: row.get(0)?,
            resolution,
            case_sensitive: row.get::<_, i64>(2)? != 0,
        })
    })?;
    for row in rows {
        descriptors.push(row?);
    }
    let mut records = Vec::new();
    // One joined scan groups fields under their records without a per-record subquery
    // (the previous N+1 pattern), and NULL field rows from `LEFT JOIN` keep records that
    // carry no fields in the output.
    let mut statement = connection.prepare(
        "SELECT r.table_name, r.logical_id, r.source_order, f.field_name, f.field_value
         FROM rule_records r
         LEFT JOIN rule_fields f USING (table_name, logical_id)
         ORDER BY r.table_name, r.logical_id, f.field_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut current: Option<(String, String, u32, BTreeMap<String, String>)> = None;
    for row in rows {
        let (table, logical_id, source_order, field_name, field_value) = row?;
        match &mut current {
            Some((current_table, current_id, _, fields))
                if *current_table == table && *current_id == logical_id =>
            {
                if let (Some(field_name), Some(field_value)) = (field_name, field_value) {
                    fields.insert(field_name, field_value);
                }
            }
            _ => {
                if let Some((table, logical_id, source_order, fields)) = current.take() {
                    records.push(RuleRecord {
                        table,
                        logical_id,
                        source_order,
                        fields,
                    });
                }
                let mut fields = BTreeMap::new();
                if let (Some(field_name), Some(field_value)) = (field_name, field_value) {
                    fields.insert(field_name, field_value);
                }
                current = Some((table, logical_id, source_order, fields));
            }
        }
    }
    if let Some((table, logical_id, source_order, fields)) = current {
        records.push(RuleRecord {
            table,
            logical_id,
            source_order,
            fields,
        });
    }
    let semantic = read_semantic_model(connection)?;
    Ok(RulesModel {
        game_id: String::new(),
        file_categories: categories,
        symbol_descriptors: descriptors,
        records,
        semantic,
        profile: GameProfile::default(),
    })
}

fn scripted_macro_columns_available(connection: &Connection) -> Result<bool, RulesError> {
    for name in [
        "scripted_macro_body_context",
        "scripted_macro_enabled",
        "scripted_macro_replacement",
        "scripted_macro_condition",
        "scripted_macro_dynamic_key",
        "scripted_macro_opaque_text",
    ] {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('type_descriptors') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if present == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_semantic_model(connection: &Connection) -> Result<SemanticModel, RulesError> {
    let mut rules = Vec::new();
    let mut statement = connection.prepare(
        "SELECT id, context, parent_path, key_kind, key_value, operator, value_kind, value_arg, value_min, value_max, shape, child_context, alternative_id, severity, required, documentation, allowed_scopes, push_scope, replace_scope, min_occurs, strict_min, max_occurs, source_file, line, deprecated FROM semantic_rules ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        let key_kind: String = row.get(3)?;
        let key_value: Option<String> = row.get(4)?;
        let operator: Option<String> = row.get(5)?;
        let value_kind: String = row.get(6)?;
        let value_arg: Option<String> = row.get(7)?;
        let value_min: Option<String> = row.get(8)?;
        let value_max: Option<String> = row.get(9)?;
        let shape_name: String = row.get(10)?;
        let child_context: Option<String> = row.get(11)?;
        let alternative_id: Option<String> = row.get(12)?;
        let severity: Option<u8> = row.get(13)?;
        let required: bool = row.get::<_, i64>(14)? != 0;
        let documentation: String = row.get(15)?;
        let allowed_scopes: String = row.get(16)?;
        let push_scope: Option<String> = row.get(17)?;
        let replace_scope: Option<String> = row.get(18)?;
        let min_occurs: Option<u32> = row.get(19)?;
        let strict_min: bool = row.get::<_, i64>(20)? != 0;
        let key = decode_semantic_key(&key_kind, key_value.as_deref())
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let value = decode_semantic_value(
            &value_kind,
            value_arg.as_deref(),
            value_min.as_deref(),
            value_max.as_deref(),
        )
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let shape = RuleShape::parse(&shape_name)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let parent_path: String = row.get(2)?;
        Ok(SemanticRule {
            id: row.get(0)?,
            context: row.get(1)?,
            parent_path: if parent_path.is_empty() {
                Vec::new()
            } else {
                parent_path.split('\u{1f}').map(str::to_owned).collect()
            },
            key,
            operator,
            value,
            shape,
            child_context,
            alternative_id,
            severity,
            required,
            documentation: if documentation.is_empty() {
                Vec::new()
            } else {
                documentation.split('\u{1f}').map(str::to_owned).collect()
            },
            min_occurs,
            strict_min,
            allowed_scopes: if allowed_scopes.is_empty() {
                Vec::new()
            } else {
                allowed_scopes.split('\u{1f}').map(str::to_owned).collect()
            },
            push_scope,
            replace_scope: decode_replace_scope(replace_scope.as_deref()),
            max_occurs: row.get(21)?,
            source_file: row.get(22)?,
            line: row.get(23)?,
            deprecated: row.get::<_, i64>(24)? != 0,
        })
    })?;
    for row in rows {
        rules.push(row?);
    }

    let mut enum_values = BTreeMap::new();
    let mut statement =
        connection.prepare("SELECT enum_name, value FROM enum_values ORDER BY enum_name, value")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (name, value) = row?;
        enum_values.entry(name).or_insert_with(Vec::new).push(value);
    }
    let mut type_root_keys = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT type_name, root_key FROM type_root_keys ORDER BY type_name, root_key")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (type_name, root_key) = row?;
        type_root_keys
            .entry(type_name)
            .or_insert_with(Vec::new)
            .push(root_key);
    }
    let mut type_root_scopes = BTreeMap::new();
    let mut statement = connection.prepare(
        "SELECT type_name, root_key, scope, this_scope, from_scope, documentation FROM type_root_scopes ORDER BY type_name, root_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    for row in rows {
        let (type_name, root_key, root, this, from, documentation) = row?;
        type_root_scopes
            .entry(type_name)
            .or_insert_with(BTreeMap::new)
            .insert(
                root_key,
                TypeRootScope {
                    this: this.unwrap_or_else(|| root.clone()),
                    from: from.unwrap_or_else(|| "any".to_owned()),
                    root,
                    documentation: documentation
                        .unwrap_or_default()
                        .split('\u{1f}')
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect(),
                },
            );
    }
    let mut type_descriptors = BTreeMap::new();
    let scripted_macro_columns = if scripted_macro_columns_available(connection)? {
        "scripted_macro_body_context, scripted_macro_enabled, scripted_macro_replacement, scripted_macro_condition, scripted_macro_dynamic_key, scripted_macro_opaque_text"
    } else {
        "NULL, 0, 0, 0, 0, 0"
    };
    let descriptor_query = format!(
        "SELECT type_name, path, path_file, path_extension, path_strict, type_per_file, skip_root_keys, name_field, name_from_file, starts_with, type_key_filter, type_key_filter_negate, root_entries, body_context, {scripted_macro_columns} FROM type_descriptors ORDER BY type_name"
    );
    let mut statement = connection.prepare(&descriptor_query)?;
    let rows = statement.query_map([], |row| {
        let type_name: String = row.get(0)?;
        let skip_root_paths: String = row.get(6)?;
        let type_key_filter: String = row.get(10)?;
        let type_key_filter_negate: bool = row.get::<_, i64>(11)? != 0;
        let scripted_macro_body_context: Option<String> = row.get(14)?;
        let scripted_macro_enabled: bool = row.get::<_, i64>(15)? != 0;
        let scripted_macro_replacement: bool = row.get::<_, i64>(16)? != 0;
        let scripted_macro_condition: bool = row.get::<_, i64>(17)? != 0;
        let scripted_macro_dynamic_key: bool = row.get::<_, i64>(18)? != 0;
        let scripted_macro_opaque_text: bool = row.get::<_, i64>(19)? != 0;
        Ok(TypeDescriptor {
            name: type_name.clone(),
            path: row.get(1)?,
            path_file: row.get(2)?,
            path_extension: row.get(3)?,
            path_strict: row.get::<_, i64>(4)? != 0,
            type_per_file: row.get::<_, i64>(5)? != 0,
            skip_root_paths: if skip_root_paths.is_empty() {
                Vec::new()
            } else {
                skip_root_paths
                    .split('\u{1f}')
                    .map(|path| path.split('\u{1e}').map(str::to_owned).collect())
                    .collect()
            },
            name_field: row.get(7)?,
            name_from_file: row.get::<_, i64>(8)? != 0,
            starts_with: row.get(9)?,
            root_entries: row.get(12)?,
            body_context: row.get(13)?,
            type_key_filter: if type_key_filter.is_empty() {
                None
            } else {
                Some((
                    type_key_filter.split('\u{1f}').map(str::to_owned).collect(),
                    type_key_filter_negate,
                ))
            },
            scripted_macro: scripted_macro_body_context.map(|body_context| {
                ScriptedMacroDescriptor {
                    body_context,
                    macro_enabled: scripted_macro_enabled,
                    usage: ScriptedMacroUsage {
                        replacement: scripted_macro_replacement,
                        condition: scripted_macro_condition,
                        dynamic_key: scripted_macro_dynamic_key,
                        opaque_text: scripted_macro_opaque_text,
                    },
                }
            }),
        })
    })?;
    for row in rows {
        let descriptor = row?;
        type_descriptors.insert(descriptor.name.clone(), descriptor);
    }
    let mut localisation_bindings = Vec::new();
    let mut statement = connection.prepare(
        "SELECT type_name, field, template, required, optional, subtype, condition_field, condition_value, condition_key_prefix, explicit_field FROM localisation_bindings ORDER BY type_name, subtype, field",
    )?;
    let rows = statement.query_map([], |row| {
        let condition_field: Option<String> = row.get(6)?;
        let condition_value: Option<String> = row.get(7)?;
        let condition_key_prefix: Option<String> = row.get(8)?;
        Ok(LocalisationBinding {
            type_name: row.get(0)?,
            field: row.get(1)?,
            template: row.get(2)?,
            required: row.get::<_, i64>(3)? != 0,
            optional: row.get::<_, i64>(4)? != 0,
            subtype: row.get(5)?,
            condition: (condition_field.is_some()
                || condition_value.is_some()
                || condition_key_prefix.is_some())
            .then_some(LocalisationBindingCondition {
                field: condition_field,
                value: condition_value,
                key_prefix: condition_key_prefix,
            }),
            explicit_field: row.get(9)?,
        })
    })?;
    for row in rows {
        localisation_bindings.push(row?);
    }
    Ok(SemanticModel {
        rules,
        enum_values,
        type_root_keys,
        type_root_scopes,
        type_descriptors,
        localisation_bindings,
    })
}

fn decode_semantic_key(kind: &str, value: Option<&str>) -> Result<KeyMatcher, RulesError> {
    Ok(match kind {
        "exact" => KeyMatcher::Exact(value.unwrap_or_default().to_owned()),
        "type" => KeyMatcher::Type(value.unwrap_or_default().to_owned()),
        "enum" => KeyMatcher::Enum(value.unwrap_or_default().to_owned()),
        "any" => KeyMatcher::AnyScalar,
        "date" => KeyMatcher::Date,
        "dynamic" => KeyMatcher::Dynamic(value.unwrap_or_default().to_owned()),
        other => return Err(RulesError::InvalidRuleShape(other.to_owned())),
    })
}

fn decode_semantic_value(
    kind: &str,
    arg: Option<&str>,
    min: Option<&str>,
    max: Option<&str>,
) -> Result<ValueMatcher, RulesError> {
    Ok(match kind {
        "any" => ValueMatcher::AnyScalar,
        "exact" => ValueMatcher::Exact(arg.unwrap_or_default().to_owned()),
        "bool" => ValueMatcher::Bool,
        "int" => ValueMatcher::Int {
            min: min.map(str::parse).transpose().map_err(|_| {
                RulesError::InvalidRuleShape("invalid integer matcher bound".to_owned())
            })?,
            max: max.map(str::parse).transpose().map_err(|_| {
                RulesError::InvalidRuleShape("invalid integer matcher bound".to_owned())
            })?,
        },
        "float" => ValueMatcher::Float {
            min: min.map(str::to_owned),
            max: max.map(str::to_owned),
        },
        "date" => ValueMatcher::Date,
        "type" => ValueMatcher::Type(arg.unwrap_or_default().to_owned()),
        "enum" => ValueMatcher::Enum(arg.unwrap_or_default().to_owned()),
        "scope" => ValueMatcher::Scope(arg.map(str::to_owned)),
        "localisation" => ValueMatcher::Localisation,
        "filepath" => ValueMatcher::Filepath,
        "dynamic" => ValueMatcher::Dynamic(arg.unwrap_or_default().to_owned()),
        "dynamic-set" => ValueMatcher::DynamicSet(arg.unwrap_or_default().to_owned()),
        "opaque" => ValueMatcher::Opaque(arg.unwrap_or_default().to_owned()),
        other => return Err(RulesError::InvalidRuleShape(other.to_owned())),
    })
}
