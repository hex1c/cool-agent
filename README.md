# Novus Telegram Operations Worker

An internal operations worker that runs quotation, document, calendar, email, and reminder workflows from Telegram forum topics. Rust Lambda functions implement the workflow and provider integrations; a TypeScript Lambda embeds the Pi agent SDK. AWS SAM defines the deployment infrastructure.

> This repository is an implementation baseline, not a turnkey local web application. Local development is primarily test-driven through the credential-free AWS sandbox. See [`docs/prd/telegram-operations-worker.md`](docs/prd/telegram-operations-worker.md) for the product specification.

## Repository layout

| Path                      | Purpose                                                                   |
| ------------------------- | ------------------------------------------------------------------------- |
| `crates/`                 | Rust domain, application, storage, provider, email, and PDF libraries     |
| `functions/`              | Rust Lambda handlers                                                      |
| `services/agent-harness/` | TypeScript Lambda using the Pi SDK                                        |
| `infrastructure/`         | AWS SAM template, state machines, policies, monitoring, and local sandbox |
| `config/`                 | Non-secret company and environment configuration                          |
| `tests/integration/`      | Workspace integration and local-sandbox tests                             |
| `scripts/`                | Credential checks, release gates, budget checks, and asset validation     |
| `docs/`                   | PRD, architecture decisions, development notes, and deployment runbooks   |

## Prerequisites

Install these tools before running the complete project:

- Git
- Rust **1.97.0**; the repository pins it in [`rust-toolchain.toml`](rust-toolchain.toml)
- Node.js **22**
- Corepack and pnpm **11.15.1**
- Python **3.11+**
- Docker with Docker Compose v2
- AWS SAM CLI
- AWS CLI v2 (deployment only)
- `cargo-lambda` **1.9.1** and Zig **0.13.0** (SAM builds)

Example setup after installing Rust, Node.js, Python, Docker, SAM, and the AWS CLI:

```bash
rustup show
corepack enable
corepack prepare pnpm@11.15.1 --activate
cargo install cargo-lambda --version 1.9.1 --locked
python3 -m pip install --user jsonschema pyyaml
```

Install Zig 0.13.0 using your platform's package manager or the official release archive.

## Quick start: credential-free development

The local sandbox uses only fake credentials and sanitized fixtures. It never calls real Telegram, Google, Hostinger, AI, or AWS endpoints.

```bash
git clone https://github.com/hex1c/cool-agent.git
cd cool-agent

pnpm install --frozen-lockfile

docker compose -f infrastructure/local/docker-compose.yaml up -d
docker compose -f infrastructure/local/docker-compose.yaml ps -a
docker compose -f infrastructure/local/docker-compose.yaml logs sandbox-init

cargo test -p integration-tests --test local_sandbox --features integration
```

Wait for `sandbox-init` to exit with status `0` before running the integration test.

Stop the sandbox and remove its local data:

```bash
docker compose -f infrastructure/local/docker-compose.yaml down -v
```

Local services:

| Service                                 | Endpoint                |
| --------------------------------------- | ----------------------- |
| DynamoDB Local                          | `http://localhost:8000` |
| LocalStack S3 and SSM                   | `http://localhost:4566` |
| Step Functions Local                    | `http://localhost:8083` |
| Mock Telegram, Google, and AI providers | `http://localhost:8081` |
| MailHog SMTP                            | `localhost:1025`        |
| MailHog UI                              | `http://localhost:8025` |

See [`docs/development/local-sandbox.md`](docs/development/local-sandbox.md) for seeded data, endpoint overrides, and parity limitations.

## Install, build, and test

### Rust workspace

```bash
cargo fmt --all -- --check
python3 scripts/check-workspace-deps.py
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-features
```

CI uses `cargo-nextest`. If installed, the equivalent test command is:

```bash
cargo nextest run --workspace --all-features
```

### TypeScript agent harness

Use pnpm from the repository root:

```bash
pnpm install --frozen-lockfile
pnpm --filter @novus/agent-harness format:check
pnpm --filter @novus/agent-harness lint
pnpm --filter @novus/agent-harness test
pnpm --filter @novus/agent-harness build
```

