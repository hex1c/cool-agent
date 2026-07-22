<!-- markdownlint-disable MD013 -->

# Implementation Plan: Novus Telegram Operations Worker

## Overview

Implement the Version 1 Telegram Operations Worker defined in
`docs/prd/telegram-operations-worker.md`. The repository is greenfield: only the
PRD and `docs/sample-quotation.pdf` exist. Work starts with feasibility spikes
for the riskiest external assumptions, then freezes shared contracts, builds one
safe Telegram-to-confirmation path, and adds quotation, Calendar, and email
vertical slices. No shared development, staging, or production deployment occurs
until the applicable checkpoint and human approval are complete.

Every task must satisfy both its acceptance criteria in `tasks/todo.md` and the
project-wide Definition of Done: runtime verification, tests, formatting/lint,
no regressions, documentation, security review, observability for critical
paths, rollback consideration, and human approval before deploy.

## Architecture Decisions

- Use Rust 1.97 for domain logic, application services, adapters, and Rust
  Lambda handlers; keep AWS SDK types outside the domain crate.
- Use TypeScript on Node.js 22 only for the Pi SDK Lambda. It receives a
  versioned request and returns schema-validated JSON; it cannot call Telegram,
  Google, SMTP, S3, or other mutation APIs.
- Treat `(chat_id, message_thread_id)` as the workflow session boundary. Private
  chat is OAuth-only, and non-forum groups are unsupported. Each topic hosts
  exactly one workflow and cannot start another after reaching a terminal state.
  The topic remains available for read-only questions about retained session
  history; new or changed work requires a new topic.
- Keep BotFather privacy mode enabled and require users to mention Novus or reply
  to it on every bot-directed turn, including attachment uploads. Persist every
  image the bot receives for the workflow so later tagged turns can reuse earlier
  images. The bot cannot recover arbitrary topic messages that Telegram never
  delivered, so Task 1 must verify the exact mention, caption, and reply behavior.
- Persist workflow state, optimistic versions, confirmation revisions, audit
  records, idempotency reservations, usage counters, and conversation pointers
  in DynamoDB. Store raw documents, generated artifacts, and large sanitized
  conversation payloads in S3.
- Bind every confirmation atomically to the workflow revision, preview digest,
  mutation target, workflow owner, confirming participant, and expiry. Reject
  stale or replayed callbacks.
- Use the workflow owner's Google account for Google operations, while allowing
  any currently approved group participant to contribute, correct, confirm, or
  cancel.
- Normalize attachments behind one contract and add formats incrementally.
  MIME sniffing, decompression limits, macro rejection, and resource limits are
  required before Office formats are enabled.
- Use `docs/sample-quotation.pdf` as the immutable Version 1 visual/layout
  reference. Renderer output is configuration-driven and tested with golden
  visual fixtures; the source PDF is never modified in place. Overflow continues
  onto additional pages without shrinking or truncating financial data.
- Standardize Rust Lambda packaging on `cargo-lambda` through SAM. Select a
  Lambda-compatible PDF renderer during the layout spike, using `printpdf` if it
  passes build, font, visual-fidelity, memory, and cold-start checks.
- Use a cost-effective basic-reasoning model rather than a deep-thinking model.
  Production selection still requires reliable schema-valid output, acceptable
  fixture accuracy and latency, Pi compatibility, and provider cost.
- Check approved-group membership live at every confirmation and cancellation.
  When the Telegram membership request times out or loses its connection, permit
  positive evidence for the same forum and participant for less than 30 minutes;
  confirmation and cancellation use the same fail-closed cache policy.
- Obtain development credentials and final company/signature assets incrementally
  during implementation. Mocks may unblock early work, but real-provider tests
  and deployments remain gated on the applicable credentials or assets.
- Keep application usage counters authoritative for budget enforcement because
  AWS Budgets data can lag. An approved Phase 0 ADR defines isolated
  per-environment budget guards, consistency, least-privilege access, shared-cost
  attribution, and fail-closed behavior. Each environment has an independent
  ₹300 monthly cap.
- Use real-provider feasibility checks early, local emulators for fast feedback,
  and staging for IAM, networking, quota, and provider-parity proof.

## Dependency Graph

```text
Feasibility spikes and implementation validation
    ├── Telegram forum behavior and topic lifecycle
    ├── Rust 1.97 Lambda/SAM build
    ├── OAuth and SMTP provider behavior
    ├── AI model compatibility and fixture benchmark
    ├── quotation layout/overflow contract
    └── resource-level AWS cost forecast
              │
              ▼
Workspace, configuration, and versioned contracts
              │
              ▼
Pure domain state machine, authorization, confirmation, retries, cost policy
              │
              ▼
Persistence contracts, conditional writes, and minimal local AWS services
              │
              ▼
Telegram intake + private OAuth vertical slice
              │
              ▼
Secure intake + Pi extraction + minimal Step Functions confirmation slice
              │
              ├── Quotation → Google → PDF → S3/Drive → Telegram
              ├── Calendar preview → confirmed event
              └── Email preview → confirmed SMTP send
              │
              ▼
Step Functions, local sandbox, observability, cost guard, E2E, deployment gates
```

