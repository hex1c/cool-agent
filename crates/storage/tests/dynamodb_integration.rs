#![cfg(feature = "integration")]

use application::external_operation::{
    BeginAttemptOutcome, CompletedAttempt, ExecutionOutcome, ExternalResourceId, JournalState,
    OperationJournal, ProviderOutcome,
};
use application::repositories::{
    ConditionalWriteOutcome, ConfirmationRepository, ConsumeAndPrepareRequest, WorkflowCreation,
    WorkflowRepository,
};
use aws_sdk_dynamodb::config::{Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, BillingMode, KeySchemaElement, KeyType, ScalarAttributeType,
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
    AttemptNumber, IdempotencyKey, OperationKind, OperationTargetFingerprint, TransitionOutcome,
    TransitionRequest, WaitDeadline, Workflow, WorkflowTimestamp, WorkflowTransition,
};
use storage::audit::StoredAudit;
use storage::dynamodb::DynamoDbStore;

fn time(seconds: u64) -> WorkflowTimestamp {
    WorkflowTimestamp::from_unix_seconds(seconds)
}

fn participant(value: i64) -> Result<ParticipantId, Box<dyn std::error::Error>> {
    Ok(ParticipantId::new(value)?)
}

fn topic(thread: i64) -> Result<TopicSessionId, Box<dyn std::error::Error>> {
    Ok(TopicSessionId::new(
        ChatId::new(-1001),
        MessageThreadId::new(thread)?,
    ))
}

fn transition(
    workflow: &Workflow,
    transition: WorkflowTransition,
    at: u64,
) -> Result<TransitionOutcome, Box<dyn std::error::Error>> {
    Ok(workflow.transition(TransitionRequest {
        transition,
        expected_revision: workflow.revision(),
        actor: participant(202)?,
        source_message: MessageId::new(i64::try_from(at)?)?,
        timestamp: time(at),
    })?)
}

async fn commit(
    store: &DynamoDbStore,
    workflow: &Workflow,
    requested: WorkflowTransition,
    at: u64,
) -> Result<Workflow, Box<dyn std::error::Error>> {
    let outcome = transition(workflow, requested, at)?;
    assert_eq!(
        store.commit_transition(&outcome).await?,
        ConditionalWriteOutcome::Committed
    );
    Ok(outcome.workflow)
}

fn local_client() -> aws_sdk_dynamodb::Client {
    let endpoint =
        std::env::var("DYNAMODB_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8000".to_owned());
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version_latest()
        .endpoint_url(endpoint)
        .region(Region::new("us-east-1"))
        .credentials_provider(Credentials::new(
            "test",
            "test",
            None,
            None,
            "dynamodb-local-integration",
        ))
        .build();
    aws_sdk_dynamodb::Client::from_conf(config)
}

async fn create_table(
    client: &aws_sdk_dynamodb::Client,
    table_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()?,
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("sk")
                .attribute_type(ScalarAttributeType::S)
                .build()?,
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()?,
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("sk")
                .key_type(KeyType::Range)
                .build()?,
        )
        .send()
        .await?;
    Ok(())
}

