# Specification: Telegram Operations Worker

**Status:** Version 1 implementation baseline  
**Version:** 1.0  
**Audience:** Internal company implementation  
**Initial users:** Approximately three employees  
**Expected usage:** Approximately ten workflows per week  
**Monthly AWS budget:** ₹300 across all environments

## 1. Objective

Build an internal operations worker that lets employees perform quotation,
calendar, email follow-up, and reminder workflows from Telegram.

The worker is intended for one company. Employees should not need a separate
application UI. They work in separate topics inside a company Telegram forum
supergroup. Private Telegram chat is used only to configure and manage Google
OAuth. Workflow discussion, previews, confirmations, and artifact delivery stay
inside the relevant forum topic.

The system uses AI to interpret instructions, extract information from images,
PDFs, CSV files, Word documents, and Excel workbooks, draft content, and
calculate quotation values. External writes are performed by application
services after an approved group participant reviews and confirms the action.

### 1.1 Primary users

- Employees who are participants in the approved Telegram forum supergroup.
- Any approved participant may onboard with any Google account, including a
  personal Google account.
- The employee who starts a workflow is its owner. Any approved participant in
  the group may add information or attachments and may confirm, correct, or
  cancel the workflow from its topic.
- Unless explicitly reassigned by a future approved specification, Google
  operations use the workflow owner's connected account and personal defaults,
  even when another approved participant provides the confirmation.

### 1.2 Product outcomes

- Reduce manual preparation of quotations and follow-up communication.
- Keep Telegram as the primary user interface.
- Preserve source documents, generated artifacts, and audit history.
- Apply the workflow owner's Google permissions to Drive, Sheets, Docs, and
  Calendar.
- Keep OAuth links, authorization codes, access tokens, refresh tokens, and
  related OAuth details out of the group conversation.
- Keep total AWS expenditure within ₹300 per month.

## 2. Assumptions and Product Decisions

1. The deployment is for one company, not a multi-tenant SaaS product.
2. The primary group interface is a Telegram forum supergroup with Topics
   enabled.
3. Each forum topic is one workflow session, identified by the pair
   `(chat_id, message_thread_id)`.
4. An employee starts or directs work by tagging the bot inside the topic.
5. Every bot response and artifact sent to the group uses the same
   `message_thread_id`.
6. Workflow sessions are supported only in forum topics. Private chat is
   reserved for Google OAuth configuration, and non-forum groups are not
   supported.
7. Multiple workflows may be active concurrently in separate topics.
8. Google OAuth is completed privately through a secure browser link.
9. All required Google scopes are requested during onboarding rather than
   incrementally.
10. Gmail is not used. Email is sent from one shared Hostinger company mailbox.
11. Pi is the AI agent harness. A TypeScript Lambda embeds the Pi SDK while Rust
    Lambdas implement workflow, integration, and PDF services.
12. The AI provider and vision-capable model are deployment configuration. The
    initial recommendation is Fireworks
    `accounts/fireworks/models/kimi-k2p5`; before staging, it must be evaluated
    against a pinned OpenAI GPT-5.6 snapshot using representative document
    fixtures, schema-validity rate, latency, and provider cost.
13. AI-calculated financial values are not independently recomputed by
    deterministic Rust logic. Review and confirmation by any approved group
    participant are the financial correctness safeguard.
14. Raw inputs and generated artifacts are retained indefinitely unless the
    company later changes its retention policy.
15. New workflow intake is suspended before projected AWS expenditure would
    exceed ₹300 for the month.
16. Secrets use separate AWS Systems Manager Parameter Store standard-tier
    `SecureString` values encrypted with the AWS managed Systems Manager KMS key.
    Credentials remain isolated by environment and capability; application-only
    encryption and cost-driven credential bundling are forbidden.
17. Standard Telegram cloud encryption is accepted. Bot chats are not treated
    as end-to-end encrypted. AWS copies remain encrypted in transit and at rest.
18. Development, staging, and production use isolated stacks in the same AWS
    account and share the combined ₹300 monthly AWS budget.

## 3. Scope

### 3.1 Included

