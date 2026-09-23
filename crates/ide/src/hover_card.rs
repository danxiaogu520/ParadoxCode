//! Structured hover-card payloads for the extension's rendered previews.
//!
//! The hover middleware owns pixels; this module owns paths. A card tells the
//! client which asset file serves a hovered sprite, texture reference,
//! mission, or event, resolved through the workspace [`TextureCatalog`] exactly
//! the way the engine resolves it — so the client no longer re-derives paths
//! with a second, weaker resolver. Every card is advisory: the client degrades
//! to its legacy behaviour when a card is absent.
//!
//! Mission and event cards anchor on tokens, not whole blocks: the
//! definition's block-name token (`<mission_name> = {`,
//! `country_event`/`province_event = {`), or a reference the semantic layer
//! already resolves (`required_missions` members, `event`-keyed values) —
//! the same positions go-to-definition serves. Positions inside the block
//! body never card, so logic hovers keep the plain semantic pipeline.

use engine::{AnalysisSnapshot, DocumentId, SourceRootKind};
use rules::GameProfile;
use text::{TextRange, TextSize};

use crate::resolution::{
    DefinitionInfo, localisation_values_by_key, semantic_data_with_cancellation,
    symbol_candidates_for_hover,
};
use crate::support::{ParsedInput, contains, input_for_document, input_for_source_file};
use crate::types::{CancellationToken, Cancelled, Location};

/// Interface-definition keys whose scalar values the engine loads as texture
/// paths — the `texture_path`-typed rule values of the baked interface rules.
const TEXTURE_VALUE_KEYS: &[&str] = &[
    "alphamaskfile",
    "animationmaskfile",
    "animationtexturefile",
    "effect",
    "file",
    "normal",
    "shader_file",
    "specular",
    "texture",
    "texturefile",
    "texturefile1",
    "texturefile2",
    "texturefile3",
];

/// Upper bound on bytes read from a sprite-definition file. Vanilla `.gfx`
/// files are far smaller; the cap keeps a hostile tree from stalling a hover.
const MAX_DEFINITION_READ_BYTES: u64 = 8 * 1024 * 1024;

/// One resolved asset reference: where the file lives and which sprite owns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverCardAsset {
    /// Sprite name the reference resolved through, when the asset backs one.
    pub sprite: Option<String>,
    /// Absolute path of the serving file on this machine. For a DLC archive
    /// member this is the `.zip` itself; `archive_member` names the entry.
    pub path: String,
    /// Kind of the source root serving the file (`project`/`dependency`/`vanilla`).
    pub root_kind: String,
    /// The `.tga`/`.dds` extension drift fallback saved this reference.
    pub extension_fallback: bool,
    /// In-archive spelling when `path` is a DLC zip; the card cannot decode
    /// packed members, so previews degrade to the plain hover text.
    pub archive_member: Option<String>,
    /// Declared horizontal frame count (`noOfFrames`), when the block has one.
    pub frames: Option<u32>,
}

/// Mission facts a rendered node card needs beyond its assets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverCardMission {
    pub id: String,
    /// `icon = <sprite>` spelling, when the mission declares one.
    pub icon: Option<String>,
    /// Localisation key of the mission title (`{id}_title`).
    pub title_key: String,
    /// Resolved title localisation (language, value), when any language
    /// defines the key.
    pub title: Option<(Option<String>, String)>,
    /// Prerequisite mission ids in source order.
    pub required: Vec<String>,
}

/// The fixed chrome assets of a mission node card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissionCardAssets {
    pub frame: Option<HoverCardAsset>,
}

/// One event option: its localisation key and resolved text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverCardEventOption {
    /// `name` spelling of the `option` block, when it declares one.
    pub name_key: String,
    /// Resolved option text, when any language defines the key.
    pub name: Option<(Option<String>, String)>,
}

/// Event facts a rendered event-window card needs beyond its assets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverCardEvent {
    pub id: String,
    /// `picture = <sprite>` spelling, when the event declares one.
    pub picture: Option<String>,
    /// Localisation key of the event title, when a scalar `title` is declared.
    /// The common `<id>.t` spelling is a modder convention inside the value,
    /// not an engine default — no key is fabricated for a missing field.
    pub title_key: Option<String>,
    /// Resolved title localisation, when any language defines the key.
    pub title: Option<(Option<String>, String)>,
    /// Localisation key of the description (a scalar `desc`; a conditional
    /// `desc` block carries no single key).
    pub desc_key: Option<String>,
    /// Resolved description localisation.
    pub desc: Option<(Option<String>, String)>,
    /// Options in source order.
    pub options: Vec<HoverCardEventOption>,
}

