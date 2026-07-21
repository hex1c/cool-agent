<!-- markdownlint-disable MD013 -->

# ADR 0002: Shared Cross-Environment Budget Control

- **Status:** Proposed; Phase 0 human approval pending
- **Date:** 2026-07-14
- **Decision owners:** Pending
- **Related:** `docs/cost/aws-monthly-forecast.md`

## Context

Development, staging, and production use isolated mutable resources in one AWS
account but share a combined ₹300 monthly cap. AWS Budgets can notify about
actual and forecast spend, but billing data can be delayed and cannot safely
authorize an individual workflow operation.

The application therefore needs one authoritative, strongly consistent decision
point for projected usage across all environments. Giving every environment
direct write access to a shared table would weaken least privilege and let a
configuration error attribute or reserve cost for another environment.

The approved forecast replaces the unaffordable Secrets Manager baseline with
separate Parameter Store standard-tier `SecureString` values encrypted by the
AWS managed Systems Manager KMS key. This ADR decides the control mechanism
only; it does not declare the overall architecture affordable. Phase 1 remains
blocked until the S3 retention/lifecycle and budget evidence are resolved.

## Decision

Create a small **shared budget-control stack** in the selected deployment region.
It is the only intentional cross-environment mutable control plane.

The stack owns:

- One DynamoDB on-demand table with encryption, point-in-time recovery decision,
  bounded maximum throughput, and no environment-owned write permissions.
- One environment-bound Budget Guard Lambda entry point for each of development,
  staging, and production.
- One read-only deployment/status entry point.
- Shared CloudWatch budget metrics and consolidated alarms.
- One account-level AWS Budget for delayed invoice-based notification.

Each application environment may invoke only its own Budget Guard function. The
function obtains the environment from immutable function configuration, not from
the caller's payload. Only the Budget Guard execution role may write the shared
table. The three entry points may use one artifact and execution role policy,
but have distinct function ARNs and fixed environment configuration.

No development, staging, or production role may call shared-table write APIs
directly.

## Amount representation

Store estimated, reserved, settled, and capped amounts as integer micro-INR.
Floating-point values are forbidden in budget decisions.

The monthly configuration records:

- Invoice month in `YYYY-MM` form.
- GST treatment.
- Approved USD-to-INR conversion basis.
- Foreign-exchange buffer.
- Operational safety margin.
- A proposed ₹10 monthly operational reserve, included inside the ₹300 cap and
  subject to human approval.
- ₹240 warning threshold.
- ₹270 new-intake suspension threshold.
- ₹300 hard cap.
- Pricing-catalog version and approval reference.

Changing the conversion basis, thresholds, or safety margin is a reviewed
configuration change. It must not retroactively lower recorded actual cost.

## Data model

The exact DynamoDB key design remains subject to Task 19 review, but the budget
service requires these logical records:

1. **Monthly aggregate:** settled, reserved, and projected micro-INR for all
   environments, plus optimistic version and update timestamp.
2. **Environment monthly aggregate:** the same counters attributed to one
   environment.
3. **Operation reservation:** idempotency key, environment, workflow, operation
   class, estimate, state, creation time, and expiry/reconciliation metadata.
4. **Pricing decision:** immutable reference to the approved estimate version,
   FX basis, GST, and safety margin used by the reservation.
5. **Reconciliation record:** actual AWS cost observation and variance from
   application estimates.

Conversation data, credentials, provider payloads, and customer identifiers must
not enter the shared budget table. Workflow identifiers must use opaque internal
IDs.

## Reservation protocol

Before any new workflow or cost-bearing operation proceeds:

1. The environment calls its own Budget Guard entry point with a stable
   idempotency key, opaque workflow ID, operation class, and references to
   trusted ingestion/operation records.
2. The guard ignores caller-supplied monetary values. It derives a conservative
   reservation from an approved server-side operation-class envelope. Variable
   units must come from data the guard can verify, such as S3 object metadata or
   an immutable ingestion record written by a narrowly scoped adapter. Missing,
   unverifiable, or out-of-range units use the operation-class maximum or fail
   closed; they never reduce the reservation.
3. A DynamoDB transaction creates the operation reservation, updates the
   environment aggregate, and conditionally updates the monthly aggregate.
4. The condition rejects the transaction when the idempotency key already
   exists with different inputs, intake is suspended, or the projected total
   plus safety margin would reach or exceed the hard cap.
5. Repeating the same request returns the existing decision without reserving
   cost twice.
6. On completion, a second idempotent transaction settles trusted measured
   usage and releases unused reservation. Settlement evidence must come from a
   controlled adapter record or AWS reconciliation source, not an arbitrary
   caller amount. Ambiguous or interrupted work remains reserved until
   reconciliation rather than being silently released.

The monthly aggregate is a deliberate serialization point. Expected traffic is
low, and strict cross-environment cap enforcement is more important than write
scalability.

## Decisions at thresholds

- **Below ₹240 projected:** permit operations that pass their normal policy.
- **At or above ₹240 projected:** permit eligible work and emit one deduplicated
  warning per threshold crossing.
- **At or above ₹270 projected:** reject new workflow intake. Existing
  unconfirmed work may be viewed, corrected, cancelled, or retrieved but may not
  reserve new mutation cost.
- **Confirmed work:** proceed only when it already owns a sufficient reservation
  or a new atomic reservation keeps the projected invoice below ₹300 including
  safety margin.
- **At the projected hard cap:** reject new cost-bearing work. The proposed ₹10
  operational reserve is deducted before calculating spendable workflow budget,
  replenishes only at an approved invoice-month rollover, cannot be borrowed by
  workflow operations, and is consumed by metered status, cancellation, failure
  reporting, and authorized artifact retrieval classes.
