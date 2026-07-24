use std::time::Duration;

use application::external_operation::ExternalResourceId;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::message::EmailMessageRequest;
use crate::smtp::{SmtpClient, SmtpCredentials, SmtpProviderOutcome};

#[derive(Clone)]
pub struct LettreSmtpClient {
    host: String,
    port: u16,
    implicit_tls: bool,
}

impl LettreSmtpClient {
    pub fn new(host: String, port: u16, implicit_tls: bool) -> Result<Self, SmtpClientConfigError> {
        if host.trim().is_empty() || port == 0 {
            return Err(SmtpClientConfigError);
        }
        Ok(Self {
            host,
            port,
            implicit_tls,
        })
    }

    fn transport(
        &self,
        credentials: &SmtpCredentials,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, SmtpClientConfigError> {
        let builder = if self.implicit_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)
        }
        .map_err(|_| SmtpClientConfigError)?;
        Ok(builder
            .port(self.port)
            .credentials(Credentials::new(
                credentials.sender_address().to_owned(),
                credentials.password().to_owned(),
            ))
            .timeout(Some(Duration::from_secs(30)))
            .build())
    }

    fn message(
        credentials: &SmtpCredentials,
        request: &EmailMessageRequest,
    ) -> Result<Message, SmtpClientConfigError> {
        if request.attach_quotation_pdf() {
            // The current action contract carries the attachment decision but
            // not canonical PDF bytes/object data. Fail closed rather than send
            // a message that silently omits a confirmed attachment.
            return Err(SmtpClientConfigError);
        }
        let sender: Mailbox = credentials
            .sender_address()
            .parse()
            .map_err(|_| SmtpClientConfigError)?;
        let mut builder = Message::builder()
            .from(sender)
            .subject(request.subject().as_str());
        for recipient in request.recipients() {
            builder = builder.to(recipient
                .as_str()
                .parse()
                .map_err(|_| SmtpClientConfigError)?);
        }
        for recipient in request.cc() {
            builder = builder.cc(recipient
                .as_str()
                .parse()
                .map_err(|_| SmtpClientConfigError)?);
        }
        for recipient in request.bcc() {
            builder = builder.bcc(
                recipient
                    .as_str()
                    .parse()
                    .map_err(|_| SmtpClientConfigError)?,
            );
        }
        builder
            .body(request.body().as_str().to_owned())
            .map_err(|_| SmtpClientConfigError)
    }
}

impl SmtpClient for LettreSmtpClient {
    async fn send(
        &self,
        credentials: &SmtpCredentials,
        request: &EmailMessageRequest,
    ) -> SmtpProviderOutcome {
        let Ok(message) = Self::message(credentials, request) else {
            return SmtpProviderOutcome::Terminal;
        };
        let Ok(transport) = self.transport(credentials) else {
            return SmtpProviderOutcome::Terminal;
        };
        match transport.send(message).await {
            Ok(_) => {
                let digest = hex::encode(request.target_fingerprint().as_bytes());
                match ExternalResourceId::new(format!("smtp-{}", &digest[..24])) {
                    Ok(id) => SmtpProviderOutcome::Accepted(id),
                    Err(_) => SmtpProviderOutcome::Terminal,
                }
            }
            Err(error) if error.is_timeout() => SmtpProviderOutcome::Ambiguous,
            Err(error) if error.is_permanent() => SmtpProviderOutcome::Terminal,
            Err(_) => SmtpProviderOutcome::RetryableFailure,
        }
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("SMTP client configuration is invalid")]
pub struct SmtpClientConfigError;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use application::email::EmailPreview;

    use super::*;

    #[test]
    fn confirmed_attachment_without_bytes_fails_closed() {
        let preview = EmailPreview::new(
            vec!["recipient@example.com".to_owned()],
            Vec::new(),
            Vec::new(),
            "Subject".to_owned(),
            "Body".to_owned(),
            true,
            false,
            7,
        )
        .expect("preview");
        let request = EmailMessageRequest::from_preview(&preview).expect("request");
        let credentials =
            SmtpCredentials::new("sender@example.com".to_owned(), "password".to_owned());

        assert!(LettreSmtpClient::message(&credentials, &request).is_err());
    }
}
