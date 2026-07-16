use domain::authorization::{
    CachedMembershipApproval, LiveMembershipEvidence, MembershipAuthorizationSource,
    MembershipLookupOutage, MembershipLookupOutageKind, MembershipStatus, authorize_participant,
    authorize_participant_from_cache,
};
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumeRequest, ConfirmationError, ConfirmationIssueRequest,
    ConfirmationRecord, ConfirmationStatus, MutationTargetFingerprint, PreviewDigest,
    TopicMessageReference,
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

fn issued_confirmation(
    action: ConfirmationAction,
) -> Result<(Workflow, ConfirmationRecord), Box<dyn std::error::Error>> {
    let workflow = drafting_completed()?;
    let mut request = issue_request(&workflow)?;
    request.action = action;
    let issued = workflow.issue_confirmation(request)?;
    Ok((
        issued.transition.workflow,
        ConfirmationRecord::from(issued.confirmation),
    ))
}

fn authorized_action(
    workflow: &Workflow,
    actor: i64,
    forum: i64,
    at: u64,
) -> Result<domain::AuthorizedWorkflowAction, Box<dyn std::error::Error>> {
    let actor = participant(actor)?;
    let authorized = authorize_participant(
        ChatId::new(forum),
        actor,
        &LiveMembershipEvidence::new(
            ChatId::new(forum),
            actor,
            MembershipStatus::Approved,
            time(at),
        ),
        time(at),
    )?;
    Ok(authorized.for_workflow(workflow))
}

fn consume_request(at: u64) -> Result<ConfirmationConsumeRequest, Box<dyn std::error::Error>> {
    Ok(ConfirmationConsumeRequest {
        preview_digest: PreviewDigest::new([1; 32]),
        mutation_target: MutationTargetFingerprint::new([2; 32]),
        source: TopicMessageReference::new(topic()?, MessageId::new(i64::try_from(at)?)?),
        confirmed_at: time(at),
    })
}

fn workflow_with_json_value(
    workflow: &Workflow,
    pointer: &str,
    replacement: serde_json::Value,
) -> Result<Workflow, Box<dyn std::error::Error>> {
    let mut value = serde_json::to_value(workflow)?;
    let target = value.pointer_mut(pointer).ok_or_else(|| {
        std::io::Error::other(format!("workflow JSON pointer {pointer} must exist"))
    })?;
    *target = replacement;
    Ok(serde_json::from_value(value)?)
}

#[test]
fn approved_non_owner_consumes_confirmation_and_keeps_the_owner_principal()
-> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    let authorization = authorized_action(&workflow, 202, -1001, 8)?;
    assert_eq!(authorization.google_principal(), participant(101)?);

    let consumed = confirmation.consume(&workflow, &authorization, consume_request(8)?)?;
    assert_eq!(
        consumed.transition.workflow.state(),
        &WorkflowState::SheetOrDocWriteStarted
    );
    assert_eq!(consumed.transition.audit.actor, participant(202)?);
    assert_eq!(consumed.transition.audit.owner, participant(101)?);
    assert_eq!(consumed.confirmation.status(), ConfirmationStatus::Consumed);
    assert_eq!(
        consumed.confirmation.confirming_actor(),
        Some(participant(202)?)
    );
    assert_eq!(
        consumed.precondition.expected_workflow_revision,
        workflow.revision()
    );
    assert_eq!(
        consumed.precondition.expected_confirmation_status,
        ConfirmationStatus::Pending
    );
    assert_eq!(
        consumed.precondition.confirmation_id,
        ConfirmationId::new("confirmation-1")?
    );
    Ok(())
}

#[test]
fn outage_cache_can_authorize_confirmation_and_is_recorded()
-> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let live = LiveMembershipEvidence::new(forum, actor, MembershipStatus::Approved, time(7));
    let cached = CachedMembershipApproval::from_live(&live)?;
    let outage = MembershipLookupOutage::new(
        forum,
        actor,
        MembershipLookupOutageKind::ConnectionFailure,
        time(8),
    );
    let authorized = authorize_participant_from_cache(forum, actor, &cached, &outage, time(8))?;

    let consumed = confirmation.consume(
        &workflow,
        &authorized.for_workflow(&workflow),
        consume_request(8)?,
    )?;
    assert_eq!(
        consumed.confirmation.membership_authorization(),
        Some(MembershipAuthorizationSource::OutageCache {
            live_observed_at: time(7),
            outage: MembershipLookupOutageKind::ConnectionFailure,
        })
    );
    Ok(())
}

