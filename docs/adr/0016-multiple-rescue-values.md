# ADR-0016: Preserve every conflicting late value

- **Status:** Accepted; amends ADR-0009's rescue ref naming
- **Date:** 2026-10-23

## Context

ADR-0009 assigns one rescue ref to each original ref during a compaction. During
periodic reconciliation, an old generation can receive more than one conflicting
value for the same ref before it is sealed. The first absent-ref CAS creates the
rescue; the next value cannot replace it without losing the first. Retrying that
same absent-ref CAS forever would also prevent reclamation.

Embedding the original name directly adds another problem: two valid refs such
as `refs/heads/a` and `refs/heads/a/b` can exist at different times and conflict
with rescue paths, and long names can exceed the backend's ref-name limit after
adding the prefix.

## Decision

Use the deterministic name:

`refs/cambium-rescue/<compaction id>/<sha256 of original UTF-8 ref name>/<object id>`

The digest and object ID use lowercase hexadecimal. Every distinct conflicting
value gets a separate ref. Creating it still uses expectation `absent`; if it
already exists, its object ID must match. Never overwrite a rescue. Repeated
reconciliation of the same value is idempotent. Ref names are bounded in length
and cannot have directory/file conflicts. This applies to tags and other object
kinds as well as commits. A conflicting deletion has no object to rescue.

The compaction's metadata records the original name, observed active value,
rescued value, and rescue ref. Status and operator output show this mapping.
The rescue ref itself remains a pointer to Git data, not a lifecycle record.
Clients may fetch rescue refs but cannot write them, as before.

## Consequences

Every value observed as a conflict survives later observations of the same ref.
Operators resolve potentially several rescue refs for one original name. Tests
must cover two conflicting late values, repeated reconciliation, tags, and a
final sealed snapshot. Other ADR-0009 and ADR-0013 rules remain unchanged.