/// The fixed chrome assets of an event-window card.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCardAssets {
    pub background_top: Option<HoverCardAsset>,
    pub background_middle: Option<HoverCardAsset>,
    pub background_bottom_s: Option<HoverCardAsset>,
    pub background_bottom_m: Option<HoverCardAsset>,
    pub background_bottom_l: Option<HoverCardAsset>,
    pub option_button: Option<HoverCardAsset>,
}

/// One hover card: the kind picks the client renderer, the payload fields are
/// kind-dependent and absent otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverCard {
    pub kind: &'static str,
    pub asset: Option<HoverCardAsset>,
    pub mission: Option<HoverCardMission>,
    pub card_assets: Option<MissionCardAssets>,
    pub event: Option<HoverCardEvent>,
    pub event_assets: Option<EventCardAssets>,
}

/// Computes the hover card for a document position, if the position
/// references something a card can render.
///
/// Priority: a hovered event/mission reference renders the referenced
/// definition's card, then a definition's block-name token in a missions or
/// events file renders its own card, then a texture-valued property in an
/// interface `.gfx` document, then a sprite reference or definition anywhere.
pub fn hover_card_with_cancellation(
    snapshot: &AnalysisSnapshot,
    document: &DocumentId,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    cancellation.checkpoint()?;
    let Some(input) = input_for_document(snapshot, document) else {
        return Ok(None);
    };
    if let Some(card) = reference_card(snapshot, &input, position, cancellation)? {
        return Ok(Some(card));
    }
    if let Some(card) = mission_card(snapshot, &input, position, cancellation)? {
        return Ok(Some(card));
    }
    if let Some(card) = event_card(snapshot, &input, position, cancellation)? {
        return Ok(Some(card));
    }
    if let Some(card) = icon_card(snapshot, &input, position, cancellation)? {
        return Ok(Some(card));
    }
    if let Some(card) = texture_card(snapshot, &input, position) {
        return Ok(Some(card));
    }
    sprite_card(snapshot, &input, position, cancellation)
}

/// The call-site card: a hovered event or mission reference renders the card
/// of the definition it resolves to, wherever that definition lives —
/// answering "what does the referenced event or mission look like" without
/// opening the target file. Event references ride the semantic reference
/// layer (the same positions go-to-definition serves); mission prerequisites
/// are bare block members the semantic layer does not reference, so they are
/// spotted through the mission parse model instead. Both resolve targets
/// through the shared symbol layer.
fn reference_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if input.format != parser::FileFormat::Script
        || !input
            .profile
            .game_id
            .eq_ignore_ascii_case(game::eu4::GAME_ID)
    {
        return Ok(None);
    }
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    if let Some(reference) = semantic.references.iter().find(|reference| {
        contains(reference.range, position) && reference.kind.eq_ignore_ascii_case("event")
    }) {
        let candidates =
            symbol_candidates_for_hover(snapshot, &reference.kind, &reference.name, cancellation)?;
        for candidate in &candidates {
            cancellation.checkpoint()?;
            let Some(target) = input_for_location(snapshot, &candidate.location) else {
                continue;
            };
            let card = event_card_for_reference(
                snapshot,
                &target,
                candidate.selection_range,
                cancellation,
            )?;
            if card.is_some() {
                return Ok(card);
            }
        }
    }
    if crate::mission::is_mission_path(input.path.as_ref()) {
        cancellation.checkpoint()?;
        let Some(name) = required_mission_at(&input.source, position) else {
            return Ok(None);
        };
        let candidates = symbol_candidates_for_hover(snapshot, "mission", &name, cancellation)?;
        for candidate in &candidates {
            cancellation.checkpoint()?;
            let Some(target) = input_for_location(snapshot, &candidate.location) else {
                continue;
            };
            let card = mission_card_for_reference(
                snapshot,
                &target,
                candidate.selection_range,
                &name,
                cancellation,
            )?;
            if card.is_some() {
                return Ok(card);
            }
        }
    }
    Ok(None)
}

