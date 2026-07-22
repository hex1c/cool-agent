<!-- markdownlint-disable MD013 -->

# Task Checklist: Novus Telegram Operations Worker

Do not begin implementation until the Phase 0 feasibility checkpoint and the
open questions in `tasks/plan.md` have been reviewed by a human. Every task must
also satisfy the project-wide Definition of Done described in `tasks/plan.md`.

## Phase 0: Fail-Fast Feasibility

## Task 1: Validate Telegram forum-topic behavior

**Description:** Run a controlled test with the development Novus bot in a forum
supergroup to prove which updates Telegram delivers and which identifiers remain
stable.

**Acceptance criteria:**

- [ ] Evidence covers mentions, untagged attachments, follow-up commands, inline callbacks, duplicate updates, topic IDs, privacy mode, and removed members.
- [ ] The report identifies the required BotFather privacy/admin configuration.
- [x] No production credential or real customer data is committed.

**Verification:**

- [ ] Manual check: replay the recorded sanitized updates and confirm their documented fields.
- [x] Human approves `docs/spikes/telegram-forum.md`.

**Dependencies:** None; requires development Telegram credentials.

**Files likely touched:**

- `docs/spikes/telegram-forum.md`
- `tests/fixtures/telegram/forum-spike.json`

**Estimated scope:** Small: 2 files

## Task 2: Freeze topic lifecycle and confirmation semantics

**Description:** Record whether topics are single-workflow and define the exact
binding and replay rules for confirmations and callbacks.

**Acceptance criteria:**

- [x] The decision defines topic reuse, terminal-topic behavior, and late-message handling.
- [x] Confirmation binds workflow revision, preview digest, target, owner, actor, and expiry.
- [x] Live membership checks and outage behavior are explicitly decided.

**Verification:**

- [x] Human approves the ADR and corresponding PRD clarification if needed.

**Dependencies:** Task 1

**Files likely touched:**

- `docs/adr/0001-topic-and-confirmation-semantics.md`
- `docs/prd/telegram-operations-worker.md`

**Estimated scope:** Small: 2 files

## Task 3: Prove Rust 1.97 Lambda builds through AWS SAM

**Description:** Create a minimal Rust Lambda spike and choose a repeatable
`provided.al2023` build path before the full workspace depends on it.

**Acceptance criteria:**

- [x] Rust 1.97 produces a Lambda artifact with the expected `bootstrap` layout.
- [x] `sam validate --lint`, `sam build`, and `sam local invoke` succeed.
- [x] The chosen `cargo-lambda` or custom Makefile approach is documented.

**Verification:**

- [x] Tests pass: `sam local invoke RustBuildSpike --template .aws-sam/build/template.yaml --event tests/fixtures/lambda/ping.json`
- [x] Build succeeds: `sam build --template-file infrastructure/spikes/rust-lambda.yaml`

**Dependencies:** None

**Files likely touched:**

- `infrastructure/spikes/rust-lambda.yaml`
- `functions/build-spike/Cargo.toml`
- `functions/build-spike/src/main.rs`
- `tests/fixtures/lambda/ping.json`
- `docs/spikes/rust-sam-build.md`

**Estimated scope:** Medium: 5 files

## Task 4: Analyze the Version 1 quotation layout

**Description:** Convert `docs/sample-quotation.pdf` into an implementation-ready
layout contract without modifying the source PDF.

**Acceptance criteria:**

- [x] The specification records page geometry, fields, coordinates, fonts, spacing, table behavior, logo/signature slots, and totals.
- [x] Overflow behavior is approved for long descriptions and line-item counts.
- [x] A sanitized normal-case golden fixture is defined for later visual comparison.

**Verification:**

- [ ] Manual check: overlay the field map on the reference PDF.
- [x] Human approves `docs/quotation-layout.md` and the overflow decision.

**Dependencies:** None

**Files likely touched:**

- `docs/quotation-layout.md`
- `tests/fixtures/quotation/layout-v1.json`
- `tests/fixtures/quotation/golden-v1.pdf`

**Estimated scope:** Medium: 3 files

## Task 5: Benchmark candidate AI models and Pi compatibility

**Description:** Verify Pi SDK integration and compare Fireworks Kimi K2.5 with
the approved pinned OpenAI GPT-5.6 option on sanitized representative inputs.

**Acceptance criteria:**

- [ ] Both candidates are measured for schema validity, extraction/arithmetic accuracy, latency, and provider cost.
- [ ] Invalid model output is shown to fail closed.
- [ ] The selected model and thresholds are recorded without committing API keys.

**Verification:**

- [ ] Tests pass: `npm --prefix services/model-eval test -- --run`
- [ ] Human approves `docs/spikes/model-evaluation.md`.

**Dependencies:** None; requires model credentials and approved thresholds.

**Files likely touched:**

- `services/model-eval/package.json`
- `services/model-eval/src/evaluate.ts`
- `services/model-eval/test/evaluate.test.ts`
- `tests/fixtures/pi/evaluation-cases.json`
- `docs/spikes/model-evaluation.md`

**Estimated scope:** Medium: 5 files

## Task 6: Validate Google OAuth lifecycle

**Description:** Use a development OAuth app to prove scopes, PKCE/state,
callback URIs, refresh, revocation, consent-screen constraints, and personal
Google-account behavior.

**Acceptance criteria:**

- [x] Exact Drive, Sheets, Docs, and Calendar scopes are recorded; Gmail is absent.
- [x] State replay, expiry, revocation, and refresh behavior are demonstrated.
- [x] Test-user/verification requirements and secret-storage costs are documented.

**Verification:**

- [x] Manual check: connect, refresh, revoke, and reconnect a development account.
- [x] Human approves `docs/spikes/google-oauth.md`.

**Dependencies:** None; requires development Google OAuth credentials.

**Files likely touched:**

- `docs/spikes/google-oauth.md`
- `tests/fixtures/oauth/callback-cases.json`

**Estimated scope:** Small: 2 files

## Task 7: Validate Hostinger SMTP ambiguity behavior

**Description:** Determine what Hostinger returns for accepted, rejected, timed
out, and connection-dropped sends so retries cannot duplicate email.

**Acceptance criteria:**

- [x] The report records stable message identifiers and observable acceptance points.
- [x] Timeout cases are classified as retryable or manual-review ambiguity.
- [x] A safe idempotency strategy is approved.

