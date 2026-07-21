//! Resumable clarification, preview, and confirmation orchestration.
//!
//! Read-only application layer that completes a topic workflow through:
//! 1. A [`Preview`] with original/calculated values, assumptions, currency, taxes,
//!    and totals.
//! 2. A durable 12‑hour clarification wait with resume / timeout / cancel.
//! 3. Revision‑bound confirmation/correction with audit attribution and **no
//!    external mutation** — the returned [`ConfirmationConsumption`] carries
//!    only the transition, consumed record, and audit.

use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use domain::authorization::{
    AuthorizedActionError, AuthorizedParticipant, AuthorizedTransitionOutcome,
    AuthorizedTransitionRequest, AuthorizedWorkflowAction,
};
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumeRequest, ConfirmationConsumption,
    ConfirmationCorrectionOutcome, ConfirmationCorrectionRequest, ConfirmationError,
    ConfirmationIssueOutcome, ConfirmationIssueRequest, ConfirmationRecord,
    MutationTargetFingerprint, PreviewDigest, TopicMessageReference,
};
use domain::identity::{ConfirmationId, MessageId, ParticipantId};
use domain::transition::{
    TransitionError, TransitionOutcome, TransitionRequest, WorkflowTransition,
};
use domain::workflow::{
    ClarificationResume, WaitDeadline, Workflow, WorkflowState, WorkflowTimestamp,
};

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// A single line item in a preview shown to participants before confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PreviewLineItem {
    pub description: String,
    pub quantity: String,
    pub unit_price_micro_inr: i64,
}

/// A typed preview of calculated values shown to participants for confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Preview {
    pub currency: String,
    pub items: Vec<PreviewLineItem>,
    pub assumptions: Vec<String>,
    pub subtotal_micro_inr: i64,
    pub tax_rate_bps: u32,
    pub tax_micro_inr: i64,
    pub total_micro_inr: i64,
}

/// Validation failures for [`Preview`] construction and serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    InvalidCurrency { value: String },
    EmptyItems,
    NegativeSubtotal { value: i64 },
    NegativeTax { value: i64 },
    NegativeTotal { value: i64 },
    TaxRateOutOfRange { value: u32 },
    TotalMismatch { expected: i64, actual: i64 },
    SerializationFailed,
}

impl Display for PreviewError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCurrency { value } => write!(f, "invalid currency code: {value}"),
            Self::EmptyItems => f.write_str("preview must contain at least one line item"),
            Self::NegativeSubtotal { value } => write!(f, "negative subtotal: {value}"),
            Self::NegativeTax { value } => write!(f, "negative tax: {value}"),
            Self::NegativeTotal { value } => write!(f, "negative total: {value}"),
            Self::TaxRateOutOfRange { value } => {
                write!(f, "tax rate {value} bps out of range 0..=10000")
            }
            Self::TotalMismatch { expected, actual } => write!(
                f,
                "total {actual} does not equal subtotal + tax ({expected})"
            ),
            Self::SerializationFailed => f.write_str("preview serialization failed"),
        }
    }
}

impl std::error::Error for PreviewError {}

