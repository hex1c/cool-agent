use std::fmt::{Debug, Display};

use application::external_operation::{
    ExternalResourceId, FailureCode, OperationFailure, ProviderOutcome, SanitizedSummary,
};

use crate::auth::GoogleAccessToken;

pub use crate::existing_files::{
    ConfirmedMutationProof, ExistingFileError, ExistingFilePreview, FieldUpdate, FileResourceId,
    MutationPayload, TabOrSection,
};

/// Provider outcome reported by a mutation client to the service.
///
/// `Applied` → the mutation was confirmed applied. `Ambiguous` → outcome
/// unknown (e.g. timeout). `Terminal` → the provider permanently rejected
/// the mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationProviderOutcome {
    Applied(ExternalResourceId),
    Ambiguous,
    Terminal,
}

#[allow(async_fn_in_trait)]
pub trait GoogleMutationClient {
    type Error: Display + Debug;

    async fn apply_mutation(
        &self,
        token: &GoogleAccessToken,
        payload: &MutationPayload,
    ) -> Result<MutationProviderOutcome, Self::Error>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum MutationError {
    #[error("confirmation does not authorize this mutation")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

fn failure(code: &'static str, summary: &'static str) -> Result<OperationFailure, MutationError> {
    let code = FailureCode::new(code).map_err(|_| MutationError::Sanitization)?;
    let summary = SanitizedSummary::new(summary).map_err(|_| MutationError::Sanitization)?;
    Ok(OperationFailure::new(code, summary))
}

pub struct GoogleMutationService<Client: GoogleMutationClient> {
    client: Client,
}

impl<Client: GoogleMutationClient> GoogleMutationService<Client> {
    pub const fn new(client: Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn validate(
        proof: &ConfirmedMutationProof,
        payload: &MutationPayload,
    ) -> Result<(), MutationError> {
        if proof.mutation_target().as_bytes() != payload.target_fingerprint().as_bytes() {
            return Err(MutationError::TargetMismatch);
        }
        Ok(())
    }

    pub async fn apply_mutation(
        &self,
        token: &GoogleAccessToken,
        proof: &ConfirmedMutationProof,
        payload: &MutationPayload,
    ) -> Result<ProviderOutcome, MutationError> {
        Self::validate(proof, payload)?;

        match self.client.apply_mutation(token, payload).await {
            Ok(MutationProviderOutcome::Applied(id)) => Ok(ProviderOutcome::Accepted {
                resource_id: Some(id),
            }),
            Ok(MutationProviderOutcome::Ambiguous) => {
                let f = failure(
                    "google_mutation_ambiguous",
                    "google mutation outcome is ambiguous",
                )?;
                Ok(ProviderOutcome::Ambiguous(f))
            }
            Ok(MutationProviderOutcome::Terminal) => {
                let f = failure(
                    "google_mutation_terminal",
                    "google mutation permanently rejected",
                )?;
                Ok(ProviderOutcome::TerminalFailure(f))
            }
            Err(_) => {
                let f = failure("google_mutation_failed", "google mutation attempt failed")?;
                Ok(ProviderOutcome::RetryableFailure(f))
            }
        }
    }
}
