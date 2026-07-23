# Local AWS Sandbox

The Task 40 sandbox provides credential-free local feedback for the Novus
Telegram Operations Worker. It contains only deterministic, sanitized data and
never calls real Telegram, Google, SMTP, AI, or AWS endpoints.

## Start and verify

```bash
docker compose -f infrastructure/local/docker-compose.yaml up -d
cargo test -p integration-tests --test local_sandbox --features integration
```

`up -d` starts a one-shot `sandbox-init` container. Wait for it to exit with
status `0` before running the test:

```bash
docker compose -f infrastructure/local/docker-compose.yaml ps -a
docker compose -f infrastructure/local/docker-compose.yaml logs sandbox-init
```

Stop the sandbox and delete all in-memory/local data:

```bash
docker compose -f infrastructure/local/docker-compose.yaml down -v
```

No AWS profile is required. The compose network and Rust test use only the fake
credentials `test`/`test`. Do not replace any value in `init.sh` or
`mock-providers.json` with a development, staging, production, or customer
credential.

## Services

| Path | Local endpoint | Implementation |
| --- | --- | --- |
| DynamoDB conditional writes | `http://localhost:8000` | DynamoDB Local |
| S3 and SSM | `http://localhost:4566` | LocalStack 4.14.0 |
| Step Functions wait/resume | `http://localhost:8083` | Step Functions Local |
| Telegram, OAuth, Google, and AI | `http://localhost:8081` | `mock_server.py` |
| SMTP | `localhost:1025` | MailHog |
| Captured mail API/UI | `http://localhost:8025` | MailHog |

Endpoint overrides supported by `local_sandbox.rs` are
`DYNAMODB_ENDPOINT`, `LOCALSTACK_ENDPOINT`, `STEPFUNCTIONS_ENDPOINT`,
`MOCK_PROVIDERS_ENDPOINT`, `SMTP_ENDPOINT`, and `MAILHOG_ENDPOINT`.

## Seeded sanitized data

`sandbox-init` creates:

- `novus-development-application` and `novus-development-budget` tables with
  lowercase `pk`/`sk` keys matching the Rust storage adapters;
- `novus-development-artifacts-local` with one sanitized history object;
- fake development SSM parameters under `/novus/development/`;
- a successful one-second `novus-local-wait-resume` Step Functions execution.

`mock-providers.json` supplies deterministic Telegram membership/delivery,
Google OAuth token, Drive file, Calendar event, and AI extraction responses.
MailHog accepts SMTP and exposes captured messages through its API/UI. All
identifiers are marked `sandbox`, `local`, or `not-real`.

The integration test proves:

- a duplicate DynamoDB conditional write is rejected;
- an S3 object round-trips and a fake SSM secret resolves;
- the seeded Step Functions Wait execution completes;
- Telegram webhook secret verification and update normalization run through the
  real intake code, while delivery/membership, OAuth, Drive, Calendar, and AI
  provider paths respond with versioned sanitized fixtures;
- an SMTP message is accepted and visible through the MailHog API.

The explicitly selected live test fails when any required service is missing.
CI starts the same credential-free compose sandbox before running all-feature
Rust tests, so a green result always represents exercised local integrations.

## Local parity limits

The sandbox is fast feedback, not AWS/provider parity:

- LocalStack does not prove IAM evaluation, KMS key policies, bucket policies,
  VPC behavior, AWS quotas, CloudWatch delivery, or AWS service latency.
- Step Functions Local runs a Wait-to-Succeed proof. Task-token Lambda callback,
  timeout, retry, and ambiguous-failure paths require Task 42 E2E coverage.
- DynamoDB Local does not prove point-in-time recovery, encryption, adaptive
  capacity, streams, or regional consistency behavior.
- MailHog proves SMTP protocol acceptance only. It cannot reproduce Hostinger
  authentication, deliverability, throttling, or post-terminator ambiguity.
- Fake Telegram responses cannot prove BotFather privacy mode, topic lifecycle,
  membership visibility, callback timing, or delivery limits.
- Fake Google OAuth/Drive/Calendar responses cannot prove consent screens,
  token refresh/revocation, scope enforcement, quota, sharing, or invitation
  behavior.
- The AI fixture proves request/response plumbing and schema handling only; it
  does not measure model accuracy, latency, or provider cost.

## Staging responsibilities

Before shared deployment approval, staging must verify:

1. generated least-privilege IAM and environment isolation;
2. KMS, S3 policy, DynamoDB recovery/encryption, logs, alarms, and retention;
3. real Step Functions task-token callback, timeout, retry, and manual-review
   paths through packaged Lambda functions;
4. Telegram forum privacy/topic behavior and live membership checks;
5. Google consent, refresh/revocation, Drive and Calendar permissions/quotas;
6. Hostinger SMTP authentication and ambiguous post-terminator handling;
7. selected AI model schema reliability, latency, accuracy, and budget usage.

Use only development/staging credentials from the approved secret store. Never
copy provider credentials or customer documents into sandbox fixtures, logs,
or commits.
