use domain::authorization::{
    AuthorizationError, LiveMembershipEvidence, MembershipStatus, authorize_participant,
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
