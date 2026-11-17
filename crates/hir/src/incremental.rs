//! Conservative rebasing of base facts when one ignored quoted scalar changes.
use crate::*;
use parser::{FileFormat, ParsedFile};
use std::sync::Arc;
use text::TextRange;

struct Shift {
    token: TextRange,
    delta: i64,
}
impl Shift {
    fn point(&self, p: u32) -> Option<u32> {
        if p <= self.token.start() {
            Some(p)
        } else if p >= self.token.end() {
            u32::try_from(i64::from(p) + self.delta).ok()
        } else {
            None
        }
    }
    fn range(&self, r: TextRange) -> Option<TextRange> {
        TextRange::new(self.point(r.start())?, self.point(r.end())?)
    }
}
fn contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

/// Reuses a full, no-workspace-facts base under the same rules/path/profile.
/// An unknown block must isolate the edited scalar from semantic dispatch;
/// every structural property and every retained source range is checked.
/// Anything less certain returns `None` for ordinary full lowering.
pub fn rebase_ignored_quoted_scalar(
    previous: &HirFile,
    syntax: Arc<ParsedFile>,
) -> Option<HirFile> {
    rebase_ignored_quoted_scalar_impl(previous, syntax, false)
}

/// Reuses structural facts only with the parser's exact previous-tree proof.
pub fn rebase_ignored_quoted_edit(
    previous: &HirFile,
    edit: &parser::QuotedScriptEdit<'_>,
) -> Option<HirFile> {
    if !edit.is_from(previous.syntax()) {
        return None;
    }
    rebase_ignored_quoted_scalar_impl(previous, edit.parsed(), true)
}

