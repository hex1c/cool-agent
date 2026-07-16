use domain::identity::{
    ChatId, ConfirmationId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::{
    ConfirmationAction, ConfirmationIssueRequest, MutationTargetFingerprint, PreviewDigest,
    TransitionAudit, TransitionError, TransitionOutcome, TransitionRequest, WaitDeadline, Workflow,
    WorkflowRevision, WorkflowState, WorkflowStateKind, WorkflowTimestamp, WorkflowTransition,
};

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn deadline(seconds: u64) -> WaitDeadline {
    WaitDeadline::at(time(seconds))
}

fn workflow() -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(Workflow::new(
        WorkflowId::new("workflow-15")?,
        ParticipantId::new(101)?,
        time(1),
    ))
}

fn apply(
    workflow: &Workflow,
    transition: WorkflowTransition,
    at: u64,
) -> Result<TransitionOutcome, Box<dyn std::error::Error>> {
    Ok(workflow.transition(TransitionRequest {
        transition,
        expected_revision: workflow.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(i64::try_from(at)?)?,
        timestamp: time(at),
    })?)
}

fn advance(
    workflow: &Workflow,
    transition: WorkflowTransition,
    at: u64,
) -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(apply(workflow, transition, at)?.workflow)
}

fn request_confirmation(
    workflow: &Workflow,
    at: u64,
    expires_at: u64,
) -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(workflow
        .issue_confirmation(ConfirmationIssueRequest {
            confirmation_id: ConfirmationId::new(format!("confirmation-{at}"))?,
            expected_workflow_revision: workflow.revision(),
            topic: TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77)?),
            preview_digest: PreviewDigest::new([1; 32]),
            mutation_target: MutationTargetFingerprint::new([2; 32]),
            action: ConfirmationAction::StartSheetOrDocWrite,
            deadline: deadline(expires_at),
            actor: ParticipantId::new(202)?,
            source_message: MessageId::new(i64::try_from(at)?)?,
            timestamp: time(at),
        })?
        .transition
        .workflow)
}

fn drafting_completed() -> Result<Workflow, Box<dyn std::error::Error>> {
    let workflow = workflow()?;
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

fn waiting_for_confirmation() -> Result<Workflow, Box<dyn std::error::Error>> {
    let workflow = drafting_completed()?;
    request_confirmation(&workflow, 7, 100)
}

#[test]
fn workflow_state_quotation_branch_reports_every_stage_and_audit_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let initial = workflow()?;
    assert_eq!(initial.state(), &WorkflowState::RequestAccepted);

    let first = apply(
        &initial,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(30),
        },
        2,
    )?;
    assert_eq!(first.audit.owner, ParticipantId::new(101)?);
    assert_eq!(first.audit.actor, ParticipantId::new(202)?);
    assert_eq!(first.audit.source_message, MessageId::new(2)?);
    assert_eq!(first.audit.from, WorkflowStateKind::RequestAccepted);
    assert_eq!(first.audit.to, WorkflowStateKind::CollectingAttachments);
    assert_eq!(first.audit.old_revision, WorkflowRevision::INITIAL);
    assert_eq!(first.audit.new_revision, WorkflowRevision::new(1));
    assert_eq!(first.audit.timestamp, time(2));
    assert_eq!(initial.revision(), WorkflowRevision::INITIAL);

    let workflow = advance(
        &first.workflow,
        WorkflowTransition::FinishAttachmentCollection,
        3,
    )?;
    assert_eq!(workflow.state(), &WorkflowState::ExtractionStarted);
    let workflow = advance(&workflow, WorkflowTransition::CompleteExtraction, 4)?;
    assert_eq!(workflow.state(), &WorkflowState::ExtractionCompleted);
    let workflow = advance(&workflow, WorkflowTransition::StartCalculationOrDrafting, 5)?;
    assert_eq!(
        workflow.state(),
        &WorkflowState::CalculationOrDraftingStarted
    );
    let workflow = advance(
        &workflow,
        WorkflowTransition::CompleteCalculationOrDrafting,
        6,
    )?;
    assert_eq!(
        workflow.state(),
        &WorkflowState::CalculationOrDraftingCompleted
    );
    let workflow = request_confirmation(&workflow, 7, 100)?;
    assert!(matches!(
        workflow.state(),
        WorkflowState::WaitingForConfirmation { .. }
    ));
    let workflow = advance(&workflow, WorkflowTransition::StartSheetOrDocWrite, 8)?;
    assert_eq!(workflow.state(), &WorkflowState::SheetOrDocWriteStarted);
    let workflow = advance(&workflow, WorkflowTransition::CompleteSheetOrDocWrite, 9)?;
    assert_eq!(workflow.state(), &WorkflowState::SheetOrDocWriteCompleted);
    let workflow = advance(&workflow, WorkflowTransition::StartPdfGeneration, 10)?;
    assert_eq!(workflow.state(), &WorkflowState::PdfGenerationStarted);
    let workflow = advance(&workflow, WorkflowTransition::CompletePdfGeneration, 11)?;
    assert_eq!(workflow.state(), &WorkflowState::PdfGenerationCompleted);
    let workflow = advance(&workflow, WorkflowTransition::CompleteArtifactDelivery, 12)?;
    assert_eq!(workflow.state(), &WorkflowState::ArtifactDeliveryCompleted);
    let workflow = advance(&workflow, WorkflowTransition::CompleteWorkflow, 13)?;
    assert_eq!(workflow.state(), &WorkflowState::Completed);
    assert_eq!(workflow.updated_at(), time(13));
    Ok(())
}