impl Preview {
    /// Construct a validated [`Preview`].
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError`] when any validation rule is violated.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        currency: String,
        items: Vec<PreviewLineItem>,
        assumptions: Vec<String>,
        subtotal_micro_inr: i64,
        tax_rate_bps: u32,
        tax_micro_inr: i64,
        total_micro_inr: i64,
    ) -> Result<Self, PreviewError> {
        if !is_valid_currency(&currency) {
            return Err(PreviewError::InvalidCurrency { value: currency });
        }
        if items.is_empty() {
            return Err(PreviewError::EmptyItems);
        }
        if subtotal_micro_inr < 0 {
            return Err(PreviewError::NegativeSubtotal {
                value: subtotal_micro_inr,
            });
        }
        if tax_micro_inr < 0 {
            return Err(PreviewError::NegativeTax {
                value: tax_micro_inr,
            });
        }
        if total_micro_inr < 0 {
            return Err(PreviewError::NegativeTotal {
                value: total_micro_inr,
            });
        }
        if tax_rate_bps > 10_000 {
            return Err(PreviewError::TaxRateOutOfRange {
                value: tax_rate_bps,
            });
        }
        let expected_total = subtotal_micro_inr
            .checked_add(tax_micro_inr)
            .unwrap_or(i64::MAX);
        if total_micro_inr != expected_total {
            return Err(PreviewError::TotalMismatch {
                expected: expected_total,
                actual: total_micro_inr,
            });
        }
        Ok(Self {
            currency,
            items,
            assumptions,
            subtotal_micro_inr,
            tax_rate_bps,
            tax_micro_inr,
            total_micro_inr,
        })
    }

    /// Compute a SHA‑256 [`PreviewDigest`] from a canonical JSON serialization.
    ///
    /// # Canonicalization
    ///
    /// The preview is serialized to a [`serde_json::Value`] (which uses
    /// [`BTreeMap`](std::collections::BTreeMap) for objects, producing sorted
    /// keys), then re‑serialized to compact bytes.  Equal previews therefore
    /// produce identical digests regardless of struct‑field declaration order.
    pub fn digest(&self) -> Result<PreviewDigest, PreviewError> {
        let value = serde_json::to_value(self).map_err(|_| PreviewError::SerializationFailed)?;
        let canonical =
            serde_json::to_vec(&value).map_err(|_| PreviewError::SerializationFailed)?;
        let hash: [u8; 32] = Sha256::digest(&canonical).into();
        Ok(PreviewDigest::new(hash))
    }

    /// Compute a [`MutationTargetFingerprint`] that binds the preview to an
    /// intended mutation target label.
    ///
    /// The fingerprint is `SHA‑256(target_label ":" hex(digest))`.
    pub fn mutation_target(
        &self,
        target_label: &str,
    ) -> Result<MutationTargetFingerprint, PreviewError> {
        let digest = self.digest()?;
        let combined = format!("{}:{}", target_label, hex::encode(digest.as_bytes()));
        let hash: [u8; 32] = Sha256::digest(combined.as_bytes()).into();
        Ok(MutationTargetFingerprint::new(hash))
    }
}

fn is_valid_currency(s: &str) -> bool {
    s.len() == 3 && s.as_bytes().iter().all(|b| b.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// ResumableError
// ---------------------------------------------------------------------------

/// Typed errors raised by [`ResumableConfirmationService`].
#[derive(Debug)]
pub enum ResumableError {
    Preview(PreviewError),
    Transition(TransitionError),
    Confirmation(ConfirmationError),
    Authorization(AuthorizedActionError),
    /// The clarification `resume` stage is not legal from the current workflow
    /// state (e.g. trying to resume to `CalculationOrDrafting` from
    /// `ExtractionCompleted`).
    IllegalClarificationStage,
    /// The wait deadline has not elapsed yet.
    DeadlineNotElapsed,
}

impl Display for ResumableError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preview(e) => write!(f, "preview error: {e}"),
            Self::Transition(e) => write!(f, "transition error: {e}"),
            Self::Confirmation(e) => write!(f, "confirmation error: {e}"),
            Self::Authorization(e) => write!(f, "authorization error: {e}"),
            Self::IllegalClarificationStage => {
                f.write_str("clarification stage is not legal from current workflow state")
            }
            Self::DeadlineNotElapsed => f.write_str("deadline has not elapsed yet"),
        }
    }
}

impl std::error::Error for ResumableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Preview(e) => Some(e),
            Self::Transition(e) => Some(e),
            Self::Confirmation(e) => Some(e),
            Self::Authorization(e) => Some(e),
            Self::IllegalClarificationStage | Self::DeadlineNotElapsed => None,
        }
    }
}

impl From<PreviewError> for ResumableError {
    fn from(e: PreviewError) -> Self {
        Self::Preview(e)
    }
}

impl From<TransitionError> for ResumableError {
    fn from(e: TransitionError) -> Self {
        Self::Transition(e)
    }
}

impl From<ConfirmationError> for ResumableError {
    fn from(e: ConfirmationError) -> Self {
        Self::Confirmation(e)
    }
}

impl From<AuthorizedActionError> for ResumableError {
    fn from(e: AuthorizedActionError) -> Self {
        Self::Authorization(e)
    }
}

// ---------------------------------------------------------------------------
// ResumableConfirmationService
// ---------------------------------------------------------------------------

/// Stateless orchestration for resumable clarification, preview, and confirmation.
///
/// Every method is pure — it operates over domain types and returns only the
/// resulting transition or confirmation outcome.  External I/O and persistence
/// are the caller's responsibility.
pub struct ResumableConfirmationService;

/// Inputs for [`ResumableConfirmationService::issue_confirmation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueConfirmationRequest<'a> {
    pub preview: &'a Preview,
    pub action: ConfirmationAction,
    pub target_label: &'a str,
    pub confirmation_id: ConfirmationId,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub deadline: WaitDeadline,
    pub timestamp: WorkflowTimestamp,
}