/// The `required_missions` member under `position`, if any: bare block
/// members carry no semantic reference, so the parse model's prerequisite
/// token ranges are the anchor.
fn required_mission_at(source: &str, position: TextSize) -> Option<String> {
    let loaded = game::eu4::mission::parse_file(source);
    for mission in loaded
        .file
        .trees
        .iter()
        .flat_map(|tree| tree.missions.iter())
    {
        for (index, range) in mission.required_ranges.iter().enumerate() {
            if contains(*range, position) {
                return mission.required.get(index).cloned();
            }
        }
    }
    None
}

/// Loads the parsed input behind a definition location: the open overlay
/// when one backs it (freshest text), else the indexed disk file.
fn input_for_location(snapshot: &AnalysisSnapshot, location: &Location) -> Option<ParsedInput> {
    if let Some(document) = location.document.as_ref()
        && let Some(input) = input_for_document(snapshot, document)
    {
        return Some(input);
    }
    let file = location.file.as_ref()?;
    input_for_source_file(snapshot, *file)
}

fn mission_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if input.format != parser::FileFormat::Script
        || !input
            .profile
            .game_id
            .eq_ignore_ascii_case(game::eu4::GAME_ID)
        || !crate::mission::is_mission_path(input.path.as_ref())
    {
        return Ok(None);
    }
    cancellation.checkpoint()?;
    let loaded = game::eu4::mission::parse_file(&input.source);
    let Some(mission) = loaded
        .file
        .trees
        .iter()
        .flat_map(|tree| tree.missions.iter())
        .find(|mission| contains(mission.id_range, position))
    else {
        return Ok(None);
    };
    mission_card_for_mission(snapshot, mission, cancellation)
}

/// Renders the mission card for a reference's target: the mission in `target`
/// whose key token is the definition selection, else the first with a
/// matching id (name-keyed definitions and parsed ids share spelling).
fn mission_card_for_reference(
    snapshot: &AnalysisSnapshot,
    target: &ParsedInput,
    selection: TextRange,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if !crate::mission::is_mission_path(target.path.as_ref()) {
        return Ok(None);
    }
    cancellation.checkpoint()?;
    let loaded = game::eu4::mission::parse_file(&target.source);
    let mut by_name = None;
    for mission in loaded
        .file
        .trees
        .iter()
        .flat_map(|tree| tree.missions.iter())
    {
        if contains(mission.id_range, selection.start()) {
            return mission_card_for_mission(snapshot, mission, cancellation);
        }
        if by_name.is_none() && mission.id.eq_ignore_ascii_case(name) {
            by_name = Some(mission);
        }
    }
    match by_name {
        Some(mission) => mission_card_for_mission(snapshot, mission, cancellation),
        None => Ok(None),
    }
}

/// Renders the mission card for one parsed mission: localised title, icon
/// texture, and the fixed node chrome.
fn mission_card_for_mission(
    snapshot: &AnalysisSnapshot,
    mission: &game::eu4::mission::Mission,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    // The title key comes from the mission's `$_title` binding — the JSON is
    // the single source; an absent binding degrades to no title (the client
    // falls back to the raw id).
    let title_key = snapshot
        .rules()
        .localisation_template_key("mission", "name", &mission.id)
        .unwrap_or_default();
    let titles = localisation_values_by_key(snapshot, &[title_key.as_str()], cancellation)?;
    let title = titles.get(&title_key).cloned();
    let icon = mission.icon.clone();
    let icon_asset = match icon.as_deref() {
        Some(name) => sprite_asset(snapshot, name, cancellation)?,
        None => None,
    };
    // The frame sprite is card chrome: it comes from the profile's mission
    // card declaration, not from this renderer.
    let frame = match snapshot
        .game_profile()
        .hover_card("mission")
        .and_then(|spec| spec.chrome.get("frame"))
    {
        Some(name) => sprite_asset(snapshot, name, cancellation)?,
        None => None,
    };
    if icon_asset.is_none() && frame.is_none() {
        // Nothing renders without at least a frame; a text-only hover is
        // already served by the semantic pipeline.
        return Ok(None);
    }
    Ok(Some(HoverCard {
        kind: "mission",
        asset: icon_asset,
        mission: Some(HoverCardMission {
            id: mission.id.clone(),
            icon,
            title_key,
            title,
            required: mission.required.clone(),
        }),
        card_assets: Some(MissionCardAssets { frame }),
        event: None,
        event_assets: None,
    }))
}

