use serde::{Deserialize, Serialize};

use crate::identity::WorkflowId;
use crate::workflow::WorkflowRevision;

/// External side-effect category. The category is part of the idempotency key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    AiProviderCall,
    TelegramSend,
    GoogleWrite,
    SmtpSend,
    S3Write,
    PdfRender,
}

/// Fingerprint of the provider target and exact logical operation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationTargetFingerprint([u8; 32]);

impl OperationTargetFingerprint {
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Stable logical operation identity reused by every physical retry attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdempotencyKey {
    workflow_id: WorkflowId,
    workflow_revision: WorkflowRevision,
    operation_kind: OperationKind,
    target: OperationTargetFingerprint,
}

impl IdempotencyKey {
    pub const fn new(
        workflow_id: WorkflowId,
        workflow_revision: WorkflowRevision,
        operation_kind: OperationKind,
        target: OperationTargetFingerprint,
    ) -> Self {
        Self {
            workflow_id,
            workflow_revision,
            operation_kind,
            target,
        }
    }

    pub const fn workflow_id(&self) -> &WorkflowId {
        &self.workflow_id
    }

    pub const fn workflow_revision(&self) -> WorkflowRevision {
        self.workflow_revision
    }

    pub const fn operation_kind(&self) -> OperationKind {
        self.operation_kind
    }

    pub const fn target(&self) -> OperationTargetFingerprint {
        self.target
    }
}
