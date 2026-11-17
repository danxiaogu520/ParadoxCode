//! Exact CST rebasing when a single complete quoted token changes.
use super::*;

/// A parsed edit whose unchanged CST structure was proved against one tree.
/// The previous tree is borrowed so the proof cannot be applied to another
/// document or to a later revision with the same text.
pub struct QuotedScriptEdit<'a> {
    previous: &'a ParsedFile,
    parsed: Arc<ParsedFile>,
}
impl QuotedScriptEdit<'_> {
    /// Whether this proof was constructed from this exact immutable tree.
    pub fn is_from(&self, previous: &ParsedFile) -> bool {
        std::ptr::eq(self.previous, previous)
    }
    /// The full parsed document with updated source and ranges.
    pub fn parsed(&self) -> Arc<ParsedFile> {
        self.parsed.clone()
    }
}

/// Reuses a valid full Script tree when the changed bytes stay inside one
/// quoted token. A small token parse proves the new closing/escape boundary;
/// all arena, token and ancestor ranges are checked while rebasing. The result
/// is equivalent to `parse(Script, source)` with its fresh revision of zero.
/// Any recovery syntax or edit crossing token boundaries requests a full parse.
#[must_use]
pub fn parse_script_after_quoted_edit(
    previous: &ParsedFile,
    source: Arc<str>,
) -> Option<ParsedFile> {
    prepare_script_quoted_edit(previous, source).map(|edit| Arc::unwrap_or_clone(edit.parsed))
}

/// Constructs a reusable proof of unchanged Script structure for HIR rebasing.
#[must_use]
pub fn prepare_script_quoted_edit(
    previous: &ParsedFile,
    source: Arc<str>,
) -> Option<QuotedScriptEdit<'_>> {
    if previous.format != FileFormat::Script || !previous.errors.is_empty() {
        return None;
    }
    // Tokens omit whitespace. Reject bounded prefixes by proving that every
    // omitted byte is whitespace rather than unparsed source.
    let mut end = 0;
    for token in &previous.tokens {
        if !previous
            .source
            .get(end as usize..token.range().start() as usize)?
            .bytes()
            .all(|b| b.is_ascii_whitespace())
        {
            return None;
        }
        end = token.range().end();
    }
    if !previous
        .source
        .get(end as usize..)?
        .bytes()
        .all(|b| b.is_ascii_whitespace())
    {
        return None;
    }
    if previous.source == source {
        let mut parsed = previous.clone();
        parsed.source = source;
        parsed.revision = 0;
        return Some(QuotedScriptEdit {
            previous,
            parsed: Arc::new(parsed),
        });
    }
    let old = previous.source.as_ref();
    let prefix = old
        .bytes()
        .zip(source.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(source.as_bytes()[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let token = previous.tokens.iter().find(|token| {
        token.kind() == TokenKind::Quoted
            && token.range().start() as usize <= prefix
            && old.len() - suffix <= token.range().end() as usize
    })?;
    let delta = source.len() as i64 - old.len() as i64;
    let point = |p: u32| {
        if p <= token.range().start() {
            Some(p)
        } else if p >= token.range().end() {
            u32::try_from(i64::from(p) + delta).ok()
        } else {
            None
        }
    };
    let range = |r: TextRange| TextRange::new(point(r.start())?, point(r.end())?);
    let edited = range(token.range())?;
    let text = source.get(edited.start() as usize..edited.end() as usize)?;
    // A standalone quoted token is not a valid top-level Script statement.
    // Assign it to a synthetic key to validate its complete lexical boundary.
    let proof_source = format!("x={text}");
    let proof = parse(FileFormat::Script, &proof_source);
    if !proof.errors.is_empty()
        || proof.tokens.len() != 3
        || proof.tokens[2].kind() != TokenKind::Quoted
        || proof.tokens[2].range() != TextRange::new(2, u32::try_from(proof_source.len()).ok()?)?
    {
        return None;
    }
    let tree = previous.tree.try_map_ranges(range)?;
    let tokens = previous
        .tokens
        .iter()
        .map(|token| Some(SyntaxToken::new(token.kind(), range(token.range())?)))
        .collect::<Option<Vec<_>>>()?;
    Some(QuotedScriptEdit {
        previous,
        parsed: Arc::new(ParsedFile {
            format: FileFormat::Script,
            source,
            tree,
            tokens,
            errors: Vec::new(),
            revision: 0,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_rebasing_matches_fresh_trees_tokens_and_ancestors() {
        let fixtures = [
            "\u{feff}# 中文😀\r\nroot={value=\"PAYLOAD\" next={a=1}} tail=yes",
            "root={\"PAYLOAD\"=yes} tail={x=2}",
            "[[ARG] root={value=\"PAYLOAD\"}]",
            "root=\"PAYLOAD\" { nested=yes }",
        ];
        let alphabet = [
            'x', '"', '\\', '\n', '中', '😀', '$', '{', '}', ']', '=', '#',
        ];
        let mut seed = 991u64;
        for fixture in fixtures {
            let original = fixture.replace("PAYLOAD", "original");
            let previous = parse(FileFormat::Script, &original);
            assert!(previous.errors().is_empty(), "{fixture}");
            for n in 0..256 {
                let mut payload = String::new();
                for _ in 0..n % 32 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    payload.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
                }
                let source = fixture.replace("PAYLOAD", &encode_quoted_script_text(&payload));
                let incremental =
                    parse_script_after_quoted_edit(&previous, Arc::from(source.as_str()))
                        .unwrap_or_else(|| panic!("single quoted token: {source:?}"));
                assert_eq!(incremental, parse(FileFormat::Script, &source), "{source}");
                assert!(incremental.is_valid_for(FileFormat::Script, &source));
            }
            assert_eq!(previous, parse(FileFormat::Script, &original));
        }
        let large =
            "before=1 ".repeat(5000) + "value=\"payload\" " + &"after={x=yes} ".repeat(5000);
        let previous = parse(FileFormat::Script, &large);
        let source = large.replace("payload", "new 中😀");
        assert_eq!(
            parse_script_after_quoted_edit(&previous, Arc::from(source.as_str())).unwrap(),
            parse(FileFormat::Script, &source)
        );
    }
    #[test]
    fn recovery_cross_token_and_early_quote_edits_fall_back() {
        let source = "a=\"one\" b=\"two\" # comment";
        let previous = parse(FileFormat::Script, source);
        for source in [
            "a=\"changed\" b=\"also_changed\" # comment",
            "a=\"one\" b=\"two\" # edit",
            "a=\"one\" b=\"too\"early\" # comment",
            "a=\"one\" b=\"unterminated # comment",
        ] {
            assert!(
                parse_script_after_quoted_edit(&previous, Arc::from(source)).is_none(),
                "{source}"
            );
        }
        let bad = parse(FileFormat::Script, "root={value=\"old\"");
        assert!(parse_script_after_quoted_edit(&bad, Arc::from("root={value=\"new\"")).is_none());
        let source = "value=\"old\" second=yes third={x=1}";
        let prefix = parse_script_prefix_bounded::<std::convert::Infallible>(
            source,
            ScriptParseBudget {
                nodes: 10,
                ..Default::default()
            },
            &mut || Ok(()),
        )
        .unwrap();
        assert!(prefix.frontier.is_some());
        assert!(
            parse_script_after_quoted_edit(
                &prefix.parsed.unwrap(),
                Arc::from(source.replace("old", "new"))
            )
            .is_none()
        );
    }
}
