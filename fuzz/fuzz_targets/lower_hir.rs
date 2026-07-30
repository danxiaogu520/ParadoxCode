#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use pdx_engine::hir::{HirFile, lower, lower_with_profile};
use pdx_rules::{GameProfile, RuleSet};
use pdx_parser::{FileFormat, parse};
use pdx_text::LogicalPath;

static PROFILE_INPUTS: OnceLock<(RuleSet, GameProfile, LogicalPath)> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let hir = lower(parse(FileFormat::Script, source), &RuleSet::empty());
        check_ranges(&hir, source.len());

        let (rules, profile, path) = PROFILE_INPUTS.get_or_init(|| {
            (
                pdx_game::eu4::bootstrap_rules(),
                pdx_game::eu4::profile(),
                LogicalPath::parse("common/scripted_effects/fuzz.txt")
                    .expect("static fuzz path is valid"),
            )
        });
        let profile_hir =
            lower_with_profile(parse(FileFormat::Script, source), path, rules, profile);
        check_ranges(&profile_hir, source.len());
    }
});

fn check_ranges(hir: &HirFile, source_len: usize) {
    let source_len = u32::try_from(source_len).unwrap_or(u32::MAX);
    assert!(hir.properties().iter().all(|property| {
        property.key_range.start() >= property.range.start()
            && property.key_range.end() <= property.range.end()
            && property.range.end() <= source_len
    }));
    assert!(hir.bare_values().iter().all(|value| value.range.end() <= source_len));
    assert!(hir.definitions().iter().all(|definition| {
        definition.selection_range.start() >= definition.range.start()
            && definition.selection_range.end() <= definition.range.end()
            && definition.range.end() <= source_len
    }));
    assert!(hir.references().iter().all(|reference| reference.range.end() <= source_len));
    assert!(hir.scope_facts().iter().all(|fact| fact.range.end() <= source_len));
    assert!(hir.unknown_constructs().iter().all(|unknown| unknown.range.end() <= source_len));
    assert!(hir.parameter_conditionals().iter().all(|conditional| {
        conditional.name_range.start() >= conditional.condition_range.start()
            && conditional.name_range.end() <= conditional.condition_range.end()
            && conditional.condition_range.end() <= conditional.range.end()
            && conditional.range.end() <= source_len
    }));
    assert!(hir.parameter_definitions().iter().all(|definition| {
        definition.name_range.start() >= definition.range.start()
            && definition.name_range.end() <= definition.range.end()
            && definition.range.start() >= definition.owner_range.start()
            && definition.range.end() <= definition.owner_range.end()
            && definition.owner_range.end() <= source_len
    }));
    assert!(hir.parameter_references().iter().all(|reference| {
        reference.name_range.start() >= reference.range.start()
            && reference.name_range.end() <= reference.range.end()
            && reference.range.start() >= reference.owner_range.start()
            && reference.range.end() <= reference.owner_range.end()
            && reference.owner_range.end() <= source_len
    }));
    assert!(
        hir.parameter_definitions()
            .windows(2)
            .all(|items| items[0].range.start() <= items[1].range.start())
    );
    assert!(
        hir.parameter_references()
            .windows(2)
            .all(|items| items[0].range.end() <= items[1].range.start())
    );
}