## Task List

### Phase 0: Fail-Fast Feasibility

- [ ] Task 1: Validate tagged Telegram forum turns and retained image behavior
- [x] Task 2: Freeze one-workflow-per-topic and confirmation semantics
- [ ] Task 3: Prove Rust 1.97 Lambda builds through SAM with `cargo-lambda`
- [ ] Task 4: Analyze quotation pagination and select a Lambda-compatible renderer
- [ ] Task 5: Benchmark basic-reasoning AI models and Pi compatibility
- [ ] Task 6: Validate Google OAuth lifecycle
- [ ] Task 7: Validate Hostinger SMTP ambiguity behavior
- [ ] Task 8: Forecast AWS costs and decide per-environment budget control

### Checkpoint: Feasibility Approved

- [ ] All spike reports and decisions are recorded
- [ ] No unresolved blocker invalidates the PRD architecture
- [ ] Human approves the contracts, cost forecast, and provider choices

### Phase 1: Workspace and Shared Contracts

- [ ] Task 9: Initialize the repository and Rust workspace
- [ ] Task 10: Scaffold the TypeScript agent workspace
- [ ] Task 11: Define validated deployment configuration
- [ ] Task 11A: Implement the configuration loader
- [ ] Task 12: Define shared workflow and AI contracts
- [ ] Task 13: Establish baseline CI quality gates

### Checkpoint: Foundation Green

- [ ] Rust, TypeScript, and schema checks pass
- [ ] CI and local commands match the PRD
- [ ] Shared contracts are reviewed before adapter work begins

### Phase 2: Domain and Persistence Safety

- [x] Task 14: Implement domain identities and topic routing
- [x] Task 15: Implement the workflow state machine
- [x] Task 16: Implement live membership authorization and revision-bound confirmation
- [x] Task 17: Implement idempotency and retry policy
- [x] Task 18: Implement the budget decision policy
- [x] Task 19: Define persistence ports and DynamoDB keys
- [x] Task 20: Implement conditional workflow and idempotency storage
- [ ] Task 21: Implement object, history, and secret storage

### Checkpoint: Safety Core Proven

- [ ] Domain branch coverage meets the PRD target
- [ ] Concurrency tests prove one accepted transition/reservation
- [ ] Stale confirmations, duplicate events, and ambiguous writes fail safely

### Phase 3: Telegram and OAuth Vertical Slice

- [x] Task 22: Implement Telegram webhook verification and normalization
- [x] Task 23: Implement topic commands, callbacks, and safe delivery
- [x] Task 24: Implement private Google OAuth onboarding
- [x] Task 25: Wire webhook and OAuth Lambda entry points

### Checkpoint: Intake and Identity Work Locally

- [ ] A forum mention creates one durable workflow
- [ ] Private/non-forum workflow attempts are rejected
- [ ] OAuth data appears only in private flow and redacted logs

### Phase 4: Secure Intake, AI, and Confirmation

- [x] Task 26: Implement attachment collection and limits
- [x] Task 27: Implement secure image and PDF normalization
- [x] Task 28: Add CSV, Word, and Excel normalization safely
- [x] Task 29: Implement the Pi session factory and safety boundary
- [x] Task 30: Implement schema-validated extraction and history rehydration
- [x] Task 31: Implement resumable clarification, preview, and confirmation

### Checkpoint: Read-Only Workflow Slice

- [ ] A topic workflow reaches confirmation without external mutation
- [ ] Invalid AI output cannot advance state
- [ ] Restarted invocations continue from sanitized persisted history

### Phase 5: Quotation Vertical Slice

- [x] Task 32: Implement new Google Sheet and Doc creation
- [x] Task 33: Implement the paginated Version 1 Rust quotation renderer
- [x] Task 34: Implement S3, Drive, and topic artifact delivery
- [x] Task 34A: Package quotation workflow Lambda handlers
- [x] Task 35: Implement confirmed existing-file modification

### Checkpoint: Quotation Flow Complete

- [ ] Confirmed quotation completes without duplicate writes
- [ ] Visual regression and overflow cases pass
- [ ] Production quotation deployment remains gated on final signature/details

### Phase 6: Calendar and Email Slices

- [x] Task 36: Implement the confirmed Calendar workflow
- [x] Task 37: Implement the confirmed Hostinger email workflow
- [ ] Task 37A: Package Google, Calendar, and email Lambda handlers