/// The generic icon card: hovering the definition token of a type that
/// carries icon bindings (or a reference to such a definition) renders the
/// bound sprite's texture. The specialized mission and event cards own their
/// families; this card serves every remaining icon-bound type, from holy
/// orders to subject types.
fn icon_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if input.format != parser::FileFormat::Script
        || !input
            .profile
            .game_id
            .eq_ignore_ascii_case(game::eu4::GAME_ID)
    {
        return Ok(None);
    }
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    let current_document = input.document.as_ref();
    // The card gate is reference-driven: any definition whose block carries a
    // sprite reference (schema-typed or binding-derived) can render an icon.
    let definition_has_sprite = |definition: &DefinitionInfo| {
        semantic.references.iter().any(|reference| {
            reference.kind.eq_ignore_ascii_case("sprite")
                && within(definition.symbol.range, reference.range)
        })
    };
    let hovered_definition = semantic.definitions.iter().find(|definition| {
        definition.document.as_ref() == current_document
            && contains(definition.symbol.selection_range, position)
            && definition_has_sprite(definition)
    });
    if let Some(definition) = hovered_definition {
        return icon_card_for_definition(snapshot, input, definition, cancellation);
    }
    // A hovered reference to an icon-bearing definition renders the target's
    // icon; sprite and localisation references themselves have their own
    // card paths further down the chain.
    let Some(reference) = semantic.references.iter().find(|reference| {
        contains(reference.range, position)
            && !reference.kind.eq_ignore_ascii_case("sprite")
            && !reference.kind.eq_ignore_ascii_case("localisation")
    }) else {
        return Ok(None);
    };
    let candidates =
        symbol_candidates_for_hover(snapshot, &reference.kind, &reference.name, cancellation)?;
    for candidate in &candidates {
        cancellation.checkpoint()?;
        let Some(target) = input_for_location(snapshot, &candidate.location) else {
            continue;
        };
        let target_semantic = semantic_data_with_cancellation(snapshot, &target, cancellation)?;
        let Some(definition) = target_semantic
            .definitions
            .iter()
            .find(|definition| {
                definition
                    .document
                    .as_ref()
                    .is_some_and(|document| target.document.as_ref() == Some(document))
                    && contains(
                        definition.symbol.selection_range,
                        candidate.selection_range.start(),
                    )
            })
            .or_else(|| {
                target_semantic.definitions.iter().find(|definition| {
                    definition
                        .document
                        .as_ref()
                        .is_some_and(|document| target.document.as_ref() == Some(document))
                        && definition.name.eq_ignore_ascii_case(&reference.name)
                })
            })
        else {
            continue;
        };
        let card = icon_card_for_definition(snapshot, &target, definition, cancellation)?;
        if card.is_some() {
            return Ok(card);
        }
    }
    Ok(None)
}

/// Resolves the icon sprites bound to one definition — derived icon
/// references (templates, self bindings, semantic fields) plus any sprite
/// references already lowered inside the block — and renders the first that
/// resolves to a loadable texture.
fn icon_card_for_definition(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    definition: &DefinitionInfo,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    let mut candidates = Vec::<(String, TextRange)>::new();
    if let Some(hir) = input.hir.as_deref()
        && let Some(path) = input.path.as_ref()
    {
        candidates.extend(
            hir::derived_sprite_references_for_hover(hir, path, snapshot.rules())
                .into_iter()
                .map(|reference| (reference.name, reference.range)),
        );
    }
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    candidates.extend(
        semantic
            .references
            .iter()
            .filter(|reference| {
                reference.kind.eq_ignore_ascii_case("sprite")
                    && within(definition.symbol.range, reference.range)
            })
            .map(|reference| (reference.name.clone(), reference.range)),
    );
    candidates.retain(|(_, range)| within(definition.symbol.range, *range));
    candidates.sort_by_key(|(_, range)| (range.start(), range.end()));
    candidates.dedup_by(|left, right| left.0.eq_ignore_ascii_case(&right.0));
    for (name, _) in candidates {
        cancellation.checkpoint()?;
        if let Some(asset) = sprite_asset(snapshot, &name, cancellation)? {
            return Ok(Some(HoverCard {
                kind: "sprite",
                asset: Some(asset),
                mission: None,
                card_assets: None,
                event: None,
                event_assets: None,
            }));
        }
    }
    Ok(None)
}