- Official Telegram Bot API integration through HTTPS webhooks.
- Forum-topic workflow isolation using `message_thread_id`.
- Bot invocation by tagging the bot inside a topic.
- Concurrent workflow sessions across multiple forum topics.
- Private Google OAuth onboarding and configuration.
- Image, PDF, CSV, Word, and Excel input, including multi-message attachment
  collection.
- AI instruction interpretation and document extraction.
- AI quotation calculations with mandatory confirmation by an approved group
  participant.
- New and existing Google Sheets and Docs operations.
- Rust-native quotation PDF generation using
  `docs/sample-quotation.pdf` as the canonical visual and layout template.
- Google Drive folder selection and remembered defaults.
- Google Calendar event and reminder creation.
- AI-drafted and company-template follow-up emails.
- Hostinger SMTP delivery from one company mailbox.
- AWS Step Functions orchestration and DynamoDB workflow state.
- S3 storage for raw inputs and generated PDFs.
- Detailed workflow audit records and progress notifications.
- Development, staging, and production environments.

### 3.2 Excluded

- A web or native mobile product UI.
- A CRM or customer/vendor relationship database.
- Payment tracking and financial reporting.
- Multi-company tenancy.
- Autonomous financial approval.
- Incoming Hostinger mailbox or IMAP monitoring.
- Automatic processing of email replies.
- Analytics dashboards.
- Alternative or placeholder quotation layouts that do not match
  `docs/sample-quotation.pdf`.
- Workflow sessions in private chats or non-forum groups.

## 4. User Interaction Contract

### 4.1 Commands

The bot display name is **Novus**. Its unique Telegram `@username` is created
with BotFather during development and stored as deployment configuration.

Inside a forum topic, the topic itself is the workflow session. A user starts a
workflow by tagging `@<bot_username>` and supplying a natural-language
instruction:

```text
@<bot_username> create a quotation from these documents
```

After a workflow has started, participants use follow-up messages in that same
topic. For example:

```text
<attachments and additional instructions>
/done
/status
/correct <correction>
/confirm
/stop
```

`/done`, `/status`, `/correct`, `/confirm`, and `/stop` operate only on the
existing workflow in the current topic; they are not workflow-start commands
and do not accept session names. Confirmations and corrections should use inline
buttons where practical. Typed commands remain available as an accessibility
and recovery path.

Private chat supports only Google OAuth configuration commands and links, such
as `/connect_google`, `/oauth_status`, and `/disconnect_google`. It cannot start
or continue a workflow. Non-forum groups are unsupported.

The AI infers the workflow type from the initiating instruction. Topic messages
are associated only by `(chat_id, message_thread_id)`. They must never be
attached to another topic, even when the same employee participates in both.

### 4.2 Topic and OAuth privacy rules

- The forum topic receives acknowledgments, stage updates, customer details,
  source values, calculated amounts, filenames, artifact links, email content,
  extraction previews, corrections, confirmations, generated PDFs, and file
  links as needed for the workflow.
- OAuth links, authorization codes, access tokens, refresh tokens, OAuth errors
  containing credential material, and other OAuth details must never be sent to
  a topic or group.
- OAuth configuration and recovery are handled only in private chat. A topic may
  contain a generic deep link asking a participant to configure Google access,
  but no OAuth state or credential material.
- The workflow pauses when the workflow owner's Google account is required but
  is not connected or must be reauthorized.

### 4.3 Progress stages

At minimum, progress is reported for:

1. Request accepted.
2. Attachments being collected.
3. Extraction started and completed.
4. Calculation or drafting started and completed.
5. Waiting for clarification or confirmation.
6. Sheet or Doc write started and completed.
7. PDF generation started and completed.
8. Calendar or email action started and completed.
9. Artifact delivery completed.
10. Workflow failed, expired, stopped, or completed.

## 5. Identity and Google Onboarding

1. An approved group participant starts onboarding in private Telegram chat.
2. The worker sends a short-lived HTTPS OAuth URL in that private chat.
3. The employee completes Google consent in their browser.
4. The callback verifies OAuth state and binds the Google identity to the
   participant's stable Telegram user ID.
5. The worker requests Calendar, Drive, Sheets, and Docs permissions during the
   initial onboarding flow.
