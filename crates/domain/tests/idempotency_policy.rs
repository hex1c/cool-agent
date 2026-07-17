use domain::identity::WorkflowId;
use domain::{
    AttemptNumber, IdempotencyKey, JitterSample, MAX_EXTERNAL_OPERATION_ATTEMPTS, OperationKind,
    OperationTargetFingerprint, RetryDecision, RetryPolicy, RetryPolicyError, WorkflowRevision,
};

fn workflow(value: &str) -> Result<WorkflowId, Box<dyn std::error::Error>> {
    Ok(WorkflowId::new(value)?)
}

#[test]
fn stable_operation_inputs_produce_stable_keys_and_distinct_targets_do_not_collide()
-> Result<(), Box<dyn std::error::Error>> {
    let first = IdempotencyKey::new(
        workflow("workflow-17")?,
        WorkflowRevision::new(4),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([1; 32]),
    );
    let same = IdempotencyKey::new(
        workflow("workflow-17")?,
        WorkflowRevision::new(4),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([1; 32]),
    );
    let different_target = IdempotencyKey::new(
        workflow("workflow-17")?,
        WorkflowRevision::new(4),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([2; 32]),
    );
    let corrected_revision = IdempotencyKey::new(
        workflow("workflow-17")?,
        WorkflowRevision::new(5),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([1; 32]),
    );

    assert_eq!(first, same);
    assert_ne!(first, different_target);
    assert_ne!(first, corrected_revision);
    assert_eq!(first.workflow_id(), &workflow("workflow-17")?);
    assert_eq!(first.workflow_revision(), WorkflowRevision::new(4));
    assert_eq!(first.operation_kind(), OperationKind::GoogleWrite);
    assert_eq!(first.target(), OperationTargetFingerprint::new([1; 32]));
    Ok(())
}

#[test]
fn retry_policy_rejects_more_than_three_attempts_and_invalid_backoff() {
    assert_eq!(MAX_EXTERNAL_OPERATION_ATTEMPTS, 3);
    assert!(matches!(
        RetryPolicy::new(4, 500, 10_000, 2_000),
        Err(RetryPolicyError::TooManyAttempts { requested: 4 })
    ));
    assert!(matches!(
        RetryPolicy::new(0, 500, 10_000, 2_000),
        Err(RetryPolicyError::ZeroAttempts)
    ));
    assert!(matches!(
        RetryPolicy::new(3, 0, 10_000, 2_000),
        Err(RetryPolicyError::ZeroBaseDelay)
    ));
    assert!(matches!(
        RetryPolicy::new(3, 500, 499, 2_000),
        Err(RetryPolicyError::MaximumBelowBase { .. })
    ));
    assert!(matches!(
        RetryPolicy::new(3, 500, 10_000, 10_001),
        Err(RetryPolicyError::InvalidJitterBasisPoints { .. })
    ));
}

#[test]
fn retry_policy_applies_capped_exponential_backoff_with_deterministic_jitter()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = RetryPolicy::new(3, 500, 900, 2_000)?;

    let second_attempt = policy.after_failure(AttemptNumber::new(1)?, JitterSample::new(0)?)?;
    let third_attempt = policy.after_failure(AttemptNumber::new(2)?, JitterSample::new(5_000)?)?;
    let exhausted = policy.after_failure(AttemptNumber::new(3)?, JitterSample::new(0)?)?;

    assert_eq!(
        second_attempt,
        RetryDecision::Retry {
            next_attempt: AttemptNumber::new(2)?,
            delay_ms: 500,
        }
    );
    assert_eq!(
        third_attempt,
        RetryDecision::Retry {
            next_attempt: AttemptNumber::new(3)?,
            delay_ms: 900,
        }
    );
    assert_eq!(exhausted, RetryDecision::Exhausted);
    Ok(())
}

#[test]
fn retry_policy_supports_negative_jitter_without_underflow()
-> Result<(), Box<dyn std::error::Error>> {
    let policy = RetryPolicy::new(3, 5, 100, 10_000)?;

    assert_eq!(
        policy.after_failure(AttemptNumber::new(1)?, JitterSample::new(-10_000)?)?,
        RetryDecision::Retry {
            next_attempt: AttemptNumber::new(2)?,
            delay_ms: 0,
        }
    );
    assert!(matches!(
        JitterSample::new(10_001),
        Err(RetryPolicyError::InvalidJitterSample { .. })
    ));
    assert!(matches!(
        AttemptNumber::new(0),
        Err(RetryPolicyError::InvalidAttempt { requested: 0 })
    ));
    assert!(matches!(
        AttemptNumber::new(4),
        Err(RetryPolicyError::InvalidAttempt { requested: 4 })
    ));
    Ok(())
}
