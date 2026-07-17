<!-- markdownlint-disable MD013 -->

# DynamoDB Key Design and Persistence Boundaries

## Status

Approved for Task 19 implementation on 2026-07-17. This document freezes the logical key and conditional-write contracts; Tasks 20 and 21 select AWS SDK representations and implement the adapters.

## Isolation model

Development, staging, and production use separate physical resources. An adapter receives its environment from immutable deployment configuration, never from a request payload.

Each environment owns:

- one application table for workflows, topic claims, confirmations, audit entries, operation journals, history metadata, object metadata, OAuth state, and membership evidence;
- one budget table for that environment's monthly aggregate and reservations, as required by ADR 0002;
- one S3 bucket or environment-bound bucket namespace with separate `raw/`, `artifacts/`, and `history/` prefixes;
- one environment-bound Parameter Store path.

The same logical PK/SK may appear in two environments because the physical tables are different. No repository method accepts an environment argument, so callers cannot select a different environment at runtime. Consolidated account reporting is read-only and is not represented by these write ports.

## Key encoding rules

- Key prefixes and separators are ASCII constants.
- User-visible text, credentials, Telegram content, and provider payloads never enter keys.
- Numeric revisions and history sequence numbers use zero-padded 20-digit decimal encoding so lexical order equals numeric order.
- Binary fingerprints use lowercase hexadecimal.
- IDs must already be validated typed identities before encoding.
- Key builders reject control characters and values that would exceed DynamoDB key limits.

## Application table

| Entity | PK | SK | Access pattern and invariants |
| --- | --- | --- | --- |
| Workflow metadata | `WF#<workflow_id>` | `META` | Strongly consistent load by workflow ID; stores current `Workflow` only, not unbounded history |
| Topic claim | `TOPIC#<chat_id>#<thread_id>` | `WORKFLOW` | Strongly consistent lookup from Telegram topic to workflow; created atomically with workflow metadata and never reassigned |
| Transition/authorization audit | `WF#<workflow_id>` | `AUDIT#<revision:020>` | Ordered, append-only pagination by workflow revision |
| Confirmation | `WF#<workflow_id>` | `CONFIRMATION#<confirmation_id>` | Pending-to-consumed conditional update; consumed records are immutable |
| External-operation journal | `OP#<workflow_id>#<revision:020>#<kind>#<target_hex>` | `META` | Atomic reservation and durable attempt history for the existing `OperationJournal` port |
| Conversation/history pointer | `WF#<workflow_id>` | `HISTORY#<sequence:020>` | Ordered metadata and sanitized S3 pointer; no conversation body in DynamoDB |
| Object metadata | `WF#<workflow_id>` | `OBJECT#<kind>#<object_id>` | Canonical raw input or artifact pointer, checksum, size, media type, and creation metadata |
| OAuth state | `OAUTH#<state_digest_hex>` | `META` | Short-lived, participant-bound, single-use state; stores no access or refresh token |
| Membership approval cache | `MEMBERSHIP#<forum_chat_id>#<participant_id>` | `META` | Positive evidence only; domain age checks authorize use |

A GSI is intentionally avoided for topic routing because an eventually consistent index cannot safely decide whether a topic is already claimed. The dedicated topic-claim item supports a strongly consistent `GetItem` and a transactional uniqueness condition.

## Budget table

The budget table is physically isolated per environment.

| Entity | PK | SK | Access pattern and invariants |
| --- | --- | --- | --- |
| Monthly aggregate | `MONTH#<yyyy-mm>` | `AGGREGATE` | Strongly consistent read and conditional version update; one serialization point per environment/month |
| Operation reservation | `MONTH#<yyyy-mm>` | `RESERVATION#<operation_key>` | Idempotent reserve/settle/reconcile lifecycle; contains only opaque workflow identity and approved pricing references |
| Pricing decision | `MONTH#<yyyy-mm>` | `PRICING#<version>` | Immutable approved FX, GST, margin, reserve, and catalog reference |
| Reconciliation record | `MONTH#<yyyy-mm>` | `RECONCILIATION#<observed_at>#<record_id>` | Append-only attributed billing evidence |

An operation crossing an invoice boundary writes reservations under both month partitions in one transaction. TTL never releases a reservation or changes authorization state.