### AWS SAM package

```bash
sam validate --lint --template-file infrastructure/template.yaml
sam build --template-file infrastructure/template.yaml
```

SAM builds require `cargo-lambda`, Zig 0.13.0, Node.js 22, and pnpm.

## Credentials and configuration

### Short answer

- **Credential-free local sandbox:** no credential file is required.
- **Local checks against real providers:** use the conventional root **`.env`**
  file.
- **Deployed environments:** store runtime secrets in AWS Systems Manager
  Parameter Store `SecureString` parameters. Do not put secret values in YAML
  or commit them.
- **AWS deployment identity:** use the normal AWS CLI credential chain, such as
  an SSO profile or deployment-role variables, not `.env`.

The repository provides a safe, tracked [`.env.example`](.env.example). The real
`.env` file is ignored by Git. It is consumed by the credential preflight and
local run harness; the deployed application does not load local dotenv files.

### Local provider credential file

Create `.env` from the example and restrict its permissions:

```bash
cp .env.example .env
chmod 600 .env
```

Fill in only the Telegram, Google OAuth, and Hostinger values you want to
verify. Never commit `.env`. Process environment variables override values from
the file.

Run the non-mutating preflight checks:

```bash
python3 scripts/verify-phase0-credentials.py
python3 scripts/verify-phase0-credentials.py telegram
python3 scripts/verify-phase0-credentials.py google
python3 scripts/verify-phase0-credentials.py hostinger
```

The checker authenticates and performs read-only operations. It does not send a Telegram message, mutate a Google resource, revoke OAuth access, or send an email.

Legacy ignored files are also supported:

- `telpass`: exactly one raw Telegram bot-token line
- `googleoauth`: downloaded Google **Web application** OAuth JSON

Prefer `.env` for new setups. Use `--env-file PATH` only when a separate credential file is required.

### Local run harness

[`scripts/local-run.sh`](scripts/local-run.sh) safely loads the root `.env`
without evaluating it as shell code. Variables already present in the process
environment take precedence. It maps local names to the names expected by the
Lambda handlers:

- `GEMINI_API_KEY` → `NOVUS_AI_PROVIDER_KEY`
- `TELEGRAM_WEBHOOK_SECRET` → `TELEGRAM_SECRET_TOKEN`
- `GOOGLE_OAUTH_CLIENT_ID` → `OAUTH_CLIENT_ID`
- `GOOGLE_OAUTH_REDIRECT_URI` → `OAUTH_REDIRECT_URI`

Set up and inspect the redacted configuration:

```bash
cp .env.example .env
chmod 600 .env
# Fill the providers needed for the test.
scripts/local-run.sh check
scripts/local-run.sh preflight
```

Run arbitrary development commands with the same environment:

```bash
scripts/local-run.sh run -- pnpm --filter @novus/agent-harness test
```

Build and start the local SAM webhook/OAuth API:

```bash
scripts/local-run.sh sam-build
scripts/local-run.sh sam-api
```

To receive Telegram updates, leave that process running and open two more
terminals. Start a Cloudflare Quick Tunnel in the second terminal:

```bash
scripts/local-run.sh telegram-tunnel
```

Copy the printed `https://...trycloudflare.com` URL and register it in the third
terminal. The command appends `/webhook` and sends the configured
`TELEGRAM_WEBHOOK_SECRET` as Telegram's webhook secret:

```bash
scripts/local-run.sh telegram-set-webhook https://example.trycloudflare.com
```

Remove the provider webhook when the local session ends:

```bash
scripts/local-run.sh telegram-delete-webhook
```

Invoke the agent Lambda with an AI request fixture:

```bash
scripts/local-run.sh sam-agent /path/to/ai-request.json
```

Set `NOVUS_ENV_FILE=/path/to/file` to select another ignored dotenv file. The
harness creates a temporary mode-`0600` SAM environment JSON and removes it on
exit.

For a fully automated Telegram setup, run `sam-api` in one terminal and
`telegram-dev` in another. It starts a Cloudflare Quick Tunnel, captures the
public HTTPS URL, registers the webhook with Telegram, and cleans up on
Ctrl-C:

```bash
scripts/local-run.sh sam-api        # terminal 1
scripts/local-run.sh telegram-dev   # terminal 2
```

The local API exposes the current webhook verifier and OAuth flow. Receiving a
Telegram update currently proves webhook verification and normalization only;
the webhook Lambda does not dispatch the complete workflow or reply to the
chat.

The OAuth Lambda uses `StubTokenEndpoint` and `StubRefreshTokenStore`, so it does
not send the configured client secret to Google or persist a real refresh token.
Google/email action Lambda binaries return `adapter_not_configured` and do not
construct real Drive/Calendar/SMTP clients. Supplying credentials cannot enable
code paths that have not been wired yet. The Docker sandbox remains the
supported end-to-end contract test for those adapters.

### Non-secret configuration files

Edit these tracked files with environment-appropriate, non-secret values and secret **references**:

- [`config/company.yaml`](config/company.yaml): company identity, quotation defaults, Drive/Calendar/email settings, model selection, limits, and budgets
- [`config/environments/dev.yaml`](config/environments/dev.yaml)
- [`config/environments/staging.yaml`](config/environments/staging.yaml)
- [`config/environments/production.yaml`](config/environments/production.yaml)

Do not replace `botTokenRef`, `webhookSecretRef`, `clientSecretRef`, or `secretRef` with raw credentials. References must remain under the matching environment prefix:

```text
/novus/development/...
/novus/staging/...
/novus/production/...
```

Expected secret paths include:

```text
/novus/<environment>/telegram/bot-token
/novus/<environment>/telegram/webhook-secret
/novus/<environment>/google/client-secret
/novus/<environment>/smtp/credentials
/novus/<environment>/ai/provider-key
```

The SAM template also expects `/novus/<environment>/config` to contain the Base64-encoded merged deployment configuration YAML. Secret and configuration parameters are provisioned out-of-band; the repository intentionally does not embed their values.

### AWS credentials for deployment

Prefer short-lived AWS SSO or an assumed deployment role:

```bash
aws configure sso --profile novus-dev
export AWS_PROFILE=novus-dev
export AWS_REGION=ap-south-1
aws sts get-caller-identity
```

Do not put `AWS_ACCESS_KEY_ID` or `AWS_SECRET_ACCESS_KEY` in the provider credential file unless your organization explicitly requires environment-based temporary credentials. Never commit AWS credentials.

## Release gates and deployment

Run the release gate before deployment:

```bash
scripts/verify-release.sh dev --require-sam-build
scripts/verify-release.sh staging --require-sam-build
scripts/verify-release.sh production --require-sam-build --approve
```

For the first development deployment:

```bash
sam validate --lint --template-file infrastructure/template.yaml
sam build --template-file infrastructure/template.yaml
sam deploy --guided \
  --template-file .aws-sam/build/template.yaml \
  --config-file infrastructure/samconfig.toml
```

Confirm that the selected stack, `EnvironmentName`, config parameter, secret paths, AWS profile, and region all belong to the same environment. Production additionally requires a completed staging sign-off.

Read [`docs/deployment/runbook.md`](docs/deployment/runbook.md) before deploying. It documents environment isolation, budget gates, approval requirements, and rollback.

## Useful documentation

- [Product specification](docs/prd/telegram-operations-worker.md)
- [Local sandbox](docs/development/local-sandbox.md)
- [Local vs production runtime](docs/development/local-vs-production.md)
- [Deployment runbook](docs/deployment/runbook.md)
- [DynamoDB key design](docs/architecture/dynamodb-keys.md)
- [AWS cost forecast](docs/cost/aws-monthly-forecast.md)
- [Quotation layout](docs/quotation-layout.md)

## Security rules

- Never commit provider credentials, OAuth tokens, customer documents, or real secrets.
- Never place real credentials in `infrastructure/local/init.sh` or `infrastructure/local/mock-providers.json`.
- Keep `.env` and downloaded OAuth files mode `0600`.
- Use separate secrets and mutable AWS resources for development, staging, and production.
- Treat presigned S3 links as bearer credentials.
- Use only the approved non-Gmail Google scopes documented by the project.
