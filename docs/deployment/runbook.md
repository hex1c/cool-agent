# Deployment Runbook: Novus Telegram Operations Worker

## Prerequisites

- Rust 1.97 toolchain (see `rust-toolchain.toml`)
- `cargo-lambda` 1.9.1 and `zig` 0.13.0 (for cross-compilation)
- AWS SAM CLI (validates and packages Lambda artifacts)
- Python 3.11+ with `jsonschema` and `pyyaml` (for config validation)
- Node.js 22 + `pnpm` (for agent-harness, non-critical path)
- AWS credentials with appropriate deployment-role access
- Approved pricing, FX, GST, safety-margin, and shared-charge allocation
- Per-environment budget table and guard Lambda deployed (see ADR 0002)

## The Single Gate Command

```bash
scripts/verify-release.sh <dev|staging|production> [OPTIONS]
```

This command MUST pass before any deployment proceeds. It runs the following
gates in order, failing fast on the first failure:

| # | Gate | Script / Check |
| --- | ------ | ---------------- |
| 1 | Environment validation | Explicit selection; blocks production without `--approve` |
| 2 | Config schema validation | `config/company.yaml` + `config/environments/<env>.yaml` against `config/schema/company.schema.json` |
| 3 | Required assets | `scripts/check-assets.sh <env>` |
| 4 | Budget margin | `scripts/check-budget.py <env>` |
| 5 | Critical tests | `cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace --all-features` |
| 6 | SAM validate | `sam validate --lint --template-file infrastructure/template.yaml` |
| 7 | SAM build | (optional, via `--require-sam-build`) |
| 8 | Environment isolation | Secret refs scoped to `/novus/<env>/`; environment field matches |
| 9 | Approval gate | Production: requires non-empty `docs/verification/staging-signoff.md` |

### Options

| Option | Effect |
| -------- | -------- |
| `--skip-tests` | Skip non-critical test suites (agent-harness unit tests/build). Critical suites are NEVER skippable. |
| `--require-sam-build` | Require `sam build` and `.aws-sam/build/template.yaml` artifacts. |
| `--approve` | Required for production deployment. |
| `--approval-file PATH` | Override approval file (default: `docs/verification/staging-signoff.md`). |
| `--usage-snapshot PATH` | JSON file with authoritative DynamoDB usage for budget gate. |
| `--dry-run` | Run all gates; report whether approval would be required without validating the file. |
| `--root DIR` | Operate on a copy at DIR (for testing). |

`--skip-critical` is explicitly refused (exit 2).

## Per-Environment Procedures

### Development (`dev`)

```bash
# Standard gate run (no approval, no SAM build required)
scripts/verify-release.sh dev

# With SAM build validation
scripts/verify-release.sh dev --require-sam-build

# Skip non-critical TS tests
scripts/verify-release.sh dev --skip-tests
```

Development is the local sandbox environment. It does not require approval
and has no deployment gate restrictions beyond the standard gates.

### Staging (`staging`)

```bash
# Standard gate run with SAM build
scripts/verify-release.sh staging --require-sam-build

# With budget usage snapshot from DynamoDB
scripts/verify-release.sh staging --require-sam-build \
  --usage-snapshot /path/to/staging-usage.json
```

Staging requires explicit environment selection. Approval (`--approve`) is
optional but strongly recommended before production promotion. The staging
deployment should use real IAM, provider credentials, and verified
infrastructure to validate parity before production.

### Production (`production`)

```bash
# Production gate with mandatory approval
scripts/verify-release.sh production --require-sam-build --approve \
  --approval-file docs/verification/staging-signoff.md \
  --usage-snapshot /path/to/production-usage.json
```

Production requires:

1. **Explicit `--approve` flag** — the gate refuses without it.
2. **Non-empty approval file** — default `docs/verification/staging-signoff.md`
   must exist and be non-empty, signed by the project owner after successful
   staging verification (Task 44).