**Verification:**

- [x] Manual check: test against a non-production mailbox and verify received-message counts.
- [x] Human approves `docs/spikes/hostinger-smtp.md`.

**Dependencies:** None; requires development Hostinger credentials.

**Files likely touched:**

- `docs/spikes/hostinger-smtp.md`
- `tests/fixtures/smtp/provider-cases.json`

**Estimated scope:** Small: 2 files

## Task 8: Forecast AWS costs and decide per-environment budget control

**Description:** Calculate fixed and usage-based monthly costs for isolated dev,
staging, and production stacks under independent ₹300 caps and a ₹900 aggregate
account ceiling.

**Acceptance criteria:**

- [x] The worksheet includes Step Functions, Lambda, API Gateway, DynamoDB, S3, Parameter Store/KMS, logs, alarms, retention growth, and safety margin.
- [x] An approved ADR defines isolated per-environment usage aggregation, shared-cost attribution, ownership, consistency, fail-closed behavior, and least-privilege access.
- [x] Warning, suspension, and deployment-block thresholds remain feasible; an unaffordable design produces a PRD change proposal.

**Verification:**

- [x] Manual check: recalculate with PRD expected usage and worst-case attachment limits.
- [x] Human approves the per-environment forecasts and budget-control ADR.

**Dependencies:** None

**Files likely touched:**

- `docs/cost/aws-monthly-forecast.md`
- `docs/cost/aws-monthly-forecast.csv`
- `docs/adr/0002-shared-budget-control.md`

**Estimated scope:** Medium: 3 files

## Checkpoint: Feasibility Approved

- [ ] Tasks 1-8 are complete or explicitly waived by the human.
- [ ] Open questions in `tasks/plan.md` have decisions.
- [ ] The architecture and budget remain viable.

## Phase 1: Workspace and Shared Contracts

## Task 9: Initialize the repository and Rust workspace

**Description:** Establish git, the Rust 1.97 workspace, formatting/lint policy,
and empty domain/application crates without implementing business behavior.

**Acceptance criteria:**

- [x] Rust 1.97 is pinned and all workspace crates compile.
- [x] Production lint policy denies warnings and disallows unchecked panic patterns.
- [x] Generated artifacts, credentials, and local emulator data are ignored.

**Verification:**

