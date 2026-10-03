//! Exhaustive finite-domain oracle: join full assignments before projecting the focus.
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(untagged)]
enum Part {
    Literal { literal: String },
    Parameter { parameter: String },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Constraint {
    pieces: Vec<Part>,
    allowed: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    domains: BTreeMap<String, Vec<String>>,
    constraints: Vec<Constraint>,
    focus: String,
    expected: BTreeSet<String>,
}

fn enumerate(case: &Case, limit: usize) -> Result<(BTreeSet<String>, usize), String> {
    let variables = case.domains.iter().collect::<Vec<_>>();
    let count = variables.iter().try_fold(1usize, |count, (_, domain)| {
        count
            .checked_mul(domain.len())
            .filter(|count| *count <= limit)
            .ok_or("finite oracle assignment budget exceeded")
    })?;
    if !case.domains.contains_key(&case.focus) {
        return Err("focus is outside finite domain".into());
    }
    for constraint in &case.constraints {
        for part in &constraint.pieces {
            if let Part::Parameter { parameter } = part
                && !case.domains.contains_key(parameter)
            {
                return Err(format!("unknown relation parameter: {parameter}"));
            }
        }
    }
    let mut accepted = BTreeSet::new();
    for ordinal in 0..count {
        let mut remainder = ordinal;
        let mut binding = BTreeMap::new();
        for (name, domain) in &variables {
            binding.insert(name.as_str(), domain[remainder % domain.len()].as_str());
            remainder /= domain.len();
        }
        let valid = case.constraints.iter().all(|constraint| {
            let rendered = constraint
                .pieces
                .iter()
                .map(|part| match part {
                    Part::Literal { literal } => literal.as_str(),
                    Part::Parameter { parameter } => binding[parameter.as_str()],
                })
                .collect::<String>();
            constraint.allowed.contains(&rendered)
        });
        if valid {
            accepted.insert(binding[case.focus.as_str()].to_owned());
        }
    }
    Ok((accepted, count))
}

// This product check does not call the inversion implementation: ordinary declarative
// rules and actual IDE completion must agree with the exhaustive full-assignment oracle.
fn product_candidates(case: &Case) -> Result<BTreeSet<String>, String> {
    fn literal(value: &str) -> String {
        format!(
            "'{}'",
            value
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('{', "\\{")
                .replace('}', "\\}")
        )
    }
    let mut fields = serde_json::Map::new();
    let mut body = String::new();
    for (index, constraint) in case.constraints.iter().enumerate() {
        let key = format!("constraint{index}");
        fields.insert(key.clone(),json!({"value":constraint.allowed.iter().map(|v|literal(v)).collect::<Vec<_>>().join(" | "),"card":"0..*"}));
        let text = constraint
            .pieces
            .iter()
            .map(|part| match part {
                Part::Literal { literal } => literal.clone(),
                Part::Parameter { parameter } => format!("${parameter}$"),
            })
            .collect::<String>();
        body.push_str(&format!("{key} = {text}\n"));
    }
    for (name, domain) in &case.domains {
        let key = format!("domain_{name}");
        fields.insert(key.clone(),json!({"value":domain.iter().map(|v|literal(v)).collect::<Vec<_>>().join(" | "),"card":"0..*"}));
        body.push_str(&format!("{key} = ${name}$\n"));
    }
    let file=serde_json::from_value(json!({
        "traits":{"Template":{}},"types":{"owned_template":{"resolution":"replace","impl":{"Template":{"body":"body"}}}},
        "files":{"definitions":{"path":"templates","ext":"txt","root":"definitions"},"calls":{"path":"events","ext":"txt","root":"calls"}},
        "schemas":{"definitions":{"map":{"key":"def<owned_template>","body":"body"}},
            "calls":{"patterns":[{"key":"ref<owned_template>","body":"arguments","card":"0..*"}]},
            "arguments":{"map":{"key":"scalar","value":"scalar"}},"body":{"fields":fields}}
    })).map_err(|e|e.to_string())?;
    let ir = std::sync::Arc::new(
        rules::lower::lower(
            &[("owned.json".into(), file)],
            rules::ir::GameConfig {
                profile: rules::GameProfile::empty("owned"),
            },
        )
        .map_err(|e| format!("{e:?}"))?,
    );
    let mut host = engine::AnalysisHost::with_ir(
        rules::RuleSet::from_ir_catalog(&ir),
        ir.game.profile.clone(),
        ir,
    );
    host.open_document(
        engine::DocumentId::new("file:///owned/templates/definitions.txt"),
        1,
        format!("owned = {{ {body} }}"),
        None,
    )
    .map_err(|e| format!("{e:?}"))?;
    let call = format!("owned = {{ {} =  }}", case.focus);
    let id = engine::DocumentId::new("file:///owned/events/call.txt");
    host.open_document(id.clone(), 1, call.clone(), None)
        .map_err(|e| format!("{e:?}"))?;
    let position = (call.find("=  }").ok_or("owned cursor")? + 2) as u32;
    let completed = ide::complete(&host.snapshot(), &id, position);
    if !completed.coverage.is_complete() {
        return Err(format!(
            "owned finite completion {} unfinished: {:?}",
            case.id, completed.coverage
        ));
    }
    Ok(completed.items.into_iter().map(|item| item.label).collect())
}

pub fn check() -> Result<Value, String> {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("finite.json")).map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    let mut ids = BTreeSet::new();
    for case in cases {
        if !ids.insert(case.id.clone()) {
            return Err("duplicate finite case".into());
        }
        let (actual, count) = enumerate(&case, 4096)?;
        if actual != case.expected {
            return Err(format!("finite oracle {}: {actual:?}", case.id));
        }
        let product = product_candidates(&case)?;
        if product != actual {
            return Err(format!(
                "finite product {}: expected {actual:?}, got {product:?}",
                case.id
            ));
        }
        records.push(
            json!({"id":case.id,"assignments":count,"candidates":actual,"product_matches":true}),
        );
    }
    Ok(json!({"status":"exhaustive-owned-domain-passed","cases":records}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_relations_require_one_shared_assignment() {
        let result = check().unwrap();
        assert_eq!(result["cases"].as_array().unwrap().len(), 5);
        assert_eq!(result["cases"][2]["candidates"], json!([]));
    }
    #[test]
    fn finite_limits_and_empty_domains_do_not_mean_valid() {
        let case = Case {
            id: "limit".into(),
            domains: BTreeMap::from([("P".into(), vec!["a".into(), "b".into()])]),
            constraints: Vec::new(),
            focus: "P".into(),
            expected: BTreeSet::new(),
        };
        assert!(enumerate(&case, 1).is_err());
        let empty = Case {
            domains: BTreeMap::from([("P".into(), Vec::new())]),
            ..case
        };
        assert_eq!(enumerate(&empty, 10).unwrap(), (BTreeSet::new(), 0));
    }
}
