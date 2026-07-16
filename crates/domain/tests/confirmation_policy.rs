use domain::confirmation::{
    ConfirmationAction, ConfirmationIssueRequest, MutationTargetFingerprint, PreviewDigest,
};
use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::{
    TransitionError, TransitionRequest, WaitDeadline, Workflow, WorkflowRevision, WorkflowState,
    WorkflowTimestamp, WorkflowTransition,
};

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn deadline(seconds: u64) -> WaitDeadline {
    WaitDeadline::at(time(seconds))
}

fn participant(value: i64) -> Result<ParticipantId, Box<dyn std::error::Error>> {
    Ok(ParticipantId::new(value)?)
}

fn topic() -> Result<TopicSessionId, Box<dyn std::error::Error>> {
    Ok(TopicSessionId::new(
        ChatId::new(-1001),
        MessageThreadId::new(77)?,
    ))
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
        WorkflowId::new("workflow-confirmation")?,
        participant(101)?,
        time(1),
    );
    let workflow = advance(
        &workflow,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(30),
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

fn issue_request(
    workflow: &Workflow,
) -> Result<ConfirmationIssueRequest, Box<dyn std::error::Error>> {
    Ok(ConfirmationIssueRequest {
        confirmation_id: ConfirmationId::new("confirmation-1")?,
        expected_workflow_revision: workflow.revision(),
        topic: topic()?,
        preview_digest: PreviewDigest::new([1; 32]),
        mutation_target: MutationTargetFingerprint::new([2; 32]),
        action: ConfirmationAction::StartSheetOrDocWrite,
        deadline: deadline(100),
        actor: participant(202)?,
        source_message: MessageId::new(7)?,
        timestamp: time(7),
    })
}

#[test]
fn issuing_confirmation_enters_waiting_state_and_binds_the_resulting_revision()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = drafting_completed()?;
    let old_revision = workflow.revision();
    let outcome = workflow.issue_confirmation(issue_request(&workflow)?)?;

    assert!(matches!(
        outcome.transition.workflow.state(),
        WorkflowState::WaitingForConfirmation { deadline: value } if *value == deadline(100)
    ));
    assert_eq!(outcome.transition.audit.old_revision, old_revision);
    assert_eq!(
        outcome.confirmation.workflow_revision(),
        outcome.transition.workflow.revision()
    );
    assert_eq!(outcome.confirmation.workflow_id(), workflow.id());
    assert_eq!(outcome.confirmation.owner(), participant(101)?);
    assert_eq!(outcome.confirmation.topic(), topic()?);
    assert_eq!(
        outcome.confirmation.preview_digest(),
        PreviewDigest::new([1; 32])
    );
    assert_eq!(
        outcome.confirmation.mutation_target(),
        MutationTargetFingerprint::new([2; 32])
    );
    assert_eq!(
        outcome.confirmation.action(),
        ConfirmationAction::StartSheetOrDocWrite
    );
    assert_eq!(outcome.confirmation.expires_at(), deadline(100));
    assert_eq!(
        outcome.precondition.expected_workflow_revision,
        old_revision
    );
    assert_eq!(
        outcome.precondition.confirmation_id_must_not_exist,
        ConfirmationId::new("confirmation-1")?
    );
    Ok(())
}

#[test]
fn confirmation_issuance_rejects_direct_or_invalid_entry() -> Result<(), Box<dyn std::error::Error>>
{
    let workflow = drafting_completed()?;
    let direct = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::RequestConfirmation {
            deadline: deadline(100),
        },
        expected_revision: workflow.revision(),
        actor: participant(202)?,
        source_message: MessageId::new(7)?,
        timestamp: time(7),
    });
    assert!(matches!(
        direct,
        Err(TransitionError::ConfirmationBoundaryRequired { .. })
    ));

    let mut stale = issue_request(&workflow)?;
    stale.expected_workflow_revision = WorkflowRevision::INITIAL;
    assert!(matches!(
        workflow.issue_confirmation(stale),
        Err(TransitionError::RevisionConflict { .. })
    ));

    let initial = Workflow::new(WorkflowId::new("wrong-state")?, participant(101)?, time(1));
    let wrong_state = ConfirmationIssueRequest {
        expected_workflow_revision: initial.revision(),
        ..issue_request(&workflow)?
    };
    assert!(matches!(
        initial.issue_confirmation(wrong_state),
        Err(TransitionError::IllegalTransition { .. })
    ));
    Ok(())
}

#[test]
fn pending_confirmation_serialization_is_strict() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = drafting_completed()?;
    let pending = workflow
        .issue_confirmation(issue_request(&workflow)?)?
        .confirmation;
    let json = serde_json::to_string(&pending)?;
    let restored = serde_json::from_str(&json)?;
    assert_eq!(pending, restored);

    let mut value: serde_json::Value = serde_json::from_str(&json)?;
    value
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("pending confirmation must serialize as an object"))?
        .insert("unexpected".to_owned(), serde_json::json!(true));
    assert!(serde_json::from_value::<domain::confirmation::PendingConfirmation>(value).is_err());
    assert!(serde_json::from_str::<PreviewDigest>("[1,2,3]").is_err());
    Ok(())
}
