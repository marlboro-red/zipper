# M33. Clean rewrites of the copied checks

The few checks that the licence audit found copied from 7-Zip, inside readers that are otherwise Zipper's own, are written again in a clean room.

The audit of 12 November 2026 found these readers independent apart from small pieces. WIM has four validators, for the security data, stream entries, folder entries and chunk table. CAB has its item sort and equality rule, and Quantum its chunk driver's order of checks. MSLZ has its decode step, and LZH one window-size rule.

Scope:

- Rewrite each piece from the format's specification or observed behaviour, by an author who has not read 7-Zip's source or the audit.
- Keep each reader's tests, corpus and hostile checks as the acceptance tests.

Exit checks:

1. Each rewritten piece passes the reader's corpus, hostile and formal checks.
2. The second audit of M32 finds none of these pieces derived.