impl ResumableConfirmationService {
    // -- clarification -------------------------------------------------------

    /// Enter a durable clarification wait.
    ///
    /// The `resume` stage must be legal from the current workflow state:
    /// * [`WorkflowState::CalculationOrDraftingCompleted`] →
    ///   [`ClarificationResume::CalculationOrDrafting`]
    /// * [`WorkflowState::ExtractionCompleted`] →
    ///   [`ClarificationResume::Extraction`]
    pub fn enter_clarification_wait(
        &self,
        workflow: &Workflow,
        actor: ParticipantId,
        source_message: MessageId,
        resume: ClarificationResume,
        deadline: WaitDeadline,
        timestamp: WorkflowTimestamp,
    ) -> Result<TransitionOutcome, ResumableError> {
        match (workflow.state(), resume) {
            (
                WorkflowState::CalculationOrDraftingCompleted,
                ClarificationResume::CalculationOrDrafting,
            ) => {}
            (WorkflowState::ExtractionCompleted, ClarificationResume::Extraction) => {}
            _ => return Err(ResumableError::IllegalClarificationStage),
        }

        let outcome = workflow.transition(TransitionRequest {
            transition: WorkflowTransition::RequestClarification { deadline },
            expected_revision: workflow.revision(),
            actor,
            source_message,
            timestamp,
        })?;
        Ok(outcome)
    }

    /// Resume from a clarification wait.
    ///
    /// * `AttachmentCollection` resume requires a `deadline_for_resume`.
    /// * `Extraction` / `CalculationOrDrafting` resume ignores
    ///   `deadline_for_resume`.
    pub fn resume_clarification(
        &self,
        workflow: &Workflow,
        actor: ParticipantId,
        source_message: MessageId,
        deadline_for_resume: Option<WaitDeadline>,
        timestamp: WorkflowTimestamp,
    ) -> Result<TransitionOutcome, ResumableError> {
        let resume = match workflow.state() {
            WorkflowState::WaitingForClarification { resume, .. } => *resume,
            _ => return Err(ResumableError::IllegalClarificationStage),
        };

        let transition = match resume {
            ClarificationResume::AttachmentCollection => {
                let deadline =
                    deadline_for_resume.ok_or(ResumableError::IllegalClarificationStage)?;
                WorkflowTransition::ResumeAttachmentCollection { deadline }
            }
            ClarificationResume::Extraction | ClarificationResume::CalculationOrDrafting => {
                WorkflowTransition::ProvideClarification
            }
        };

        let outcome = workflow.transition(TransitionRequest {
            transition,
            expected_revision: workflow.revision(),
            actor,
            source_message,
            timestamp,
        })?;
        Ok(outcome)
    }

    /// Timeout a clarification wait.  The deadline must have elapsed.
    pub fn timeout_clarification(
        &self,
        workflow: &Workflow,
        actor: ParticipantId,
        source_message: MessageId,
        timestamp: WorkflowTimestamp,
    ) -> Result<TransitionOutcome, ResumableError> {
        let outcome = workflow.transition(TransitionRequest {
            transition: WorkflowTransition::ExpireWorkflow,
            expected_revision: workflow.revision(),
            actor,
            source_message,
            timestamp,
        })?;
        Ok(outcome)
    }

    /// Cancel a workflow as an authorized participant.
    ///
    /// Audit attribution is included in the returned
    /// [`AuthorizedTransitionOutcome`].
    pub fn cancel(
        &self,
        workflow: &Workflow,
        authorization: &AuthorizedParticipant,
        source_message: MessageId,
        timestamp: WorkflowTimestamp,
    ) -> Result<AuthorizedTransitionOutcome, ResumableError> {
        let outcome = workflow.stop_authorized(
            authorization,
            AuthorizedTransitionRequest {
                expected_workflow_revision: workflow.revision(),
                topic: workflow.topic(),
                source_message,
                timestamp,
            },
        )?;
        Ok(outcome)
    }

