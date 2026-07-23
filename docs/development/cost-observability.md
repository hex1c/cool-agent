# Cost enforcement and observability verification

Task 41 implements one environment-bound budget guard per stack. The guard reads
its environment from Lambda configuration, derives the current invoice month
from server-side UTC time, accepts no caller-supplied monetary values, and uses a
strongly consistent DynamoDB aggregate plus conditional transactions. Quotation,
Calendar, and email state machines reserve intake, AI, and external-write
capacity before those paths run; class-specific Lambda configuration selects
those envelopes, so workflow payloads cannot downgrade the operation class.
Envelopes cover all bounded AI calls/provider attempts in that logical operation.
Accepted external writes settle their reservation, and successful workflows then
settle AI and intake reservations; ambiguous outcomes remain charged in manual
review.

## Threshold verification

The focused `cost_observability` suite injects the approved boundaries and
operation envelopes. It proves:

| Projection | Expected result | Automated evidence |
| --- | --- | --- |
| Below ₹240 | Permit and reserve | `cost_observability_permits_and_persists_below_warning` |
| Exactly ₹240 | Permit and emit warning crossing | `cost_observability_warns_at_80_percent_inclusive` |
| Exactly ₹270 | Deny new intake | `cost_observability_suspends_intake_at_90_percent_inclusive` |
| Exactly ₹300 | Deny at the hard cap | `cost_observability_denies_projection_equal_to_hard_cap` |
| Reconciled usage above application projection | Use reconciled usage | `cost_observability_uses_higher_reconciled_projection` |
| Missing aggregate, envelope, or conditional winner | Fail closed and emit failure | focused outage/conflict tests |

Run:

```sh
cargo test -p application cost_observability
cargo test -p integration-tests --test cost_guard
cargo test --manifest-path functions/workflow-actions/Cargo.toml --test cost_guard
```

## Signal contract

CloudWatch Embedded Metric Format records use namespace `Novus/Operations`.
Only environment is required for aggregate alarms; the optional detailed
dimension set is limited to environment, operation class, budget band, and
decision. Stable SHA-256 workflow references, stage, and outcome are structured
log properties, not metric dimensions; raw workflow IDs are never logged. OAuth
values, Telegram identifiers, credentials, provider payloads, and document
content are absent by construction.

`infrastructure/monitoring.yaml` alarms on warning, suspension, hard cap, missing
budget projections, cost-guard failures, manual-review ambiguity, workflow-stage
and Step Functions failures, and application-level webhook/OAuth error rates. A
five-minute server-side snapshot heartbeat keeps the authoritative projection
visible; a distinct stale-data alarm treats two missing periods as breaching.
Step Functions logs include state/execution outcomes with execution data disabled,
so stages are visible without document payloads. IAM keeps budget-table
transactions on the cost-guard role only.

## Operational limits

- The first heartbeat conditionally initializes an empty month from the
  environment-bound pricing version and approval. Existing aggregates are never
  overwritten. Stale reconciliation, incomplete attribution, or mismatched and
  expired pricing evidence fails closed.
- The code-reviewed operation envelopes are conservative maxima. Changing them
  is a reviewed pricing-policy change.
- AWS billing observations may tighten the reconciled projection; they never
  loosen an application reservation automatically. Stale or unattributed
  reconciliation evidence blocks new reservations.
- Each server-side operation class declares a bounded cost horizon. A
  reservation is denied only when that horizon crosses the UTC invoice-month
  boundary and dual-month capacity is unavailable; short operations earlier on
  the final day remain eligible.
- IAM and monitoring still require the Infrastructure Ready human approval
  before shared deployment.