fn texture_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
) -> Option<HoverCard> {
    let path = input.path.as_ref()?.as_str();
    if !path.to_ascii_lowercase().ends_with(".gfx") {
        return None;
    }
    let hir = input.hir.as_deref()?;
    let property = hir.properties().iter().find(|property| {
        TEXTURE_VALUE_KEYS
            .iter()
            .any(|key| key.eq_ignore_ascii_case(&property.key))
            && property
                .scalar
                .as_ref()
                .is_some_and(|scalar| contains(scalar.range, position))
    })?;
    let value = property.scalar.as_ref()?;
    let resolution = snapshot.resolve_texture_path(&value.value)?;
    Some(HoverCard {
        kind: "texture",
        asset: Some(asset_from(&resolution, None, None)),
        mission: None,
        card_assets: None,
        event: None,
        event_assets: None,
    })
}

fn sprite_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    let semantic = semantic_data_with_cancellation(snapshot, input, cancellation)?;
    // The hovered symbol: a reference (icon/picture values, .gfx uses) or the
    // sprite's own definition name. Kind `sprite` covers all six GUI sprite
    // block kinds; bitmap fonts and pdxmesh objects carry no texture.
    let mut hit: Option<String> = None;
    for reference in &semantic.references {
        if reference.kind.eq_ignore_ascii_case("sprite") && contains(reference.range, position) {
            hit = Some(reference.name.clone());
            break;
        }
    }
    if hit.is_none() {
        for definition in &semantic.definitions {
            if definition.kind.eq_ignore_ascii_case("sprite")
                && definition.document.as_ref().is_some_and(|document| {
                    input
                        .document
                        .as_ref()
                        .is_some_and(|current| current == document)
                })
                && contains(definition.symbol.selection_range, position)
            {
                hit = Some(definition.name.clone());
                break;
            }
        }
    }
    let Some(name) = hit else {
        return Ok(None);
    };
    let Some(asset) = sprite_asset(snapshot, &name, cancellation)? else {
        return Ok(None);
    };
    Ok(Some(HoverCard {
        kind: "sprite",
        asset: Some(asset),
        mission: None,
        card_assets: None,
        event: None,
        event_assets: None,
    }))
}

/// True when `inner` lies fully inside `outer` (both bounds inclusive).
fn within(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}

/// One cardable definition block: the profile symbol declaration that anchored
/// the card, plus the block's range.
struct CardAnchor<'a> {
    /// Symbol kind of the anchored definition (`event`, `mission`, …).
    kind: &'a str,
    /// Nested scalar field supplying the definition name, when the profile
    /// declares one (events name themselves through `id`; missions are their
    /// own key).
    name_field: Option<&'a str>,
    /// Full range of the anchored top-level block.
    block_range: TextRange,
}

/// Returns whether `path`/`key` declare a symbol kind a hover card exists for.
fn cardable_definition<'a>(
    profile: &'a GameProfile,
    path: &str,
    key: &str,
) -> Option<(&'a str, Option<&'a str>)> {
    let rule = profile.definition(path, key)?;
    profile
        .hover_card(&rule.kind)
        .map(|_| (rule.kind.as_str(), rule.name_field.as_deref()))
}

/// Finds the cardable top-level block whose key token contains `position`.
fn card_anchor_at(input: &ParsedInput, position: TextSize) -> Option<CardAnchor<'_>> {
    let hir = input.hir.as_deref()?;
    let path = input.path.as_ref()?;
    let profile = input.profile.as_ref();
    for property in hir
        .properties()
        .iter()
        .filter(|property| property.top_level)
    {
        if !contains(property.key_range, position) {
            continue;
        }
        let (kind, name_field) = cardable_definition(profile, path.as_str(), &property.key)?;
        return Some(CardAnchor {
            kind,
            name_field,
            block_range: property.range,
        });
    }
    None
}

