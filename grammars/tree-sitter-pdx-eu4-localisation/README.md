# tree-sitter-pdx-eu4-localisation

The Tree-sitter grammar for EU4 localisation files. It is a separate frontend
from Script because localisation has a language header, line-oriented
entries, and quoted text with EU4 substitutions.

Run the corpus and parser checks with:

```text
npm install
npm test
```

The fixtures are original examples covering BOMs, language headers, duplicate
keys, escaped text, `$parameter$` substitutions, `[Scope.GetName]` references,
colour tags, comments, and incomplete entries.
