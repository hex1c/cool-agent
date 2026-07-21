<!-- markdownlint-disable MD013 -->

# ADR 0001: Topic Lifecycle and Confirmation Semantics

- **Status:** Accepted
- **Date:** 2026-07-16
- **Decision owner:** Product owner, approved through annotated review
- **Related:** `docs/prd/telegram-operations-worker.md`, `tasks/plan.md`

## Context

A Telegram forum topic is the durable conversational boundary for one workflow.
The worker must handle messages that arrive after the workflow becomes terminal,
old inline callbacks, corrected previews, duplicate updates, participant removal,
and temporary Telegram membership-API outages without allowing an unintended
external mutation.

The system also needs useful continuity after completion. Employees must be able
to ask questions about the completed session and its retained history without
reopening the workflow or silently turning a terminal topic into a second
workflow.

Telegram group membership is the authorization boundary. A live membership
lookup is preferred for confirmation and cancellation, but a temporary API
connection failure must not make those controls unusable indefinitely.

## Decision

### One workflow and continued conversation per topic

Each `(chat_id, message_thread_id)` hosts exactly one workflow identity. A second
workflow cannot be started in the same topic, and a terminal workflow is never
reopened.

After the workflow reaches `Completed`, `Stopped`, `Failed`, or `Expired`, the
topic remains available for read-only conversational follow-up about that
workflow. The worker may answer questions from sanitized retained conversation,
workflow history, audit records, and existing artifact metadata or links.

Read-only follow-up must not:

- Change workflow state or revision.
- Accept new attachments as workflow inputs.
- Apply a correction or create a new preview.
- Reserve or perform a Google, Calendar, email, PDF, S3, Drive, or other external
  mutation.
- Treat the message as the start of another workflow.

A message that requests new or changed work in a terminal topic receives a clear
instruction to create a new topic. A late attachment is not associated with the
terminal workflow and receives the same instruction. Duplicate terminal-topic
messages remain subject to normal Telegram update deduplication.

### Confirmation binding and replay behavior

A pending confirmation binds the workflow ID, resulting waiting revision,
preview digest, mutation-target fingerprint, workflow owner, topic, selected
sensitive action, and expiry. The confirming actor and topic-qualified source
message are captured when the confirmation is consumed.

Confirmation succeeds only when all bindings still match and the actor has
acceptable membership evidence. Correcting, stopping, failing, expiring, or
completing the workflow invalidates the old confirmation. A consumed
confirmation cannot be consumed again. Persistence must conditionally commit the
workflow transition and confirmation consumption together.

Late or duplicate callbacks never reopen a workflow and never perform a new
mutation. They return an expired, stale, terminal, or already-consumed result as
appropriate.

### Membership checks and outage cache

Confirmation and cancellation use the same membership policy:

1. Attempt a live Telegram membership lookup for the participant in the approved
   forum.
2. A live `NotApproved` result is authoritative and rejects the action.
3. Cached approval may be used only when the live lookup is unresponsive because
   the request times out or a connection cannot be established or maintained.
4. Only a prior positive approval for the same participant and approved forum is
   eligible. Negative, mismatched, future-dated, malformed, or already stale
   evidence never authorizes an action.
5. Cached approval is valid for at most 30 minutes from its live observation
   time. At exactly 30 minutes it is expired.
6. The outage must be observed at the action time. A normal Telegram API
   response, including an API error or explicit non-membership response, does
   not permit cache fallback.
7. Confirmation or cancellation records whether live or outage-cache evidence
   authorized the action, without logging Telegram payloads or sensitive
   workflow content.

Corrections may return a waiting workflow to calculation or drafting and
invalidate its current preview. That behavior is accepted, but the correcting
participant must still be an approved group participant.

## Consequences

### Positive

- Employees retain useful question-and-answer continuity in the original topic.
- Terminal workflows remain immutable and cannot acquire a second mutation path.
- Old callbacks and late messages have deterministic behavior.
- A bounded cache keeps confirmation and cancellation usable during short
  Telegram connection outages.
- The 30-minute limit bounds exposure after a participant is removed.

### Negative

- Read-only follow-up requires a classifier or application boundary that cannot
  invoke mutation tools.
- A removed participant may retain cached authority for less than 30 minutes if
  Telegram is simultaneously unreachable.
- Membership evidence must record observation time, forum, participant, source,
  and outage context.
- New mutating work requires users to create another topic.

## Alternatives considered

### Close terminal topics completely

Rejected because employees need to ask follow-up questions about retained
session context and delivered artifacts.

### Reopen the workflow or start a second workflow in the same topic

Rejected because it weakens topic-to-workflow identity, makes late callbacks
ambiguous, and increases the chance of applying a stale confirmation to new work.

### Require live membership with no outage fallback

Rejected because the approved product behavior permits bounded operation during
a temporarily unresponsive Telegram API.

### Cache approval for longer than 30 minutes

Rejected because it expands the authorization window after participant removal.

## Implementation requirements

- Topic routing and persistence retain the one-to-one topic/workflow mapping.
- Terminal-topic follow-up is explicitly read-only and has no mutation-capable
  application port or Pi tool.
- Authorization represents live and outage-cache evidence as distinct typed
  sources.
- Cache age uses checked timestamp arithmetic and expires at 30 minutes.
- Confirmation, correction, and cancellation derive the actor from validated
  membership evidence rather than a caller-supplied participant ID.
- Task 20 atomically enforces workflow-revision and pending-confirmation
  preconditions.

## Approval

- [x] One workflow per topic with read-only terminal follow-up approved.
- [x] Late mutation requests and attachments require a new topic.
- [x] Confirmation binding and replay behavior approved.
- [x] Thirty-minute positive membership cache approved for unresponsive API
  connections.
- [x] Confirmation and cancellation use the same membership policy.
- [x] Corrections invalidate the preview and still require an approved
  participant.
- [x] Approver/date: **Product owner, 2026-07-16**
