<!-- markdownlint-disable MD013 -->

# ADR 0002: Isolated Per-Environment Budget Control

- **Status:** Accepted; platform and security implementation reviews pending
- **Date:** 2026-07-14
- **Last reviewed:** 2026-07-17
- **Decision owners:** Project owner; platform and security reviews pending
- **Related:** `docs/cost/aws-monthly-forecast.md`

## Context

Development, staging, and production use isolated mutable resources in one AWS
account. Each environment has its own ₹300 monthly AWS service-cost cap:
₹300 for development, ₹300 for staging, and ₹300 for production. The approved
aggregate ceiling is therefore ₹900 if all three environments fully use their
independent budgets. AWS Budgets can notify about actual and forecast spend, but
billing data can be delayed and cannot safely authorize an individual workflow
operation.

Each environment therefore needs its own authoritative, strongly consistent
decision point for projected usage. A shared cross-environment aggregate is not
an authorization boundary because one environment's spend must not consume or
release another environment's ₹300 allocation. Account-level reporting may
provide a consolidated read-only view, but it cannot authorize operations.

The forecast replaces the unaffordable Secrets Manager baseline with separate
Parameter Store standard-tier `SecureString` values encrypted by the AWS managed
Systems Manager KMS key. This ADR decides the control mechanism only; it does not
declare every environment affordable. Phase 1 remains blocked until the forecast
is recalculated per environment and the S3 retention/lifecycle evidence is
resolved.

## Decision

Deploy one **isolated Budget Guard** inside each development, staging, and
production stack. There is no intentional cross-environment mutable budget
control plane.

Each environment stack owns:

- One environment-specific DynamoDB on-demand budget table with encryption,
  a point-in-time recovery decision, and bounded maximum throughput.
- One environment-bound Budget Guard Lambda entry point.
- One read-only deployment/status entry point.
- Environment-specific CloudWatch budget metrics and alarms.
- AWS Budgets or Cost Explorer views filtered by mandatory environment cost
  allocation tags where provider billing supports reliable attribution.

A consolidated read-only dashboard and account-level alarm may sum all three
environments, but that roll-up is informational only. It cannot reserve cost,
release reservations, or authorize deployment or workflow operations.

The Budget Guard obtains its environment from immutable function configuration,
not from the caller's payload. Only that environment's Budget Guard execution
role may write its budget table. Application and deployment roles cannot call
budget-table write APIs directly and cannot access another environment's budget
resources.

Shared or unattributed account charges must be assigned by an approved,
conservative allocation rule. Missing or unreliable attribution never reduces
an environment's forecast; the affected deployment or operation fails closed
until the charge is assigned.

## Amount representation

Store estimated, reserved, settled, and capped amounts as integer micro-INR.
Floating-point values are forbidden in budget decisions.

Each environment's monthly configuration records:

- Invoice month in `YYYY-MM` form.
- GST treatment.
- Approved USD-to-INR conversion basis.
- Foreign-exchange buffer.
- Operational safety margin.
- An approved ₹10 monthly operational reserve inside that environment's ₹300
  cap.
- ₹240 warning threshold for that environment.
- ₹270 new-intake suspension threshold for that environment.
- ₹300 hard cap for that environment.
- Pricing-catalog version and approval reference.

The same values may be inherited from company configuration, but every
environment enforces them against only its own aggregate. Changing the conversion
basis, thresholds, reserve, or safety margin is a reviewed configuration change.
It must not retroactively lower recorded actual cost.

## Data model

The exact DynamoDB key design remains subject to Task 19 review, but each
environment's budget service requires these logical records in its isolated
table:

1. **Environment monthly aggregate:** settled, reserved, and projected micro-INR
   for that environment, plus optimistic version and update timestamp.
2. **Operation reservation:** idempotency key, opaque workflow ID, operation
   class, estimate, state, creation time, and expiry/reconciliation metadata.
3. **Pricing decision:** immutable reference to the approved estimate version,
   FX basis, GST, and safety margin used by the reservation.
4. **Reconciliation record:** attributed AWS actual-cost observation and variance
   from application estimates.

A consolidated account roll-up, if created, is read-only and non-authoritative.
It is not updated in the reservation transaction and cannot approve spending.

Conversation data, credentials, provider payloads, Telegram identifiers, and
customer identifiers must not enter a budget table. Workflow identifiers must
use opaque internal IDs.

## Reservation protocol

Before any new workflow or cost-bearing operation proceeds:

1. The environment calls its own Budget Guard with a stable idempotency key,
   opaque workflow ID, operation class, and references to trusted
   ingestion/operation records.
