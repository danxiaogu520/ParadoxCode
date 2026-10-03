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
        records.push(json!({"id":case.id,"assignments":count,"candidates":actual}));
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
