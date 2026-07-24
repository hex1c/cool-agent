# Local vs Production Runtime

How the Telegram Operations Worker is wired today, what is stubbed, and what
changes are needed to run real-provider paths locally the same way they run in
AWS.

## Current state: what works and what is stubbed

| Handler                       | Lambda binary                      | Real work?          | Notes                                                                                                                                                                                                          |
| ----------------------------- | ---------------------------------- | ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Webhook intake                | `functions/webhook`                | **Partial**         | Verifies the secret token and normalizes the update. Does **not** persist to DynamoDB, start a Step Functions execution, or reply to Telegram.                                                                 |
| OAuth authorize/callback      | `functions/oauth`                  | **Stub**            | Uses `InMemoryStateStore`, `StubTokenEndpoint`, and `StubRefreshTokenStore`. Generates a real PKCE/authorize URL, but the callback does not call Google's token endpoint and does not persist a refresh token. |
| Attachment collection         | `functions/workflow-actions` (bin) | **Stub**            | Validates the event schema then returns `adapter_not_configured`. No S3 upload, no DynamoDB metadata.                                                                                                          |
| Workflow action (PDF trigger) | `functions/workflow-actions` (bin) | **Real**            | Pure domain transition: consumes a `StartDirectPdfGeneration` confirmation. No external calls.                                                                                                                 |
| PDF render                    | `functions/workflow-actions` (bin) | **Real**            | Pure compute via `printpdf`. No credentials.                                                                                                                                                                   |
| Delivery                      | `functions/workflow-actions` (bin) | **Stub**            | Validates the event then returns `adapter_not_configured`. No S3/Drive/Telegram delivery.                                                                                                                      |
| Google create (Sheets/Docs)   | `functions/external-actions` (bin) | **Stub**            | Validates the event then returns `adapter_not_configured`. No Drive/Sheets/Docs client.                                                                                                                        |
| Calendar create               | `functions/external-actions` (bin) | **Stub**            | Validates the event then returns `adapter_not_configured`. No Calendar client.                                                                                                                                 |
| Email send                    | `functions/external-actions` (bin) | **Stub**            | Validates the event then returns `adapter_not_configured`. No SMTP client.                                                                                                                                     |
| Cost guard                    | `functions/workflow-actions` (bin) | **Real (DynamoDB)** | Reads `BUDGET_TABLE` and `ENVIRONMENT` from the environment and exercises the real DynamoDB usage aggregate.                                                                                                   |
| Agent harness (AI)            | `services/agent-harness`           | **Real**            | Creates a real Pi SDK session. Reads `NOVUS_AI_PROVIDER_KEY` from the environment. Works with Gemini if the key and model are configured.                                                                      |

The adapter libraries exist but are **not wired** into the Lambda binaries:

- `crates/email/src/smtp.rs` — `HostingerSmtpService<Client: SmtpClient>` and the
  `SmtpClient` trait exist, with full outcome classification. There is no
  concrete `SmtpClient` implementation that opens a real TLS SMTP connection.
- `crates/google/src/calendar.rs` — `GoogleCalendarService<Client:
GoogleCalendarClient>` exists. There is no concrete `GoogleCalendarClient`
  that calls the Calendar REST API.
- `crates/google/src/sheets_docs.rs` / `drive.rs` — type definitions and
  destination resolution exist. No HTTP client implementation.
- `crates/oauth/src/tokens.rs` — `TokenEndpoint` trait exists. No concrete
  implementation that calls `oauth2.googleapis.com`.

## How production would work (target architecture)

Each Lambda binary would construct concrete adapter implementations from
runtime configuration and credentials, inject them into the service, and
execute the operation:

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────────┐
│ API Gateway │────▶│  WebhookFunction │────▶│ Step Functions  │
│ (Telegram)  │     │  (verify +       │     │ (quotation /    │
└─────────────┘     │   normalize +    │     │  calendar /     │
                    │   start SFN)     │     │  email ASL)     │
                    └──────────────────┘     └─────────────────┘
                                                        │
                    ┌──────────────────┐                ▼
                    │  OAuthFunction   │     ┌──────────────────┐
                    │  (real Google    │     │ AttachmentCollection│
                    │   token endpoint │     │ (S3 upload + DDB)   │
                    │   + SSM refresh  │     └──────────────────┘
                    │   store)         │              ▼
                    └──────────────────┘     ┌──────────────────┐
                                             │ AgentHarness     │
                                             │ (Pi SDK + Gemini)│
                                             └──────────────────┘
                                                      ▼
                    ┌──────────────────┐     ┌──────────────────┐
                    │ GoogleActions    │     │ PdfRender        │
                    │ (real Drive/     │     │ (pure compute)   │
                    │  Sheets/Docs)    │     └──────────────────┘
                    └──────────────────┘              ▼
                    ┌──────────────────┐     ┌──────────────────┐
                    │ CalendarActions  │     │ Delivery         │
                    │ (real Calendar   │     │ (S3 + Telegram   │
                    │  REST API)       │     │  topic reply)    │
                    └──────────────────┘     └──────────────────┘
                    ┌──────────────────┐
                    │ EmailActions     │
                    │ (real Hostinger  │
                    │  SMTP)           │
                    └──────────────────┘
