# Canonical formatting and quoted scripts

ParadoxCode uses one non-configurable canonical layout so formatting converges across editors;
standard LSP indentation options are accepted but ignored. Multiline PdxScript quoted strings
whose payload parses as non-empty PdxScript are formatted recursively because EU4 constructs can
carry script in strings; candidates that cannot be proven to be script remain opaque so ordinary
text is not rewritten.