- [x] Build succeeds: `cargo build --workspace --all-features`
- [x] Checks pass: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`

**Dependencies:** Feasibility checkpoint, Task 3

**Files likely touched:**

- `Cargo.toml`
- `rust-toolchain.toml`
- `.gitignore`
- `crates/domain/Cargo.toml`
- `crates/application/Cargo.toml`

**Estimated scope:** Medium: 5 files

## Task 10: Scaffold the TypeScript agent workspace

**Description:** Create the strict Node.js 22 workspace for the Pi Lambda with
format, lint, test, and build commands matching the PRD.

**Acceptance criteria:**

- [x] TypeScript strict mode is enabled and untyped `any` is rejected at boundaries.
- [x] Vitest and formatting/lint scripts run from the repository root.
- [x] A stub Lambda handler builds without external mutation tools.

**Verification:**

- [x] Checks pass: `npm ci && npm run format:check --workspace services/agent-harness && npm run lint --workspace services/agent-harness`
- [x] Tests/build pass: `npm test --workspace services/agent-harness -- --run && npm run build --workspace services/agent-harness`

**Dependencies:** Feasibility checkpoint

**Files likely touched:**

- `package.json`
- `package-lock.json`
- `services/agent-harness/package.json`
- `services/agent-harness/tsconfig.json`
- `services/agent-harness/src/index.ts`

**Estimated scope:** Medium: 5 files

## Task 11: Define validated deployment configuration

**Description:** Define non-secret company/environment YAML and a JSON Schema
covering PRD configuration while representing secrets only by references.

**Acceptance criteria:**

- [x] Dev, staging, and production overrides validate against one versioned schema.
- [x] Template, model, attachment, timeout, retry, budget, Telegram, OAuth, SMTP, Drive, and company fields are represented.
- [x] Invalid or secret-bearing configuration fails validation.

**Verification:**

- [x] Manual check: base and all three environment files validate against `config/schema/company.schema.json`.

**Dependencies:** Tasks 8-10

**Files likely touched:**

- `config/schema/company.schema.json`
- `config/company.yaml`
- `config/environments/dev.yaml`
- `config/environments/staging.yaml`
- `config/environments/production.yaml`

**Estimated scope:** Medium: 5 files

## Task 11A: Implement the configuration loader

**Description:** Load company and environment YAML into typed Rust configuration,
validate it against the shared schema, and reject unresolved secret values.

**Acceptance criteria:**

- [x] Environment overrides merge deterministically without crossing environments.
- [x] Invalid, incomplete, or secret-bearing values return typed startup errors.
- [x] Application services receive typed configuration independent of AWS SDK types.

**Verification:**

- [x] Tests pass: `cargo test -p application config_loader`

**Dependencies:** Task 11

**Files likely touched:**

- `crates/application/src/config.rs`
- `crates/application/src/config_loader.rs`
- `crates/application/tests/config_loader.rs`
- `tests/fixtures/config/valid.yaml`
- `tests/fixtures/config/invalid.yaml`

**Estimated scope:** Medium: 5 files

## Task 12: Define shared workflow and AI contracts

**Description:** Freeze versioned JSON contracts between Rust workflow services,
Step Functions, and the TypeScript Pi Lambda.

**Acceptance criteria:**

- [x] Contracts cover extraction, calculation, drafting, Calendar data, history checkpoints, model/prompt versions, and typed errors.
- [x] Rust and TypeScript validate the same schema fixtures.
- [x] Contract changes require an explicit schema-version update.

**Verification:**

- [x] Tests pass: `cargo test -p domain contracts`
- [x] Tests pass: `npm test --workspace services/agent-harness -- --run contracts`

**Dependencies:** Tasks 9-11A

**Files likely touched:**

- `config/schema/ai-request.schema.json`
- `config/schema/ai-response.schema.json`
- `crates/domain/src/contracts.rs`
- `services/agent-harness/src/contracts.ts`
- `tests/fixtures/contracts/v2.json`

**Estimated scope:** Medium: 5 files

## Task 13: Establish baseline CI quality gates

**Description:** Add independent Rust, TypeScript, and SAM checks that invoke the
same commands developers run locally.

**Acceptance criteria:**

- [x] Pull requests run formatting, lint, unit tests, builds, and schema validation.
- [x] Dependency caching does not cache secrets or generated credentials.
- [x] A deliberately broken fixture proves each job fails correctly.

**Verification:**

- [ ] Manual check: all CI jobs pass on the foundation branch.

**Dependencies:** Tasks 9-12, including Task 11A

**Files likely touched:**

- `.github/workflows/rust.yml`
- `.github/workflows/typescript.yml`
- `.github/workflows/sam.yml`
- `.github/dependabot.yml`

**Estimated scope:** Medium: 4 files

## Checkpoint: Foundation Green

- [ ] Tasks 9-13 pass locally and in CI.
- [ ] Shared schemas are reviewed and versioned.
- [ ] No provider credential is present in repository history.

## Phase 2: Domain and Persistence Safety

## Task 14: Implement domain identities and topic routing

**Description:** Add explicit identifiers, money/currency types, attachment
kinds, and pure routing for forum, private, and unsupported chats.

**Acceptance criteria:**

- [x] Raw strings are not used for workflow, participant, attachment, or session identities.
- [x] Routing uses `(chat_id, message_thread_id)` and cannot cross-associate topics.
- [x] Private chats expose OAuth-only routing; non-forum workflows are rejected.

**Verification:**

- [x] Tests pass: `cargo test -p domain`

**Dependencies:** Foundation Green checkpoint, Task 12

**Files likely touched:**

- `crates/domain/src/identity.rs`
- `crates/domain/src/money.rs`
- `crates/domain/src/attachment.rs`
- `crates/domain/src/routing.rs`
- `crates/domain/src/lib.rs`

**Estimated scope:** Medium: 5 files

## Task 15: Implement the workflow state machine

**Description:** Model legal stages, terminal states, waits, actor attribution,
and expiry as pure domain transitions.

**Acceptance criteria:**

- [x] Every PRD progress stage and terminal outcome maps to a typed state.
- [x] Illegal, terminal, expired, and race-lost transitions return typed errors.
- [x] Every transition records owner, actor, source message, revision, and time.

**Verification:**

- [x] Tests pass: `cargo test -p domain workflow_state`
- [x] Coverage check: workflow state transitions have 100% branch coverage.

**Dependencies:** Task 14

**Files likely touched:**

- `crates/domain/src/workflow.rs`
- `crates/domain/src/transition.rs`
- `crates/domain/tests/workflow_transitions.rs`

**Estimated scope:** Medium: 3 files

## Task 16: Implement authorization and revision-bound confirmation

**Description:** Enforce group participation, owner-bound Google access, and
atomic preview confirmation semantics independently of adapters.

**Acceptance criteria:**

- [x] Any approved participant may act, but Google operations remain bound to the workflow owner.
- [x] Confirmation validates revision, digest, target, actor, owner, membership, and expiry.
- [x] Stale, replayed, corrected, expired, or cancelled confirmations fail safely.

**Verification:**

- [x] Tests pass: `cargo test -p domain --test authorization_policy && cargo test -p domain --test confirmation_policy`
- [x] Coverage check: authorization and confirmation reach 100% branch coverage.

**Dependencies:** Tasks 2, 15

**Files likely touched:**

- `crates/domain/src/authorization.rs`
- `crates/domain/src/confirmation.rs`
- `crates/domain/tests/confirmation_policy.rs`

**Estimated scope:** Medium: 3 files

## Task 17: Implement idempotency and retry policy

**Description:** Define stable operation keys, attempt limits, backoff metadata,
and manual-review handling before any external write adapter exists.

**Acceptance criteria:**

- [x] Stable inputs produce stable keys, distinct targets produce distinct keys, and no operation exceeds three attempts.
- [x] Ambiguous acceptance cannot be retried automatically.
- [x] A reusable executor persists attempts, applies exponential backoff with jitter, and emits terminal failure after exhaustion.

**Verification:**

- [x] Tests pass: `cargo test -p domain --test idempotency_policy`
- [x] Tests pass: `cargo test -p application --test external_operation`

**Dependencies:** Task 14

**Files likely touched:**

- `crates/domain/src/idempotency.rs`
- `crates/domain/src/retry.rs`
- `crates/domain/tests/idempotency_policy.rs`
- `crates/application/src/external_operation.rs`
- `crates/application/tests/external_operation.rs`

**Estimated scope:** Medium: 5 files

## Task 18: Implement the budget decision policy

**Description:** Implement pure warn, suspend, hard-cap, confirmed-operation, and
deployment decisions from current and projected usage.

**Acceptance criteria:**

- [x] Configured 80%, 90%, and ₹300 boundaries behave exactly as the PRD specifies.
- [x] Already-confirmed work proceeds only while projected cost remains below the cap.
- [x] Status and permitted retrieval remain available during intake suspension.

**Verification:**

- [x] Tests pass: `cargo test -p domain --test cost_policy`
- [x] Coverage check: cost-stop policy reaches 100% branch coverage.

**Dependencies:** Tasks 8, 14

**Files likely touched:**

- `crates/domain/src/cost.rs`
- `crates/domain/tests/cost_policy.rs`

**Estimated scope:** Small: 2 files

## Task 19: Define persistence ports and DynamoDB keys

**Description:** Define repository traits and a reviewed key design only after
topic, revision, history, audit, idempotency, and cost semantics are stable.

**Acceptance criteria:**

- [x] Ports cover workflows, confirmations, idempotency, history, OAuth state, usage, objects, and secrets.
- [x] Key design supports conditional writes, TTL, pagination, and three environments without cross-talk.
- [x] Large conversation content is stored by S3 pointer rather than in one DynamoDB item.

**Verification:**

- [x] Tests pass: `cargo test -p application --test repository_contracts`
- [x] Human approves `docs/architecture/dynamodb-keys.md`.

**Dependencies:** Tasks 8, 15-18

**Files likely touched:**

- `crates/application/src/ports.rs`
- `crates/application/src/repositories.rs`
- `docs/architecture/dynamodb-keys.md`
- `tests/fixtures/storage/key-cases.json`

**Estimated scope:** Medium: 4 files

## Task 20: Implement conditional workflow and idempotency storage

**Description:** Implement DynamoDB workflow, confirmation, audit, and
idempotency repositories with optimistic concurrency and atomic reservations.

**Acceptance criteria:**

- [x] Concurrent transition attempts yield exactly one accepted write.
- [x] External actions require an idempotency reservation before invocation.
- [x] Audit entries retain owner, actor, source message, revision, and resource IDs.

**Verification:**

- [x] Tests pass against DynamoDB Local: `cargo test -p storage dynamodb --features integration`
- [x] Fault check: concurrent confirmation test accepts one mutation reservation.

**Dependencies:** Task 19

**Files likely touched:**

- `crates/storage/Cargo.toml`
- `crates/storage/src/dynamodb.rs`
- `crates/storage/src/workflows.rs`
- `crates/storage/tests/dynamodb_integration.rs`
- `infrastructure/local/storage-compose.yaml`

**Estimated scope:** Medium: 5 files

## Task 21: Implement object, history, and secret storage

**Description:** Implement S3 raw/artifact/history storage and Parameter Store
`SecureString` access behind application ports, including sanitization and size
guards.

**Acceptance criteria:**

- [x] Raw inputs, generated PDFs, and large sanitized histories use separate prefixes. Per-class IAM access policies are deferred to Task 39A under an approved exception.
- [x] OAuth/SMTP/model secrets are retrieved only through secret references and never logged.
- [x] Presigned links obey configured expiry and sanitized history excludes credential material.
- [x] Object publication coordinator proves S3 acceptance before publishing DynamoDB object/history pointers, disambiguates ambiguous S3 outcomes, and fails closed when the object cannot be confirmed.
- [x] Pagination tokens are authenticated (HMAC-SHA256) and bound to the exact table, repository/query family, scan direction, workflow partition, and sort-key family.

**Approved security exceptions:**

1. **Per-class IAM access policies** — Deferred to Task 39A. No shared environment should deploy before Task 39A provides reviewed prefix-scoped IAM and environment-isolation controls.
2. **In-process trust boundary** — `SecretValue::new` is `#[doc(hidden)] pub` and `HistorySanitizer::from_secret_values` is `pub` because Rust's visibility system does not allow `pub(crate)` construction across crate boundaries (the `SecretProvider` adapter is in the storage crate). The threat model treats the Lambda process boundary as the trust boundary: all deployed code is trusted, no untrusted code runs in-process. Defense is against external input (webhook payloads, user messages), not in-process code. A future dedicated secrets-runtime crate could provide sealed provenance if cross-crate trust separation is required.

