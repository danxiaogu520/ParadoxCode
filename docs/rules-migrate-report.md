# rules-v2 migration report

Historical conversion snapshot retained when `rules-migrate` was retired on 2026-10-02 (source format 13).

The former converter generated this report from the legacy tree; repeated runs over unchanged input were byte-identical. Its implementation and dedicated tests have been removed. The maintained `rules/eu4` sources have subsequent refinements, so this report describes conversion coverage rather than their current contents. The counters and manual checklist below are preserved as recorded.

## Coverage

| counter | rows |
|---|---:|
| call-position ref patterns | 20 |
| cross-directory ref patterns | 5 |
| duplicate patterns merged | 10 |
| file categories with extension lists | 4 |
| magic key segments | 69 |
| magic keys converted to def maps | 13 |
| magic keys kept as ref patterns | 25 |
| parameter-key rows folded into Callable dynamic keys | 6 |
| positions correlated loosely | 1 |
| rows deduplicated | 1227 |
| rows folded as register shifts | 10 |
| rows folded into on_action_body<S> | 2 |
| rows folded into scopes.links | 52 |
| rows inspected for fields | 7079 |
| rows into fields | 7011 |
| rows into items | 149 |
| rows into on_action fold | 8 |
| rows total | 8463 |
| starts_with on-actions folded into one pattern | 1 |
| typed-prefix operand filters into enums | 3 |

## Manual checklist

Empty: every conversion decision is either mechanical or an entry in the coverage table above.
