//! Canonical source formatting and mechanical expansion of language defaults.

use serde_json::{Value, json};

use crate::source::RuleFile;

/// Formats one parsed source file without reordering overloads, patterns or enum members.
///
/// Expanded output spells out defaults; compact output omits them. Declaration names and
/// literal values are never treated as default-valued structure members.
pub fn render(source: &RuleFile, expanded: bool) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(source)?;
    for spec in value["files"]
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        if spec["root"].is_object() {
            field(&mut spec["root"], expanded);
        }
        defaults(
            spec,
            expanded,
            &[
                ("ext", Value::Null),
                ("file", Value::Null),
                ("strict", json!(false)),
                ("exclude", json!([])),
                ("parser", json!("script")),
                ("resolution", json!("merge")),
                ("root", Value::Null),
            ],
        );
    }
    for spec in value["schemas"]
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        if spec.get("map").is_some() {
            map(&mut spec["map"], expanded);
        } else if spec.get("list").is_none() {
            fields(&mut spec["fields"], expanded);
            for pattern in spec["patterns"].as_array_mut().into_iter().flatten() {
                field(pattern, expanded);
            }
            for form in spec["forms"].as_array_mut().into_iter().flatten() {
                defaults(
                    form,
                    expanded,
                    &[("fields", json!({})), ("patterns", json!({}))],
                );
            }
            defaults(
                spec,
                expanded,
                &[
                    ("include", json!([])),
                    ("fields", json!({})),
                    ("patterns", json!([])),
                    ("items", Value::Null),
                    ("forms", json!([])),
                    ("open", json!(false)),
                ],
            );
        }
    }
    for spec in value["mixins"]
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        fields(&mut spec["fields"], expanded);
        defaults(spec, expanded, &[("fields", json!({}))]);
    }
    for spec in value["types"]
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        for implementation in spec["impl"]
            .as_object_mut()
            .into_iter()
            .flat_map(|m| m.values_mut())
        {
            for binding in implementation
                .as_object_mut()
                .into_iter()
                .flat_map(|m| m.values_mut())
            {
                if binding.is_object() {
                    defaults(
                        binding,
                        expanded,
                        &[
                            ("loc", Value::Null),
                            ("sprite", Value::Null),
                            ("required", json!(false)),
                        ],
                    );
                }
            }
        }
        defaults(
            spec,
            expanded,
            &[
                ("resolution", json!("independent")),
                ("subtypes", json!({})),
                ("open", json!(false)),
                ("builtin", json!([])),
                ("impl", json!({})),
            ],
        );
    }
    if value["scopes"].is_object() {
        let scopes = &mut value["scopes"];
        for register in scopes["registers"]
            .as_object_mut()
            .into_iter()
            .flat_map(|m| m.values_mut())
        {
            defaults(register, expanded, &[("chain", json!(false))]);
        }
        defaults(
            scopes,
            expanded,
            &[
                ("types", json!([])),
                ("registers", json!({})),
                ("links", json!({})),
                ("compat", json!([])),
            ],
        );
    }
    defaults(
        &mut value,
        expanded,
        &[
            ("files", json!({})),
            ("schemas", json!({})),
            ("mixins", json!({})),
            ("types", json!({})),
            ("traits", json!({})),
            ("enums", json!({})),
            ("scopes", Value::Null),
        ],
    );
    value.sort_all_objects();
    Ok(serde_json::to_string_pretty(&value)? + "\n")
}

fn fields(value: &mut Value, expanded: bool) {
    for overloads in value
        .as_object_mut()
        .into_iter()
        .flat_map(|m| m.values_mut())
    {
        if let Some(overloads) = overloads.as_array_mut() {
            for spec in overloads {
                field(spec, expanded);
            }
        } else {
            field(overloads, expanded);
        }
    }
}

