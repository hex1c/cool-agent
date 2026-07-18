use application::repositories::{
    ConsumeAndPrepareError, ConsumeAndPrepareOutcome, ConsumeAndPrepareRequest,
};
use domain::authorization::{LiveMembershipEvidence, MembershipStatus, authorize_participant};
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumeRequest, ConfirmationIssueRequest, ConfirmationRecord,
    MutationTargetFingerprint, PreviewDigest, TopicMessageReference,
};
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::{
    ConfirmationConsumption, IdempotencyKey, OperationKind, OperationTargetFingerprint,
    TransitionRequest, WaitDeadline, Workflow, WorkflowRevision, WorkflowTimestamp,
    WorkflowTransition,
};

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn topic() -> Result<TopicSessionId, Box<dyn std::error::Error>> {
    Ok(TopicSessionId::new(
        ChatId::new(-1001),
        MessageThreadId::new(77)?,
    ))
}

fn participant(value: i64) -> Result<ParticipantId, Box<dyn std::error::Error>> {
    Ok(ParticipantId::new(value)?)
}

fn advance(
    workflow: &Workflow,
    transition: WorkflowTransition,
    at: u64,
) -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(workflow
        .transition(TransitionRequest {
            transition,
            expected_revision: workflow.revision(),
            actor: participant(202)?,
            source_message: MessageId::new(i64::try_from(at)?)?,
            timestamp: time(at),
        })?
        .workflow)
}

fn drafting_completed() -> Result<Workflow, Box<dyn std::error::Error>> {
    let workflow = Workflow::new(
        WorkflowId::new("workflow-confirmation-operation")?,
        topic()?,
        participant(101)?,
        time(1),
    );
    let workflow = advance(
        &workflow,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: WaitDeadline::at(time(30)),
        },
        2,
    )?;
    let workflow = advance(&workflow, WorkflowTransition::FinishAttachmentCollection, 3)?;
    let workflow = advance(&workflow, WorkflowTransition::CompleteExtraction, 4)?;
    let workflow = advance(&workflow, WorkflowTransition::StartCalculationOrDrafting, 5)?;
    advance(
        &workflow,
        WorkflowTransition::CompleteCalculationOrDrafting,
        6,
    )
}

fn consumed(
    action: ConfirmationAction,
) -> Result<ConfirmationConsumption, Box<dyn std::error::Error>> {
    let workflow = drafting_completed()?;
    let issued = workflow.issue_confirmation(ConfirmationIssueRequest {
        confirmation_id: ConfirmationId::new("confirmation-operation-1")?,
        expected_workflow_revision: workflow.revision(),
        topic: topic()?,
        preview_digest: PreviewDigest::new([1; 32]),
        mutation_target: MutationTargetFingerprint::new([2; 32]),
        action,
        deadline: WaitDeadline::at(time(100)),
        actor: participant(202)?,
        source_message: MessageId::new(7)?,
        timestamp: time(7),
    })?;
    let waiting = issued.transition.workflow;
    let confirmation = ConfirmationRecord::from(issued.confirmation);
    let actor = participant(202)?;
    let authorized = authorize_participant(
        topic()?.chat_id(),
        actor,
        &LiveMembershipEvidence::new(
            topic()?.chat_id(),
            actor,
            MembershipStatus::Approved,
            time(8),
        ),
        time(8),
    )?;
    Ok(confirmation.consume(
        &waiting,
        &authorized.for_workflow(&waiting),
        ConfirmationConsumeRequest {
            preview_digest: PreviewDigest::new([1; 32]),
            mutation_target: MutationTargetFingerprint::new([2; 32]),
            source: TopicMessageReference::new(topic()?, MessageId::new(8)?),
            confirmed_at: time(8),
        },
    )?)
}

fn operation_key(
    consumption: &ConfirmationConsumption,
    kind: OperationKind,
    target: [u8; 32],
) -> IdempotencyKey {
    IdempotencyKey::new(
        consumption.transition.workflow.id().clone(),
        consumption.transition.workflow.revision(),
        kind,
        OperationTargetFingerprint::new(target),
    )
}

#[test]
fn confirmed_actions_accept_only_bound_operation_kinds() -> Result<(), Box<dyn std::error::Error>> {
    for (action, kind) in [
        (
            ConfirmationAction::StartSheetOrDocWrite,
            OperationKind::GoogleWrite,
        ),
        (
            ConfirmationAction::StartDirectPdfGeneration,
            OperationKind::PdfRender,
        ),
        (
            ConfirmationAction::StartCalendarOrEmailAction,
            OperationKind::GoogleWrite,
        ),
        (
            ConfirmationAction::StartCalendarOrEmailAction,
            OperationKind::SmtpSend,
        ),
    ] {
        let consumption = consumed(action)?;
        let key = operation_key(&consumption, kind, [2; 32]);
        let request = ConsumeAndPrepareRequest::new(consumption, key)?;
        let outcome = ConsumeAndPrepareOutcome::from(request);
        assert_eq!(
            outcome.authorization_audit.actor,
            outcome.transition.audit.actor
        );
        assert_eq!(outcome.operation_key.operation_kind(), kind);
    }
    Ok(())
}

#[test]
fn confirmation_operation_binding_rejects_mismatches_before_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let consumption = consumed(ConfirmationAction::StartSheetOrDocWrite)?;

    let wrong_workflow = IdempotencyKey::new(
        WorkflowId::new("another-workflow")?,
        consumption.transition.workflow.revision(),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([2; 32]),
    );
    assert!(matches!(
        ConsumeAndPrepareRequest::new(consumption.clone(), wrong_workflow),
        Err(ConsumeAndPrepareError::WorkflowIdMismatch)
    ));

    let wrong_revision = IdempotencyKey::new(
        consumption.transition.workflow.id().clone(),
        WorkflowRevision::new(
            consumption
                .transition
                .workflow
                .revision()
                .get()
                .saturating_add(1),
        ),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([2; 32]),
    );
    assert!(matches!(
        ConsumeAndPrepareRequest::new(consumption.clone(), wrong_revision),
        Err(ConsumeAndPrepareError::RevisionMismatch { .. })
    ));

    let wrong_target = operation_key(&consumption, OperationKind::GoogleWrite, [9; 32]);
    assert_eq!(
        ConsumeAndPrepareRequest::new(consumption.clone(), wrong_target),
        Err(ConsumeAndPrepareError::TargetMismatch)
    );

    let wrong_kind = operation_key(&consumption, OperationKind::SmtpSend, [2; 32]);
    assert!(matches!(
        ConsumeAndPrepareRequest::new(consumption, wrong_kind),
        Err(ConsumeAndPrepareError::InvalidActionKindPair {
            action: ConfirmationAction::StartSheetOrDocWrite,
            kind: OperationKind::SmtpSend,
        })
    ));
    Ok(())
}
