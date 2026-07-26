#![deny(unsafe_code)]

//! Shared event/result DTOs and pure processing functions for the
//! workflow-actions Lambda package (Task 34A).
//!
//! The package exposes three thin Lambda binaries:
//! - `pdf` — renders a Version 1 quotation PDF (pure compute).
//! - `workflow` — consumes a pending `StartDirectPdfGeneration` confirmation
//!   and emits a revision-bound proof (pure domain transition).
//! - `delivery` — orchestrates S3 publication, Drive copy, and topic
//!   delivery through the shared executor (I/O via injected ports).
//!
//! Each binary deserializes a versioned event, calls exactly one application
//! port, and returns a typed result. Adapter wiring (S3/Drive/Telegram
//! clients) is injected so the pure processing functions are unit-testable
//! without AWS credentials.
//!
//! # SAM handler requirements (documented for Task 39)
//!
//! | Handler     | Runtime        | Memory (MB) | Timeout (s) | IAM needs |
//! |-------------|----------------|------------:|------------:|-----------|
//! | `pdf`       | provided.al2023| 1024        | 60          | none (pure compute; writes are deferred to `delivery`) |
//! | `workflow`  | provided.al2023| 256         | 10          | DynamoDB: `ConditionCheck`/`UpdateItem` on workflow + confirmation tables (consumed in the caller; this handler is pure) |
//! | `delivery`  | provided.al2023| 1024        | 120         | S3 `PutObject`/`GetObject`/`HeadObject` on artifact prefix; Drive `files.copy`; DynamoDB `PutItem` on operation-journal + object-metadata tables; Secrets Manager read for owner token; `ssm:GetParameter` for presigning key |
//!
//! All handlers log structured, redacted labels only — no OAuth tokens,
//! presigned links, or raw document content.

pub mod cost_guard;
pub mod delivery;
pub mod pdf;
pub mod workflow;

pub use application::artifact_delivery::{DeliveryError, DeliveryRequest, DeliveryResult};
pub use delivery::{DeliveryEvent, DeliveryResultDto, DeliveryRunner, process_delivery};
pub use pdf::{PdfRenderError, PdfRenderEvent, PdfRenderResult};
pub use workflow::{WorkflowActionError, WorkflowActionEvent, WorkflowActionResult};

/// Schema version stamp carried by every event. Handlers reject mismatched
/// versions before any work.
pub const EVENT_SCHEMA_VERSION: &str = "novus.workflow-actions.v1";
