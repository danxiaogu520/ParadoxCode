//! Explicit-stack concrete call reference. Ordinary CST is read only after text substitution.
//! This deliberately does not use Template lowering, HIR replay, scope contracts or IDE completion.
use super::oracle::{self, Binding};
use parser::{CstKind, FileFormat, ParsedFile};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone)]
enum Task {
    Enter {
        name: String,
        bindings: Vec<Binding>,
        ancestors: Vec<String>,
        caller: Option<(usize, [usize; 2])>,
    },
    Emit {
        text: String,
        frame: usize,
        range: [usize; 2],
    },
}

struct Call {
    range: [usize; 2],
    name: String,
    bindings: Vec<Binding>,
}

fn scalar(parsed: &ParsedFile, node: parser::CstNode<'_>) -> Result<String, String> {
    let raw = parsed
        .text(node.range())
        .ok_or("reference range outside source")?;
    match node.kind() {
        CstKind::BareValue | CstKind::Key => Ok(raw.to_owned()),
        CstKind::QuotedString => parser::parse_quoted_script(raw)
            .filter(|quoted|quoted.is_closed())
            .map(|quoted|quoted.parsed().source().to_owned())
            .ok_or_else(||"unclosed call argument".into()),
        _=>Err("concrete call reference supports scalar arguments; block argument semantics remain unverified".into()),
    }
}

fn call_sites(
    parsed: &ParsedFile,
    definitions: &BTreeMap<String, String>,
) -> Result<Vec<Call>, String> {
    let mut pending = vec![parsed.root()];
    let mut calls = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == CstKind::Property {
            let key = node
                .children()
                .find(|child| child.kind() == CstKind::Key)
                .ok_or("reference property has no key")?;
            let name = scalar(parsed, key)?.to_ascii_lowercase();
            if definitions.contains_key(&name) {
                let operator = node
                    .children()
                    .find(|child| child.kind() == CstKind::Operator)
                    .and_then(|node| parsed.text(node.range()));
                if operator != Some("=") {
                    return Err("concrete reference call must use assignment".into());
                }
                let value = node
                    .children()
                    .find(|child| child.kind() == CstKind::Value)
                    .and_then(|value| value.children().next())
                    .ok_or("reference call has no value")?;
                if value.kind() != CstKind::Block {
                    return Err("concrete call reference requires an argument block".into());
                }
                let mut bindings = Vec::new();
                for argument in value
                    .children()
                    .filter(|child| child.kind() == CstKind::Property)
                {
                    let key = argument
                        .children()
                        .find(|child| child.kind() == CstKind::Key)
                        .ok_or("argument has no key")?;
                    let value = argument
                        .children()
                        .find(|child| child.kind() == CstKind::Value)
                        .and_then(|value| value.children().next())
                        .ok_or("argument has no value")?;
                    bindings.push(Binding {
                        name: scalar(parsed, key)?,
                        value: Some(scalar(parsed, value)?),
                    });
                }
                calls.push(Call {
                    range: [node.range().start() as usize, node.range().end() as usize],
                    name,
                    bindings,
                });
                continue; // Arguments are inputs, not independently executed script.
            }
        }
        pending.extend(node.children());
    }
    calls.sort_by_key(|call| call.range[0]);
    Ok(calls)
}

/// Expands finite, concrete owned call registries. Unresolved frames remain explicitly incomplete.
fn expand(
    definitions: &BTreeMap<String, String>,
    entry: &str,
    bindings: Vec<Binding>,
    budget: usize,
) -> Result<Value, String> {
    let mut work = vec![Task::Enter {
        name: entry.into(),
        bindings,
        ancestors: Vec::new(),
        caller: None,
    }];
    let mut frames = Vec::new();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut status = "complete";
    let mut remaining_steps = 64 * 1024;
    while let Some(task) = work.pop() {
        match task {
            Task::Emit {
                text: literal,
                frame,
                range,
            } => {
                if literal.len() > 65536usize.saturating_sub(text.len()) {
                    status = "limited";
                    break;
                }
                let start = text.len();
                text.push_str(&literal);
                pieces.push(json!({"output_range":[start,text.len()],"frame":frame,"expanded_frame_range":range}));
            }
            Task::Enter {
                name,
                bindings,
                mut ancestors,
                caller,
            } => {
                if frames.len() >= budget {
                    status = "limited";
                    break;
                }
                let source = definitions
                    .get(&name)
                    .ok_or_else(|| format!("unknown owned reference definition: {name}"))?;
                let canonical = bindings
                    .iter()
                    .map(|b| (b.name.to_ascii_lowercase(), b.value.clone()))
                    .collect::<BTreeMap<_, _>>();
                let state = format!(
                    "{name}:{}",
                    serde_json::to_string(&canonical).map_err(|e| e.to_string())?
                );
                if ancestors.contains(&state) {
                    status = "repeated-concrete-state";
                    break;
                }
                ancestors.push(state);
                let expanded = oracle::expand(source, &bindings, 65536, remaining_steps);
                remaining_steps = remaining_steps.saturating_sub(expanded.steps);
                let frame = frames.len();
                frames.push(json!({"definition":name,"bindings":canonical,"caller_frame_and_expanded_range":caller,"expansion":expanded}));
                if expanded.status != "complete" {
                    status = expanded.status;
                    break;
                }
                let parsed = parser::parse(FileFormat::Script, &expanded.text);
                if !parsed.errors().is_empty() {
                    status = "expanded-script-error";
                    break;
                }
                let calls = call_sites(&parsed, definitions)?;
                let mut tasks = Vec::new();
                let mut start = 0;
                for Call {
                    range: [from, to],
                    name,
                    bindings,
                } in calls
                {
                    tasks.push(Task::Emit {
                        text: expanded.text[start..from].to_owned(),
                        frame,
                        range: [start, from],
                    });
                    tasks.push(Task::Enter {
                        name,
                        bindings,
                        ancestors: ancestors.clone(),
                        caller: Some((frame, [from, to])),
                    });
                    start = to;
                }
                tasks.push(Task::Emit {
                    text: expanded.text[start..].to_owned(),
                    frame,
                    range: [start, expanded.text.len()],
                });
                work.extend(tasks.into_iter().rev());
            }
        }
    }
    Ok(
        json!({"status":status,"text":text,"frames":frames,"output_pieces":pieces,"scope":"concrete scalar argument blocks under one-pass project reference; shared parser and quoted codec, independent substitution/call stack"}),
    )
}