fn field(value: &mut Value, expanded: bool) {
    if value["map"].is_object() {
        map(&mut value["map"], expanded);
    }
    if value["scope"].is_object() {
        defaults(
            &mut value["scope"],
            expanded,
            &[("in", json!([])), ("push", Value::Null), ("set", json!({}))],
        );
    }
    if value["def"].is_object() {
        defaults(
            &mut value["def"],
            expanded,
            &[
                ("name", json!("key")),
                ("strip_prefix", Value::Null),
                ("strip_suffix", Value::Null),
            ],
        );
    }
    if value["control"].is_object() {
        defaults(
            &mut value["control"],
            expanded,
            &[
                ("guard", Value::Null),
                ("chain", json!([])),
                ("op", Value::Null),
                ("on", Value::Null),
                ("selector_schema", Value::Null),
            ],
        );
    }
    defaults(
        value,
        expanded,
        &[
            ("key", Value::Null),
            ("value", Value::Null),
            ("body", Value::Null),
            ("list", Value::Null),
            ("map", Value::Null),
            ("scope", Value::Null),
            ("def", Value::Null),
            ("control", Value::Null),
            ("doc", Value::Null),
            ("severity", json!("error")),
            ("deprecated", json!(false)),
            ("override", json!(false)),
        ],
    );
}

fn map(value: &mut Value, expanded: bool) {
    defaults(
        value,
        expanded,
        &[("value", Value::Null), ("body", Value::Null)],
    );
}

fn defaults(value: &mut Value, expanded: bool, defaults: &[(&str, Value)]) {
    let object = value.as_object_mut().expect("source structure object");
    for (key, default) in defaults {
        if expanded {
            if object.get(*key).is_none_or(Value::is_null) {
                object.insert((*key).to_owned(), default.clone());
            }
        } else if object
            .get(*key)
            .is_some_and(|v| v.is_null() || v == default)
        {
            object.remove(*key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::GameConfig;
    use crate::{GameProfile, lower};

    #[test]
    fn formatting_preserves_ir_and_order_in_both_modes() {
        let source: RuleFile = serde_json::from_value(json!({
            "files": {"test": {"path":"test", "root":"s"}},
            "schemas": {"s": {"fields": {
                "open": {"value":"bool", "card":"0..1", "deprecated":false},
                "items": [{"value":"int", "card":"0..*"}, {"value":"'other'", "card":"0..*"}],
                "decl": {"map":{"key":"def<thing>", "value":"enum<choices>"}, "card":"0..*"}
            }, "patterns":[{"key":"'z'", "value":"int", "card":"0..*"},
                           {"key":"'a'", "value":"int", "card":"0..*"}]}},
            "types": {"thing": {"impl":{"Localised":{"name":{"loc":"$"}}}}},
            "traits": {"Localised":{}}, "enums":{"choices":["z","a"]}
        }))
        .unwrap();
        let compile = |source| {
            lower::lower(
                &[("test.json".into(), source)],
                GameConfig {
                    profile: GameProfile::empty("test"),
                },
            )
            .unwrap()
            .fingerprint()
        };
        let expected = compile(source.clone());
        for expanded in [false, true] {
            let rendered = render(&source, expanded).unwrap();
            let parsed: RuleFile = serde_json::from_str(&rendered).unwrap();
            assert_eq!(compile(parsed.clone()), expected);
            assert_eq!(render(&parsed, expanded).unwrap(), rendered);
            let value: Value = serde_json::from_str(&rendered).unwrap();
            assert!(value["schemas"]["s"]["fields"]["open"].is_object());
            assert_eq!(value["schemas"]["s"]["patterns"][0]["key"], "'z'");
            assert_eq!(value["schemas"]["s"]["fields"]["items"][0]["value"], "int");
            assert_eq!(value["enums"]["choices"], json!(["z", "a"]));
            assert_eq!(value["traits"]["Localised"], json!({}));
            assert_eq!(value["files"]["test"].get("parser").is_some(), expanded);
            assert_eq!(
                value["types"]["thing"]["impl"]["Localised"]["name"]
                    .get("required")
                    .is_some(),
                expanded
            );
        }
    }
}