#[test]
fn generic_transitions_cannot_bypass_confirmation() -> Result<(), Box<dyn std::error::Error>> {
    let (workflow, _) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    for transition in [
        WorkflowTransition::StartSheetOrDocWrite,
        WorkflowTransition::StartPdfGeneration,
        WorkflowTransition::StartCalendarOrEmailAction,
    ] {
        let result = workflow.transition(TransitionRequest {
            transition,
            expected_revision: workflow.revision(),
            actor: participant(202)?,
            source_message: MessageId::new(8)?,
            timestamp: time(8),
        });
        assert!(matches!(
            result,
            Err(TransitionError::ConfirmationBoundaryRequired { .. })
        ));
    }
    Ok(())
}

#[test]
fn confirmation_rejects_stale_authorization_and_wrong_topic_or_forum()
-> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;

    let stale_authorization = authorized_action(&workflow, 202, -1001, 7)?;
    assert!(matches!(
        confirmation.consume(&workflow, &stale_authorization, consume_request(8)?),
        Err(ConfirmationError::AuthorizationNotCurrent { .. })
    ));

    let wrong_forum = authorized_action(&workflow, 202, -2002, 8)?;
    assert!(matches!(
        confirmation.consume(&workflow, &wrong_forum, consume_request(8)?),
        Err(ConfirmationError::ForumMismatch { .. })
    ));

    let authorization = authorized_action(&workflow, 202, -1001, 8)?;
    let mut wrong_topic = consume_request(8)?;
    wrong_topic.source = TopicMessageReference::new(
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(88)?),
        MessageId::new(8)?,
    );
    assert!(matches!(
        confirmation.consume(&workflow, &authorization, wrong_topic),
        Err(ConfirmationError::TopicMismatch { .. })
    ));
    Ok(())
}

#[test]
fn confirmation_rejects_changed_bindings_expiry_and_replay()
-> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    let authorization = authorized_action(&workflow, 202, -1001, 8)?;

    let mut wrong_digest = consume_request(8)?;
    wrong_digest.preview_digest = PreviewDigest::new([9; 32]);
    assert!(matches!(
        confirmation.consume(&workflow, &authorization, wrong_digest),
        Err(ConfirmationError::PreviewDigestMismatch)
    ));

    let mut wrong_target = consume_request(8)?;
    wrong_target.mutation_target = MutationTargetFingerprint::new([9; 32]);
    assert!(matches!(
        confirmation.consume(&workflow, &authorization, wrong_target),
        Err(ConfirmationError::MutationTargetMismatch)
    ));

    let expiry_authorization = authorized_action(&workflow, 202, -1001, 100)?;
    assert!(matches!(
        confirmation.consume(&workflow, &expiry_authorization, consume_request(100)?),
        Err(ConfirmationError::Expired { .. })
    ));

    let consumed = confirmation.consume(&workflow, &authorization, consume_request(8)?)?;
    assert!(matches!(
        consumed
            .confirmation
            .consume(&workflow, &authorization, consume_request(8)?),
        Err(ConfirmationError::AlreadyConsumed)
    ));
    Ok(())
}