6. OAuth refresh tokens are stored as separate AWS Systems Manager Parameter
   Store standard-tier `SecureString` values, encrypted with the AWS managed
   Systems Manager KMS key.
7. OAuth links, authorization codes, access tokens, refresh tokens, and secrets
   must never be sent to a topic/group or written to logs.
8. A participant without connected Google access may still contribute to a
   topic, but cannot own a Google-dependent workflow. The bot posts a generic
   topic notice and provides private onboarding instructions.
9. Revoked or expired authorization returns the workflow owner to private
   onboarding without deleting prior workflow history.

Group membership is the authorization boundary. Google accounts are not
restricted to a company domain.

## 6. Workflow Requirements

### 6.1 Attachment collection

- Accepted inputs are images, PDFs, CSV files, Microsoft Word documents
  (`.doc` and `.docx`), and Microsoft Excel workbooks (`.xls` and `.xlsx`).
- Text and tabular formats are parsed deterministically before their extracted
  content is sent to the AI; images and rendered document pages use the
  configured vision model.
- A workflow accepts at most 10 attachments.
- Each attachment may be at most 20 MB.
- Attachments are associated with the current `(chat_id, message_thread_id)`
  session.
- Any approved group participant finishes collection with `/done` in the topic.
- The default collection timeout is 30 minutes and is configurable in YAML.
- On timeout, the worker asks in the topic whether to continue collecting or
  stop.
- Clarification and confirmation waits expire after 12 hours by default.
- Any approved group participant may stop a workflow explicitly at any time.

### 6.2 Quotation workflow

`docs/sample-quotation.pdf` is the canonical quotation template for Version 1.
It is a one-page A4 visual/layout reference and must remain version-controlled.
The renderer must reproduce its structure while substituting configuration and
workflow data; it must not modify the source PDF in place. Field mapping,
coordinates, fonts, spacing, and overflow behavior are established from this
reference during renderer implementation. The final quotation signature and
remaining company-specific details will be supplied during development and
must be represented as configuration rather than hard-coded values.

Every quotation supports at least:

- Customer identity.
- Quotation number.
- Quotation date.
- Currency.
- Line-item description.
- Quantity.
- Unit price.
- Tax values.
- Subtotals and totals.
- Validity period.
- Terms.

The workflow is:

1. Collect images, PDFs, CSV files, Word documents, Excel workbooks, and
   instructions.
2. Store raw inputs in S3.
3. Ask Pi to extract structured source values and calculate requested values.
4. Validate only the response schema, required fields, data types, and business
   rule presence. Do not independently recalculate AI totals.
5. Send a preview in the topic showing original values, AI-calculated values,
   assumptions, currency, taxes, and totals.
6. Wait for confirmation from any approved group participant.
7. If an approved participant corrects the preview, apply the correction,
   regenerate the topic preview, and request confirmation again.
8. After confirmation, create or modify the Google Sheet or Doc.
9. Render the approved quotation with the configured Rust PDF adapter.
10. Store the canonical PDF in S3.
11. Copy the PDF to the selected Google Drive folder.
12. Send the PDF and relevant links in the topic.

Original/source values and AI-calculated amounts may be discussed in the topic,
but they must not be written to a Sheet, Doc, or PDF before explicit
confirmation by an approved group participant.

### 6.3 Existing Google files

- The worker may read existing Sheets and Docs when authorized.
- Every modification to an existing file requires an immediate topic preview
  and confirmation by an approved group participant.
- Confirmation must identify the target file, tab or section, fields to change,
  resulting values, workflow owner, and confirming participant.
- Retries must not apply the same mutation more than once.
- The worker returns the updated file link after a confirmed write.

### 6.4 Drive destination

- Configuration defines one company-wide default Drive folder.
- Each employee may choose and remember a personal override.
- The workflow owner or another approved participant may override the
  destination for one workflow.
- If no default is usable, the worker asks in the topic for a folder choice.
- The folder may be in My Drive or a Shared Drive if OAuth permissions permit.
- Google Drive is canonical for live Sheets and Docs.
- S3 is canonical for raw inputs and generated PDFs.
- Generated PDFs are always copied to the configured Drive destination.

