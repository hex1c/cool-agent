use domain::authorization::{
    AuthorizationError, CachedMembershipApproval, LiveMembershipEvidence,
    MembershipAuthorizationSource, MembershipLookupOutage, MembershipLookupOutageKind,
    MembershipStatus, authorize_participant, authorize_participant_from_cache,
};
use domain::identity::{
    ChatId, MessageId, MessageThreadId, ParticipantId, TopicSessionId, WorkflowId,
};
use domain::{
    AuthorizedActionError, AuthorizedTransitionRequest, TransitionError, TransitionRequest,
    Workflow, WorkflowState, WorkflowTimestamp, WorkflowTransition,
};

fn participant(value: i64) -> Result<ParticipantId, Box<dyn std::error::Error>> {
    Ok(ParticipantId::new(value)?)
}

fn workflow() -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(Workflow::new(
        WorkflowId::new("workflow-16")?,
        TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(77)?),
        participant(101)?,
        WorkflowTimestamp::from_unix_seconds(1),
    ))
}

fn evidence(
    forum: i64,
    actor: i64,
    status: MembershipStatus,
    observed_at: u64,
) -> Result<LiveMembershipEvidence, Box<dyn std::error::Error>> {
    Ok(LiveMembershipEvidence::new(
        ChatId::new(forum),
        participant(actor)?,
        status,
        WorkflowTimestamp::from_unix_seconds(observed_at),
    ))
}

#[test]
fn authorization_allows_an_approved_non_owner_but_keeps_google_bound_to_the_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let authorized = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 10)?,
        WorkflowTimestamp::from_unix_seconds(10),
    )?;

    let workflow_action = authorized.for_workflow(&workflow()?);
    assert_eq!(
        authorized.authorized_at(),
        WorkflowTimestamp::from_unix_seconds(10)
    );
    assert_eq!(workflow_action.actor(), actor);
    assert_eq!(workflow_action.google_principal(), participant(101)?);
    assert_eq!(workflow_action.approved_forum(), forum);
    assert_eq!(
        workflow_action.authorized_at(),
        WorkflowTimestamp::from_unix_seconds(10)
    );
    Ok(())
}

#[test]
fn authorization_rejects_non_members_and_mismatched_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let now = WorkflowTimestamp::from_unix_seconds(10);

    let removed = authorize_participant(
        ChatId::new(-1001),
        actor,
        &evidence(-1001, 202, MembershipStatus::NotApproved, 10)?,
        now,
    );
    assert!(matches!(removed, Err(AuthorizationError::NotApproved)));

    let wrong_forum = authorize_participant(
        ChatId::new(-1001),
        actor,
        &evidence(-2002, 202, MembershipStatus::Approved, 10)?,
        now,
    );
    assert!(matches!(
        wrong_forum,
        Err(AuthorizationError::ForumMismatch { .. })
    ));

    let wrong_actor = authorize_participant(
        ChatId::new(-1001),
        actor,
        &evidence(-1001, 303, MembershipStatus::Approved, 10)?,
        now,
    );
    assert!(matches!(
        wrong_actor,
        Err(AuthorizationError::ParticipantMismatch { .. })
    ));
    Ok(())
}

#[test]
fn authorization_requires_membership_evidence_from_the_current_live_check()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let now = WorkflowTimestamp::from_unix_seconds(10);

    let stale = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 9)?,
        now,
    );
    assert!(matches!(
        stale,
        Err(AuthorizationError::EvidenceNotCurrent { .. })
    ));

    let future = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 11)?,
        now,
    );
    assert!(matches!(
        future,
        Err(AuthorizationError::EvidenceNotCurrent { .. })
    ));
    Ok(())
}

#[test]
fn authorization_allows_positive_cache_for_less_than_thirty_minutes_during_outage()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let observed_at = WorkflowTimestamp::from_unix_seconds(10);
    let evaluated_at = WorkflowTimestamp::from_unix_seconds(1_809);
    let live = evidence(-1001, 202, MembershipStatus::Approved, 10)?;
    let cached = CachedMembershipApproval::from_live(&live)?;
    let outage = MembershipLookupOutage::new(
        forum,
        actor,
        MembershipLookupOutageKind::Timeout,
        evaluated_at,
    );

    let authorized =
        authorize_participant_from_cache(forum, actor, &cached, &outage, evaluated_at)?;
    assert_eq!(
        authorized.authorization_source(),
        MembershipAuthorizationSource::OutageCache {
            live_observed_at: observed_at,
            outage: MembershipLookupOutageKind::Timeout,
        }
    );
    assert_eq!(
        authorized.for_workflow(&workflow()?).authorization_source(),
        authorized.authorization_source()
    );
    Ok(())
}

