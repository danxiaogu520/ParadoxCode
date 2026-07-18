; PdxScript syntax-only highlighting. EU4 command/scope semantics stay in the server.

(comment) @comment
(property key: (key) @property)
(operator) @operator
(quoted_string) @string
(escape_sequence) @string.escape
(bare_value) @string
(header_block header: (bare_value) @type)
(parameter_condition name: (bare_scalar) @variable.parameter)

; Preserve useful visual distinctions for syntax-level special scalars without
; embedding an EU4 command or context name table in the extension.
(bare_scalar) @variable.special
  (#match? @variable.special "^@")
(bare_scalar) @variable.parameter
  (#match? @variable.parameter "^\\$[^$]+\\$$")
