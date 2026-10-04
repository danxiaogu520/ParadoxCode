//! Script quote encoding and snippet escaping are distinct, composable text layers.
/// Escapes literal text in a generated snippet, preserving its exact inserted characters.
pub(crate) fn snippet_literal(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        if matches!(c, '\\' | '$') {
            result.push('\\');
        }
        result.push(c);
    }
    result
}
fn transform(text: &str, quote: bool) -> String {
    let mut result = String::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    let flush = |literal: &mut String, result: &mut String| {
        if quote {
            result.push_str(&snippet_literal(&parser::encode_quoted_script_text(
                literal,
            )));
        } else {
            result.push_str(literal);
        }
        literal.clear();
    };
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|c| matches!(c, '\\' | '$' | '}')) {
            literal.push(chars.next().expect("peeked character"));
        } else if c == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            flush(&mut literal, &mut result);
            if quote {
                result.push('$');
            }
            while chars.peek().is_some_and(char::is_ascii_digit) {
                let c = chars.next().expect("digit");
                if quote {
                    result.push(c);
                }
            }
        } else {
            literal.push(c);
        }
    }
    flush(&mut literal, &mut result);
    result
}
/// Converts a generated snippet to the concrete text inserted by a client, with empty tab stops.
pub fn snippet_plain_text(text: &str) -> String {
    transform(text, false)
}
/// Encodes one script-string layer without turning literal escapes into snippet syntax.
pub(crate) fn quoted_insertion(text: &str, snippet: bool) -> String {
    if snippet {
        transform(text, true)
    } else {
        parser::encode_quoted_script_text(text)
    }
}

/// Materializes only generated numeric tab stops as explicit trial holes.
pub(crate) fn snippet_trial(text: &str, prefix: &str) -> (String, Vec<String>) {
    let mut output = String::new();
    let mut markers = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(|c| matches!(c, '\\' | '$' | '}')) {
            output.push(chars.next().expect("escape"));
        } else if c == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            let mut number = String::new();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                number.push(chars.next().expect("digit"));
            }
            let marker = format!("{prefix}_{number}");
            output.push_str(&marker);
            markers.push(marker);
        } else {
            output.push(c);
        }
    }
    (output, markers)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quote_and_snippet_layers_preserve_literals_and_tabstops() {
        let source = "name = \\\"$VAR$\\\\path\\\"";
        let snippet = format!("{}\nnext = $0", snippet_literal(source));
        assert_eq!(snippet_plain_text(&snippet), format!("{source}\nnext = "));
        let encoded = quoted_insertion(&snippet, true);
        assert_eq!(
            snippet_plain_text(&encoded),
            parser::encode_quoted_script_text(&format!("{source}\nnext = "))
        );
        let twice = quoted_insertion(&encoded, true);
        assert_eq!(
            snippet_plain_text(&twice),
            parser::encode_quoted_script_text(&parser::encode_quoted_script_text(&format!(
                "{source}\nnext = "
            )))
        );
    }
}