**Verification:**

- [x] Unit tests pass: `cargo test -p storage --lib`
- [x] Live S3/SSM integration tests pass against LocalStack: `LOCALSTACK_ENDPOINT=http://127.0.0.1:4566 cargo test -p storage --test object_integration --features integration -- --nocapture`
- [x] Live DynamoDB Local integration tests pass: `cargo test -p storage --test dynamodb_integration --features integration -- --nocapture`
- [x] Publication coordinator tests pass: `cargo test -p application publication --lib`

**Dependencies:** Task 19

**Files likely touched:**

- `crates/storage/src/s3.rs`
- `crates/storage/src/history.rs`
- `crates/storage/src/secrets.rs`
- `crates/storage/tests/object_integration.rs`
- `infrastructure/local/storage-compose.yaml`

**Estimated scope:** Medium: 5 files

## Checkpoint: Safety Core Proven

- [x] Tasks 14-21 pass required branch and concurrency checks.
- [x] Persistence contracts are frozen before handlers and adapters consume them.
- [x] Security reviewer approves confirmation, idempotency, and secret boundaries.

## Phase 3: Telegram and OAuth Vertical Slice

## Task 22: Implement Telegram webhook verification and normalization

**Description:** Convert authenticated Telegram updates into typed internal
events while deduplicating `update_id` and rejecting unsupported chat types.

**Acceptance criteria:**

- [x] Invalid webhook secret tokens are rejected before parsing business events.
- [x] Recorded fixtures normalize mentions, replies, callbacks, media, and commands correctly.
- [x] Duplicate and cross-topic updates cannot create or mutate another workflow.

**Verification:**

- [x] Tests pass: `cargo test -p telegram webhook normalize deduplicate`

**Dependencies:** Safety Core Proven checkpoint, Tasks 1, 14, 17, 20

**Files likely touched:**

- `crates/telegram/Cargo.toml`
- `crates/telegram/src/webhook.rs`
- `crates/telegram/src/normalize.rs`
- `crates/telegram/src/client.rs`
- `crates/telegram/tests/update_contracts.rs`

**Estimated scope:** Medium: 5 files

## Task 23: Implement topic commands, callbacks, and safe delivery

**Description:** Parse the PRD command set, validate callbacks, and structurally
separate topic workflow delivery from private OAuth delivery.

**Acceptance criteria:**

- [x] Start mentions and follow-up `/done`, `/status`, `/correct`, `/confirm`, `/stop` target only the current topic workflow.
- [x] Callback replay and stale preview revisions are rejected.
- [x] OAuth material cannot pass through topic delivery; Telegram sends use the shared three-attempt executor and persist terminal failures.

**Verification:**

- [x] Tests pass: `cargo test -p telegram commands callbacks delivery_privacy`

**Dependencies:** Tasks 16, 22

**Files likely touched:**

- `crates/telegram/src/commands.rs`
- `crates/telegram/src/callbacks.rs`
- `crates/telegram/src/delivery.rs`
- `crates/telegram/src/privacy.rs`
- `crates/telegram/tests/delivery_contracts.rs`

**Estimated scope:** Medium: 5 files

## Task 24: Implement private Google OAuth onboarding

**Description:** Implement PKCE/state creation, callback verification, token
exchange/storage, status, disconnect, refresh, and reauthorization for private
chat only.

**Acceptance criteria:**

- [x] State is short-lived, single-use, participant-bound, and replay-resistant.
- [x] Refresh tokens enter separate Parameter Store `SecureString` values; token
  values never enter topics or logs.
- [x] Revocation pauses Google-dependent work without deleting history; token endpoint calls use the shared retry/ambiguity executor.

**Verification:**

- [x] Tests pass: `cargo test -p oauth --all-features`
- [x] Integration check: mocked connect, refresh, revoke, and reconnect pass.

**Dependencies:** Tasks 6, 21, 23