## Conditional-write contracts

### Create workflow

One transaction:

1. put workflow metadata with `attribute_not_exists(PK)`;
2. put topic claim with `attribute_not_exists(PK)`;
3. put revision-zero audit entry with `attribute_not_exists(PK) AND attribute_not_exists(SK)`.

Exactly one concurrent creator can claim a topic.

### Commit workflow transition

Condition the workflow update on the expected revision. Put the resulting audit item in the same transaction with a non-existence condition. A failed condition returns a typed revision conflict and performs no partial write.

### Issue confirmation

Atomically:

- condition workflow revision equals `ConfirmationIssuePrecondition.expected_workflow_revision`;
- update the workflow to the resulting waiting revision;
- put the pending confirmation only when its key does not exist;
- append transition and authorization audit entries.

### Consume or correct confirmation

Atomically:

- condition workflow revision equals `ConfirmationConsumePrecondition.expected_workflow_revision`;
- condition confirmation status is pending and the confirmation ID matches;
- update workflow and confirmation together for consumption, or update workflow and invalidate the pending confirmation for correction;
- append transition and authorization audit entries.

Replay, stale revision, or a race loser changes nothing.

### Reserve external operation

The existing `OperationJournal.reserve` contract owns this boundary. Creating a new operation item uses a non-existence condition. Re-reading an identical key returns its durable state; a mismatched representation fails closed. Attempt-start is durable before provider invocation. Accepted, terminal, ambiguous, and exhausted outcomes are durable before replay.

### Append history and object metadata

History sequence and object identity are immutable per workflow. Writes use non-existence conditions. The S3 object must be durably accepted before its pointer becomes visible; recovery may delete an unreferenced object, but must never expose a pointer to a missing object.

### Reserve budget

A transaction puts an idempotent reservation and conditionally updates the matching monthly aggregate version and approved cap expression. Settlement and reconciliation are separate idempotent transactions. Caller-supplied monetary values are not trusted inputs.

## Pagination

Repository pages contain a bounded item list plus an opaque continuation token. The token represents the last evaluated key and is valid only for the same environment-bound repository, workflow, entity prefix, and direction.

- Audit pages query `PK = WF#...` and `begins_with(SK, "AUDIT#")`.
- History pages query `PK = WF#...` and `begins_with(SK, "HISTORY#")`.
- Object pages query `PK = WF#...` and `begins_with(SK, "OBJECT#")`.
- Budget reservation and reconciliation pages query the relevant `MONTH#...` partition and prefix.

No port exposes an unbounded scan.

## TTL and retention

DynamoDB TTL is cleanup metadata, not authorization.

- Workflow, confirmation, audit, idempotency, history-pointer, object, pricing, and reconciliation records have no automatic retention TTL in Version 1.
- OAuth state and membership-cache items may carry cleanup TTLs, but state expiry and the 30-minute membership-cache limit are checked by application/domain timestamps.
- Budget reservation TTL, if added by Task 20, may remove only records already conclusively settled or released. Expiry alone never releases reserved cost.
- Raw inputs, generated artifacts, and sanitized history are retained indefinitely under the Version 1 policy.

## S3 pointer contract

A persisted pointer contains:

- environment-bound bucket reference supplied by the adapter, not the caller;
- key under exactly one of `raw/`, `artifacts/`, or `history/`;
- byte length;
- SHA-256 checksum;
- media type;
- creation timestamp;
- optional model and prompt versions for sanitized history.

History payloads must be sanitized before upload and must exclude OAuth material, secrets, and raw credential-bearing provider payloads. Presigned URLs are generated on demand and are never stored as canonical pointers.

## Secret boundary

OAuth refresh tokens, SMTP credentials, model keys, and Telegram secrets exist only as environment-bound Parameter Store `SecureString` values. DynamoDB and S3 may store validated secret references where required, never secret values. Secret ports return redaction-safe errors and must not expose values through `Debug` output.

## Deferred implementation details

Tasks 20 and 21 own AWS SDK dependencies, attribute names, transaction expressions, table definitions, encryption/IAM, local emulator wiring, serialization adapters, S3 upload recovery, and Parameter Store clients. Those tasks must implement this contract without changing keys or stored schemas unless the project owner approves a new revision of this document and the storage fixture schema.