### 6.5 Calendar workflow

1. Extract the title, date, time, duration, timezone, calendar, description,
   attendees, reminder settings, and invitation preference.
2. Ask for any missing or ambiguous required value.
3. Default to the workflow owner's primary Google Calendar.
4. Default to the workflow owner's Calendar timezone.
5. Allow another calendar only when an approved participant explicitly selects
   it and the workflow owner's Google account has access.
6. Present a confirmation preview in the topic immediately before creation.
7. Include “send attendee invitations?” in the confirmation.
8. Create the event only after confirmation by an approved group participant.
9. Return the event link in the topic.

### 6.6 Follow-up workflow

Follow-up workflows support both:

- AI-drafted or company-template email follow-ups.
- Google Calendar reminders for the workflow owner.

Email behavior:

1. Draft from natural-language instructions or a configured company template.
2. Permit external customer or vendor recipients.
3. Present recipient, CC/BCC, subject, body, attachments, and link expiry in a
   topic confirmation preview.
4. Require explicit approval from an approved group participant immediately
   before sending.
5. Send from one shared Hostinger company mailbox through authenticated SMTP.
6. Attach the quotation PDF directly when requested.
7. Include a secure S3 link that expires after seven days.
8. Record the SMTP outcome and stable message identifier.
9. Do not monitor the Hostinger inbox or process replies.

## 7. Reliability and Failure Handling

### 7.1 Retries

Every external operation may make at most three attempts: one initial attempt
and two retries with exponential backoff and jitter. This applies to:

- AI provider calls.
- Telegram Bot API calls.
- Google APIs.
- Hostinger SMTP.
- S3 operations.
- PDF generation tasks.

Retries require stable idempotency keys. For operations without native
idempotency, the application records whether the provider accepted the request
before retrying. An ambiguous result that could duplicate an email, message,
event, or file mutation must enter manual-review failure instead of being
blindly repeated.

After the final failed attempt, the worker:

1. Persists the failed state and attempt history.
2. Identifies the stage that failed.
3. Sends a failure status in the workflow topic.
4. Sends detailed failure information in that topic, excluding OAuth details
   and secret material.
5. States that no further automatic attempts will occur.

### 7.2 Workflow durability

- AWS Step Functions Standard coordinates multi-step workflows.
- DynamoDB stores workflow state, session identity, confirmation state,
  external resource identifiers, and idempotency records.
- Human waits are resumable and survive Lambda process termination.
- Duplicate Telegram updates must not create duplicate workflows or actions.
- Every state transition is attributable to the acting participant, workflow
  owner, and source message.

## 8. Architecture and Tech Stack

### 8.1 Runtime stack

| Concern | Technology |
| --- | --- |
| Domain and integration services | Rust 1.97, pinned by the repository |
| Lambda Rust runtime | AWS `provided.al2023` OS-only runtime |
| AI harness | `@earendil-works/pi-coding-agent` SDK, initially 0.80.x |
| AI service runtime | TypeScript on Node.js 22 |
| Infrastructure as code | AWS SAM |
| Workflow orchestration | AWS Step Functions Standard |
| Workflow metadata | Amazon DynamoDB |
| Raw and generated objects | Amazon S3 |
| Secrets | AWS Systems Manager Parameter Store standard-tier `SecureString` + AWS managed KMS key |
| Public endpoints | Amazon API Gateway and Lambda |
| CI | GitHub Actions |
| Configuration | Version-controlled YAML plus JSON Schema |

Pi's SDK is TypeScript. The system therefore uses a hybrid Lambda architecture
rather than embedding Pi directly in Rust.

### 8.2 Components

- **Telegram webhook Lambda (Rust):** verifies the webhook secret token,
  normalizes and deduplicates updates, maps topics to sessions, and
  starts/resumes workflows.
- **OAuth Lambda (Rust):** creates OAuth requests and processes callbacks.
- **Workflow services (Rust):** enforce state transitions, confirmations,
  authorization, idempotency, and privacy rules.
- **Pi agent Lambda (TypeScript):** runs an in-memory Pi SDK session with a
  deployment-selected model and returns schema-constrained extraction,
  calculation, classification, and drafting results.