**Files likely touched:**

- `crates/oauth/Cargo.toml`
- `crates/oauth/src/flow.rs`
- `crates/oauth/src/tokens.rs`
- `crates/oauth/src/redaction.rs`
- `crates/oauth/tests/oauth_contracts.rs`

**Estimated scope:** Medium: 5 files

## Task 25: Wire webhook and OAuth Lambda entry points

**Description:** Add thin Rust handlers that deserialize API Gateway events and
invoke the tested Telegram and OAuth application paths without domain logic.

**Acceptance criteria:**

- [x] Handlers return bounded, typed HTTP responses for success and failure.
- [x] Webhook acknowledgment does not wait on long-running workflow work.
- [x] OAuth callback responses and logs contain no tokens or secret values.

**Verification:**

- [x] Tests pass: `cargo test -p webhook-function -p oauth-function`
- [x] Local invoke succeeds for recorded webhook and OAuth fixtures.

**Dependencies:** Tasks 3, 22-24

**Files likely touched:**

- `functions/webhook/Cargo.toml`
- `functions/webhook/src/main.rs`
- `functions/oauth/Cargo.toml`
- `functions/oauth/src/main.rs`
- `tests/fixtures/lambda/oauth-callback.json`

**Estimated scope:** Medium: 5 files

## Checkpoint: Intake and Identity Work Locally

- [ ] A recorded forum mention creates one durable workflow.
- [ ] OAuth private flow completes against mocks with clean logs.
- [ ] Unsupported chats and replayed callbacks fail safely.

## Phase 4: Secure Intake, AI, and Confirmation

## Task 26: Implement attachment collection and limits

**Description:** Collect Telegram attachments into the current topic workflow,
validate count/size, and support `/done`, timeout, continuation, and stop.

**Acceptance criteria:**

- [ ] At most 10 attachments of at most 20 MB each are accepted and durably stored in S3 before normalization.
- [ ] Duplicate/concurrent uploads produce exactly one object with stable workflow, source-message, MIME, size, and checksum metadata.
- [ ] Thirty-minute timeout and participant continue/stop behavior are durable.

**Verification:**

- [ ] Tests pass: `cargo test -p application attachment_collection`
- [ ] Storage integration: accepted attachment fixture creates one canonical raw S3 object before normalization.

**Dependencies:** Intake and Identity checkpoint, Tasks 20-23, including Task 21

**Files likely touched:**

- `crates/application/src/attachments.rs`
- `crates/application/src/collection.rs`
- `crates/application/tests/attachment_collection.rs`
- `tests/fixtures/telegram/attachments.json`

**Estimated scope:** Medium: 4 files

## Task 27: Implement secure image and PDF normalization

**Description:** Normalize images and PDF pages for AI use with type detection,
page/pixel/decompressed-size limits, malformed-input handling, and bounded
Lambda resources.

**Acceptance criteria:**

- [ ] Content type is sniffed rather than trusted from filename or Telegram metadata.
- [ ] Malformed, oversized, excessive-page, and decompression-bomb fixtures fail safely.
- [ ] Normalized output contains no executable content and fits configured AI limits.

**Verification:**

- [ ] Tests pass: `cargo test -p application document_normalization image pdf`

**Dependencies:** Task 26; dependency additions require approval.

**Files likely touched:**

- `crates/application/src/normalization/mod.rs`
- `crates/application/src/normalization/image.rs`
- `crates/application/src/normalization/pdf.rs`
- `crates/application/tests/image_pdf_security.rs`
- `tests/fixtures/documents/README.md`

**Estimated scope:** Medium: 5 files

## Task 28: Add CSV, Word, and Excel normalization safely

**Description:** Extend the normalization contract to deterministic text/table
extraction while rejecting macros, archive bombs, excessive rows/cells, and
unsupported legacy content.

**Acceptance criteria:**

- [ ] CSV encoding/dialect, workbook row/cell, and Word page/text limits are configurable.
- [ ] Macro-bearing, encrypted, malformed, and decompression-bomb Office fixtures are rejected.
- [ ] `.doc`/`.xls` are enabled only if the approved library safely supports them; otherwise the PRD is amended before release.

**Verification:**

- [ ] Tests pass: `cargo test -p application document_normalization csv office`

**Dependencies:** Task 27; parser dependencies require approval.

**Files likely touched:**

- `crates/application/src/normalization/csv.rs`
- `crates/application/src/normalization/word.rs`
- `crates/application/src/normalization/excel.rs`
- `crates/application/tests/office_security.rs`
- `tests/fixtures/documents/office-cases.json`

**Estimated scope:** Medium: 5 files

## Task 29: Implement the Pi session factory and safety boundary

**Description:** Create in-memory Pi sessions with the selected configured model,
no built-in mutation tools, and only narrowly scoped extraction tools.

**Acceptance criteria:**

- [ ] `SessionManager.inMemory()` is used and process memory is not treated as durable.
- [ ] Built-in shell/filesystem mutation tools and external service tools are absent.
- [ ] Model credentials are loaded at runtime through the secret provider and never enter prompts or logs.

**Verification:**

- [ ] Tests pass: `npm test --workspace services/agent-harness -- --run session-factory`
- [ ] Build succeeds: `npm run build --workspace services/agent-harness`

**Dependencies:** Tasks 5, 10-12, 21

**Files likely touched:**

- `services/agent-harness/src/session.ts`
- `services/agent-harness/src/models.ts`
- `services/agent-harness/src/tools/extract-documents.ts`
- `services/agent-harness/test/session.test.ts`

**Estimated scope:** Medium: 4 files

## Task 30: Implement schema-validated extraction and history rehydration

**Description:** Invoke Pi with normalized input, validate versioned JSON output,
and round-trip sanitized ordered history and checkpoints through DynamoDB/S3.

**Acceptance criteria:**

- [ ] Output includes extraction and quotation calculation data, validates schema/business-rule presence, and cannot advance when invalid.
- [ ] Rehydration preserves ordered history, model/prompt versions, and sanitized checkpoints across invocations.
- [ ] AI retries persist attempt history; fixtures prove Rust does not independently recompute AI totals.

**Verification:**

- [ ] Tests pass: `npm test --workspace services/agent-harness -- --run extraction history`
- [ ] Integration check: two-invocation continuation fixture passes.

