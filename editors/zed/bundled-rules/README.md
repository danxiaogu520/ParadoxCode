# Bundled rules

`eu4.pdxrules` is the read-only rules artifact shipped with the extension. It must be regenerated
from the project-level `rules/eu4.pdxrules` artifact only when the canonical `rule_hash` changes;
the Phase 6A release checks compare both copies and the committed manifest.