fn rebase_ignored_quoted_scalar_impl(
    previous: &HirFile,
    syntax: Arc<ParsedFile>,
    proven: bool,
) -> Option<HirFile> {
    if !previous.uses_ir
        || previous.symbol_reads.is_some()
        || syntax.format() != FileFormat::Script
        || !previous.dynamic_templates.is_empty()
        || !previous.parameter_definitions.is_empty()
        || !previous.parameter_references.is_empty()
    {
        return None;
    }
    let old = previous.syntax.source();
    let new = syntax.source();
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old.len() && prefix == new.len() {
        let mut result = previous.clone();
        result.syntax = syntax;
        return Some(result);
    }
    let suffix = old.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(new.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let property = previous.properties.iter().find(|p| {
        p.scalar.as_ref().is_some_and(|s| {
            s.quoted
                && s.range.start() as usize <= prefix
                && old.len() - suffix <= s.range.end() as usize
        })
    })?;
    let scalar = property.scalar.as_ref()?;
    // A complete unknown block has no selected child schema. Its siblings can
    // observe only its unchanged block shape/key, never this nested scalar.
    let ignored = previous
        .properties
        .iter()
        .filter(|p| {
            p.scalar.is_none()
                && contains(p.range, property.range)
                && p.key_range != property.key_range
                && previous
                    .field_fact_at(p.key_range)
                    .is_none_or(|f| f.fields.is_empty())
        })
        .min_by_key(|p| p.range.len())?;
    let ignored_text = previous.syntax.text(ignored.value_range?)?.trim();
    if !ignored_text.starts_with('{') || !ignored_text.ends_with('}') {
        return None;
    }
    if previous
        .overload_facts
        .iter()
        .any(|fact| contains(fact.container, scalar.range))
        || previous
            .field_facts
            .iter()
            .any(|fact| contains(ignored.range, fact.range) && fact.range != ignored.key_range)
        || previous
            .scope_facts
            .iter()
            .any(|fact| contains(ignored.range, fact.range) && fact.range != ignored.key_range)
        || previous
            .schema_facts
            .iter()
            .any(|fact| contains(ignored.range, fact.range))
        || previous
            .definitions
            .iter()
            .any(|d| contains(ignored.range, d.selection_range))
        || previous
            .references
            .iter()
            .chain(&previous.binding_references)
            .any(|r| contains(ignored.range, r.range))
    {
        return None;
    }
    let shift = Shift {
        token: scalar.range,
        delta: new.len() as i64 - old.len() as i64,
    };
    let target = shift.range(scalar.range)?;
    // The lexer must still recognize exactly one closed quoted carrier.
    for (parsed, range) in [(&*previous.syntax, scalar.range), (&*syntax, target)] {
        if !parsed
            .tokens()
            .iter()
            .any(|token| token.kind() == parser::TokenKind::Quoted && token.range() == range)
            || parsed
                .errors()
                .iter()
                .any(|error| contains(range, error.range))
        {
            return None;
        }
    }
    let collected = if proven {
        rebase_collected(previous, &syntax, &shift)?
    } else {
        crate::collector::collect(&syntax)
    };
    if previous.properties.len() != collected.properties.len() {
        return None;
    }
    if !proven {
        for (old, new) in previous.properties.iter().zip(collected.properties.iter()) {
            if old.key != new.key
                || old.operator != new.operator
                || old.path != new.path
                || old.top_level != new.top_level
                || shift.range(old.key_range) != Some(new.key_range)
                || shift.range(old.range) != Some(new.range)
                || old.value_range.map(|r| shift.range(r)).transpose_option()? != new.value_range
            {
                return None;
            }
            match (&old.scalar, &new.scalar) {
                (None, None) => {}
                (Some(a), Some(b))
                    if a.quoted == b.quoted
                        && shift.range(a.range) == Some(b.range)
                        && (old.key_range == property.key_range || a.value == b.value) => {}
                _ => return None,
            }
        }
    }
    let mut result = previous.clone();
    for fact in &mut result.field_facts {
        fact.range = shift.range(fact.range)?;
    }
    for fact in &mut result.scope_facts {
        fact.range = shift.range(fact.range)?;
    }
    for fact in &mut result.schema_facts {
        fact.range = shift.range(fact.range)?;
    }
    for fact in &mut result.definitions {
        fact.range = shift.range(fact.range)?;
        fact.selection_range = shift.range(fact.selection_range)?;
    }
    for fact in result
        .references
        .iter_mut()
        .chain(&mut result.binding_references)
    {
        fact.range = shift.range(fact.range)?;
    }
    for fact in &mut result.definition_attributes {
        fact.definition_range = shift.range(fact.definition_range)?;
    }
    for fact in &mut result.overload_facts {
        fact.range = shift.range(fact.range)?;
        fact.container = shift.range(fact.container)?;
    }
    for (container, guard) in &mut result.runtime_parameter_guards {
        *container = shift.range(*container)?;
        *guard = guard.map(|r| shift.range(r)).transpose_option()?;
    }
    result.syntax = syntax;
    result.properties = collected.properties;
    result.localisation_entries = collected.localisation_entries;
    result.bare_values = collected.bare_values;
    result.unknown_constructs = collected.unknown_constructs;
    result.parameter_conditionals = collected.parameter_conditionals;
    Some(result)
}
// Unchanged CST kinds/edges and unchanged bytes outside one scalar prove
// identical collector traversal, key paths, ordering, and decoded siblings.
fn rebase_collected(
    previous: &HirFile,
    syntax: &ParsedFile,
    shift: &Shift,
) -> Option<crate::collector::CollectedFacts> {
    let scalar = |value: &mut HirScalar| -> Option<()> {
        if value.range == shift.token {
            value.value =
                parser::decode_quoted_script(syntax.text(shift.range(value.range)?)?.trim())?.0;
        }
        value.range = shift.range(value.range)?;
        Some(())
    };
    let mut properties = previous.properties.as_ref().clone();
    for p in &mut properties {
        p.key_range = shift.range(p.key_range)?;
        p.range = shift.range(p.range)?;
        p.value_range = p.value_range.map(|r| shift.range(r)).transpose_option()?;
        if let Some(value) = &mut p.scalar {
            scalar(value)?;
        }
    }
    let mut bare_values = previous.bare_values.as_ref().clone();
    for value in &mut bare_values {
        scalar(value)?;
    }
    let mut unknown_constructs = previous.unknown_constructs.as_ref().clone();
    for value in &mut unknown_constructs {
        value.range = shift.range(value.range)?;
    }
    let mut parameter_conditionals = previous.parameter_conditionals.as_ref().clone();
    for value in &mut parameter_conditionals {
        value.range = shift.range(value.range)?;
        value.condition_range = shift.range(value.condition_range)?;
        value.name_range = shift.range(value.name_range)?;
    }
    if !previous.localisation_entries.is_empty() {
        return None;
    }
    Some(crate::collector::CollectedFacts {
        properties: Arc::new(properties),
        bare_values: Arc::new(bare_values),
        unknown_constructs: Arc::new(unknown_constructs),
        parameter_conditionals: Arc::new(parameter_conditionals),
        localisation_entries: previous.localisation_entries.clone(),
    })
}
trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}
impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            None => Some(None),
            Some(value) => value.map(Some),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_quoted_edits_match_full_base_lowering_and_reject_semantic_edits() {
        let ir = game::eu4::first_party_ir().unwrap();
        let rules = game::eu4::runtime_rules().unwrap();
        let profile = game::eu4::profile();
        let path = text::LogicalPath::parse("events/quoted.txt").unwrap();
        let source = r#"country_event = { id = sample.1 title = "title_key" immediate = { for = { BODY = "spawn_rebels = { size = 1 }" } add_stability = 1 } } country_event = { id = sample.2 immediate = { set_country_flag = after } }"#;
        let lower = |source: &str| {
            crate::lower_shared_with_ir(
                Arc::new(parser::parse(FileFormat::Script, source)),
                &path,
                &rules,
                &profile,
                &ir,
            )
        };
        let base = lower(source);
        for value in [
            "spawn = { size = 1 }",
            " = { size = 1 }",
            "中😀 = yes",
            r#"a = \"quoted\""#,
        ] {
            let edited = source.replace("spawn_rebels = { size = 1 }", value);
            let syntax = Arc::new(parser::parse(FileFormat::Script, &edited));
            let actual =
                rebase_ignored_quoted_scalar(&base, syntax).expect("isolated unknown invocation");
            assert_eq!(actual, lower(&edited), "{value}");
            let edit =
                parser::prepare_script_quoted_edit(base.syntax(), Arc::from(edited.as_str()))
                    .unwrap();
            let incremental = rebase_ignored_quoted_edit(&base, &edit).unwrap();
            assert_eq!(incremental, actual, "proven structure: {value}");
            let other = lower(source);
            assert!(rebase_ignored_quoted_edit(&other, &edit).is_none());
        }
        for edited in [
            source.replace("title_key", "new_title"),
            source.replace("spawn_rebels = { size = 1 }", "\" } add_prestige = 2 #"),
            source.replace("for =", "capital ="),
        ] {
            assert!(
                rebase_ignored_quoted_scalar(
                    &base,
                    Arc::new(parser::parse(FileFormat::Script, &edited))
                )
                .is_none(),
                "{edited}"
            );
        }
        let with_facts = crate::lower_shared_with_ir_and_facts(
            Arc::new(parser::parse(FileFormat::Script, source)),
            &path,
            &rules,
            &profile,
            &ir,
            &rules::ir::NoSymbolFacts,
        );
        assert!(rebase_ignored_quoted_scalar(&with_facts, base.syntax.clone()).is_none());
    }
}