**Dependencies:** Tasks 12, 21, 27-29

**Files likely touched:**

- `services/agent-harness/src/handler.ts`
- `services/agent-harness/src/history.ts`
- `services/agent-harness/src/validation.ts`
- `services/agent-harness/test/extraction.test.ts`
- `services/agent-harness/test/history.test.ts`

**Estimated scope:** Medium: 5 files

## Task 31: Implement resumable clarification, preview, and confirmation

**Description:** Complete a read-only topic workflow through a minimal Step
Functions wait/resume path, revision-bound confirmation, and no external write.

**Acceptance criteria:**

- [ ] Missing data enters a durable 12-hour wait; restart, duplicate resume, timeout, and cancellation cases pass locally.
- [ ] Preview shows original/calculated values, assumptions, currency, taxes, and totals; corrections invalidate prior revisions.
- [ ] Any approved participant can confirm/cancel with audit attribution, and no external mutation occurs.

**Verification:**

- [ ] Tests pass: `cargo test -p application preview clarification confirmation`
- [ ] Step Functions Local check: minimal wait/resume/timeout fixture passes.

**Dependencies:** Tasks 16, 20, 23, 30

**Files likely touched:**

- `crates/application/src/resumable_confirmation.rs`
- `crates/application/tests/resumable_confirmation.rs`
- `infrastructure/statemachines/confirmation-slice.asl.json`
- `infrastructure/local/storage-compose.yaml`

**Estimated scope:** Medium: 4 files

## Checkpoint: Read-Only Workflow Slice

- [ ] Tasks 26-31 pass security and continuation tests.
- [ ] Invalid or stale input cannot cross the confirmation boundary.
- [ ] Human approves the first complete read-only topic flow.

## Phase 5: Quotation Vertical Slice

## Task 32: Implement new Google Sheet and Doc creation

**Description:** Add owner-authorized Drive/Sheets/Docs clients for confirmed
new-file creation with target-folder selection and idempotency.

**Acceptance criteria:**

- [ ] Company default, employee override, per-workflow override, My Drive, and Shared Drive are handled.
- [ ] No source or calculated amount is written before valid confirmation.
- [ ] Retries cannot create duplicate files or duplicate mutations.

**Verification:**

- [ ] Tests pass: `cargo test -p google create_file drive_destination`
- [ ] Integration check: mocked confirmed create returns one stable resource ID.

**Dependencies:** Read-Only Workflow checkpoint, Tasks 17, 21, 24, 31

**Files likely touched:**

- `crates/google/Cargo.toml`
- `crates/google/src/auth.rs`
- `crates/google/src/drive.rs`
- `crates/google/src/sheets_docs.rs`
- `crates/google/tests/create_contracts.rs`

**Estimated scope:** Medium: 5 files

## Task 33: Implement the Version 1 Rust quotation renderer

**Description:** Render configuration and confirmed quotation data into a PDF
matching the approved sample layout without modifying the reference PDF.

**Acceptance criteria:**

- [ ] Required fields, currency, taxes, totals, terms, logo, and signature slot map from configuration/domain data.
- [ ] Normal, maximum-line, wrapping, and approved overflow fixtures render without truncating financial data.
- [ ] Pixel/structural golden comparison passes; renderer failures use the shared three-attempt executor and durable terminal history.

**Verification:**

- [ ] Tests pass: `cargo test -p pdf --all-features`
- [ ] Manual check: review rendered golden against `docs/sample-quotation.pdf`.

**Dependencies:** Task 4, Tasks 11-12, Task 31; PDF dependency requires approval.

**Files likely touched:**

- `crates/pdf/Cargo.toml`
- `crates/pdf/src/layout.rs`
- `crates/pdf/src/render.rs`
- `crates/pdf/tests/render_golden.rs`
- `tests/fixtures/quotation/render-cases.json`

**Estimated scope:** Medium: 5 files

## Task 34: Implement S3, Drive, and topic artifact delivery

**Description:** Complete the confirmed quotation path by storing the canonical
PDF, copying it to Drive, and returning the PDF and links in the same topic.

**Acceptance criteria:**

- [x] S3 and Drive resource IDs are durable and retries do not duplicate copies.
- [x] Topic delivery always uses the originating `message_thread_id`.
- [x] S3, Drive, and Telegram operations use the shared executor; partial failure resumes after the last accepted operation.

**Verification:**

- [x] Tests pass: `cargo test -p application quotation_delivery`
- [x] Integration check: confirmed fixture creates one S3 object, one Drive copy, and one topic delivery record.

**Dependencies:** Tasks 21, 23, 32-33

**Files likely touched:**

- `crates/application/src/quotation.rs`
- `crates/application/src/artifact_delivery.rs`
- `crates/application/tests/quotation_flow.rs`
- `crates/application/tests/quotation_recovery.rs`

**Estimated scope:** Medium: 4 files

## Task 34A: Package quotation workflow Lambda handlers

**Description:** Add thin workflow, PDF, and delivery binaries in one Rust
function package without duplicating application or adapter logic.

**Acceptance criteria:**

- [x] Each handler deserializes a versioned event, calls one application port, and returns a typed result.
- [x] Handler IAM needs and timeout/memory requirements are documented for SAM.
- [x] Local fixtures cover success, retryable failure, and terminal failure.

**Verification:**

- [x] Tests pass: `cargo test -p workflow-actions-functions`
- [x] Build succeeds: `cargo build -p workflow-actions-functions`

**Dependencies:** Task 34

**Files likely touched:**

- `functions/workflow-actions/Cargo.toml`
- `functions/workflow-actions/src/bin/workflow.rs`
- `functions/workflow-actions/src/bin/pdf.rs`
- `functions/workflow-actions/src/bin/delivery.rs`
- `functions/workflow-actions/tests/handlers.rs`

**Estimated scope:** Medium: 5 files

## Task 35: Implement confirmed existing-file modification

**Description:** Support reading and modifying authorized Sheets/Docs only after
a target-specific revision-bound preview is confirmed.

**Acceptance criteria:**

- [ ] Preview identifies file, tab/section, fields, resulting values, owner, and actor.
- [ ] Confirmation digest binds the exact resource and mutation payload.
- [ ] Fault injection proves retries do not apply the same change twice.

**Verification:**

- [ ] Tests pass: `cargo test -p google existing_file_mutation`
- [ ] Integration check: stale and duplicate confirmations perform zero extra writes.