#[test]
fn authorization_rejects_cache_at_thirty_minutes_or_from_the_future()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let live = evidence(-1001, 202, MembershipStatus::Approved, 10)?;
    let cached = CachedMembershipApproval::from_live(&live)?;

    let boundary = WorkflowTimestamp::from_unix_seconds(1_810);
    let boundary_outage = MembershipLookupOutage::new(
        forum,
        actor,
        MembershipLookupOutageKind::ConnectionFailure,
        boundary,
    );
    assert!(matches!(
        authorize_participant_from_cache(forum, actor, &cached, &boundary_outage, boundary),
        Err(AuthorizationError::CachedApprovalExpired { .. })
    ));

    let future_live = evidence(-1001, 202, MembershipStatus::Approved, 11)?;
    let future_cache = CachedMembershipApproval::from_live(&future_live)?;
    let now = WorkflowTimestamp::from_unix_seconds(10);
    let outage =
        MembershipLookupOutage::new(forum, actor, MembershipLookupOutageKind::Timeout, now);
    assert!(matches!(
        authorize_participant_from_cache(forum, actor, &future_cache, &outage, now),
        Err(AuthorizationError::CachedApprovalFromFuture { .. })
    ));
    Ok(())
}

#[test]
fn authorized_participant_can_stop_only_the_bound_topic_with_current_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = workflow()?;
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let at = WorkflowTimestamp::from_unix_seconds(10);
    let authorized = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 10)?,
        at,
    )?;

    let direct = workflow.transition(TransitionRequest {
        transition: WorkflowTransition::StopWorkflow,
        expected_revision: workflow.revision(),
        actor,
        source_message: MessageId::new(10)?,
        timestamp: at,
    });
    assert!(matches!(
        direct,
        Err(TransitionError::ConfirmationBoundaryRequired { .. })
    ));

    let outcome = workflow.stop_authorized(
        &authorized,
        AuthorizedTransitionRequest {
            expected_workflow_revision: workflow.revision(),
            topic: workflow.topic(),
            source_message: MessageId::new(10)?,
            timestamp: at,
        },
    )?;
    assert_eq!(outcome.transition.workflow.state(), &WorkflowState::Stopped);
    assert_eq!(outcome.authorization.actor, actor);
    assert_eq!(
        outcome.authorization.membership_source,
        MembershipAuthorizationSource::Live
    );
    assert_eq!(outcome.authorization.topic, workflow.topic());
    let audit_json = serde_json::to_string(&outcome.authorization)?;
    let restored = serde_json::from_str(&audit_json)?;
    assert_eq!(outcome.authorization, restored);
    Ok(())
}

#[test]
fn cached_membership_can_authorize_cancellation_during_connection_outage()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = workflow()?;
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let evaluated_at = WorkflowTimestamp::from_unix_seconds(20);
    let cached = CachedMembershipApproval::from_live(&evidence(
        -1001,
        202,
        MembershipStatus::Approved,
        10,
    )?)?;
    let outage = MembershipLookupOutage::new(
        forum,
        actor,
        MembershipLookupOutageKind::ConnectionFailure,
        evaluated_at,
    );
    let authorized =
        authorize_participant_from_cache(forum, actor, &cached, &outage, evaluated_at)?;

    let outcome = workflow.stop_authorized(
        &authorized,
        AuthorizedTransitionRequest {
            expected_workflow_revision: workflow.revision(),
            topic: workflow.topic(),
            source_message: MessageId::new(20)?,
            timestamp: evaluated_at,
        },
    )?;
    assert!(matches!(
        outcome.authorization.membership_source,
        MembershipAuthorizationSource::OutageCache { .. }
    ));
    Ok(())
}

#[test]
fn authorized_stop_rejects_terminal_workflows() -> Result<(), Box<dyn std::error::Error>> {
    let workflow = workflow()?;
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let stopped_at = WorkflowTimestamp::from_unix_seconds(10);
    let first_authorization = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 10)?,
        stopped_at,
    )?;
    let stopped = workflow
        .stop_authorized(
            &first_authorization,
            AuthorizedTransitionRequest {
                expected_workflow_revision: workflow.revision(),
                topic: workflow.topic(),
                source_message: MessageId::new(10)?,
                timestamp: stopped_at,
            },
        )?
        .transition
        .workflow;

    let attempted_at = WorkflowTimestamp::from_unix_seconds(11);
    let retry_authorization = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 11)?,
        attempted_at,
    )?;
    let result = stopped.stop_authorized(
        &retry_authorization,
        AuthorizedTransitionRequest {
            expected_workflow_revision: stopped.revision(),
            topic: stopped.topic(),
            source_message: MessageId::new(11)?,
            timestamp: attempted_at,
        },
    );
    assert!(matches!(
        result,
        Err(AuthorizedActionError::Transition(
            TransitionError::TerminalState { .. }
        ))
    ));
    Ok(())
}