    // -- confirmation --------------------------------------------------------

    /// Issue a revision‑bound confirmation, entering
    /// [`WorkflowState::WaitingForConfirmation`].
    ///
    /// The preview digest and mutation‑target fingerprint are derived from
    /// `preview` and `target_label`.
    pub fn issue_confirmation(
        &self,
        workflow: &Workflow,
        request: IssueConfirmationRequest<'_>,
    ) -> Result<ConfirmationIssueOutcome, ResumableError> {
        let digest = request.preview.digest()?;
        let mutation_target = request.preview.mutation_target(request.target_label)?;

        let outcome = workflow.issue_confirmation(ConfirmationIssueRequest {
            confirmation_id: request.confirmation_id,
            expected_workflow_revision: workflow.revision(),
            topic: workflow.topic(),
            preview_digest: digest,
            mutation_target,
            action: request.action,
            deadline: request.deadline,
            actor: request.actor,
            source_message: request.source_message,
            timestamp: request.timestamp,
        })?;
        Ok(outcome)
    }

    /// Consume a pending confirmation with a matching preview and authorized
    /// participant.
    ///
    /// **No external write is performed.** The returned
    /// [`ConfirmationConsumption`] contains only the transition, consumed
    /// record, and authorization audit — the caller (a later task) performs
    /// the external operation.
    #[allow(clippy::too_many_arguments)]
    pub fn consume_confirmation(
        &self,
        confirmation: &ConfirmationRecord,
        workflow: &Workflow,
        authorization: &AuthorizedWorkflowAction,
        preview: &Preview,
        target_label: &str,
        source: TopicMessageReference,
        confirmed_at: WorkflowTimestamp,
    ) -> Result<ConfirmationConsumption, ResumableError> {
        let digest = preview.digest()?;
        let mutation_target = preview.mutation_target(target_label)?;

        let consumption = confirmation.consume(
            workflow,
            authorization,
            ConfirmationConsumeRequest {
                preview_digest: digest,
                mutation_target,
                source,
                confirmed_at,
            },
        )?;
        Ok(consumption)
    }