- **Google adapter Lambdas (Rust):** call Drive, Sheets, Docs, and Calendar.
- **Email Lambda (Rust):** sends confirmed messages through Hostinger SMTP.
- **PDF Lambda (Rust):** renders the Version 1 quotation natively using
  `docs/sample-quotation.pdf` as the canonical visual/layout reference and
  configured company assets.
- **Delivery Lambda (Rust):** uploads Telegram media, sends topic-scoped
  workflow messages, and sends private OAuth configuration messages.
- **Cost guard (Rust):** tracks application usage, evaluates configured monthly
  limits, warns users, and suspends new intake before projected overage.

### 8.3 Pi safety boundary

The Pi Lambda must:

- Use `SessionManager.inMemory()` for each invocation, rehydrated with the
  application-owned workflow conversation context when a session continues.
- Load model credentials from the application's secret provider at runtime.
- Disable built-in shell and filesystem mutation tools.
- Expose only narrowly scoped custom tools required for extraction and
  structured response production.
- Return JSON that passes a versioned schema before the workflow continues.
- Never call Google, Telegram, SMTP, or artifact mutation APIs directly.
- Never bypass application confirmation rules.

DynamoDB and S3 remain the durable source of truth. To support continuation,
the application also persists a sanitized, ordered workflow conversation
history and AI checkpoints in DynamoDB/S3, then rehydrates the in-memory Pi
session on the next invocation. This persisted history includes model and prompt
versions but excludes OAuth data, secrets, and raw credential-bearing provider
payloads. Native ephemeral Pi session state is not the sole durable record.

## 9. Configuration

Non-secret deployment configuration lives in version-controlled YAML. Secrets
remain in separate AWS Systems Manager Parameter Store standard-tier
`SecureString` values encrypted with the AWS managed Systems Manager KMS key.

The configuration schema includes:

- Company identity and contact information.
- Logo and signature references.
- Currency, tax, validity, and business rules.
- Quotation numbering rules.
- Quotation template version, `docs/sample-quotation.pdf` reference, and field
  mapping.
- Company Drive folder.
- Calendar defaults.
- Sensitive-action definitions.
- Hostinger SMTP host, port, sender address, and secret reference.
- Pi provider, model ID, reasoning setting, and vision capability.
- Attachment count and size limits.
- Collection and clarification timeouts.
- Retry and backoff settings.
- S3-link expiry.
- AWS monthly budget, warning threshold, and suspension threshold.
- Environment-specific Telegram bot, forum chat, Google OAuth, and callback
  identifiers.

Configuration is managed at deployment time only. Telegram commands cannot
change company configuration.

## 10. Cost Controls

The total AWS budget is ₹300 per month across development, staging, and
production unless a later approved specification changes the scope.

Default controls are:

- Warn administrators and active users at 80% of the monthly budget (₹240).
- Suspend new workflow intake at 90% of the monthly budget (₹270).
- Allow already-confirmed operations to finish only when the projected total
  remains below ₹300.
- Continue serving non-mutating status and artifact retrieval where doing so
  does not risk the cap.
- Retain existing data indefinitely even while new intake is suspended.
- Estimate Parameter Store/KMS request use and retained-object storage cost
  before each deployment; standard Parameter Store storage has no additional
  monthly charge.
- Configure AWS Budgets actual and forecast alerts.
- Record per-workflow estimated AWS usage.

AWS Budgets reporting can be delayed. Application usage counters and a safety
margin are therefore required. The system must not claim that AWS Budgets alone
can enforce the cap.

A deployment is blocked when forecast fixed costs or configured resource use
leave insufficient safety margin below ₹300.

## 11. Project Structure