#[test]
fn workflow_state_calendar_email_and_direct_pdf_branches_are_explicit()
-> Result<(), Box<dyn std::error::Error>> {
    let calendar = waiting_for_confirmation()?;
    let calendar = advance(&calendar, WorkflowTransition::StartCalendarOrEmailAction, 8)?;
    assert_eq!(
        calendar.state(),
        &WorkflowState::CalendarOrEmailActionStarted
    );
    let calendar = advance(
        &calendar,
        WorkflowTransition::CompleteCalendarOrEmailAction,
        9,
    )?;
    assert_eq!(
        calendar.state(),
        &WorkflowState::CalendarOrEmailActionCompleted
    );
    let calendar = advance(&calendar, WorkflowTransition::CompleteArtifactDelivery, 10)?;
    let calendar = advance(&calendar, WorkflowTransition::CompleteWorkflow, 11)?;
    assert_eq!(calendar.state(), &WorkflowState::Completed);

    let sheet = waiting_for_confirmation()?;
    let sheet = advance(&sheet, WorkflowTransition::StartSheetOrDocWrite, 8)?;
    let sheet = advance(&sheet, WorkflowTransition::CompleteSheetOrDocWrite, 9)?;
    let sheet = advance(&sheet, WorkflowTransition::CompleteArtifactDelivery, 10)?;
    let sheet = advance(&sheet, WorkflowTransition::CompleteWorkflow, 11)?;
    assert_eq!(sheet.state(), &WorkflowState::Completed);

    let pdf = waiting_for_confirmation()?;
    let pdf = advance(&pdf, WorkflowTransition::StartPdfGeneration, 8)?;
    let pdf = advance(&pdf, WorkflowTransition::CompletePdfGeneration, 9)?;
    let pdf = advance(&pdf, WorkflowTransition::CompleteArtifactDelivery, 10)?;
    let pdf = advance(&pdf, WorkflowTransition::CompleteWorkflow, 11)?;
    assert_eq!(pdf.state(), &WorkflowState::Completed);
    Ok(())
}

