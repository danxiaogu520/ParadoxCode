; Europa Universalis IV syntax-only fallback highlighting.
; Semantic classification (rule-known command keys, control-flow keys, @variables,
; $parameters$, numeric/boolean scalars) is provided by pdx-ls semantic tokens; this query only
; colours the editor-level syntax so files still read correctly before the server reports.
;
; Alignment contract: every capture below must produce the same visual category as the VS Code
; TextMate grammar (editors/vscode/syntaxes/eu4.tmLanguage.json) and the pdx-ls semantic token
; mapping (editors/vscode/package.json semanticTokenScopes). The only intended differences are
; rule-known keys (upgraded from @property to @function, mapped to support.function because many
; themes italicize entity.name.function and ~80% of keys are function-classified) and
; control-flow keys (upgraded to @keyword); both are provided by the semantic layer only. Keep
; this query syntax-only: no regex or literal-list captures that reclassify scalars by spelling.

(comment) @comment
(property key: (key) @property)
(operator) @operator
(quoted_string) @string
(escape_sequence) @string.escape

; Header blocks such as `rgb { ... }`.
(header_block header: (bare_value) @type)

; Parameter-block condition names such as `country` in `[[!country] ... ]`.
(parameter_condition name: (bare_scalar) @variable.parameter)

; Remaining bare scalars stay plain string values, matching the TextMate string.unquoted
; fallback and the semantic layer's String classification for non-numeric, non-boolean values.
(bare_value) @string
