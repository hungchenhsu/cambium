# Architecture Decision Records

Each ADR records one decision: its context, the decision itself, and its consequences. An accepted ADR is not rewritten. To change a decision, add a new ADR that supersedes it and update the status of the old one.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-scope-and-phases.md) | Scope, phases, and deployment models | Accepted |
| [0002](0002-git-compatible-edge.md) | Git protocol at the edge, storage through pluggable backends | Accepted |
| [0003](0003-licensing.md) | Licensing | Accepted |
| [0004](0004-agent-first-interfaces.md) | Agent-first interface principles | Accepted |
| [0005](0005-internationalization.md) | Internationalization | Accepted |
| [0006](0006-enroute-spike.md) | Enroute 0.3.0 spike results | Accepted |
| [0007](0007-storage-backend-interface.md) | Storage backend interface and conformance suite | Accepted |
| [0008](0008-git-traffic-authorization.md) | How git traffic is authorized | Accepted |
| [0009](0009-ephemeral-branch-reclamation.md) | Reclaiming storage from expired ephemeral branches | Accepted |
| [0010](0010-metadata-store.md) | Cambium's metadata store | Accepted |
| [0011](0011-tenancy-and-authentication.md) | Tenancy and authentication model | Accepted |
| [0012](0012-api-transport-and-errors.md) | API transport and error code registry | Accepted |
| [0013](0013-sealed-generation-reclamation.md) | Seal an old generation before final reconciliation | Accepted; amends 0006, 0007, and 0009 |
| [0014](0014-encrypted-idempotency-responses.md) | Encrypted idempotency responses | Accepted; amends 0010 and 0011 |
| [0015](0015-durable-ref-creation-receipts.md) | Durable receipts for ref creation | Accepted; amends 0006, 0007, and 0009 |

New ADRs use the next number and the sections Status, Date, Context, Decision, and Consequences.
