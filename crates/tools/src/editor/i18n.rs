//! Source/bundle coverage, UI translation sinks and webview fallback parity.
use crate::report;

use regex::Regex;

use std::collections::{BTreeMap, BTreeSet};

use std::fs;

use std::path::Path;

const LITERAL: &str = r#"(?:'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|`(?:[^`\\]|\\.)*`)"#;

fn literal(value: &str) -> Result<String, String> {
    let mut result = String::new();

    let mut chars = value[1..value.len() - 1].chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);

            continue;
        }

        let escaped = chars.next().ok_or("truncated string escape")?;

        match escaped {
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            'u' => {
                let mut digits = String::new();

                if chars.peek() == Some(&'{') {
                    chars.next();

                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(ch) => digits.push(ch),
                            None => return Err("unterminated unicode escape".into()),
                        }
                    }
                } else {
                    for _ in 0..4 {
                        digits.push(chars.next().ok_or("short unicode escape")?);
                    }
                }

                let code = u32::from_str_radix(&digits, 16).map_err(|e| e.to_string())?;

                result.push(char::from_u32(code).ok_or("invalid Unicode scalar in literal")?);
            }

            other => result.push(other),
        }
    }

    Ok(result)
}

fn chain(value: &str) -> Result<String, String> {
    Regex::new(&format!("(?s){LITERAL}"))
        .map_err(|e| e.to_string())?
        .find_iter(value)
        .map(|m| literal(m.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.concat())
}

fn data_only(template: &str) -> bool {
    let value = Regex::new(r"\$\{[^}]*\}")
        .unwrap()
        .replace_all(template, "");

    let value = Regex::new(r#"\\[nrt'"`\\]"#)
        .unwrap()
        .replace_all(&value, "");

    !value.chars().any(|c| c.is_ascii_alphabetic())
}

fn tables(source: &str) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let functions = Regex::new(
        r"(?s)export function (\w+)\(\): Record<string, string> \{\s*return \{(.*?)\n    \};",
    )
    .unwrap();

    let entries = Regex::new(&format!(
        r"(?s)(\w+):\s*vscode\.l10n\.t\(\s*(({LITERAL})(?:\s*\+\s*{LITERAL})*)"
    ))
    .map_err(|e| e.to_string())?;

    functions
        .captures_iter(source)
        .map(|function| {
            let values = entries
                .captures_iter(&function[2])
                .map(|entry| Ok((entry[1].into(), chain(&entry[2])?)))
                .collect::<Result<_, String>>()?;

            Ok((function[1].into(), values))
        })
        .collect()
}

fn defaults(source: &str) -> Result<BTreeMap<String, String>, String> {
    let block = Regex::new(r"(?s)const DEFAULT_STRINGS = \{(.*?)\n    \};").unwrap();

    let entry = Regex::new(r"(\w+):\s*'((?:[^'\\]|\\.)*)'").unwrap();

    let matched = block
        .captures(source)
        .ok_or("missing webview DEFAULT_STRINGS")?;

    entry
        .captures_iter(&matched[1])
        .map(|e| Ok((e[1].into(), literal(&format!("'{}'", &e[2]))?)))
        .collect()
}

pub fn check(ext: &Path) -> Result<Vec<String>, String> {
    let en = report::json(&ext.join("l10n/bundle.l10n.json"))?;

    let zh = report::json(&ext.join("l10n/bundle.l10n.zh-cn.json"))?;

    let en = en.as_object().ok_or("English bundle must be an object")?;

    let zh = zh.as_object().ok_or("Chinese bundle must be an object")?;

    let mut errors = Vec::new();

    let mut used = BTreeSet::new();

    let calls = Regex::new(&format!(
        r"(?s)vscode\.l10n\.t\(\s*(({LITERAL})(?:\s*\+\s*{LITERAL})*)"
    ))
    .map_err(|e| e.to_string())?;

    let sinks=[(r"vscode\.window\.show(?:Information|Warning|Error)Message\(\s*(.{0,200}?)[,)]",false),(r"vscode\.window\.setStatusBarMessage\(\s*(.{0,200}?)[,)]",false),(r"createWebviewPanel\(\s*[^,]+,\s*(.{0,120}?)[,)]",false),(r"\b(?:title|placeHolder|openLabel|saveLabel|prompt|label|detail|description)\s*:\s*(.{0,80}?)[,}\n]",false),(r"\.tooltip\s*=\s*(.{0,120}?)[;)\n]",true),(r"throw new Error\(\s*(.{0,200}?)[,)]",false),(r"vscode\.FileSystemError\.[A-Za-z]+\(\s*(.{0,200}?)[,)]",false),(r"report\(\{\s*message\s*:\s*(.{0,80}?)[,}\n]",false)].into_iter().map(|(p,templates)|(Regex::new(&format!("(?s){p}")).unwrap(),templates)).collect::<Vec<_>>();

    let status = Regex::new(r"(?s)\.text\s*=\s*(.{0,120}?)[;\n]").unwrap();

    let tooltip = Regex::new(r"(?s)\.tooltip\s*=\s*`(.{0,120}?)[;]").unwrap();

    let brand = Regex::new(r"^ParadoxCode(?: \$\([-\w~]+\))?$").unwrap();

    for path in report::files(&ext.join("src"))?.into_iter().filter(|p| {
        p.extension().is_some_and(|s| s == "ts") && !p.starts_with(ext.join("src/agent"))
    }) {
        let source = fs::read_to_string(&path).map_err(|e| e.to_string())?;

        let file = path.strip_prefix(ext).unwrap().to_string_lossy();

        for value in calls.captures_iter(&source) {
            used.insert(chain(&value[1])?);
        }

        for (sink, templates) in &sinks {
            for matched in sink.captures_iter(&source) {
                let value = matched[1].trim();

                if value.starts_with(['\'', '"', '`']) && !(*templates && value.starts_with('`')) {
                    errors.push(format!("{file}: untranslated UI literal: {value}"));
                }
            }
        }

        for matched in status.captures_iter(&source) {
            let value = matched[1].trim();

            if value.starts_with(['\'', '"']) && !brand.is_match(&value.replace('\'', "")) {
                errors.push(format!("{file}: non-brand status literal {value}"));
            }

            if value.starts_with('`') && !value.contains("vscode.l10n.t(") && !data_only(value) {
                errors.push(format!("{file}: untranslated status template {value}"));
            }
        }

        for matched in tooltip.captures_iter(&source) {
            if !matched[0].contains("vscode.l10n.t(") && !data_only(&matched[1]) {
                errors.push(format!("{file}: untranslated tooltip template"));
            }
        }
    }

    for key in &used {
        if !en.contains_key(key) {
            errors.push(format!("English bundle missing source string {key:?}"));
        }
    }

    for (key, value) in en {
        if value != key {
            errors.push(format!("English bundle must map {key:?} to itself"));
        }

        if !used.contains(key) {
            errors.push(format!("stale English bundle string {key:?}"));
        }

        if !zh.contains_key(key) {
            errors.push(format!("Chinese bundle missing {key:?}"));
        }
    }

    for key in zh.keys() {
        if !en.contains_key(key) {
            errors.push(format!("Chinese bundle has no English source for {key:?}"));
        }
    }

    let source = fs::read_to_string(ext.join("src/webviewI18n.ts")).map_err(|e| e.to_string())?;

    let tables = tables(&source)?;

    let html_keys =
        Regex::new(r#"data-i18n(?:-title|-placeholder|-aria-label)?="([^"]+)""#).unwrap();

    for (name, media, html) in [
        ("missionPreviewStrings", "renderer.js", "index.html"),
        (
            "missionIconPickerStrings",
            "icon-picker.js",
            "icon-picker.html",
        ),
    ] {
        let table = tables
            .get(name)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| format!("missing nonempty {name}"))?;

        let text = fs::read_to_string(ext.join("media").join(media)).map_err(|e| e.to_string())?;

        let media = defaults(&text)?;

        if table != &media {
            errors.push(format!("webview source/fallback strings differ: {name}"));
        }

        let html = fs::read_to_string(ext.join("media").join(html)).map_err(|e| e.to_string())?;

        for matched in html_keys.captures_iter(&html) {
            if !table.contains_key(&matched[1]) {
                errors.push(format!(
                    "HTML references unknown {name} key {}",
                    &matched[1]
                ));
            }
        }
    }

    Ok(errors)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn literal_chains_and_templates_preserve_source_strings() {
        assert_eq!(chain("'a\\n' + \"b\\u4e2d\"").unwrap(), "a\nb中");

        assert!(data_only("`${path}:${line}`"));

        assert!(!data_only("`File ${path}`"));
    }
}
