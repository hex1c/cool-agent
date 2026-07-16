#![deny(unsafe_code)]

//! Pure workflow and policy contracts.

pub mod attachment;
pub mod contracts;
pub mod identity;
pub mod money;
pub mod routing;
pub mod transition;
pub mod workflow;

pub use transition::{
    TransitionAudit, TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
    WorkflowTransitionKind,
};
pub use workflow::{
    ClarificationResume, WaitDeadline, Workflow, WorkflowRevision, WorkflowState,
    WorkflowStateKind, WorkflowTimestamp,
};