**Dependencies:** Tasks 16-17, 31-32

**Files likely touched:**

- `crates/google/src/existing_files.rs`
- `crates/google/src/mutations.rs`
- `crates/google/tests/existing_file_contracts.rs`
- `crates/application/src/existing_files.rs`
- `crates/application/tests/existing_file_flow.rs`

**Estimated scope:** Medium: 5 files

## Checkpoint: Quotation Flow Complete

- [ ] Tasks 32-35, including Task 34A, complete one confirmed quotation path end to end.
- [ ] Visual and duplicate-write checks pass.
- [ ] Production remains gated on final signature and company details.

## Phase 6: Calendar and Email Slices

## Task 36: Implement the confirmed Calendar workflow

**Description:** Extract and clarify Calendar data, preview it in the topic, and
create an owner-account event only after confirmation.

**Acceptance criteria:**

- [x] Owner primary calendar/timezone defaults and accessible alternate calendars work.
- [x] Reminder settings, attendees, and send-invitations choice are extracted, clarified, previewed, and bound to confirmation.
- [x] Event and workflow-owner reminder fixtures cannot duplicate on retry; ambiguity enters manual review.

**Verification:**

- [x] Tests pass: `cargo test -p google calendar`
- [x] Integration check: invitation-on and invitation-off fixtures each create one event.

**Dependencies:** Quotation Flow checkpoint, Tasks 17, 24, 30-32

**Files likely touched:**

- `crates/google/src/calendar.rs`
- `crates/google/tests/calendar_contracts.rs`
- `crates/application/src/calendar.rs`
- `crates/application/tests/calendar_flow.rs`

**Estimated scope:** Medium: 4 files

## Task 37: Implement the confirmed Hostinger email workflow

**Description:** Draft and preview email content, then send the confirmed message
with PDF attachment and seven-day S3 link through the shared mailbox.

**Acceptance criteria:**

- [ ] Recipient, CC/BCC, subject, body, attachments, and link expiry are bound to confirmation.
- [ ] Provider outcome and stable message identifier are recorded without logging content/secrets.
- [ ] Rejection retries follow policy; ambiguous acceptance enters manual review.

**Verification:**

- [ ] Tests pass: `cargo test -p email --all-features`
- [ ] Integration check: mock SMTP covers success, rejection, timeout, and ambiguity.

**Dependencies:** Tasks 7, 17, 21, 31, 34

**Files likely touched:**

- `crates/email/Cargo.toml`
- `crates/email/src/smtp.rs`
- `crates/email/src/message.rs`
- `crates/email/tests/smtp_contracts.rs`
- `crates/application/src/email.rs`

**Estimated scope:** Medium: 5 files

## Task 37A: Package Google, Calendar, and email Lambda handlers

**Description:** Add thin Google and email action binaries that reuse tested
application services and expose versioned Step Functions events.

**Acceptance criteria:**

- [ ] Google/Calendar and email handlers contain no confirmation or retry business logic.
- [ ] Handler results distinguish accepted, retryable, ambiguous, and terminal outcomes.
- [ ] Local fixtures cover each result category without real credentials.

**Verification:**

- [ ] Tests pass: `cargo test -p external-actions-functions`
- [ ] Build succeeds: `cargo build -p external-actions-functions`

**Dependencies:** Tasks 35-37

**Files likely touched:**

- `functions/external-actions/Cargo.toml`
- `functions/external-actions/src/bin/google.rs`
- `functions/external-actions/src/bin/calendar.rs`
- `functions/external-actions/src/bin/email.rs`
- `functions/external-actions/tests/handlers.rs`

**Estimated scope:** Medium: 5 files

## Checkpoint: External Actions Proven

- [ ] Tasks 36-37, including Task 37A, pass confirmation and fault-injection tests.
- [ ] No retry scenario duplicates Calendar or email actions.
- [ ] Detailed failures appear in-topic without OAuth/secret material.

## Phase 7: Orchestration and Infrastructure

## Task 38: Implement resumable Step Functions workflows

**Description:** Define Standard Workflows for human waits, task retries,
timeouts, terminal failures, and workflow-specific branches.

**Acceptance criteria:**

- [ ] Wait/resume survives Lambda termination and uses durable task-token metadata.
- [ ] Clarification/confirmation expiry and three-attempt failure paths are explicit.
- [ ] Duplicate resume, cancellation race, and late callback cases are safe.

**Verification:**

- [ ] Tests pass: Step Functions Local wait/resume/timeout/retry suite.
- [ ] Validation passes: ASL definitions validate before SAM packaging.

**Dependencies:** External Actions Proven checkpoint, Tasks 15-17, 31, 34A, 36-37A

**Files likely touched:**

- `infrastructure/statemachines/quotation.asl.json`
- `infrastructure/statemachines/calendar.asl.json`
- `infrastructure/statemachines/email.asl.json`
- `tests/integration/step_functions.rs`

**Estimated scope:** Medium: 4 files

## Task 39: Define core SAM resources and function packaging

**Description:** Package all Rust and TypeScript functions with API Gateway,
DynamoDB, S3, Parameter Store/KMS, and Step Functions resources.

**Acceptance criteria:**

- [ ] Every handler has an explicit runtime, artifact, timeout, memory limit, and event contract.
- [ ] Tables, buckets, secrets, state machines, webhook/OAuth endpoints, and outputs are explicit.
- [ ] `sam validate --lint` and `sam build` package every function deterministically.

**Verification:**

- [ ] Validation/build pass: `sam validate --lint --template-file infrastructure/template.yaml && sam build --template-file infrastructure/template.yaml`
- [ ] Packaging check: generated artifacts contain every expected handler.

**Dependencies:** Tasks 3, 11A, 20-25, 29-30, 32-38

**Files likely touched:**

- `infrastructure/template.yaml`
- `infrastructure/samconfig.toml`
- `infrastructure/build-rust.mk`
- `infrastructure/build-agent.mk`

**Estimated scope:** Medium: 4 files

## Task 39A: Add IAM and isolated environment controls

**Description:** Apply least-privilege IAM, encryption, retention, logging, and
strict environment-isolation controls to the core SAM resources.

**Acceptance criteria:**

