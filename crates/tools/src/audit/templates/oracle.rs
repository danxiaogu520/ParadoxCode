//! Bounded, per-binding-frame text reference. It never calls HIR replay or parameter-site queries.
//! The profile is a project convention, not a claim about unobserved EU4 engine behavior.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    /// None is a present editing Hole; an absent entry is Missing. Empty text is present.
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub source: String,
    pub bindings: Vec<Binding>,
    pub expected: String,
    pub status: String,
    pub policy: String,
    pub engine_evidence: String,
    pub target_game_version: String,
    pub evidence_source: String,
    pub max_bytes: Option<usize>,
    pub max_steps: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Piece {
    pub output_range: [usize; 2],
    pub source_range: [usize; 2],
    /// None means literal source; otherwise identifies the selected binding.
    pub binding: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Expansion {
    pub text: String,
    pub status: &'static str,
    pub missing: BTreeSet<String>,
    pub holes: BTreeSet<String>,
    pub pieces: Vec<Piece>,
    pub steps: usize,
    pub frontier: Option<usize>,
}

impl Expansion {
    fn append(
        &mut self,
        text: &str,
        range: [usize; 2],
        binding: Option<String>,
        limit: usize,
    ) -> bool {
        if text.len() > limit.saturating_sub(self.text.len()) {
            self.status = "limited";
            self.frontier = Some(range[0]);
            return false;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if let Some(previous) = self.pieces.last_mut()
            && binding.is_none()
            && previous.binding.is_none()
            && previous.source_range[1] == range[0]
        {
            previous.source_range[1] = range[1];
            previous.output_range[1] = self.text.len();
        } else if !text.is_empty() {
            self.pieces.push(Piece {
                output_range: [start, self.text.len()],
                source_range: range,
                binding,
            });
        }
        true
    }
}

/// One-pass, case-insensitive bindings with last occurrence winning. Inserted text is not rescanned.
/// Guards are meta syntax outside comments/quotes; scalar substitutions are recognized in quotes.
pub fn expand(source: &str, bindings: &[Binding], max_bytes: usize, max_steps: usize) -> Expansion {
    let environment = bindings
        .iter()
        .map(|binding| (binding.name.to_ascii_lowercase(), &binding.value))
        .collect::<BTreeMap<_, _>>();
    let mut result = Expansion {
        text: String::new(),
        status: "complete",
        missing: BTreeSet::new(),
        holes: BTreeSet::new(),
        pieces: Vec::new(),
        steps: 0,
        frontier: None,
    };
    let mut guards = Vec::new();
    let mut active = true;
    let mut quoted = false;
    let mut comment = false;
    let mut escaped = false;
    let mut offset = 0;
    while offset < source.len() {
        if result.steps >= max_steps {
            result.status = "limited";
            result.frontier = Some(offset);
            return result;
        }
        result.steps += 1;
        let remaining = &source[offset..];
        // Bound delimiter lookahead too: a single huge marker must not bypass the scan budget.
        let search_bytes = (max_steps - result.steps + 1).min(remaining.len());
        let searchable = &remaining.as_bytes()[..search_bytes];
        if !comment && !quoted && remaining.starts_with("[[") {
            let Some(end) = searchable.iter().position(|byte| *byte == b']') else {
                result.status = if search_bytes < remaining.len() {
                    "limited"
                } else {
                    "source_error"
                };
                result.frontier = Some(offset);
                return result;
            };
            result.steps += end;
            let condition = &remaining[2..end];
            let (negated, name) = condition
                .strip_prefix('!')
                .map_or((false, condition), |name| (true, name));
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                result.status = "source_error";
                result.frontier = Some(offset);
                return result;
            }
            guards.push(active);
            active &= environment.contains_key(&name.to_ascii_lowercase()) != negated;
            offset += end + 1;
            continue;
        }
        if !comment && !quoted && remaining.starts_with(']') && !guards.is_empty() {
            active = guards.pop().unwrap_or(false);
            offset += 1;
            continue;
        }
        if !comment && remaining.starts_with('$') {
            let Some(end) = searchable
                .get(1..)
                .and_then(|bytes| bytes.iter().position(|byte| *byte == b'$'))
                .map(|end| end + 1)
            else {
                result.status = if search_bytes < remaining.len() {
                    "limited"
                } else {
                    "source_error"
                };
                result.frontier = Some(offset);
                return result;
            };
            result.steps += end;
            let name = remaining[1..end].to_ascii_lowercase();
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                result.status = "source_error";
                result.frontier = Some(offset);
                return result;
            }
            if active {
                let value = match environment.get(&name) {
                    Some(Some(value)) => value.as_str(),
                    Some(None) => {
                        result.holes.insert(name.clone());
                        &remaining[..=end]
                    }
                    None => {
                        result.missing.insert(name.clone());
                        &remaining[..=end]
                    }
                };
                if !result.append(value, [offset, offset + end + 1], Some(name), max_bytes) {
                    return result;
                }
            }
            offset += end + 1;
            continue;
        }
        // Scan source lexical state only: this profile deliberately never rescans inserted text.
        let Some(ch) = remaining.chars().next() else {
            break;
        };
        if comment {
            comment = ch != '\n';
        } else if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else if ch == '#' {
            comment = true;
        } else if ch == '"' {
            quoted = true;
        }
        let next = offset + ch.len_utf8();
        if active && !result.append(&source[offset..next], [offset, next], None, max_bytes) {
            return result;
        }
        offset = next;
    }
    result.status = if !guards.is_empty() {
        result.frontier = Some(offset);
        "source_error"
    } else if !result.missing.is_empty() {
        "missing"
    } else if !result.holes.is_empty() {
        "hole"
    } else {
        "complete"
    };
    result
}

