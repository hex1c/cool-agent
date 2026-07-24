use std::fmt::Display;

use application::repositories::{ConditionalWriteOutcome, WorkflowCreation, WorkflowRepository};
use domain::WorkflowTimestamp;
use domain::identity::{ParticipantId, WorkflowId};
use domain::routing::Route;
use domain::workflow::{Workflow, WorkflowStateKind};
use serde::Serialize;
use telegram::normalize::{EventKind, NormalizedUpdate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowKind {
    Quotation,
    Calendar,
    Email,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStartRequest {
    pub schema_version: &'static str,
    pub workflow_id: String,
    pub update_id: i64,
    pub intake_result: IntakeResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntakeResult {
    pub schema_version: &'static str,
    pub workflow_id: String,
    pub update_id: i64,
    pub source_message_id: i64,
    pub chat_id: i64,
    pub message_thread_id: i64,
    pub actor_id: i64,
    pub instruction: String,
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    Started,
    Existing,
    Ignored,
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("workflow repository failed")]
    Repository,
    #[error("workflow start failed")]
    Start,
    #[error("normalized update could not create a workflow")]
    InvalidUpdate,
}

#[allow(async_fn_in_trait)]
pub trait WorkflowStarter {
    type Error: Display;

    async fn start(
        &self,
        kind: WorkflowKind,
        request: &WorkflowStartRequest,
    ) -> Result<(), Self::Error>;
}

pub struct WorkflowDispatchService<R, S> {
    repository: R,
    starter: S,
}

impl<R, S> WorkflowDispatchService<R, S>
where
    R: WorkflowRepository,
    S: WorkflowStarter,
{
    pub const fn new(repository: R, starter: S) -> Self {
        Self {
            repository,
            starter,
        }
    }

    pub async fn dispatch(
        &self,
        update: &NormalizedUpdate,
        accepted_at: WorkflowTimestamp,
    ) -> Result<DispatchOutcome, DispatchError> {
        let (instruction, actor) = match &update.event {
            EventKind::Mention { text, from } => (text.as_str(), *from),
            _ => return Ok(DispatchOutcome::Ignored),
        };
        let Route::ForumTopic { session } = update.route else {
            return Ok(DispatchOutcome::Ignored);
        };

        if let Some(existing) = self
            .repository
            .load_by_topic(session)
            .await
            .map_err(|_| DispatchError::Repository)?
        {
            if existing.state().kind() == WorkflowStateKind::RequestAccepted {
                let request = start_request(existing.id(), update, instruction, actor)?;
                self.starter
                    .start(classify_workflow(instruction), &request)
                    .await
                    .map_err(|_| DispatchError::Start)?;
            }
            return Ok(DispatchOutcome::Existing);
        }

        let workflow_id = WorkflowId::new(format!(
            "telegram-{}-{}",
            session.chat_id.get(),
            session.message_thread_id.get()
        ))
        .map_err(|_| DispatchError::InvalidUpdate)?;
        let workflow = Workflow::new(workflow_id, session, actor, accepted_at);
        let creation = WorkflowCreation::new(workflow, actor, update.source_message_id)
            .map_err(|_| DispatchError::InvalidUpdate)?;

        match self
            .repository
            .create(&creation)
            .await
            .map_err(|_| DispatchError::Repository)?
        {
            ConditionalWriteOutcome::Committed => {
                let request = start_request(creation.workflow().id(), update, instruction, actor)?;
                self.starter
                    .start(classify_workflow(instruction), &request)
                    .await
                    .map_err(|_| DispatchError::Start)?;
                Ok(DispatchOutcome::Started)
            }
            ConditionalWriteOutcome::Conflict => Ok(DispatchOutcome::Existing),
        }
    }
}

fn start_request(
    workflow_id: &WorkflowId,
    update: &NormalizedUpdate,
    instruction: &str,
    actor: ParticipantId,
) -> Result<WorkflowStartRequest, DispatchError> {
    let Route::ForumTopic { session } = update.route else {
        return Err(DispatchError::InvalidUpdate);
    };
    let workflow_id = workflow_id.as_str().to_owned();
    Ok(WorkflowStartRequest {
        schema_version: "novus.workflow-start.v1",
        workflow_id: workflow_id.clone(),
        update_id: update.update_id,
        intake_result: IntakeResult {
            schema_version: "novus.workflow-actions.v1",
            workflow_id,
            update_id: update.update_id,
            source_message_id: update.source_message_id.get(),
            chat_id: session.chat_id.get(),
            message_thread_id: session.message_thread_id.get(),
            actor_id: actor.get(),
            instruction: instruction.to_owned(),
            attachments: Vec::new(),
        },
    })
}

pub fn classify_workflow(instruction: &str) -> WorkflowKind {
    let normalized = instruction.to_ascii_lowercase();
    if normalized.contains("calendar") || normalized.contains("meeting") {
        WorkflowKind::Calendar
    } else if normalized.contains("email") || normalized.contains("mail") {
        WorkflowKind::Email
    } else {
        WorkflowKind::Quotation
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, clippy::get_first)]

    use std::sync::Mutex;

    use application::repositories::ConditionalWriteOutcome;
    use domain::identity::{ChatId, MessageId, MessageThreadId, TopicSessionId};
    use telegram::normalize::NormalizedUpdate;

    use super::*;

    #[derive(Default)]
    struct FakeRepository {
        workflow: Mutex<Option<Workflow>>,
    }

    impl WorkflowRepository for FakeRepository {
        type Error = &'static str;

        async fn load(&self, _workflow_id: &WorkflowId) -> Result<Option<Workflow>, Self::Error> {
            Ok(self.workflow.lock().map_err(|_| "lock")?.clone())
        }

        async fn load_by_topic(
            &self,
            _topic: TopicSessionId,
        ) -> Result<Option<Workflow>, Self::Error> {
            Ok(self.workflow.lock().map_err(|_| "lock")?.clone())
        }

        async fn create(
            &self,
            creation: &WorkflowCreation,
        ) -> Result<ConditionalWriteOutcome, Self::Error> {
            *self.workflow.lock().map_err(|_| "lock")? = Some(creation.workflow().clone());
            Ok(ConditionalWriteOutcome::Committed)
        }

        async fn commit_transition(
            &self,
            _transition: &domain::TransitionOutcome,
        ) -> Result<ConditionalWriteOutcome, Self::Error> {
            Err("unused")
        }
    }

    #[derive(Default)]
    struct FakeStarter {
        requests: Mutex<Vec<(WorkflowKind, WorkflowStartRequest)>>,
    }

    impl WorkflowStarter for FakeStarter {
        type Error = &'static str;

        async fn start(
            &self,
            kind: WorkflowKind,
            request: &WorkflowStartRequest,
        ) -> Result<(), Self::Error> {
            self.requests
                .lock()
                .map_err(|_| "lock")?
                .push((kind, request.clone()));
            Ok(())
        }
    }

    fn update(text: &str) -> NormalizedUpdate {
        NormalizedUpdate {
            update_id: 77,
            source_message_id: MessageId::new(9).expect("message id"),
            route: Route::ForumTopic {
                session: TopicSessionId::new(
                    ChatId::new(-1001),
                    MessageThreadId::new(4).expect("thread id"),
                ),
            },
            event: EventKind::Mention {
                text: text.to_owned(),
                from: ParticipantId::new(8).expect("participant"),
            },
        }
    }

    #[tokio::test]
    async fn mention_creates_and_starts_workflow() {
        let service =
            WorkflowDispatchService::new(FakeRepository::default(), FakeStarter::default());

        let outcome = service
            .dispatch(
                &update("create a calendar meeting"),
                WorkflowTimestamp::from_unix_seconds(1),
            )
            .await
            .expect("dispatch");

        assert_eq!(outcome, DispatchOutcome::Started);
        let requests = service.starter.requests.lock().expect("starter lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, WorkflowKind::Calendar);
        assert_eq!(
            requests.first().expect("first request").1.workflow_id,
            "telegram--1001-4"
        );
    }

    #[test]
    fn workflow_kind_defaults_to_quotation() {
        assert_eq!(
            classify_workflow("please prepare a quote"),
            WorkflowKind::Quotation
        );
        assert_eq!(classify_workflow("send an email"), WorkflowKind::Email);
        assert_eq!(classify_workflow("book a meeting"), WorkflowKind::Calendar);
    }
}