2. The guard ignores caller-supplied monetary values. It derives a conservative
   reservation from an approved server-side operation-class envelope. Variable
   units must come from data the guard can verify, such as S3 object metadata or
   an immutable ingestion record written by a narrowly scoped adapter. Missing,
   unverifiable, or out-of-range units use the operation-class maximum or fail
   closed; they never reduce the reservation.
3. A DynamoDB transaction creates the operation reservation and conditionally
   updates that environment's monthly aggregate.
4. The condition rejects the transaction when the idempotency key already
   exists with different inputs, that environment's intake is suspended, or
   that environment's projected total plus safety margin would reach or exceed
   its ₹300 hard cap.
5. Repeating the same request returns the existing decision without reserving
   cost twice.
6. On completion, a second idempotent transaction settles trusted measured
   usage and releases unused reservation. Settlement evidence must come from a
   controlled adapter record or attributed AWS reconciliation source, not an
   arbitrary caller amount. Ambiguous or interrupted work remains reserved until
   reconciliation rather than being silently released.

Each environment aggregate is a deliberate serialization point. Expected traffic
is low, and strict enforcement of that environment's cap is more important than
write scalability. Independent environments do not serialize on one global item.

## Decisions at thresholds

All thresholds below apply independently to the calling environment:

- **Below ₹240 projected:** permit operations that pass their normal policy.
- **At or above ₹240 projected:** permit eligible work and emit one deduplicated
  warning per threshold crossing in that environment.
- **At or above ₹270 projected:** reject new workflow intake in that environment.
  Existing unconfirmed work may be viewed, corrected, cancelled, or retrieved
  but may not reserve new mutation cost.
- **Confirmed work:** proceed only when it already owns a sufficient reservation
  or a new atomic reservation keeps that environment's projected invoice below
  ₹300 including safety margin.
- **At the projected hard cap:** reject new cost-bearing work in that
  environment. The approved ₹10 operational reserve is deducted from that
  environment's spendable workflow budget, replenishes only at an approved
  invoice-month rollover, cannot be borrowed by workflow operations, and is
  consumed by metered status, cancellation, failure reporting, and authorized
  artifact retrieval classes.
- **Actual invoice evidence above projection:** immediately use the greater of
  that environment's application projection and its attributed reconciled AWS
  actual/forecast cost.

Threshold comparison is inclusive at warning and suspension boundaries. The hard
cap is exclusive: a decision requiring one environment's projected spend to
equal ₹300 is denied.

The ₹10 reserve is approved per environment, but approval is not evidence that
all allowed paths fit within ₹10. Task 8 must still measure their worst-case
monthly calls and propose a change if the reserve is insufficient. Exhausting an environment's reserve disables its
retrieval but retains a no-new-work fail-closed decision. It must not borrow from
another environment or a future month.

## Invoice-month rollover and stale reservations

The guard derives the invoice month from server-side UTC time and an approved AWS
billing-calendar rule, never from caller input.

- Initialize an environment's next month only from approved pricing, FX, GST,
  margin, allocation, and reserve configuration. Missing approval makes that
  environment fail closed.
- An operation expected to cross the boundary reserves its conservative maximum
  in both months atomically within the same environment. Settlement attributes
  usage to the month in which AWS records it and releases only the unused
  counterpart.
- Do not authorize a long-running operation near rollover when a dual-month
  reservation cannot fit in either month for that environment.
- TTL is cleanup metadata, not authorization. Expiry never releases money by
  itself.
- A stale reservation with conclusive adapter evidence may be settled or
  released by an idempotent reconciliation transaction. An ambiguous stale
  reservation remains charged and enters manual review.
- Unreconciled prior-month reservations do not migrate silently. They remain in
  the original environment and month; stale reconciliation beyond the approved
  freshness limit blocks new intake only in that environment.
- Actual AWS billing adjustments for a closed month are recorded as immutable
  reconciliation deltas and tighten the affected environment's current forecast
  when they reveal systematic underestimation.

## Consistency and outage behavior

All authorization decisions use transactional writes or strongly consistent
reads. Eventually consistent caches must not authorize cost-bearing operations.

If an environment's guard, table, pricing configuration, cost allocation, or
reconciliation state is unavailable or invalid:

- Fail closed in that environment for new intake, AI calls, external writes,
  retries, and deployment.
- Do not fall back to another environment's counters, unused budget, or local
  unguarded estimates.
- Permit only bounded status, cancellation, and already-authorized retrieval
  paths covered by that environment's operational reserve.
- Emit a redacted operational alarm without workflow content or credentials.
- Require manual review before releasing ambiguous reservations.

An unaffected environment may continue only when its own guard and attributed
billing evidence remain healthy. No environment may override this outage policy
through local configuration.