```text
Cargo.toml                         Rust workspace
rust-toolchain.toml                Pinned Rust toolchain
package.json                       Node workspace and shared commands
crates/domain/                     Pure workflow and policy types
crates/application/                Use cases and state transitions
crates/telegram/                   Telegram Bot API adapter
crates/google/                     Drive, Sheets, Docs, Calendar adapters
crates/email/                      Hostinger SMTP adapter
crates/pdf/                        Rust-native PDF adapter
crates/storage/                    DynamoDB, S3, and secret adapters
functions/                         Rust Lambda entry points
services/agent-harness/            TypeScript Pi SDK Lambda
infrastructure/template.yaml       AWS SAM template
infrastructure/statemachines/      Step Functions definitions
infrastructure/local/              Local AWS sandbox configuration
config/company.yaml                Non-secret company configuration
config/environments/               Dev, staging, and production overrides
config/schema/                     Configuration and AI-response schemas
tests/fixtures/                    Sanitized API and document fixtures
tests/integration/                 Cross-component integration tests
docs/sample-quotation.pdf          Canonical Version 1 quotation layout
docs/prd/                          Approved living specification
tasks/                             Approved plan and implementation tasks
```

## 12. Developer Commands

These commands are the required project interface once scaffolding exists:

```bash
# Rust formatting, linting, testing, and build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-features

# TypeScript agent harness
npm ci
npm run format:check --workspace services/agent-harness
npm run lint --workspace services/agent-harness
npm test --workspace services/agent-harness -- --run
npm run build --workspace services/agent-harness

# Infrastructure and local AWS sandbox
sam validate --lint --template-file infrastructure/template.yaml
sam build --template-file infrastructure/template.yaml
docker compose -f infrastructure/local/docker-compose.yaml up -d
sam local start-api \
  --template infrastructure/template.yaml \
  --env-vars config/environments/dev.local.json

# Deployment; requires explicit approval
sam deploy --config-env dev
sam deploy --config-env staging
sam deploy --config-env production
```

## 13. Code Style

### 13.1 Rust

- Use explicit domain types instead of raw strings for identities and money.
- Return typed errors; do not use `unwrap()` or `expect()` in production paths.
- Keep domain logic independent of AWS SDK types.
- Use integer minor units or a decimal type for stored monetary values.
- Keep async boundaries in adapters and application services.

```rust
pub async fn confirm_workflow(
    repository: &dyn WorkflowRepository,
    actor: ParticipantId,
    workflow_id: WorkflowId,
) -> Result<ConfirmedWorkflow, ConfirmWorkflowError> {
    let workflow = repository.load(&workflow_id).await?;
    let confirmed = workflow.confirm(actor)?;
    repository.save(&confirmed).await?;
    Ok(confirmed)
}
```

### 13.2 TypeScript

- Enable TypeScript strict mode.
- Validate all AI input and output at runtime.
- Do not rely on Lambda process memory between invocations; rehydrate persisted
  workflow conversation context explicitly.
- Disable built-in mutation tools.
- Do not use untyped `any` at provider or workflow boundaries.

```typescript
const { session } = await createAgentSession({
  model,
  noTools: "builtin",
  customTools: [extractDocuments],
  sessionManager: SessionManager.inMemory(),
});
```

## 14. Testing Strategy

### 14.1 Unit tests

- Rust domain transitions, authorization, expiry, session routing, cost guard,
  idempotency, and confirmation policy.
- TypeScript prompt construction, model selection, schema validation, and Pi
  event/error handling.
- Quotation renderer layout, field-mapping, pagination/overflow, and visual
  regression tests against `docs/sample-quotation.pdf`; signature assertions
  are enabled when the final signature asset is supplied.

### 14.2 Contract tests

- Telegram update fixtures, including duplicate updates, forum topics,
  mentions, replies, callback queries, and unsupported messages.
- Google API request and response fixtures.
- Hostinger SMTP success, rejection, timeout, and ambiguous acceptance.
- Pi provider fixtures for Fireworks Kimi K2.5 and OpenAI GPT-5.6 model paths.
- Configuration schema and environment override tests.

### 14.3 Integration tests

- DynamoDB state transitions and conditional writes.
- S3 upload, presigned-link expiry, and access controls.
- Step Functions wait, resume, timeout, retry, and failure paths.
- OAuth state verification and token refresh.
- Existing-file read and confirmed modification.

### 14.4 Local sandbox tests

