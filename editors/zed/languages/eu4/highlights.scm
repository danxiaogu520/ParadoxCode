; Europa Universalis IV syntax-only fallback highlighting.
; Semantic classification (@variables, $parameters$, numeric/boolean scalars, and rule-known
; command keys) is provided by pdx-ls semantic tokens; this query only colours the editor-level
; syntax so files still read correctly before the server reports. Keep it syntax-only: do not
; reintroduce regex-based semantic captures here.

(comment) @comment
(property key: (key) @property)
(operator) @operator
(quoted_string) @string
(escape_sequence) @string.escape
(bare_value) @string
(header_block header: (bare_value) @type)
(parameter_condition name: (bare_scalar) @variable.parameter)