/// Finds the cardable top-level block a `selection` points into: the block
/// whose name-field value contains the selection wins (definition selection),
/// else the first block whose range contains it.
fn card_anchor_for_selection(input: &ParsedInput, selection: TextSize) -> Option<CardAnchor<'_>> {
    let hir = input.hir.as_deref()?;
    let path = input.path.as_ref()?;
    let profile = input.profile.as_ref();
    let properties = hir.properties();
    let mut by_range = None;
    for property in properties.iter().filter(|property| property.top_level) {
        let Some((kind, name_field)) = cardable_definition(profile, path.as_str(), &property.key)
        else {
            continue;
        };
        let name_value_range = name_field.and_then(|field| {
            properties
                .iter()
                .find(|nested| {
                    nested.path.len() == 2
                        && within(property.range, nested.range)
                        && nested.key.eq_ignore_ascii_case(field)
                })
                .and_then(|nested| nested.scalar.as_ref())
                .map(|scalar| scalar.range)
        });
        let anchor = || CardAnchor {
            kind,
            name_field,
            block_range: property.range,
        };
        if name_value_range.is_some_and(|range| contains(range, selection)) {
            return Some(anchor());
        }
        if by_range.is_none() && contains(property.range, selection) {
            by_range = Some(anchor());
        }
    }
    by_range
}

/// The event-window card: a hovered cardable definition's block-name token
/// (see [`card_anchor_at`]) renders that block as the in-game event window.
fn event_card(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    position: TextSize,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if input.format != parser::FileFormat::Script
        || !input
            .profile
            .game_id
            .eq_ignore_ascii_case(game::eu4::GAME_ID)
    {
        return Ok(None);
    }
    cancellation.checkpoint()?;
    let Some(anchor) = card_anchor_at(input, position) else {
        return Ok(None);
    };
    event_card_for_anchor(snapshot, input, anchor, cancellation)
}

/// Renders the event card for a reference's target: the cardable block the
/// selection points into (see [`card_anchor_for_selection`]).
fn event_card_for_reference(
    snapshot: &AnalysisSnapshot,
    target: &ParsedInput,
    selection: TextRange,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if target.format != parser::FileFormat::Script
        || !target
            .profile
            .game_id
            .eq_ignore_ascii_case(game::eu4::GAME_ID)
    {
        return Ok(None);
    }
    let Some(anchor) = card_anchor_for_selection(target, selection.start()) else {
        return Ok(None);
    };
    event_card_for_anchor(snapshot, target, anchor, cancellation)
}