pub fn cases() -> Result<Vec<Case>, String> {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("cases.json")).map_err(|e| e.to_string())?;
    let mut ids = BTreeSet::new();
    for case in &cases {
        if !ids.insert(&case.id)
            || case.engine_evidence != "unverified"
            || case.evidence_source.is_empty()
            || case.target_game_version.is_empty()
        {
            return Err(format!("invalid fixture evidence: {}", case.id));
        }
    }
    Ok(cases)
}

pub fn check() -> Result<serde_json::Value, String> {
    let mut records = Vec::new();
    for case in cases()? {
        let expanded = expand(
            &case.source,
            &case.bindings,
            case.max_bytes.unwrap_or(64 * 1024),
            case.max_steps.unwrap_or(64 * 1024),
        );
        if expanded.text != case.expected || expanded.status != case.status {
            return Err(format!("oracle fixture {}: {expanded:?}", case.id));
        }
        // Every output byte has provenance, and all boundaries are valid UTF-8 boundaries.
        let mut covered = 0;
        for piece in &expanded.pieces {
            if piece.output_range[0] != covered
                || !expanded.text.is_char_boundary(piece.output_range[1])
                || !case.source.is_char_boundary(piece.source_range[0])
                || !case.source.is_char_boundary(piece.source_range[1])
            {
                return Err(format!("invalid source projection: {}", case.id));
            }
            covered = piece.output_range[1];
        }
        if covered != expanded.text.len() {
            return Err(format!("unmapped output: {}", case.id));
        }
        records.push(serde_json::json!({"id":case.id,"policy":case.policy,"engine_evidence":case.engine_evidence,"target_game_version":case.target_game_version,"evidence_source":case.evidence_source,"expansion":expanded}));
    }
    Ok(
        serde_json::json!({"profile":"one-pass-per-binding-frame-v1","status":"owned-reference-passed","scope":"per-frame substitution; no engine execution, quoted decoding or recursive call expansion","cases":records}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_matrix_and_every_output_byte_have_independent_expectations() {
        let report = check().unwrap();
        assert_eq!(report["cases"].as_array().unwrap().len(), 33);
    }

    #[test]
    fn increasing_budget_restores_complete_unicode_expansion() {
        let bindings = [Binding {
            name: "P".into(),
            value: Some("界😀".into()),
        }];
        let limited = expand("x=$P$", &bindings, 3, 100);
        assert_eq!(limited.status, "limited");
        assert_eq!(limited.text, "x=");
        let complete = expand("x=$P$", &bindings, 100, 100);
        assert_eq!(complete.text, "x=界😀");
        assert_eq!(complete.status, "complete");
    }

    #[test]
    fn large_insertions_and_nested_guards_stay_bounded() {
        let bindings = [Binding {
            name: "P".into(),
            value: Some("a".repeat(1_000_000)),
        }];
        let limited = expand("$P$", &bindings, 8, 100);
        assert_eq!(limited.status, "limited");
        assert!(limited.text.is_empty());
        let text = format!("{}{}", "[[P]".repeat(10_000), "]".repeat(10_000));
        assert_eq!(expand(&text, &bindings, 8, 32).status, "limited");
        let marker = format!("${}$", "a".repeat(1_000_000));
        assert_eq!(expand(&marker, &bindings, 8, 32).status, "limited");
    }
}