- [ ] Dev, staging, and production resource names and mutable data cannot cross-reference each other.
- [ ] Functions access only their required tables, prefixes, secrets, and state-machine actions.
- [ ] Each isolated budget-control resource follows the approved consistency, attribution, and fail-closed ADR.

**Verification:**

- [ ] Security check: generated IAM policies receive human review.
- [ ] Validation passes for dev, staging, and production parameter sets.

**Dependencies:** Tasks 8, 39

**Files likely touched:**

- `infrastructure/policies/functions.yaml`
- `infrastructure/security.yaml`
- `config/environments/dev.yaml`
- `config/environments/staging.yaml`
- `config/environments/production.yaml`

**Estimated scope:** Medium: 5 files

## Task 40: Complete the local AWS sandbox

**Description:** Extend the early storage/Step Functions sandbox with full
LocalStack, mock SMTP, and fake provider services using seeded sanitized data.

**Acceptance criteria:**

- [ ] Sandbox starts without cloud credentials and contains no real secret/customer data.
- [ ] Webhook, conditional write, S3, wait/resume, OAuth, Google, SMTP, and AI mock paths are exercisable.
- [ ] Documentation states local parity limits and staging responsibilities.

**Verification:**

- [ ] Start succeeds: `docker compose -f infrastructure/local/docker-compose.yaml up -d`
- [ ] Tests pass: `cargo test --test local_sandbox --features integration`

**Dependencies:** Tasks 20-25, 30-39A

**Files likely touched:**

- `infrastructure/local/docker-compose.yaml`
- `infrastructure/local/init.sh`
- `infrastructure/local/mock-providers.json`
- `tests/integration/local_sandbox.rs`
- `docs/development/local-sandbox.md`

**Estimated scope:** Medium: 5 files

## Task 41: Implement cost enforcement and observability

**Description:** Connect usage counters, cost decisions, metrics, structured
redacted logs, alarms, and stage progress to all critical workflow paths.

**Acceptance criteria:**

- [ ] Application counters warn at 80%, suspend intake at 90%, and prevent projected cap breach across environments.
- [ ] Metrics/logs identify workflow/stage/outcome without secrets, OAuth data, or raw document content.
- [ ] Alarms cover failures, manual-review ambiguity, cost thresholds, and webhook/OAuth error rates.

**Verification:**

- [ ] Tests pass: `cargo test -p application cost_observability`
- [ ] Manual check: injected thresholds produce expected metrics, notices, and intake behavior.

**Dependencies:** Tasks 8, 18, 20, 38-40, including Task 39A

**Files likely touched:**

- `crates/application/src/cost_guard.rs`
- `crates/application/src/observability.rs`
- `functions/workflow-actions/src/bin/cost_guard.rs`
- `infrastructure/monitoring.yaml`
- `tests/integration/cost_guard.rs`

**Estimated scope:** Medium: 5 files

## Checkpoint: Infrastructure Ready

- [ ] Tasks 38-41, including Task 39A, pass local orchestration, SAM, security, and cost checks.
- [ ] LocalStack/SAM limitations are documented rather than hidden.
- [ ] Human approves IAM and monitoring before shared deployment.

## Phase 8: System Verification and Release

## Task 42: Add integration and end-to-end suites

**Description:** Automate the PRD integration and E2E scenarios with local mocks
first and real development/staging providers where required.

**Acceptance criteria:**

- [ ] Coverage includes OAuth, topic isolation, concurrent sessions, all inputs, quotation, Calendar, email, retries, history, and cost suspension.
- [ ] Security cases cover malformed documents, stale callbacks, removed participants, secret redaction, and presigned-link expiry.
- [ ] Each PRD success criterion links to an automated test or explicit manual evidence.

**Verification:**

- [ ] Tests pass: `cargo test --workspace --all-features`
- [ ] Tests pass: `npm test --workspace services/agent-harness -- --run`
- [ ] Manual check: `docs/verification/prd-traceability.md` has no uncovered criterion.

**Dependencies:** Infrastructure Ready checkpoint, Tasks 33-41

**Files likely touched:**

- `tests/integration/quotation_e2e.rs`
- `tests/integration/calendar_e2e.rs`
- `tests/integration/email_e2e.rs`
- `tests/integration/security_e2e.rs`
- `docs/verification/prd-traceability.md`

**Estimated scope:** Medium: 5 files

## Task 43: Add deployment and rollback gates

**Description:** Provide one explicit gate command for quality, budget, assets,
configuration, tests, SAM build, approval, and rollback readiness.

**Acceptance criteria:**

- [ ] Deployment fails on any required check, skipped critical test, insufficient budget margin, missing asset, or missing approval.
- [ ] Staging/production require explicit environment selection and cannot share mutable resources.
- [ ] Rollback steps preserve durable workflow/history data and restore the previous known-good artifact/config.

**Verification:**

- [ ] Manual check: deliberately fail each gate and confirm deployment is blocked.
- [ ] Human approves `docs/deployment/runbook.md`.

**Dependencies:** Tasks 8, 13, 39-42

**Files likely touched:**

- `scripts/verify-release.sh`
- `scripts/check-budget.py`
- `scripts/check-assets.sh`
- `docs/deployment/runbook.md`
- `.github/workflows/deploy.yml`

**Estimated scope:** Medium: 5 files

## Task 44: Verify staging and prepare production approval

**Description:** Run the complete system in staging with real IAM and approved
provider credentials, capture evidence, and stop before production deployment.

**Acceptance criteria:**

- [ ] Real Telegram, OAuth, Google, SMTP, model, S3 expiry, IAM, encryption, logging, alarm, and cost scenarios pass.
- [ ] Final signature/company details and production configuration are present and reviewed.
- [ ] Residual risks, rollback point, and human production approval are recorded.

**Verification:**

- [ ] Checks pass: `scripts/verify-release.sh staging`
- [ ] Manual check: human signs `docs/verification/staging-signoff.md` before any production deploy.

**Dependencies:** Tasks 42-43 and all pending credentials/assets

**Files likely touched:**

- `docs/verification/staging-signoff.md`
- `docs/verification/evidence-index.md`
- `config/environments/production.yaml`

**Estimated scope:** Medium: 3 files

## Checkpoint: Version 1 Ready

- [ ] All 48 tasks and checkpoint conditions are complete.
- [ ] The full Definition of Done is satisfied.
- [ ] Production deployment has explicit human approval.