## Ownership and least privilege

A designated platform/budget owner defines the common guard policy. Each
isolated environment stack deploys its own guard, table, alarms, and recovery
controls.

- Environment application role: `lambda:InvokeFunction` only on its own
  environment-bound guard ARN and read-only status ARN where required.
- Budget Guard role: only required DynamoDB item/transaction actions on its own
  environment's table, required KMS actions if a customer-managed key is
  approved, and bounded metric/log actions.
- Deployment role: read-only budget decision plus deployment of its isolated
  environment; it cannot mutate budget counters or inspect another environment.
- Reconciliation role: read attributed billing data and submit signed
  reconciliation records to the matching environment; it cannot execute
  workflow operations.
- Human break-glass access: separately approved, logged, time-bounded, and never
  used by normal deployment or application paths.

Generated IAM policies require human review before any environment deployment.

## Deployment gate

An environment deployment is denied unless its read-only guard proves all of the
following:

- Pricing, FX, GST, safety-margin, and shared-charge allocation configuration is
  current and approved.
- That environment's fixed monthly cost plus accrued actual cost, active
  reservations, deployment increment, retention growth, and safety margin remain
  below its ₹300 cap.
- That environment's intake is not suspended and reconciliation is within its
  freshness limit.
- The target environment matches the environment-bound deployment identity.
- The environment's budget resources are healthy and strongly consistent reads
  succeed.

A failed or timed-out gate is a denial, not a warning.

## Observability and reconciliation

Emit low-cardinality metrics for environment, decision, operation class, and
threshold state. Do not put workflow content, Telegram identifiers, OAuth data,
or secret values in metric dimensions or logs.

Reconcile application estimates with environment-attributed AWS Cost
Explorer/Budgets data when fresh billing data becomes available. Billing evidence
may tighten decisions but may not loosen an application reservation
automatically. Estimate variance outside an approved tolerance suspends intake
only in the affected environment until reviewed. Missing attribution fails the
affected environment closed rather than charging another environment silently.

## Consequences

### Positive

- Each environment has an independently enforced ₹300 cap.
- Development or staging usage cannot consume production's allocation.
- Independent aggregate items remove a global reservation serialization point.
- Environment callers cannot claim another environment identity.
- Idempotent reservations make retries safe.
- Delayed AWS billing data supplements rather than authorizes operations.
- An outage in one isolated guard need not suspend a healthy environment.

### Negative

- The account can spend up to ₹900 per month before GST when all three
  environments fully use their approved caps.
- Three isolated guards, tables, alarms, and recovery paths increase
  infrastructure and operational work.
- Shared and unattributed account charges need a conservative allocation rule.
- Pricing estimates and reconciliation require ongoing per-environment
  maintenance.
- Separate caps do not solve a retention pattern that exceeds ₹300 inside one
  environment.

## Alternatives considered

### One combined ₹300 cap across all environments

Rejected because the project owner approved a separate ₹300 allocation for each
environment. A combined pool would let development or staging consume budget
needed by production and would require a cross-environment serialization point.

### AWS Budgets as the only guard

Rejected because billing and forecast data can lag and cannot atomically reserve
cost for one operation.

### Direct budget-table writes from application roles

Rejected because it weakens least privilege, trusts application code to enforce
reservation invariants, and increases the blast radius of a compromised role.

### Eventually consistent aggregate reads

Rejected because stale reads can authorize concurrent operations beyond an
environment's cap.

### Bundle all credentials to make the design affordable

Rejected. Bundling reduces credential isolation and may give functions access to
secrets they do not need. The selected Parameter Store design avoids the fixed
Secrets Manager storage charge without expanding credential blast radius.

## Approval conditions

- [x] Human approves separate ₹300 monthly caps for development, staging, and
  production, with an aggregate account ceiling of ₹900 before GST.
- [x] Human approves integer micro-INR, GST, FX, and safety-margin semantics.
- [x] Human approves the ₹10 operational reserve per environment.
- [x] Human approves inclusive warning/suspension and exclusive hard-cap rules
  as per-environment decisions.
- [x] Human agrees no environment may override the fail-closed outage policy
  through local configuration.
- [x] Human approves the bounded outage paths and their reserve usage.
- [x] Human requires generated IAM policies to receive human review before
  deployment.
- [x] Human approves denial on a failed or timed-out deployment gate.
- [ ] Security reviewer approves environment-bound invocation and table IAM.
- [ ] Per-environment cost forecasts show the resulting architecture is
  affordable or an explicit PRD/budget change is approved.
- [x] Approver/date: **Project owner, 2026-07-17**
