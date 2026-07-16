use domain::authorization::{
    AuthorizationError, CachedMembershipApproval, LiveMembershipEvidence,
    MembershipAuthorizationSource, MembershipLookupOutage, MembershipLookupOutageKind,
    MembershipStatus, authorize_participant, authorize_participant_from_cache,
};
use domain::identity::{ChatId, ParticipantId, WorkflowId};
use domain::{Workflow, WorkflowTimestamp};

fn participant(value: i64) -> Result<ParticipantId, Box<dyn std::error::Error>> {
    Ok(ParticipantId::new(value)?)
}

fn workflow() -> Result<Workflow, Box<dyn std::error::Error>> {
    Ok(Workflow::new(
        WorkflowId::new("workflow-16")?,
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