3. **`--require-sam-build`** — production must have built SAM artifacts.
4. **Budget margin** — projected run-rate + operational reserve MUST stay below
   ₹300 monthly cap. An authoritative usage snapshot is strongly recommended.

## Explicit Environment Selection and Mutable Resource Isolation

Each environment (dev, staging, production) uses its own isolated set of mutable
AWS resources. The SAM template parameter `EnvironmentName` drives the naming
of tables, buckets, state machines, and secret paths:

| Environment | `EnvironmentName` | Secret scope | Budget table |
| ------------- | ------------------- | -------------- | -------------- |
| dev | `development` | `/novus/development/...` | `novus-development-budget` |
| staging | `staging` | `/novus/staging/...` | `novus-staging-budget` |
| production | `production` | `/novus/production/...` | `novus-production-budget` |

**Mutable resources CANNOT be shared across environments.** The environment
isolation gate (gate 8) enforces:

- `config/environments/<env>.yaml` declares `environment: <correct-value>`
- All `secretRef` / `tokenRef` / `SecretRef` values are scoped to
  `/novus/<env>/...` — no cross-environment references

DynamoDB tables (`novus-<env>-*`), S3 buckets, IAM roles, and state machines
are all environment-scoped. Deploying production must never affect staging
or development resources.

## Budget Margin Requirement

The budget gate (`scripts/check-budget.py <env>`) enforces:

- **Forecast baseline** = the month-12 "Total service cost with safety margin"
  row from `docs/cost/aws-monthly-forecast.csv`, divided by the environment
  count (3). The default scenario is `expected`; pass
  `--forecast-scenario worst-case-attachment` for a stricter stress check.
- **Projected run-rate** = max(forecast baseline, authoritative snapshot usage
  if provided). The conservative max means a low forecast never masks an
  over-budget snapshot.
- **Available** = ₹300 cap - projected run-rate
- **Margin floor** = operational reserve (`operationalReserveMicroInr`,
  ₹10/month per environment by default; configurable via
  `--margin-floor-micro-inr`)
- **Gate FAILS** if `available < margin floor` (insufficient margin for
  deployment)

The forecast CSV is account-aggregate across all three environments
(see `docs/cost/aws-monthly-forecast.md`). The equal per-environment split is a
conservative placeholder pending the approved per-environment allocation, so
forecast-only mode also prints a WARNING recommending an authoritative
snapshot. The expected-scenario baseline (≈₹28.72/env at month 12) leaves
comfortable margin; the worst-case stress baseline (≈₹287.74/env) leaves only
≈₹12 below the cap and triggers the warning and suspension thresholds.

Snapshot format:

```json
{
  "invoice_month": "2026-07",
  "settled_micro_inr": 50000000,
  "reserved_micro_inr": 25000000,
  "reconciled_micro_inr": 5000000
}
```

`invoice_month` must be the current or previous calendar month (YYYY-MM); an
older snapshot fails the gate as stale. For real deployments, provide an
authoritative usage snapshot exported from the environment's
`novus-<env>-budget` DynamoDB table via `--usage-snapshot`.

See ADR 0002 (`docs/adr/0002-shared-budget-control.md`) for the full budget
control architecture, threshold semantics, and fail-closed policy.

## Manually Forcing Each Gate to Fail

| # | Gate | How to force-fail |
| --- | ------ | ------------------ |
| 1 | Environment validation | Pass `production` without `--approve`, or pass an unknown env like `foo` |
| 2 | Config schema | Temporarily remove a required field from `config/company.yaml` or make `kind` wrong |
| 3 | Required assets | Delete or truncate any asset listed in `scripts/check-assets.sh` |
| 4 | Budget margin | Provide `--usage-snapshot` with `settled_micro_inr: 500000000` (exceeds ₹300 cap) |
| 5 | Critical tests | Introduce a `cargo fmt` violation or a failing unit test |
| 6 | SAM validate | Corrupt `infrastructure/template.yaml` (remove a required resource property) |
| 7 | SAM build | Delete `cargo-lambda` binary or zig so `sam build` cross-compilation fails |
| 8 | Environment isolation | Change `environment:` field in env YAML to wrong value |
| 9 | Approval | Delete or truncate `docs/verification/staging-signoff.md` |