    /// Apply an approved correction, invalidating the pending revision.
    ///
    /// The domain moves the workflow to
    /// [`WorkflowState::CalculationOrDraftingStarted`], so a subsequent
    /// re‑issue produces a fresh digest and revision.
    pub fn correct(
        &self,
        confirmation: &ConfirmationRecord,
        workflow: &Workflow,
        authorization: &AuthorizedWorkflowAction,
        source: TopicMessageReference,
        corrected_at: WorkflowTimestamp,
    ) -> Result<ConfirmationCorrectionOutcome, ResumableError> {
        let outcome = confirmation.correct(
            workflow,
            authorization,
            ConfirmationCorrectionRequest {
                source,
                corrected_at,
            },
        )?;
        Ok(outcome)
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use super::*;
    use domain::authorization::{LiveMembershipEvidence, MembershipStatus, authorize_participant};
    use domain::identity::{ChatId, MessageThreadId, TopicSessionId, WorkflowId};
    use std::error::Error as _;

    // -- helpers ------------------------------------------------------------

    fn topic() -> TopicSessionId {
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77).unwrap())
    }

    fn participant(value: i64) -> ParticipantId {
        ParticipantId::new(value).unwrap()
    }

    fn time(seconds: u64) -> WorkflowTimestamp {
        WorkflowTimestamp::from_unix_seconds(seconds)
    }

    fn advance(workflow: &Workflow, transition: WorkflowTransition, at: u64) -> Workflow {
        workflow
            .transition(TransitionRequest {
                transition,
                expected_revision: workflow.revision(),
                actor: participant(202),
                source_message: MessageId::new(i64::try_from(at).unwrap()).unwrap(),
                timestamp: time(at),
            })
            .unwrap()
            .workflow
    }

    fn extraction_completed() -> Workflow {
        let wf = Workflow::new(
            WorkflowId::new("wf-extraction").unwrap(),
            topic(),
            participant(101),
            time(1),
        );
        let wf = advance(
            &wf,
            WorkflowTransition::BeginAttachmentCollection {
                deadline: WaitDeadline::at(time(30)),
            },
            2,
        );
        let wf = advance(&wf, WorkflowTransition::FinishAttachmentCollection, 3);
        advance(&wf, WorkflowTransition::CompleteExtraction, 4)
    }

    fn drafting_completed() -> Workflow {
        let wf = extraction_completed();
        let wf = advance(&wf, WorkflowTransition::StartCalculationOrDrafting, 5);
        advance(&wf, WorkflowTransition::CompleteCalculationOrDrafting, 6)
    }

    fn authorized_participant(at: u64) -> AuthorizedParticipant {
        authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(at),
            ),
            time(at),
        )
        .unwrap()
    }

    fn preview() -> Preview {
        Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Consulting".to_string(),
                quantity: "2".to_string(),
                unit_price_micro_inr: 50_000_000,
            }],
            vec!["standard rate".to_string()],
            100_000_000,
            1800,
            18_000_000,
            118_000_000,
        )
        .unwrap()
    }

    // -- preview tests ------------------------------------------------------

    #[test]
    fn valid_preview_builds() {
        let p = preview();
        assert_eq!(p.currency, "INR");
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.total_micro_inr, 118_000_000);
    }

    #[test]
    fn digest_is_deterministic() {
        let p = preview();
        let d1 = p.digest().unwrap();
        let d2 = p.digest().unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn digest_differs_for_different_previews() {
        let p1 = preview();
        let p2 = Preview::new(
            "USD".to_string(),
            vec![PreviewLineItem {
                description: "Item".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        )
        .unwrap();
        assert_ne!(p1.digest().unwrap(), p2.digest().unwrap());
    }

    #[test]
    fn total_not_equal_subtotal_plus_tax_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            200,
        );
        assert!(matches!(err, Err(PreviewError::TotalMismatch { .. })));
    }

    #[test]
    fn bad_currency_rejected() {
        let err = Preview::new(
            "inr".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::InvalidCurrency { .. })));

        let err = Preview::new(
            "IN".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            0,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::InvalidCurrency { .. })));
    }

    #[test]
    fn negative_amount_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            -1,
            0,
            0,
            -1,
        );
        assert!(matches!(err, Err(PreviewError::NegativeSubtotal { .. })));
    }

    #[test]
    fn empty_items_rejected() {
        let err = Preview::new("INR".to_string(), vec![], vec![], 0, 0, 0, 0);
        assert!(matches!(err, Err(PreviewError::EmptyItems)));
    }

    #[test]
    fn tax_rate_out_of_range_rejected() {
        let err = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "x".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 100,
            }],
            vec![],
            100,
            10001,
            0,
            100,
        );
        assert!(matches!(err, Err(PreviewError::TaxRateOutOfRange { .. })));
    }

    #[test]
    fn mutation_target_differs_per_label() {
        let p = preview();
        let t1 = p.mutation_target("sheet-write").unwrap();
        let t2 = p.mutation_target("pdf-gen").unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn mutation_target_stable_for_same_label() {
        let p = preview();
        let t1 = p.mutation_target("sheet-write").unwrap();
        let t2 = p.mutation_target("sheet-write").unwrap();
        assert_eq!(t1, t2);
    }

    // -- clarification tests ------------------------------------------------

    #[test]
    fn enter_clarification_wait_from_drafting_completed() {
        let wf = drafting_completed();
        let svc = ResumableConfirmationService;
        let deadline = WaitDeadline::at(time(43_200));
        let outcome = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                ClarificationResume::CalculationOrDrafting,
                deadline,
                time(10),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::WaitingForClarification { .. }
        ));
    }

    #[test]
    fn enter_clarification_wait_illegal_stage_rejected() {
        let wf = drafting_completed();
        let svc = ResumableConfirmationService;
        let err = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(10).unwrap(),
                ClarificationResume::Extraction,
                WaitDeadline::at(time(43_200)),
                time(10),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    fn waiting_for_clarification() -> Workflow {
        let svc = ResumableConfirmationService;
        let wf = drafting_completed();
        svc.enter_clarification_wait(
            &wf,
            participant(202),
            MessageId::new(10).unwrap(),
            ClarificationResume::CalculationOrDrafting,
            WaitDeadline::at(time(43_200)),
            time(10),
        )
        .unwrap()
        .workflow
    }

    #[test]
    fn resume_clarification_via_provide() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let outcome = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap();
        assert!(matches!(
            outcome.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));
    }

    #[test]
    fn timeout_clarification_after_deadline() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let outcome = svc
            .timeout_clarification(
                &wf,
                participant(202),
                MessageId::new(44_000).unwrap(),
                time(44_000),
            )
            .unwrap();
        assert!(matches!(outcome.workflow.state(), WorkflowState::Expired));
    }

    #[test]
    fn timeout_clarification_before_deadline_rejected() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let err = svc
            .timeout_clarification(&wf, participant(202), MessageId::new(20).unwrap(), time(20))
            .unwrap_err();
        assert!(matches!(err, ResumableError::Transition(_)));
    }

    #[test]
    fn cancel_clarification_authorized() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(12);
        let outcome = svc
            .cancel(&wf, &authz, MessageId::new(12).unwrap(), time(12))
            .unwrap();
        assert!(matches!(
            outcome.transition.workflow.state(),
            WorkflowState::Stopped
        ));
        // audit attribution is present
        assert_eq!(outcome.authorization.actor, participant(202));
    }

    #[test]
    fn duplicate_resume_rejected() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let wf = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap()
            .workflow;
        // second resume fails — state is now CalculationOrDraftingStarted
        let err = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(12).unwrap(),
                None,
                time(12),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    #[test]
    fn reenter_wait_from_non_waiting_rejected() {
        let wf = waiting_for_clarification();
        let svc = ResumableConfirmationService;
        let wf = svc
            .resume_clarification(
                &wf,
                participant(202),
                MessageId::new(11).unwrap(),
                None,
                time(11),
            )
            .unwrap()
            .workflow;
        let err = svc
            .enter_clarification_wait(
                &wf,
                participant(202),
                MessageId::new(12).unwrap(),
                ClarificationResume::CalculationOrDrafting,
                WaitDeadline::at(time(44_000)),
                time(12),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::IllegalClarificationStage));
    }

    // -- confirmation tests -------------------------------------------------

    fn waiting_for_confirmation() -> (Workflow, Preview, ConfirmationRecord) {
        let svc = ResumableConfirmationService;
        let wf = drafting_completed();
        let p = preview();
        let outcome = svc
            .issue_confirmation(
                &wf,
                IssueConfirmationRequest {
                    preview: &p,
                    action: ConfirmationAction::StartSheetOrDocWrite,
                    target_label: "sheet-write",
                    confirmation_id: ConfirmationId::new("confirmation-1").unwrap(),
                    actor: participant(202),
                    source_message: MessageId::new(7).unwrap(),
                    deadline: WaitDeadline::at(time(43_200)),
                    timestamp: time(7),
                },
            )
            .unwrap();
        let record = ConfirmationRecord::from(outcome.confirmation);
        (outcome.transition.workflow, p, record)
    }

    #[test]
    fn issue_confirmation_enters_waiting_state() {
        let (wf, _preview, _record) = waiting_for_confirmation();
        assert!(matches!(
            wf.state(),
            WorkflowState::WaitingForConfirmation { .. }
        ));
    }

    #[test]
    fn consume_with_matching_digest_and_target() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);
        let consumption = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap();

        // No external write performed — the outcome carries only
        // transition + confirmation + audit. The consume transitioned to
        // SheetOrDocWriteStarted (action = StartSheetOrDocWrite), which is
        // NOT terminal — the external operation is a later task's job.
        assert!(!consumption.transition().workflow.state().is_terminal());
        assert!(matches!(
            consumption.transition().workflow.state().kind(),
            domain::WorkflowStateKind::SheetOrDocWriteStarted
        ));

        // Confirm the confirmation is now consumed
        assert!(matches!(
            consumption.confirmation().status(),
            domain::confirmation::ConfirmationStatus::Consumed
        ));
    }

    #[test]
    fn stale_revision_after_correction_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(9).for_workflow(&wf);

        // First, correct to invalidate the pending revision.
        let correction = svc
            .correct(
                &record,
                &wf,
                &authz,
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap();
        assert!(matches!(
            correction.transition.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));

        // Now try to consume with the stale (original) record — should fail.
        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(10).unwrap()),
                time(10),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn wrong_digest_rejected() {
        let (wf, _preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);

        let different_preview = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Other".to_string(),
                quantity: "1".to_string(),
                unit_price_micro_inr: 500,
            }],
            vec![],
            500,
            0,
            0,
            500,
        )
        .unwrap();

        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &different_preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn expired_confirmation_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;

        // Advance time past the deadline.
        let authz_late = authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(50_000),
            ),
            time(50_000),
        )
        .unwrap()
        .for_workflow(&wf);

        let err = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz_late,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(50_000).unwrap()),
                time(50_000),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn already_consumed_rejected() {
        let (wf, preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(8).for_workflow(&wf);

        // Consume once.
        let first = svc
            .consume_confirmation(
                &record,
                &wf,
                &authz,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(8).unwrap()),
                time(8),
            )
            .unwrap();
        let consumed_record = first.confirmation();

        // Try consuming again with a new authorization at the same time
        // (the record is now consumed).
        let authz2 = authorize_participant(
            topic().chat_id(),
            participant(202),
            &LiveMembershipEvidence::new(
                topic().chat_id(),
                participant(202),
                MembershipStatus::Approved,
                time(9),
            ),
            time(9),
        )
        .unwrap()
        .for_workflow(&wf);

        let err = svc
            .consume_confirmation(
                consumed_record,
                &wf,
                &authz2,
                &preview,
                "sheet-write",
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap_err();
        assert!(matches!(err, ResumableError::Confirmation(_)));
    }

    #[test]
    fn correction_invalidates_revision() {
        let (wf, _preview, record) = waiting_for_confirmation();
        let svc = ResumableConfirmationService;
        let authz = authorized_participant(9).for_workflow(&wf);

        let correction = svc
            .correct(
                &record,
                &wf,
                &authz,
                TopicMessageReference::new(topic(), MessageId::new(9).unwrap()),
                time(9),
            )
            .unwrap();

        // Workflow moves to CalculationOrDraftingStarted after correction.
        assert!(matches!(
            correction.transition.workflow.state(),
            WorkflowState::CalculationOrDraftingStarted
        ));

        // A re-issue from the corrected workflow with a new revision should
        // produce a fresh digest and revision.
        let new_preview = Preview::new(
            "INR".to_string(),
            vec![PreviewLineItem {
                description: "Revised".to_string(),
                quantity: "3".to_string(),
                unit_price_micro_inr: 30_000_000,
            }],
            vec![],
            90_000_000,
            10_00,
            9_000_000,
            99_000_000,
        )
        .unwrap();

        let wf = correction.transition.workflow;
        // Complete drafting again
        let wf = advance(&wf, WorkflowTransition::CompleteCalculationOrDrafting, 10);

        let reissue = svc
            .issue_confirmation(
                &wf,
                IssueConfirmationRequest {
                    preview: &new_preview,
                    action: ConfirmationAction::StartSheetOrDocWrite,
                    target_label: "sheet-write",
                    confirmation_id: ConfirmationId::new("confirmation-2").unwrap(),
                    actor: participant(202),
                    source_message: MessageId::new(11).unwrap(),
                    deadline: WaitDeadline::at(time(50_000)),
                    timestamp: time(11),
                },
            )
            .unwrap();

        assert_ne!(
            reissue.confirmation.preview_digest(),
            match &record {
                ConfirmationRecord::Pending(p) => p.preview_digest(),
                ConfirmationRecord::Consumed(_) => panic!("expected pending"),
            }
        );
    }

    // -- ResumableError Display / source ------------------------------------

    #[test]
    fn resumable_error_displays_and_sources() {
        let preview_err = PreviewError::EmptyItems;
        let re: ResumableError = preview_err.into();
        let s = re.to_string();
        assert!(s.contains("preview error"));
        assert!(re.source().is_some());

        let from_transition = ResumableError::IllegalClarificationStage;
        assert!(from_transition.source().is_none());
        assert!(from_transition.to_string().contains("clarification stage"));
    }
}
