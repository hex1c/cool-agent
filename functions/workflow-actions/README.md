# workflow-actions-functions

Thin Rust Lambda handlers for the quotation workflow (Task 34A). One Cargo
package, three binaries, no duplicated application or adapter logic.

## Handlers

| Binary     | Module              | Application port called                                     | Outcome |
|------------|---------------------|-------------------------------------------------------------|---------|
| `pdf`      | `src/bin/pdf.rs`    | `pdf::QuotationRenderer::render` (pure)                     | Rendered PDF bytes + SHA-256 + length |
| `workflow` | `src/bin/workflow.rs` | `application::resumable_confirmation::ResumableConfirmationService::consume_confirmation` + `application::quotation::ConfirmedPdfProof::from_consumed` | Revision-bound proof + resulting workflow |
| `delivery` | `src/bin/delivery.rs` | `application::artifact_delivery::ArtifactDeliveryService::deliver` (via `DeliveryRunner`) | S3/Drive/Telegram outcome DTO |

Each handler deserializes a versioned event (`novus.workflow-actions.v1`),
calls exactly one application port, and returns a typed JSON result. Failures
are terminal labels (`schema_version_mismatch`, `invalid_document`,
`wrong_action`, `not_pending`, `not_authorized`, `consume_rejected`, …); no
OAuth tokens, presigned links, or raw document content appear in responses or
logs.

## SAM handler requirements (for Task 39)

| Handler     | Runtime          | Memory (MB) | Timeout (s) | IAM needs |
|-------------|------------------|------------:|------------:|-----------|
| `pdf`       | provided.al2023  | 1024        | 60          | none (pure compute) |
| `workflow`  | provided.al2023  | 256         | 10          | DynamoDB conditional write on confirmation table (consumed by the caller; handler is pure) |
| `delivery`  | provided.al2023  | 1024        | 120         | S3 `PutObject`/`GetObject`/`HeadObject` (artifact prefix); Drive `files.copy`; DynamoDB `PutItem` (operation journal + object metadata); Secrets Manager / SSM read for owner token + presigning key |

## Adapter wiring status

`pdf` and `workflow` are fully functional (pure compute, no adapters).
`delivery` validates the event and rebuilds the `DeliveryRequest` at the
Lambda boundary; full S3/Drive/Telegram adapter wiring + operation-journal
construction is deferred to Task 39 (SAM resource packaging). The pure
preprocessing, runner injection, and outcome mapping are covered by
`tests/handlers.rs` via an injected `DeliveryRunner` fake.

## Local verification

```sh
cargo build
cargo test            # 15 unit + 9 integration tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

No AWS credentials are required for any local check.
