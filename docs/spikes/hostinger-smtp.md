<!-- markdownlint-disable MD013 -->

# Hostinger SMTP Ambiguity Feasibility Spike

**Status:** Test protocol approved; controlled Hostinger run pending
**Task:** Phase 0, Task 7
**Environment:** Dedicated non-production sender and recipient mailboxes only

## Purpose

Determine what the configured Hostinger mailbox returns for accepted, rejected,
timed-out, and connection-dropped SMTP submissions so Novus never retries an
ambiguous send blindly.

This protocol does not claim observed Hostinger behavior. Do not mark Task 7
complete until exact replies, delivery evidence, duplicate behavior, and the
approved idempotency policy are recorded in this report and sanitized provider
cases are stored in `tests/fixtures/smtp/provider-cases.json`.

## Provider configuration to verify

Hostinger currently documents these common settings:

```text
Host: smtp.hostinger.com
Port: 587
Security: TLS/STARTTLS
Username: complete mailbox address
Password: mailbox password
```

The controlled test must use the values shown in the actual test account's
**Connect Apps & Devices** page. Do not commit the username, password, mailbox
address, AUTH exchange, or TLS key material.

Sources:

- <https://www.hostinger.com/support/1575756-how-to-get-email-account-configuration-details-for-hostinger-email/>
- <https://www.hostinger.com/support/4625828-parameters-and-limits-of-hostinger-email/>

## SMTP outcome vocabulary

| Outcome | Required evidence | Automatic retry policy |
| --- | --- | --- |
| Not submitted | Connection failed or closed before the final DATA terminator was transmitted, with a transport trace proving that boundary | May retry within the shared three-attempt policy when the error is transient |
| Rejected | A phase-attributed `4yz` or `5yz` reply conclusively rejects the current attempt | Retry `4yz` only when policy permits; do not retry permanent `5yz` without corrected input |
| Accepted | Client received a final `2yz`, normally `250`, after transmitting the final DATA terminator | Do not retry; persist acceptance before any later delivery step |
| Ambiguous | The final terminator may have reached Hostinger but the final reply did not conclusively reach the client | Never retry automatically; enter manual review and reconcile |
| Delivered | Recipient raw-message evidence and/or Hostinger delivery logs corroborate the accepted message | Delivery is separate from SMTP acceptance |

A `354` reply only permits the client to send message data. It is not acceptance.
Under RFC 5321, final success after DATA transfers responsibility to the
receiving server but does not prove final recipient delivery.

Sources:

- <https://www.rfc-editor.org/rfc/rfc5321.html#section-2.3.1>
- <https://www.rfc-editor.org/rfc/rfc5321.html#section-4.2.1>
- <https://www.rfc-editor.org/rfc/rfc5321.html#section-4.2.5>

## Identifier contract

Every logical test message uses:

```text
Message-ID: <phase0.RUN_UUID@controlled-test-domain>
X-Phase0-Test-ID: RUN_UUID
X-Phase0-Attempt: CASE_ID-ATTEMPT_NUMBER
```

Record these identifiers separately:

- **Logical operation/idempotency key:** application-generated and stable across
  a deliberately authorized retry.
- **Message-ID:** application-generated message header used for correlation; SMTP
  does not guarantee it prevents duplicates.
- **Attempt ID:** unique per physical submission attempt.
- **SMTP response/queue ID:** opaque provider text, if Hostinger includes one in
  the final reply. RFC 5321 does not require a portable queue-ID format.
- **Delivery evidence:** Hostinger log entry and recipient raw-message headers.

Never infer that a client Message-ID proves provider acceptance, or that a
provider response ID proves mailbox delivery.

Sources:

- <https://www.rfc-editor.org/rfc/rfc5322.html#section-3.6.4>
- <https://www.rfc-editor.org/rfc/rfc5321.html#section-4.2>

## Test safety and instrumentation

- Use a dedicated Hostinger test sender and non-production recipient.
- Disable forwarding, auto-replies, mailbox rules, and integrations on the
  recipient.
- Send harmless plain text to exactly one envelope recipient for S01-S12. Use
  two dedicated non-production recipients only for the mixed-recipient S13 case.