A Docker-based local sandbox must be available before shared AWS deployment.
It uses AWS SAM CLI for Lambda/API Gateway execution, DynamoDB Local, and Step
Functions Local. LocalStack may be used for integrated S3, Systems Manager
Parameter Store, and other AWS service emulation where no AWS-provided local
emulator exists.

The sandbox must:

- Start from `infrastructure/local/docker-compose.yaml` with documented seeded
  fixtures and no cloud credentials required.
- Exercise webhook intake, topic routing, DynamoDB conditional writes, S3
  object flows, Step Functions orchestration, and mocked OAuth/Google/SMTP/AI
  adapters.
- Use fake secrets and sanitized documents only.
- Run in CI for integration paths that do not require real provider behavior.
- Be treated as fast feedback, not as proof of IAM, networking, quotas, or exact
  AWS service parity; those remain staging-test responsibilities.

### 14.5 End-to-end tests

Run in development before staging and in staging before production:

- Employee OAuth onboarding and management in private chat.
- Bot mention handling inside a forum topic.
- Strict topic isolation by `message_thread_id`.
- Rejection of workflows in private chats and non-forum groups.
- Multiple concurrent topic sessions.
- Attachment collection and timeout.
- Quotation preview, correction, and rendering against
  `docs/sample-quotation.pdf`.
- Calendar confirmation with and without attendee invitations.
- Hostinger email confirmation, attachment, and seven-day link.
- Three-attempt failure and user notification.
- Cost warning and new-intake suspension.

### 14.6 Coverage expectations

- 100% branch coverage for authorization, confirmation, cost-stop, and workflow
  state-transition rules.
- At least 80% line coverage for Rust application/domain crates and the
  TypeScript agent harness.
- No production deployment with skipped critical-path tests.

## 15. Boundaries

### Always do

- Validate every webhook signature and deduplicate every event.
- Validate AI responses against a versioned schema.
- Keep OAuth information and secret material in private OAuth flows and out of
  topics/groups.
- Require confirmation by an approved group participant for financial writes,
  existing-file changes, calendar creation, attendee invitation choice, and
  email sending.
- Use idempotency controls for external writes.
- Run format, lint, tests, build, and SAM validation before deployment.
- Record the workflow owner, acting participant, source message, state
  transitions, confirmations, retries, and external resource IDs.
- Warn and suspend intake before projected AWS spending exceeds the cap.

### Ask first

- Adding or upgrading dependencies.
- Changing DynamoDB keys or stored schemas.
- Adding Google OAuth scopes.
- Changing IAM permissions or encryption policy.
- Changing the AI provider/model compatibility contract.
- Changing financial confirmation rules.
- Changing retention or budget thresholds.
- Deploying to staging or production.

### Never do

- Commit credentials, OAuth tokens, SMTP passwords, or API keys.
- Log secrets, raw OAuth tokens, or sensitive document contents.
- Send OAuth links, codes, tokens, credential-bearing errors, or other OAuth
  details to a Telegram topic or group.
- Reject a correction, confirmation, or cancellation solely because an
  approved group participant is not the workflow owner.
- Write AI-calculated or original amounts before confirmation.
- Send an email or Calendar invitation without confirmation.
- Continue accepting new workflows when the cost guard has suspended intake.
- Remove failing tests to make CI pass.
- Replace or materially alter the Version 1 quotation layout without approval.
- Hard-code the pending signature or company-specific details into the
  renderer.

## 16. Success Criteria

The specification is implemented successfully when:

1. An approved group participant can complete private Google OAuth onboarding.
2. Tokens are stored securely and never appear in user messages or logs.
3. Tagging the bot inside a forum topic starts or resumes that topic's workflow.
4. Multiple topic sessions run without cross-associating attachments,
   callbacks, corrections, or replies, and workflows cannot run in private chat
   or non-forum groups.
5. Up to 10 supported image, PDF, CSV, Word, or Excel attachments of 20 MB each
   can be collected and stored.
6. Every processing stage produces an update in the workflow topic while OAuth
   details remain confined to private OAuth configuration.
7. Missing information causes a resumable clarification wait that expires after
   12 hours.
8. The AI returns schema-valid extracted values and calculations.
9. Any approved group participant can correct, confirm, or cancel a workflow,
   including a financial preview started by another employee.