#[tokio::test]
async fn dynamodb_conditional_workflow_confirmation_and_journal_races()
-> Result<(), Box<dyn std::error::Error>> {
    let client = local_client();
    let table_name = format!("novus-test-{}", uuid::Uuid::new_v4().simple());
    create_table(&client, &table_name).await?;
    let store = DynamoDbStore::new(client.clone(), table_name.clone())?;

    let initial = Workflow::new(
        WorkflowId::new("workflow-race")?,
        topic(41)?,
        participant(101)?,
        time(1),
    );
    let creation = WorkflowCreation::new(initial.clone(), participant(202)?, MessageId::new(1)?)?;
    assert_eq!(
        store.create(&creation).await?,
        ConditionalWriteOutcome::Committed
    );
    assert_eq!(
        WorkflowRepository::load(&store, initial.id()).await?,
        Some(initial.clone())
    );
    assert_eq!(
        store.load_by_topic(initial.topic()).await?,
        Some(initial.clone())
    );

    let raced = transition(
        &initial,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: WaitDeadline::at(time(30)),
        },
        2,
    )?;
    let (left, right) = tokio::join!(
        store.commit_transition(&raced),
        store.commit_transition(&raced)
    );
    let outcomes = [left?, right?];
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == ConditionalWriteOutcome::Committed)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == ConditionalWriteOutcome::Conflict)
            .count(),
        1
    );
    let persisted_race = WorkflowRepository::load(&store, initial.id())
        .await?
        .ok_or("raced workflow must remain persisted")?;
    assert_eq!(persisted_race, raced.workflow);

    let confirmation_initial = Workflow::new(
        WorkflowId::new("workflow-confirmation-race")?,
        topic(42)?,
        participant(101)?,
        time(1),
    );
    let creation = WorkflowCreation::new(
        confirmation_initial.clone(),
        participant(202)?,
        MessageId::new(1)?,
    )?;
    assert_eq!(
        store.create(&creation).await?,
        ConditionalWriteOutcome::Committed
    );
    let workflow = commit(
        &store,
        &confirmation_initial,
        WorkflowTransition::BeginAttachmentCollection {
            deadline: WaitDeadline::at(time(30)),
        },
        2,
    )
    .await?;
    let workflow = commit(
        &store,
        &workflow,
        WorkflowTransition::FinishAttachmentCollection,
        3,
    )
    .await?;
    let workflow = commit(&store, &workflow, WorkflowTransition::CompleteExtraction, 4).await?;
    let workflow = commit(
        &store,
        &workflow,
        WorkflowTransition::StartCalculationOrDrafting,
        5,
    )
    .await?;
    let workflow = commit(
        &store,
        &workflow,
        WorkflowTransition::CompleteCalculationOrDrafting,
        6,
    )
    .await?;

    let confirmation_id = ConfirmationId::new("confirmation-race")?;
    let issued = workflow.issue_confirmation(ConfirmationIssueRequest {
        confirmation_id: confirmation_id.clone(),
        expected_workflow_revision: workflow.revision(),
        topic: workflow.topic(),
        preview_digest: PreviewDigest::new([1; 32]),
        mutation_target: MutationTargetFingerprint::new([2; 32]),
        action: ConfirmationAction::StartSheetOrDocWrite,
        deadline: WaitDeadline::at(time(100)),
        actor: participant(202)?,
        source_message: MessageId::new(7)?,
        timestamp: time(7),
    })?;
    assert_eq!(
        store.issue(&issued).await?,
        ConditionalWriteOutcome::Committed
    );
    let waiting = issued.transition.workflow;
    let pending = ConfirmationRecord::from(issued.confirmation);
    let actor = participant(202)?;
    let authorized = authorize_participant(
        waiting.topic().chat_id(),
        actor,
        &LiveMembershipEvidence::new(
            waiting.topic().chat_id(),
            actor,
            MembershipStatus::Approved,
            time(8),
        ),
        time(8),
    )?;
    let consumed = pending.consume(
        &waiting,
        &authorized.for_workflow(&waiting),
        ConfirmationConsumeRequest {
            preview_digest: PreviewDigest::new([1; 32]),
            mutation_target: MutationTargetFingerprint::new([2; 32]),
            source: TopicMessageReference::new(waiting.topic(), MessageId::new(8)?),
            confirmed_at: time(8),
        },
    )?;
    let operation_key = IdempotencyKey::new(
        consumed.transition.workflow.id().clone(),
        consumed.transition.workflow.revision(),
        OperationKind::GoogleWrite,
        OperationTargetFingerprint::new([2; 32]),
    );
    let request = ConsumeAndPrepareRequest::new(consumed, operation_key.clone())?;
    let (left, right) = tokio::join!(
        store.consume_and_prepare_operation(&request),
        store.consume_and_prepare_operation(&request)
    );
    let outcomes = [left?, right?];
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == ConditionalWriteOutcome::Committed)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == ConditionalWriteOutcome::Conflict)
            .count(),
        1
    );
    assert!(matches!(
        store.prepare(&operation_key).await?.state,
        JournalState::Ready { ref completed_attempts } if completed_attempts.is_empty()
    ));
    let consumed_confirmation = ConfirmationRepository::load(
        &store,
        request.consumption.transition.workflow.id(),
        &confirmation_id,
    )
    .await?;
    assert!(matches!(
        consumed_confirmation,
        Some(ConfirmationRecord::Consumed(_))
    ));

    let (audit_pk, audit_sk) = storage::keys::audit(
        request.consumption.transition.workflow.id(),
        request.consumption.transition.workflow.revision(),
    )?;
    let audit_item = client
        .get_item()
        .table_name(&table_name)
        .key("pk", aws_sdk_dynamodb::types::AttributeValue::S(audit_pk))
        .key("sk", aws_sdk_dynamodb::types::AttributeValue::S(audit_sk))
        .consistent_read(true)
        .send()
        .await?
        .item
        .ok_or("consumption audit item must exist")?;
    let audit_payload = audit_item
        .get("payload")
        .and_then(|value| value.as_s().ok())
        .ok_or("consumption audit payload must be a string")?;
    let stored_audit: StoredAudit = serde_json::from_str(audit_payload)?;
    assert!(matches!(
        stored_audit,
        StoredAudit::AuthorizedTransition {
            transition,
            authorization,
        } if transition.owner == participant(101)?
            && transition.actor == participant(202)?
            && transition.source_message == MessageId::new(8)?
            && transition.new_revision == request.consumption.transition.workflow.revision()
            && authorization.actor == participant(202)?
    ));

    let attempt = AttemptNumber::new(1)?;
    let claim = match store.begin_attempt(&operation_key, attempt).await? {
        BeginAttemptOutcome::Acquired(claim) => claim,
        BeginAttemptOutcome::RaceLost => {
            return Err("first journal attempt unexpectedly lost its race".into());
        }
    };
    let completed = CompletedAttempt::new(
        attempt,
        ProviderOutcome::Accepted {
            resource_id: Some(ExternalResourceId::new("resource-1")?),
        },
        None,
    );
    store
        .complete_attempt(&operation_key, &claim, &completed)
        .await?;
    assert!(matches!(
        store.prepare(&operation_key).await?.state,
        JournalState::Final(ExecutionOutcome::Accepted {
            attempt: value,
            resource_id: Some(ref resource_id),
        }) if value == attempt && resource_id.as_str() == "resource-1"
    ));

    client.delete_table().table_name(table_name).send().await?;
    Ok(())
}