#[test]
fn workflow_state_clarification_and_correction_loops_resume_the_right_stage()
-> Result<(), Box<dyn std::error::Error>> {
    let initial = workflow()?;
    let collecting = advance(
        &initial,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(10),
        },
        2,
    )?;
    let clarification = advance(
        &collecting,
        WorkflowTransition::AttachmentCollectionTimedOut {
            clarification_deadline: deadline(30),
        },
        10,
    )?;
    assert!(matches!(
        clarification.state(),
        WorkflowState::WaitingForClarification { .. }
    ));
    let collecting = advance(
        &clarification,
        WorkflowTransition::ResumeAttachmentCollection {
            deadline: deadline(40),
        },
        11,
    )?;
    assert!(matches!(
        collecting.state(),
        WorkflowState::CollectingAttachments { .. }
    ));
    let extraction = advance(
        &collecting,
        WorkflowTransition::FinishAttachmentCollection,
        12,
    )?;
    let extracted = advance(&extraction, WorkflowTransition::CompleteExtraction, 13)?;
    let clarification = advance(
        &extracted,
        WorkflowTransition::RequestClarification {
            deadline: deadline(30),
        },
        14,
    )?;
    let extraction = advance(&clarification, WorkflowTransition::ProvideClarification, 15)?;
    assert_eq!(extraction.state(), &WorkflowState::ExtractionStarted);
    let extracted = advance(&extraction, WorkflowTransition::CompleteExtraction, 16)?;
    let drafting = advance(
        &extracted,
        WorkflowTransition::StartCalculationOrDrafting,
        17,
    )?;
    let drafted = advance(
        &drafting,
        WorkflowTransition::CompleteCalculationOrDrafting,
        18,
    )?;
    let clarification = advance(
        &drafted,
        WorkflowTransition::RequestClarification {
            deadline: deadline(30),
        },
        19,
    )?;
    let drafting = advance(&clarification, WorkflowTransition::ProvideClarification, 20)?;
    assert_eq!(
        drafting.state(),
        &WorkflowState::CalculationOrDraftingStarted
    );
    let drafted = advance(
        &drafting,
        WorkflowTransition::CompleteCalculationOrDrafting,
        21,
    )?;
    let confirmation = request_confirmation(&drafted, 22, 30)?;
    let corrected = advance(&confirmation, WorkflowTransition::ApplyCorrection, 23)?;
    assert_eq!(
        corrected.state(),
        &WorkflowState::CalculationOrDraftingStarted
    );
    Ok(())
}

#[test]
fn workflow_state_waits_expire_and_reject_late_or_premature_actions()
-> Result<(), Box<dyn std::error::Error>> {
    let waiting = waiting_for_confirmation()?;
    let late = waiting.transition(TransitionRequest {
        transition: WorkflowTransition::ApplyCorrection,
        expected_revision: waiting.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(100)?,
        timestamp: time(100),
    });
    assert!(matches!(
        late,
        Err(TransitionError::WaitDeadlineElapsed { .. })
    ));

    let premature = waiting.transition(TransitionRequest {
        transition: WorkflowTransition::ExpireWorkflow,
        expected_revision: waiting.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(99)?,
        timestamp: time(99),
    });
    assert!(matches!(
        premature,
        Err(TransitionError::DeadlineNotReached { .. })
    ));

    let expired = advance(&waiting, WorkflowTransition::ExpireWorkflow, 100)?;
    assert_eq!(expired.state(), &WorkflowState::Expired);

    let extracted = advance(
        &advance(
            &advance(
                &workflow()?,
                WorkflowTransition::BeginAttachmentCollection {
                    deadline: deadline(10),
                },
                2,
            )?,
            WorkflowTransition::FinishAttachmentCollection,
            3,
        )?,
        WorkflowTransition::CompleteExtraction,
        4,
    )?;
    let clarification = advance(
        &extracted,
        WorkflowTransition::RequestClarification {
            deadline: deadline(30),
        },
        5,
    )?;
    let late_clarification = clarification.transition(TransitionRequest {
        transition: WorkflowTransition::ProvideClarification,
        expected_revision: clarification.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(30)?,
        timestamp: time(30),
    });
    assert!(matches!(
        late_clarification,
        Err(TransitionError::WaitDeadlineElapsed { .. })
    ));

    let expired_clarification = advance(&clarification, WorkflowTransition::ExpireWorkflow, 30)?;
    assert_eq!(expired_clarification.state(), &WorkflowState::Expired);

    let collecting = advance(
        &workflow()?,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(10),
        },
        2,
    )?;
    let late_collection = collecting.transition(TransitionRequest {
        transition: WorkflowTransition::FinishAttachmentCollection,
        expected_revision: collecting.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(10)?,
        timestamp: time(10),
    });
    assert!(matches!(
        late_collection,
        Err(TransitionError::WaitDeadlineElapsed { .. })
    ));
    Ok(())
}