Run `scripts/verify-release-selftest.sh` for an automated version that proves
gates 1-6 block correctly.

## Rollback Procedure

### Principle: Preserve Durable Data

Durable workflow/history data MUST be preserved during rollback:

- DynamoDB tables (`novus-<env>-workflows`, `novus-<env>-history`, `novus-<env>-budget`, etc.) are **NOT deleted**
- S3 raw/artifact/history buckets are **NOT deleted**
- Parameter Store secrets are **NOT deleted**
- CloudWatch log groups are retained

Only the Lambda function code (the SAM packaged artifact) and its configuration
are rolled back.

### Step 1: Identify the Previous Known-Good Artifact

```bash
# List recent git tags or commits for the environment
git log --oneline -20 -- infrastructure/template.yaml config/environments/<env>.yaml

# Check out the last known-good commit
git checkout <previous-good-commit-sha>
```

### Step 2: Preserve Current Artifacts

```bash
# Keep current .aws-sam/build artifacts for reference
cp -a .aws-sam/build .aws-sam/build-$(date +%Y%m%d-%H%M%S)-pre-rollback
```

### Step 3: Build and Deploy the Previous Artifact

```bash
# Build the previous version
sam build --template-file infrastructure/template.yaml

# Verify the gate still passes (skip tests if only code changed)
scripts/verify-release.sh <env> --require-sam-build [--approve] --skip-tests

# Deploy the previous SAM packaged artifact
sam deploy --template-file .aws-sam/build/template.yaml \
  --stack-name novus-<env> \
  --parameter-overrides EnvironmentName=<env-param> \
  --capabilities CAPABILITY_IAM \
  --no-fail-on-empty-changeset
```

### Step 4: Verify Rollback Success

```bash
# Verify the rollback gate passes
scripts/verify-release.sh <env> --require-sam-build [--approve]

# Confirm function versions match previous deployment
aws lambda list-versions-by-function --function-name novus-<env>-webhook

# Verify DynamoDB tables are intact
aws dynamodb describe-table --table-name novus-<env>-workflows

# Smoke test: send a test webhook event
# (use the documented test fixture for the environment)
```

### Step 5: Record the Rollback

```bash
# Tag the rollback point
git tag -a rollback-<env>-$(date +%Y%m%d-%H%M%S) \
  -m "Rollback to $(git rev-parse --short HEAD) after <reason>"

# Document in the incident record
echo "Rollback executed at $(date -Iseconds). Reason: <reason>. \
  Rolled back to commit $(git rev-parse HEAD). \
  Durable data preserved: DynamoDB, S3, Parameter Store." \
  >> docs/verification/rollback-log.md
```

### What Rollback Does NOT Do

- Does NOT delete DynamoDB tables or items
- Does NOT delete S3 objects
- Does NOT delete Parameter Store parameters
- Does NOT delete CloudWatch logs
- Does NOT modify budget reservations

Config revisions are tracked in git alongside the SAM template. Previous
`.aws-sam/build` artifacts can be regenerated from the git history.

## References

- ADR 0002: `docs/adr/0002-shared-budget-control.md` — budget control architecture
- Cost forecast: `docs/cost/aws-monthly-forecast.csv` — expected and worst-case monthly run-rates
- PRD traceability: `docs/verification/prd-traceability.md` — success-criterion mapping
- Staging sign-off: `docs/verification/staging-signoff.md` — required for production approval
- CI: `.github/workflows/rust.yml`, `typescript.yml`, `sam.yml`, `deploy.yml`