#[test]
fn stopped_corrected_and_mismatched_workflows_invalidate_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    let authorization = authorized_action(&workflow, 202, -1001, 8)?;

    let stopped = advance(&workflow, WorkflowTransition::StopWorkflow, 8)?;
    assert!(matches!(
        confirmation.consume(&stopped, &authorization, consume_request(8)?),
        Err(ConfirmationError::WorkflowNotWaiting { .. })
    ));

    let failed = advance(&workflow, WorkflowTransition::FailWorkflow, 8)?;
    assert!(matches!(
        confirmation.consume(&failed, &authorization, consume_request(8)?),
        Err(ConfirmationError::WorkflowNotWaiting { .. })
    ));

    let expiry_authorization = authorized_action(&workflow, 202, -1001, 100)?;
    let expired = advance(&workflow, WorkflowTransition::ExpireWorkflow, 100)?;
    assert!(matches!(
        confirmation.consume(&expired, &expiry_authorization, consume_request(100)?),
        Err(ConfirmationError::WorkflowNotWaiting { .. })
    ));

    let corrected = advance(&workflow, WorkflowTransition::ApplyCorrection, 8)?;
    assert!(matches!(
        confirmation.consume(&corrected, &authorization, consume_request(8)?),
        Err(ConfirmationError::WorkflowNotWaiting { .. })
    ));

    let other_id = Workflow::new(
        WorkflowId::new("other-workflow")?,
        participant(101)?,
        time(1),
    );
    assert!(matches!(
        confirmation.consume(&other_id, &authorization, consume_request(8)?),
        Err(ConfirmationError::WorkflowMismatch)
    ));

    let other_owner = Workflow::new(
        WorkflowId::new("workflow-confirmation")?,
        participant(303)?,
        time(1),
    );
    assert!(matches!(
        confirmation.consume(&other_owner, &authorization, consume_request(8)?),
        Err(ConfirmationError::OwnerMismatch { .. })
    ));

    let authorization_for_other_owner = authorized_action(&other_owner, 202, -1001, 8)?;
    assert!(matches!(
        confirmation.consume(
            &workflow,
            &authorization_for_other_owner,
            consume_request(8)?
        ),
        Err(ConfirmationError::OwnerMismatch { .. })
    ));

    let wrong_deadline =
        workflow_with_json_value(&workflow, "/state/deadline", serde_json::json!(99))?;
    assert!(matches!(
        confirmation.consume(&wrong_deadline, &authorization, consume_request(8)?),
        Err(ConfirmationError::DeadlineMismatch { .. })
    ));

    let redrafted = advance(
        &corrected,
        WorkflowTransition::CompleteCalculationOrDrafting,
        9,
    )?;
    let mut reissue_request = issue_request(&redrafted)?;
    reissue_request.confirmation_id = ConfirmationId::new("confirmation-2")?;
    reissue_request.expected_workflow_revision = redrafted.revision();
    reissue_request.source_message = MessageId::new(10)?;
    reissue_request.timestamp = time(10);
    let reissued = redrafted
        .issue_confirmation(reissue_request)?
        .transition
        .workflow;
    let reissue_authorization = authorized_action(&reissued, 202, -1001, 11)?;
    assert!(matches!(
        confirmation.consume(&reissued, &reissue_authorization, consume_request(11)?),
        Err(ConfirmationError::RevisionMismatch { .. })
    ));

    let consumed = confirmation.consume(&workflow, &authorization, consume_request(8)?)?;
    let completed = advance(
        &advance(
            &advance(
                &consumed.transition.workflow,
                WorkflowTransition::CompleteSheetOrDocWrite,
                9,
            )?,
            WorkflowTransition::CompleteArtifactDelivery,
            10,
        )?,
        WorkflowTransition::CompleteWorkflow,
        11,
    )?;
    let completed_authorization = authorized_action(&completed, 202, -1001, 12)?;
    assert!(matches!(
        confirmation.consume(&completed, &completed_authorization, consume_request(12)?),
        Err(ConfirmationError::WorkflowNotWaiting { .. })
    ));
    Ok(())
}

#[test]
fn consumed_confirmation_serialization_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let (workflow, confirmation) = issued_confirmation(ConfirmationAction::StartSheetOrDocWrite)?;
    let authorization = authorized_action(&workflow, 202, -1001, 8)?;
    let consumed = confirmation.consume(&workflow, &authorization, consume_request(8)?)?;

    let json = serde_json::to_string(&consumed.confirmation)?;
    let restored: ConfirmationRecord = serde_json::from_str(&json)?;
    assert_eq!(restored, consumed.confirmation);
    Ok(())
}

#[test]
fn confirmed_action_is_derived_from_the_pending_record() -> Result<(), Box<dyn std::error::Error>> {
    for (action, expected_state) in [
        (
            ConfirmationAction::StartSheetOrDocWrite,
            WorkflowState::SheetOrDocWriteStarted,
        ),
        (
            ConfirmationAction::StartDirectPdfGeneration,
            WorkflowState::PdfGenerationStarted,
        ),
        (
            ConfirmationAction::StartCalendarOrEmailAction,
            WorkflowState::CalendarOrEmailActionStarted,
        ),
    ] {
        let (workflow, confirmation) = issued_confirmation(action)?;
        let authorization = authorized_action(&workflow, 202, -1001, 8)?;
        let consumed = confirmation.consume(&workflow, &authorization, consume_request(8)?)?;
        assert_eq!(consumed.transition.workflow.state(), &expected_state);
    }
    Ok(())
}