/// Renders one anchored definition as the event window. Field semantics are
/// not hardcoded here: which fields carry localisation keys or sprite names
/// is queried from the semantic rules of the card's declared context, and the
/// fixed chrome plus any sprite-name probes come from the profile's card
/// declaration. The renderer serves the `event` kind; other cardable kinds
/// route to their own renderers.
fn event_card_for_anchor(
    snapshot: &AnalysisSnapshot,
    input: &ParsedInput,
    anchor: CardAnchor<'_>,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCard>, Cancelled> {
    if !anchor.kind.eq_ignore_ascii_case("event") {
        return Ok(None);
    }
    let profile = input.profile.as_ref();
    let Some(spec) = profile.hover_card(anchor.kind) else {
        return Ok(None);
    };
    let Some(hir) = input.hir.as_deref() else {
        return Ok(None);
    };
    let properties = hir.properties();
    // Direct children of the anchored block: paths include each property's
    // own key, so a child of the block has a two-element path.
    let child_scalar = |key: &str| -> Option<String> {
        properties
            .iter()
            .filter(|property| {
                property.path.len() == 2 && within(anchor.block_range, property.range)
            })
            .find(|property| property.key.eq_ignore_ascii_case(key))
            .and_then(|property| property.scalar.as_ref())
            .map(|scalar| scalar.value.clone())
    };
    let Some(id) = anchor.name_field.and_then(child_scalar) else {
        return Ok(None);
    };
    // Only fields the rule set types as localisation keys are read as such:
    // a field whose typing disappears from the rules stops resolving here the
    // same moment validation stops checking it.
    let field_semantics = spec
        .context
        .as_deref()
        .map(|context| crate::semantic::construct_field_semantics(snapshot, context));
    let localisation_value = |field: &str| -> Option<String> {
        field_semantics
            .as_ref()
            .is_some_and(|semantics| {
                semantics
                    .localisation_fields
                    .iter()
                    .any(|typed| typed.eq_ignore_ascii_case(field))
            })
            .then(|| child_scalar(field))
            .flatten()
    };
    // Title and description keys are read verbatim from the fields: the
    // engine looks up whatever scalar the event declares. The near-universal
    // `<id>.t`/`<id>.d` spellings live inside those values as modder
    // convention, so an absent field (or a conditional `desc` block without
    // a single scalar) yields no key rather than a fabricated one.
    let title_key = localisation_value("title");
    let desc_key = localisation_value("desc");
    // Sprite-valued fields resolve the same way; the first typed field backs
    // the window's picture slot.
    let sprite_field = field_semantics
        .as_ref()
        .and_then(|semantics| semantics.sprite_fields.first());
    let picture = sprite_field.and_then(|field| child_scalar(field));
    // Option blocks: `option`-keyed direct children of this block; each
    // one's `name` is the matching three-element-path property inside it.
    let mut options: Vec<(TextRange, Option<String>)> = properties
        .iter()
        .filter(|property| {
            property.path.len() == 2
                && property.key.eq_ignore_ascii_case("option")
                && within(anchor.block_range, property.range)
        })
        .map(|property| {
            let name = properties
                .iter()
                .find(|nested| {
                    nested.path.len() == 3
                        && nested.key.eq_ignore_ascii_case("name")
                        && within(property.range, nested.range)
                })
                .and_then(|nested| nested.scalar.as_ref())
                .map(|scalar| scalar.value.clone());
            (property.range, name)
        })
        .collect();
    options.sort_by_key(|(range, _)| range.start());
    cancellation.checkpoint()?;
    let mut keys: Vec<String> = title_key
        .clone()
        .into_iter()
        .chain(desc_key.clone())
        .chain(options.iter().filter_map(|(_, name)| name.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let lookup_keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let texts = localisation_values_by_key(snapshot, &lookup_keys, cancellation)?;
    let resolved = |key: &str| texts.get(key).cloned();
    let event = HoverCardEvent {
        id: id.clone(),
        picture: picture.clone(),
        title_key: title_key.clone(),
        title: title_key.as_deref().and_then(resolved),
        desc: desc_key.as_deref().and_then(resolved),
        desc_key,
        options: options
            .into_iter()
            .map(|(_, name)| {
                let name_key = name.unwrap_or_default();
                HoverCardEventOption {
                    name: resolved(&name_key),
                    name_key,
                }
            })
            .collect(),
    };
    let picture_asset = match picture.as_deref() {
        Some(name) => sprite_asset(snapshot, name, cancellation)?,
        None => None,
    };
    // When the exact spelling misses, the field's declared probe prefix is
    // tried once (vanilla event pictures name their sprites bare; mods
    // sometimes define them prefixed). A value already carrying the prefix is
    // not probed again — every acceptance path would double-count it.
    let picture_asset = match picture_asset {
        Some(asset) => Some(asset),
        None => match (
            picture.as_deref(),
            sprite_field.and_then(|field| spec.sprite_probes.get(field)),
        ) {
            (Some(name), Some(prefix))
                if !name
                    .to_ascii_uppercase()
                    .starts_with(&prefix.to_ascii_uppercase()) =>
            {
                sprite_asset(snapshot, &format!("{prefix}{name}"), cancellation)?
            }
            _ => None,
        },
    };
    let chrome = |slot: &str| -> Result<Option<HoverCardAsset>, Cancelled> {
        match spec.chrome.get(slot) {
            Some(name) => sprite_asset(snapshot, name, cancellation),
            None => Ok(None),
        }
    };
    let background_top = chrome("background_top")?;
    if picture_asset.is_none() && background_top.is_none() {
        return Ok(None);
    }
    let event_assets = EventCardAssets {
        background_top,
        background_middle: chrome("background_middle")?,
        background_bottom_s: chrome("background_bottom_s")?,
        background_bottom_m: chrome("background_bottom_m")?,
        background_bottom_l: chrome("background_bottom_l")?,
        option_button: chrome("option_button")?,
    };
    Ok(Some(HoverCard {
        kind: "event",
        asset: picture_asset,
        mission: None,
        card_assets: None,
        event: Some(event),
        event_assets: Some(event_assets),
    }))
}

/// Resolves a sprite name to the file serving its texture, the way the
/// engine would: the highest-priority definition whose block declares a
/// `texturefile` wins, and the raw path goes through the texture catalog.
fn sprite_asset(
    snapshot: &AnalysisSnapshot,
    name: &str,
    cancellation: &CancellationToken,
) -> Result<Option<HoverCardAsset>, Cancelled> {
    let candidates = symbol_candidates_for_hover(snapshot, "sprite", name, cancellation)?;
    for candidate in &candidates {
        cancellation.checkpoint()?;
        let Some(source) = definition_source(snapshot, &candidate.location) else {
            continue;
        };
        let Some((texturefile, frames)) = extract_sprite_texture(&source, candidate.location.range)
        else {
            continue;
        };
        if let Some(resolution) = snapshot.resolve_texture_path(&texturefile) {
            return Ok(Some(asset_from(&resolution, Some(name), frames)));
        }
    }
    Ok(None)
}

/// Reads the source behind a symbol location: the open document when the
/// definition lives in one (freshest text), else the indexed disk file.
/// Vanilla `.gfx` files are not guaranteed UTF-8, and only ASCII path and
/// number spellings are consumed, so bytes decode lossily.
fn definition_source(snapshot: &AnalysisSnapshot, location: &Location) -> Option<String> {
    if let Some(document) = location.document.as_ref() {
        return snapshot
            .document(document)
            .map(|document| document.text().to_owned());
    }
    let path = if let Some(file) = location
        .file
        .and_then(|file| snapshot.source_files().get(&file))
    {
        Some(file.physical_path.as_path().to_path_buf())
    } else {
        location.path.as_ref().and_then(|path| {
            snapshot
                .workspace_root()
                .map(|root| root.join(path.as_str()).to_path_buf())
        })
    }?;
    // Archive tiers surface their members through `<zip>!<entry>` virtual paths,
    // which plain metadata and read calls cannot serve; route them through the
    // bounded archive reader instead of the disk fast path.
    if let Some((zip_path, entry)) = vfs::split_archive_path(&path) {
        let bytes = vfs::read_archive_entry_bytes(&zip_path, &entry, MAX_DEFINITION_READ_BYTES)?;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_DEFINITION_READ_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Extracts `texturefile` and `noOfFrames` assignments from one sprite-block
/// byte range of its file source. Both are single-line assignments in every
/// shipped `.gfx`, so the scan is line-based and quote-tolerant.
fn extract_sprite_texture(source: &str, range: TextRange) -> Option<(String, Option<u32>)> {
    let start = usize::try_from(range.start()).ok()?;
    let end = usize::try_from(range.end()).ok()?;
    let block = source.get(start..end)?;
    let texturefile = scan_assignment(block, "texturefile")?;
    let frames = scan_assignment(block, "noofframes")
        .and_then(|value| value.trim_matches('"').parse::<u32>().ok())
        .filter(|frames| *frames > 0);
    Some((texturefile, frames))
}

/// Returns the unquoted value of `key = value` on the first matching line of
/// `block` (key compared ASCII-case-insensitively).
fn scan_assignment(block: &str, key: &str) -> Option<String> {
    block.lines().find_map(|line| {
        strip_key(line.trim_start(), key)
            .map(|value| value.trim().trim_matches('"').trim().to_owned())
    })
}

/// Splits `key = value` after the operator when `line` starts with `key`.
fn strip_key<'line>(line: &'line str, key: &str) -> Option<&'line str> {
    let key_len = key.len().min(line.len());
    if !line.get(..key_len)?.eq_ignore_ascii_case(key) {
        return None;
    }
    let rest = line[key_len..].trim_start();
    rest.strip_prefix('=')
}

fn asset_from(
    resolution: &engine::TextureResolution,
    sprite: Option<&str>,
    frames: Option<u32>,
) -> HoverCardAsset {
    HoverCardAsset {
        sprite: sprite.map(str::to_owned),
        path: resolution.hit.path.as_path().to_string_lossy().into_owned(),
        root_kind: match resolution.hit.root_kind {
            SourceRootKind::Project => "project",
            SourceRootKind::Dependency => "dependency",
            SourceRootKind::Vanilla => "vanilla",
        }
        .to_owned(),
        extension_fallback: resolution.extension_fallback,
        archive_member: resolution.hit.archive_member.clone(),
        frames,
    }
}