```

Credentials in production:

- All secrets live in AWS SSM Parameter Store `SecureString` values under
  `/novus/<environment>/...`.
- The Rust `SsmSecretProvider` (`crates/storage/src/secrets.rs`) reads and
  decrypts them at runtime.
- The agent harness reads `NOVUS_AI_PROVIDER_KEY` from the Lambda environment
  (injected from SSM by the adapter layer).

## What it would take to run real-provider paths locally

The same dependency-injection seams that production needs also enable local
execution. The pattern is: each binary reads a `NOVUS_RUNTIME=local|aws`
flag (or auto-detects), then constructs either the real adapter or a
local-equivalent adapter.

### Phase 1 — Concrete adapter implementations (shared by local + AWS)

These are the missing pieces that **both** local and production need:

1. **`ReqwestTokenEndpoint`** — implement `oauth::tokens::TokenEndpoint` using
   `reqwest` to call `https://oauth2.googleapis.com/token` and `/revoke`.
2. **`SsmRefreshTokenStore`** — implement `RefreshTokenStore` using the existing
   `SsmSecretProvider` (production) or a file-backed store (local).
3. **`ReqwestGoogleCalendarClient`** — implement `GoogleCalendarClient` using
   `reqwest` against the Calendar REST API.
4. **`ReqwestGoogleSheetsDocsClient`** — implement the Sheets/Docs create/mutate
   client.
5. **`LettreSmtpClient`** (or `reqwest`-based) — implement `SmtpClient` with a
   real TLS SMTP connection to `smtp.hostinger.com:587`.
6. **Webhook dispatch** — after normalization, persist the update to DynamoDB
   and start the relevant Step Functions execution.
7. **Attachment collection wiring** — construct the S3 object store and DynamoDB
   metadata repository and drive `AttachmentCollectionService`.
8. **Delivery wiring** — construct S3/Drive/Telegram clients and drive
   `DeliveryService`.

### Phase 2 — Local runtime mode

Once concrete adapters exist, add a local runtime mode:

- `NOVUS_RUNTIME=local` makes each binary construct local-equivalent infrastructure
  instead of AWS SDK clients:
  - DynamoDB Local at `http://localhost:8000` (already used by the sandbox).
  - LocalStack S3/SSM at `http://localhost:4566`.
  - File-backed `RefreshTokenStore` (e.g. `~/.novus/local/tokens.json`, mode
    `0600`) instead of SSM.
  - `EnvironmentSecretProvider` (already in the agent harness) for the AI key.
  - `reqwest`-based Google/SMTP clients pointed at real provider endpoints.
- `scripts/local-run.sh` loads `.env`, maps aliases, and starts `sam-api` with
  these variables.

### Phase 3 — Local config files

The `Environment` enum in `crates/application/src/config.rs` currently allows
only `Development | Staging | Production`. To support
`config/environments/local.yaml` with `environment: local`:

1. Add `Local` to the `Environment` enum.
2. Add `local` to `AllowedValues` in `config/schema/company.schema.json`.
3. Add `local` to `AllowedValues` in `infrastructure/policies/functions.yaml`
   `EnvironmentName` parameter (or scope IAM to exclude local).
4. Create `config/environments/local.yaml` with `environment: local` and
   localhost redirect URIs.
5. Create `config/company.local.yaml` if per-environment company overrides are
   needed (requires a `load_config` signature change or a convention for the
   base path).

Until `Local` is added to the enum, local runs reuse the `development`
environment config with `.env`-provided credentials.

## Why credentials alone do not enable local execution today

The Google/email/attachment/delivery Lambda binaries do not read credentials
from the environment and construct clients. They validate the event schema and
return `adapter_not_configured` immediately:

```rust
// functions/external-actions/src/bin/email.rs
async fn handle(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let _parsed: EmailActionEvent = match serde_json::from_value(event.payload) {
        Ok(event) => event,
        Err(_) => return Ok(adapter_not_configured()),
    };
    Ok(adapter_not_configured())  // ← returns here regardless of credentials
}
```

No amount of `.env` configuration changes this code path. The concrete adapter
implementations (Phase 1 above) must be written first. Until then:

- `scripts/local-run.sh preflight` can **authenticate** Google OAuth and
  Hostinger SMTP credentials (read-only checks).
- `scripts/local-run.sh sam-agent` can run a **real Gemini extraction**.
- The Docker sandbox can exercise the **contract plumbing** with fake
  providers.
- Real Drive/Calendar/SMTP/email delivery is not possible from any local or
  deployed runtime until the adapters are wired.