#[test]
fn workflow_state_terminal_states_are_immutable() -> Result<(), Box<dyn std::error::Error>> {
    let active = workflow()?;
    let failed = advance(&active, WorkflowTransition::FailWorkflow, 2)?;
    let stopped = advance(&active, WorkflowTransition::StopWorkflow, 2)?;
    assert_eq!(failed.state(), &WorkflowState::Failed);
    assert_eq!(stopped.state(), &WorkflowState::Stopped);

    let waiting = waiting_for_confirmation()?;
    let expired = advance(&waiting, WorkflowTransition::ExpireWorkflow, 100)?;
    let completed = advance(
        &advance(
            &advance(
                &advance(&waiting, WorkflowTransition::StartCalendarOrEmailAction, 8)?,
                WorkflowTransition::CompleteCalendarOrEmailAction,
                9,
            )?,
            WorkflowTransition::CompleteArtifactDelivery,
            10,
        )?,
        WorkflowTransition::CompleteWorkflow,
        11,
    )?;
    assert_eq!(expired.state(), &WorkflowState::Expired);
    assert_eq!(completed.state(), &WorkflowState::Completed);

    for terminal in [failed, stopped, expired, completed] {
        let result = terminal.transition(TransitionRequest {
            transition: WorkflowTransition::FailWorkflow,
            expected_revision: terminal.revision(),
            actor: ParticipantId::new(202)?,
            source_message: MessageId::new(3)?,
            timestamp: time(3),
        });
        assert!(matches!(result, Err(TransitionError::TerminalState { .. })));
    }
    Ok(())
}

#[test]
fn workflow_state_serialization_round_trips_workflow_and_audit()
-> Result<(), Box<dyn std::error::Error>> {
    let initial = workflow()?;
    let outcome = apply(
        &initial,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(30),
        },
        2,
    )?;

    let workflow_json = serde_json::to_string(&outcome.workflow)?;
    let restored_workflow: Workflow = serde_json::from_str(&workflow_json)?;
    assert_eq!(restored_workflow, outcome.workflow);

    let audit_json = serde_json::to_string(&outcome.audit)?;
    let restored_audit: TransitionAudit = serde_json::from_str(&audit_json)?;
    assert_eq!(restored_audit, outcome.audit);
    Ok(())
}

#[test]
fn workflow_state_revision_exhaustion_is_typed() -> Result<(), Box<dyn std::error::Error>> {
    let serialized = format!(
        r#"{{
            "id": "workflow-15",
            "owner": 101,
            "state": {{"stage": "request_accepted"}},
            "revision": {},
            "updated_at": 1
        }}"#,
        u64::MAX
    );
    let workflow: Workflow = serde_json::from_str(&serialized)?;
    let result = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::FailWorkflow,
        expected_revision: WorkflowRevision::new(u64::MAX),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(2)?,
        timestamp: time(2),
    });

    assert!(matches!(result, Err(TransitionError::RevisionExhausted)));
    Ok(())
}

#[test]
fn workflow_state_revision_conflicts_and_illegal_transitions_are_typed()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = workflow()?;
    let race_lost = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::FailWorkflow,
        expected_revision: WorkflowRevision::new(99),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(2)?,
        timestamp: time(2),
    });
    assert!(matches!(
        race_lost,
        Err(TransitionError::RevisionConflict {
            expected,
            actual: WorkflowRevision::INITIAL,
        }) if expected == WorkflowRevision::new(99)
    ));

    let illegal = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::CompleteWorkflow,
        expected_revision: workflow.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(2)?,
        timestamp: time(2),
    });
    assert!(matches!(
        illegal,
        Err(TransitionError::IllegalTransition { .. })
    ));

    let invalid_deadline = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::BeginAttachmentCollection {
            deadline: deadline(2),
        },
        expected_revision: workflow.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(2)?,
        timestamp: time(2),
    });
    assert!(matches!(
        invalid_deadline,
        Err(TransitionError::InvalidDeadline { .. })
    ));

    let backwards = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::FailWorkflow,
        expected_revision: workflow.revision(),
        actor: ParticipantId::new(202)?,
        source_message: MessageId::new(1)?,
        timestamp: time(0),
    });
    assert!(matches!(
        backwards,
        Err(TransitionError::TimestampBeforeLastTransition { .. })
    ));
    Ok(())
}