#[test]
fn authorized_stop_rejects_stale_or_wrong_topic_capabilities()
-> Result<(), Box<dyn std::error::Error>> {
    let workflow = workflow()?;
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let authorized = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 9)?,
        WorkflowTimestamp::from_unix_seconds(9),
    )?;
    let request = AuthorizedTransitionRequest {
        expected_workflow_revision: workflow.revision(),
        topic: workflow.topic(),
        source_message: MessageId::new(10)?,
        timestamp: WorkflowTimestamp::from_unix_seconds(10),
    };
    assert!(matches!(
        workflow.stop_authorized(&authorized, request),
        Err(AuthorizedActionError::AuthorizationNotCurrent { .. })
    ));

    let now = WorkflowTimestamp::from_unix_seconds(10);
    let current = authorize_participant(
        forum,
        actor,
        &evidence(-1001, 202, MembershipStatus::Approved, 10)?,
        now,
    )?;
    let wrong_topic = AuthorizedTransitionRequest {
        expected_workflow_revision: workflow.revision(),
        topic: TopicSessionId::new(ChatId::new(-1001), MessageThreadId::new(88)?),
        source_message: MessageId::new(10)?,
        timestamp: now,
    };
    assert!(matches!(
        workflow.stop_authorized(&current, wrong_topic),
        Err(AuthorizedActionError::TopicMismatch { .. })
    ));

    let wrong_forum = authorize_participant(
        ChatId::new(-2002),
        actor,
        &evidence(-2002, 202, MembershipStatus::Approved, 10)?,
        now,
    )?;
    let request = AuthorizedTransitionRequest {
        expected_workflow_revision: workflow.revision(),
        topic: workflow.topic(),
        source_message: MessageId::new(10)?,
        timestamp: now,
    };
    assert!(matches!(
        workflow.stop_authorized(&wrong_forum, request),
        Err(AuthorizedActionError::ForumMismatch { .. })
    ));
    Ok(())
}

#[test]
fn authorization_rejects_mismatched_cache_or_outage_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let actor = participant(202)?;
    let forum = ChatId::new(-1001);
    let now = WorkflowTimestamp::from_unix_seconds(20);
    let cached = CachedMembershipApproval::from_live(&evidence(
        -1001,
        202,
        MembershipStatus::Approved,
        10,
    )?)?;

    let wrong_forum_cache = CachedMembershipApproval::from_live(&evidence(
        -2002,
        202,
        MembershipStatus::Approved,
        10,
    )?)?;
    let valid_outage =
        MembershipLookupOutage::new(forum, actor, MembershipLookupOutageKind::Timeout, now);
    assert!(matches!(
        authorize_participant_from_cache(forum, actor, &wrong_forum_cache, &valid_outage, now,),
        Err(AuthorizationError::ForumMismatch { .. })
    ));

    let wrong_actor_cache = CachedMembershipApproval::from_live(&evidence(
        -1001,
        303,
        MembershipStatus::Approved,
        10,
    )?)?;
    assert!(matches!(
        authorize_participant_from_cache(forum, actor, &wrong_actor_cache, &valid_outage, now,),
        Err(AuthorizationError::ParticipantMismatch { .. })
    ));

    for outage in [
        MembershipLookupOutage::new(
            ChatId::new(-2002),
            actor,
            MembershipLookupOutageKind::Timeout,
            now,
        ),
        MembershipLookupOutage::new(
            forum,
            participant(303)?,
            MembershipLookupOutageKind::Timeout,
            now,
        ),
        MembershipLookupOutage::new(
            forum,
            actor,
            MembershipLookupOutageKind::Timeout,
            WorkflowTimestamp::from_unix_seconds(19),
        ),
    ] {
        assert!(authorize_participant_from_cache(forum, actor, &cached, &outage, now).is_err());
    }

    let removed = evidence(-1001, 202, MembershipStatus::NotApproved, 10)?;
    assert!(matches!(
        CachedMembershipApproval::from_live(&removed),
        Err(AuthorizationError::NotApproved)
    ));
    Ok(())
}