### Checkpoint: External Actions Proven

- [ ] Calendar invitation choice is honored
- [ ] SMTP ambiguity enters manual review instead of retrying blindly
- [ ] Fault injection proves accepted actions are not duplicated

### Phase 7: Orchestration and Infrastructure

- [ ] Task 38: Implement resumable Step Functions workflows
- [ ] Task 39: Define core SAM resources and function packaging
- [ ] Task 39A: Add IAM and isolated environment controls
- [ ] Task 40: Complete the local AWS sandbox
- [ ] Task 41: Implement cost enforcement and observability

### Checkpoint: Infrastructure Ready

- [ ] SAM validates and builds all functions
- [ ] Local wait/resume/timeout/retry/failure paths pass
- [ ] IAM, encryption, logs, alarms, and budget controls are reviewed

### Phase 8: System Verification and Release

- [ ] Task 42: Add integration and end-to-end suites
- [ ] Task 43: Add deployment and rollback gates
- [ ] Task 44: Verify staging and prepare production approval

### Checkpoint: Version 1 Ready

- [ ] Every PRD success criterion has linked evidence
- [ ] All required commands and critical-path tests pass
- [ ] Final signature/company details and production credentials are configured
- [ ] Human approves production deployment

## Parallelization Opportunities

- After Task 2 freezes semantics, Tasks 3-8 are independent feasibility tracks.
- After Tasks 11-12 land, pure domain Tasks 14-18 can proceed in parallel if
  each agent owns a separate module and no one changes shared contracts.
- Telegram, OAuth, and Pi implementation may proceed in parallel after the
  domain and persistence interfaces are frozen.
- Google, PDF, Calendar, and email adapter work may proceed in parallel only
  after confirmation and idempotency contracts are stable.
- Read-only reviewers may validate security, cost, and test coverage at any
  checkpoint. Use one writer for shared schema, SAM, and state-machine files.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Telegram privacy mode prevents arbitrary history retrieval | High | Require a mention/reply on every bot-directed turn, persist each delivered image, and verify caption/reply behavior in a real forum during Task 1 |
| A stale or replayed confirmation mutates the owner's Google data | High | Revision/digest-bound confirmations plus conditional writes and membership checks |
| Quotation line items overflow the reference page | High | Continue onto additional pages and lock paginated golden visual tests before renderer work |
| Office documents exhaust Lambda resources or contain active content | High | MIME sniffing, macro rejection, decompression/page/row limits, and per-format fixtures |
| Google/SMTP operations duplicate after ambiguous timeouts | High | Reserve idempotency records before mutation; ambiguous acceptance enters manual review |
| Pi/model output is invalid or financially inaccurate | High | Early fixture benchmark, strict schema validation, human confirmation, no mutation on invalid output |
| Per-environment budget control misses or misattributes account-level charges | High | Phase 0 ADR, isolated authoritative aggregates, conservative shared-cost allocation, conditional updates, fail-closed intake guards, and deploy gates |
| DynamoDB conversation history exceeds item limits | Medium | Store ordered metadata/pointers in DynamoDB and large sanitized payloads in S3 |
| Local emulators differ from AWS | Medium | Treat sandbox as fast feedback and repeat IAM/network/quota checks in staging |
| Credentials or company assets arrive later in development | Medium | Use mocks early, request each dependency before its provider test, and retain explicit staging/production gates |

## Resolved Product and Implementation Decisions

1. Each Telegram topic hosts exactly one workflow. After a terminal outcome,
   approved participants may continue asking read-only questions about retained
   session history, but new or changed work requires a new topic.
2. BotFather privacy mode remains enabled. Users must mention Novus or reply to
   it on every bot-directed turn, including image uploads. Novus persists images
   it receives so later tagged turns in the workflow can use them; it cannot read
   older messages that Telegram never delivered to the bot.
3. Quotation overflow continues onto additional pages. Financial data must never
   be shrunk into illegibility or truncated.
4. Task 4 will select a Lambda-compatible Rust PDF renderer. `printpdf` is the
   default if it passes the Lambda build and runtime acceptance checks.
5. Rust Lambda builds use `cargo-lambda` through AWS SAM.
6. Model selection is limited to cost-effective basic-reasoning models; deep
   thinking is unnecessary. Fixture accuracy, schema validity, latency, Pi
   compatibility, and cost remain acceptance criteria.
7. Development credentials and final company/signature assets will be supplied
   incrementally during development, before the tests or deployment gates that
   require them.
8. Approved-group membership is checked live for every confirmation and
   cancellation. If the membership request times out or its connection fails,
   positive evidence for the same forum and participant may be reused for less
   than 30 minutes. Confirmation and cancellation use the same policy.
9. Corrections may return the workflow to calculation or drafting and invalidate
   the current preview, but only an approved participant may correct it.