- **Actual invoice evidence above projection:** immediately use the greater of
  application projection and reconciled AWS actual/forecast cost.

Threshold comparison is inclusive at warning and suspension boundaries. The hard
cap is exclusive: a decision requiring projected spend to equal ₹300 is denied.

The ₹10 reserve is a proposal, not evidence that those paths fit within ₹10.
Task 8 must measure their worst-case monthly calls and replace or approve the
amount. Exhausting the reserve disables retrieval but retains a no-new-work
fail-closed decision; it must not borrow from a future month.

## Invoice-month rollover and stale reservations

The guard derives the invoice month from server-side UTC time and an approved AWS
billing-calendar rule, never from caller input.

- Initialize the next month only from approved pricing, FX, GST, margin, and
  reserve configuration. Missing approval makes the new month fail closed.
- An operation expected to cross the boundary reserves its conservative maximum
  in both months atomically. Settlement attributes usage to the month in which
  AWS records it and releases only the unused counterpart.
- Do not authorize a long-running operation near rollover when a dual-month
  reservation cannot fit in either month.
- TTL is cleanup metadata, not authorization. Expiry never releases money by
  itself.
- A stale reservation with conclusive adapter evidence may be settled or
  released by an idempotent reconciliation transaction. An ambiguous stale
  reservation remains charged and enters manual review.
- Unreconciled prior-month reservations do not migrate silently. They remain in
  the original month, and stale reconciliation beyond the approved freshness
  limit blocks new intake in the current month.
- Actual AWS billing adjustments for a closed month are recorded as immutable
  reconciliation deltas and tighten the current forecast when they reveal
  systematic underestimation.

## Consistency and outage behavior

All authorization decisions use transactional writes or strongly consistent
reads. Eventually consistent caches must not authorize cost-bearing operations.

If the shared guard, table, pricing configuration, or reconciliation state is
unavailable or invalid:

- Fail closed for new intake, AI calls, external writes, retries, and deployment.
- Do not fall back to per-environment counters.
- Permit only bounded status, cancellation, and already-authorized retrieval
  paths covered by the operational reserve.
- Emit a redacted operational alarm without workflow content or credentials.
- Require manual review before releasing ambiguous reservations.

No environment may override this outage policy through local configuration.

## Ownership and least privilege

A designated platform/budget owner deploys the shared stack separately from all
environment stacks.

- Environment roles: `lambda:InvokeFunction` only on their environment-bound
  guard ARN and the read-only status ARN where required.
- Budget Guard role: only required DynamoDB item/transaction actions on the
  shared table, required KMS actions if a customer-managed key is approved, and
  bounded metric/log actions.
- Deployment role: read-only budget decision plus deployment of its isolated
  environment; it cannot mutate budget counters.
- Reconciliation role: read billing data and submit signed reconciliation
  records; it cannot execute workflow operations.
- Human break-glass access: separately approved, logged, time-bounded, and never
  used by normal deployment or application paths.

Generated IAM policies require human review before a shared deployment.

## Deployment gate

A deployment is denied unless the read-only guard proves all of the following:

- Pricing, FX, GST, and safety-margin configuration is current and approved.
- Fixed monthly cost plus accrued actual cost, active reservations, deployment
  increment, retention growth, and safety margin remain below ₹300.
- Intake is not suspended and reconciliation is within its freshness limit.
- The target environment matches the environment-bound deployment identity.
- Shared budget resources are healthy and strongly consistent reads succeed.

A failed or timed-out gate is a denial, not a warning.

## Observability and reconciliation

Emit low-cardinality metrics for environment, decision, operation class, and
threshold state. Do not put workflow content, Telegram identifiers, OAuth data,
or secret values in metric dimensions or logs.

Reconcile application estimates with AWS Cost Explorer/Budgets data when fresh
billing data becomes available. Billing evidence may tighten decisions but may
not loosen an application reservation automatically. Estimate variance outside
an approved tolerance suspends intake until reviewed.

## Consequences

### Positive

- One atomic cap decision covers all environments.
- Environment callers cannot claim another environment identity.
- Idempotent reservations make retries safe.
- Delayed AWS billing data supplements rather than authorizes operations.
- Outages fail closed without losing status and cancellation paths.

### Negative

- The shared guard is a deliberate cross-environment dependency and availability
  bottleneck.
- The aggregate item serializes reservations.
- Pricing estimates and reconciliation require ongoing maintenance.
- One shared stack requires separate ownership, deployment, alarms, and recovery.
- This control does not solve the fixed-cost feasibility blocker.

## Alternatives considered

### Separate per-environment counters

Rejected because concurrent environments could each authorize spend below their
local limit while exceeding the shared ₹300 cap.

### AWS Budgets as the only guard

Rejected because billing and forecast data can lag and cannot atomically reserve
cost for one operation.

### Direct shared-table writes from every environment

Rejected because it weakens least privilege, trusts caller-supplied environment
identity, and increases the blast radius of an application-role compromise.

### Eventually consistent aggregate reads

Rejected because stale reads can authorize concurrent operations beyond the cap.

### Bundle all credentials to make the design affordable

Not decided here. Bundling can reduce secret-month charges but increases blast
radius and may give functions access to credentials they do not need. It requires
separate security review and a PRD decision.

## Approval conditions

- [ ] Human approves the shared-stack ownership model.
- [ ] Human approves integer micro-INR, GST, FX, and safety-margin semantics.
- [ ] Human approves inclusive warning/suspension and exclusive hard-cap rules.
- [ ] Human approves the operational reserve and permitted outage paths.
- [ ] Security reviewer approves environment-bound invocation and table IAM.
- [ ] Cost forecast shows the resulting architecture is affordable or an
  explicit PRD/budget change is approved.
- [ ] Approver/date: **Pending**