10. No original or AI-calculated amount is written before confirmation.
11. New and existing Google files are handled under the documented confirmation
    rules.
12. Drive defaults, employee overrides, My Drive, and Shared Drive are supported.
13. A confirmed Calendar event uses the workflow owner's timezone and obeys the
    confirmed attendee-invitation choice.
14. A confirmed Hostinger email is sent with the approved recipient and content,
    PDF attachment, and seven-day S3 link.
15. Incoming email replies are not processed.
16. Every unambiguously failed external call receives no more than three total
    attempts, followed by a durable failure and user notification.
17. Duplicate webhooks and retries do not duplicate confirmed writes.
18. Raw inputs and generated artifacts remain available indefinitely while
    access remains authorized.
19. The system warns at the configured budget threshold and suspends intake
    before projected monthly AWS cost exceeds ₹300.
20. Development, staging, and production pass their required automated and
    end-to-end verification gates.
21. A confirmed quotation matching the layout of
    `docs/sample-quotation.pdf` can be rendered in Rust with configured company
    details and signature, stored in S3, copied to Drive, and delivered in the
    workflow topic.
22. Local sandbox tests exercise the supported AWS-backed workflow paths before
    deployment to shared development or staging stacks.

## 17. Development Inputs and Pre-Staging Decisions

1. `docs/sample-quotation.pdf` is available and approved as the Version 1
   quotation layout reference, so renderer and mapping work may begin. The final
   signature and remaining company-specific details will be supplied during
   development. Production quotation deployment remains gated on those assets,
   but their absence does not block renderer implementation or layout tests.
2. Start model evaluation with Fireworks
   `accounts/fireworks/models/kimi-k2p5`, which supports image input through an
   OpenAI-compatible API. Before staging, compare it with a pinned OpenAI
   GPT-5.6 snapshot on sanitized representative fixtures and select the model
   that meets extraction accuracy, structured-output validity, latency, and
   provider-cost thresholds. Record the winner in each environment's YAML.
3. The Telegram bot display name is **Novus**. Its unique `@username`, forum
   supergroup ID, BotFather token, privacy-mode setting, company configuration,
   OAuth app identifiers, Hostinger sender address, and Drive folder IDs will
   be created or supplied during development before their integration tests.
4. Development, staging, and production will use isolated stacks in the same
   AWS account while sharing the combined ₹300 monthly AWS budget.

## 18. Authoritative References

- [AWS Lambda functions with Rust](https://docs.aws.amazon.com/lambda/latest/dg/lambda-rust.html)
- [AWS SAM](https://docs.aws.amazon.com/serverless-application-model/latest/developerguide/what-is-sam.html)
- [Testing and debugging serverless applications locally with AWS SAM](https://docs.aws.amazon.com/serverless-application-model/latest/developerguide/using-sam-cli-local.html)
- [DynamoDB Local](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/DynamoDBLocal.html)
- [Step Functions Local](https://docs.aws.amazon.com/step-functions/latest/dg/sfn-local.html)
- [AWS Budgets](https://docs.aws.amazon.com/cost-management/latest/userguide/budgets-managing-costs.html)
- [AWS Systems Manager pricing](https://aws.amazon.com/systems-manager/pricing/)
- [AWS Systems Manager Parameter Store SecureString](https://docs.aws.amazon.com/systems-manager/latest/userguide/secure-string-parameter-kms-encryption.html)
- [Pi SDK](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sdk.md)
- [Pi custom providers](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/custom-provider.md)
- [Fireworks vision-language models](https://docs.fireworks.ai/guides/querying-vision-language-models)
- [OpenAI GPT-5.6](https://platform.openai.com/docs/models/gpt-5.6-sol)
- [Telegram Bot API](https://core.telegram.org/bots/api)
- [Telegram bot features](https://core.telegram.org/bots/features)
- [Telegram bot FAQ](https://core.telegram.org/bots/faq)
- [Telegram encryption FAQ](https://telegram.org/faq#q-so-how-do-you-encrypt-data)
- [Hostinger email configuration](https://support.hostinger.com/en/articles/1575756-how-to-get-email-account-configuration-details-for-hostinger-email)