pub fn check() -> Result<Value, String> {
    let mut definitions = BTreeMap::new();
    for depth in 0..40 {
        definitions.insert(
            format!("chain{depth}"),
            format!("chain{} = {{ N = $N$ }}", depth + 1),
        );
    }
    definitions.insert("chain40".into(), "add_prestige = $N$".into());
    let binding = || {
        vec![Binding {
            name: "N".into(),
            value: Some("wrong".into()),
        }]
    };
    let long = expand(&definitions, "chain0", binding(), 64)?;
    if long["status"] != "complete" || long["text"] != "add_prestige = wrong" {
        return Err("long chain reference failed".into());
    }
    // Check the materialized terminal under ordinary first-party rules, without macro replay.
    let ir = game::eu4::first_party_ir().map_err(|e| e.to_string())?;
    let host = engine::AnalysisHost::with_ir(
        rules::RuleSet::from_ir_catalog(&ir),
        ir.game.profile.clone(),
        ir,
    );
    let path = text::LogicalPath::parse("events/reference.txt").map_err(|e| e.to_string())?;
    let static_source = format!(
        "country_event = {{ id = stage0.reference immediate = {{ {} }} option = {{ name = stage0_option }} }}",
        long["text"].as_str().ok_or("reference text missing")?
    );
    let rejected = ide::text_diagnostics_with_cancellation(
        &host.snapshot(),
        &path,
        &static_source,
        &ide::CancellationToken::new(),
    )
    .map_err(|_| "ordinary reference check cancelled".to_owned())?
    .iter()
    .any(|d| d.code == ide::DiagnosticCode::InvalidValue && d.message.contains("wrong"));
    if !rejected {
        return Err(
            "ordinary static checking did not reject materialized nonnumeric terminal".into(),
        );
    }
    let limited = expand(&definitions, "chain0", binding(), 8)?;
    if limited["status"] != "limited" {
        return Err("limited chain reference falsely completed".into());
    }
    definitions.insert("finite".into(), "[[P] finite = { } ]".into());
    let finite = expand(
        &definitions,
        "finite",
        vec![Binding {
            name: "P".into(),
            value: Some(String::new()),
        }],
        64,
    )?;
    if finite["status"] != "complete" {
        return Err("finite same-name reference falsely cyclic".into());
    }
    definitions.insert("cycle".into(), "cycle = { }".into());
    let cycle = expand(&definitions, "cycle", Vec::new(), 64)?;
    if cycle["status"] != "repeated-concrete-state" {
        return Err("repeated state reference failed".into());
    }
    definitions.insert("inner".into(), "[[X] log = $X$ ]".into());
    definitions.insert("outer".into(), "inner = { X = $P$ }".into());
    let missing = expand(&definitions, "outer", Vec::new(), 64)?;
    if missing["status"] != "missing" {
        return Err("callee guard hid outer missing read".into());
    }
    let quoted = expand(
        &definitions,
        "outer",
        vec![Binding {
            name: "P".into(),
            value: Some("\"owned\"".into()),
        }],
        64,
    )?;
    if quoted["status"] != "complete" || quoted["text"] != " log = owned " {
        return Err("quoted forwarding reference failed".into());
    }
    Ok(
        json!({"status":"owned-concrete-call-reference-passed","terminal_static_rejected":rejected,"cases":{"long_chain":long,"limited_chain":limited,"finite_same_name":finite,"repeated_state":cycle,"outer_missing":missing,"quoted_forward":quoted},"engine_termination_evidence":"unverified"}),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn call_reference_reaches_invalid_terminal_and_preserves_limits_and_guarded_termination() {
        let report = super::check().unwrap();
        assert_eq!(
            report["cases"]["long_chain"]["frames"]
                .as_array()
                .unwrap()
                .len(),
            41
        );
    }
}