- Remain far below the mailbox's displayed rolling limits.
- Prefer an instrumented SMTP client transport that records application-layer
  write/read boundaries inside the TLS session and can stop reading or close the
  socket at a named SMTP phase. If a proxy is required, use an explicitly
  test-only TLS-terminating proxy: the proxy must validate Hostinger upstream,
  the client must trust only the temporary local test CA, and no production
  credential may pass through it. A pass-through TLS proxy cannot prove DATA
  boundaries.
- Never log AUTH payloads, credentials, message content, or TLS key material.
- Capture UTC timestamps and client SMTP states/replies with content redacted.
- Record envelope sender/recipient separately from visible message headers.
- Download each received message as `.eml` and calculate a local hash.
- Search all recipient folders and inspect raw messages rather than relying on
  subject threading.
- Remove unsanitized transcripts after human verification unless an approved
  secure evidence policy requires temporary retention.

Hostinger documents 30-day inbound/outbound delivery logs and raw-header/message
export. The public documentation does not guarantee those logs expose Message-ID,
queue ID, or the exact SMTP transcript; the live run must establish correlation.

Sources:

- <https://www.hostinger.com/support/6404796-how-to-check-delivery-logs-for-hostinger-email/>
- <https://www.hostinger.com/support/5240979-how-to-check-email-headers-at-hostinger/>

## Scenario matrix

| ID | Action | Required evidence | Expected policy before test | Observed |
| --- | --- | --- | --- | --- |
| S01 | Submit one valid message without injected faults | `354`, final terminator, complete final reply, any opaque response ID, logs, raw `.eml` | Final `2yz` means accepted; later mailbox evidence means delivered | Pending |
| S02 | Send a deliberately invalid envelope command before DATA | Exact command phase and `4yz`/`5yz` reply | Rejected; no message submission | Pending |
| S03 | Authenticate once with a deliberately incorrect test password | Sanitized AUTH outcome without payload | Rejected before message submission | Pending |
| S04 | Delay server greeting beyond the client's test timeout | Proxy trace proving no DATA bytes were sent | Unambiguous pre-submission timeout; transient retry may be safe | Pending |
| S05 | Delay `354` beyond the client's test timeout | SMTP/proxy trace proving final terminator was not sent | Unambiguous pre-submission timeout | Pending |
| S06 | Forward the complete message and terminator but withhold final reply until client timeout | Both sides of proxy trace, Hostinger logs, recipient evidence | Ambiguous regardless of generic timeout exception | Pending |
| S07 | Drop connection after a partial body but before forwarding the terminator | Proxy trace showing exact byte boundary | Not submitted; retry may be safe if transient | Pending |
| S08 | Forward the terminator, then drop the client-facing connection before final reply | Both sides of proxy trace, logs, recipient evidence | Ambiguous; no automatic retry | Pending |
| S09 | If Hostinger support provides a safe final-DATA rejection trigger, run it once | Final `4yz`/`5yz` and phase | Classify by exact reply; otherwise document unsupported test | Pending |
| S10 | Intentionally send two calibration copies with one logical Message-ID and different attempt IDs | Two raw `.eml` files or evidence that mailbox/log UI merges them | Establish whether duplicate counting is reliable | Pending |
| S11 | Repeat one unambiguous transient failure under the maximum three-attempt policy | Attempt IDs and final result | No more than three total attempts | Pending |
| S12 | Reconcile S06 and S08 without resubmitting | Logs, all mailbox folders, raw headers, hashes | Accepted/delivered or unresolved manual-review outcome | Pending |
| S13 | Submit to two dedicated recipients while injecting one `RCPT TO` rejection | Per-recipient RCPT replies and proof of whether DATA was sent | Abort with `RSET` before DATA; send to neither recipient | Pending |

## Timeout requirements

RFC 5321 specifies a ten-minute minimum timeout while awaiting completion after
DATA termination. A shorter timeout may be used only as explicit fault injection
and does not prove a compliant production timeout policy.

Run both:

1. A short injected delay to prove classification logic.
2. A production-policy observation with at least the RFC minimum for the final
   DATA response.

The post-DATA synchronization gap is inherently dangerous: Hostinger may have
accepted and queued the message while the client did not receive the final
reply. Retrying that state can deliver duplicates.

Sources:

- <https://www.rfc-editor.org/rfc/rfc5321.html#section-4.5.3.2.6>
- <https://www.rfc-editor.org/rfc/rfc1047.html>

## Duplicate-detection procedure

1. Calibrate the recipient using S10 and verify whether two physical copies are
   separately retrievable.
2. For each ambiguous case, search inbox, spam, trash, and any quarantine.
3. Match exact Message-ID and `X-Phase0-Test-ID` in raw source.
4. Count distinct `.eml` files and compare attempt headers, `Received` headers,
   and hashes.
5. Filter Hostinger logs by sender, recipient, and a narrow UTC interval; expand
   per-recipient server results.
6. Preserve evidence through Hostinger's documented 30-day log window.
7. If evidence remains inconclusive, keep the operation in manual review. An
   absent UI result is not proof that Hostinger did not accept the message.

Message-ID is a correlation aid, not an SMTP idempotency key.

Source: <https://www.rfc-editor.org/rfc/rfc7352.html>

## Proposed safe idempotency policy

Before SMTP submission:

- Resolve and deduplicate every confirmed To, CC, and BCC address before opening
  the SMTP transaction.
- Require a positive RCPT reply for every confirmed envelope recipient. If any
  recipient is rejected, issue `RSET` and do not send DATA to any accepted
  subset. Report per-recipient rejection without exposing BCC addresses to other
  recipients. A later corrected send is a new confirmation revision.
- Reserve one durable application operation key bound to workflow revision,
  confirmation digest, sender, recipients, subject/body digest, attachment
  digests, and S3-link expiry.
- Render one stable Message-ID and MIME payload for the logical operation.
- Create a unique attempt record before opening each SMTP connection.

After submission:

- Persist the exact SMTP phase reached and sanitized reply class.
- Mark accepted only after receiving final `2yz` following DATA.
- Mark unambiguously retryable only when the trace proves the terminator was not
  transmitted or an explicit transient rejection was received.
- Mark post-terminator timeout/disconnect as ambiguous and block automatic retry.
- Treat a provider queue ID, if observed, as opaque evidence rather than an
  idempotency guarantee.
- Allow at most three total attempts for unambiguous failures.
- Require a human to reconcile ambiguous cases before deciding whether a new
  logical message may be sent.

This policy remains proposed until the Hostinger run proves the available
provider evidence.

## Sanitized provider fixture

Create `tests/fixtures/smtp/provider-cases.json` only after the controlled run.
Store sanitized protocol outcomes, not credentials, mailbox addresses, message
content, or raw AUTH/TLS transcripts.

```json
{
  "schema_version": 1,
  "cases": [
    {
      "case_id": "S01",
      "phase": "data_complete",
      "reply_class": 2,
      "provider_response_id_present": true,
      "outcome": "accepted"
    }
  ]
}
```

Preserve reply class, enhanced-status-code presence, phase, response-ID presence
and equality relationships, timing buckets, and application classification.
Replace exact response IDs and addresses with deterministic aliases. Exclude
message bodies, recipient data, credentials, AUTH payloads, TLS secrets, and
full provider logs.

## Live behavior still requiring proof

- Actual EHLO capabilities for the configured Hostinger account.
- Exact final success, authentication, envelope, and mixed-recipient rejection
  replies.
- Whether final success contains a stable response/queue ID.
- Whether that ID appears in Hostinger logs.
- Final DATA response latency.
- Hostinger behavior in post-terminator timeout/drop cases.
- Whether logs include interrupted or rejected submissions.
- Whether the selected recipient preserves or merges duplicates.
- A safe final-DATA rejection trigger, if Hostinger supports one.

## Approval

- [ ] Accepted, rejected, timed-out, dropped, and partial-recipient cases have
  phase-attributed evidence.
- [ ] Stable and opaque identifier behavior is documented.
- [ ] Acceptance and mailbox delivery are reported separately.
- [ ] Post-terminator ambiguity never retries automatically.
- [ ] Duplicate detection is calibrated against raw messages.
- [ ] No test exceeds three submission attempts.
- [ ] Sanitized fixtures contain no credentials or message content.
- [ ] Human approves the idempotency/manual-review policy.
- [x] Human approver/date: **Approved by project owner, 2026-07-15**
