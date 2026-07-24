use application::external_operation::ExternalResourceId;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};

use crate::auth::GoogleAccessToken;
use crate::calendar::{
    CalendarAccess, CalendarEventRequest, CalendarProviderOutcome, CalendarSelection,
    GoogleCalendarClient,
};
use crate::drive::ResolvedDestination;
use crate::sheets_docs::{FileKind, GoogleCreateClient};

#[derive(Clone)]
pub struct ReqwestGoogleClient {
    client: Client,
    calendar_base: Url,
    drive_files_url: Url,
}

impl ReqwestGoogleClient {
    pub fn new() -> Result<Self, GoogleClientError> {
        Self::with_endpoints(
            "https://www.googleapis.com/calendar/v3/",
            "https://www.googleapis.com/drive/v3/files",
        )
    }

    pub fn with_endpoints(
        calendar_base: &str,
        drive_files_url: &str,
    ) -> Result<Self, GoogleClientError> {
        Ok(Self {
            client: Client::new(),
            calendar_base: parse_endpoint(calendar_base)?,
            drive_files_url: parse_endpoint(drive_files_url)?,
        })
    }

    fn calendar_url(&self, segments: &[&str]) -> Result<Url, GoogleClientError> {
        let mut url = self.calendar_base.clone();
        url.path_segments_mut()
            .map_err(|_| GoogleClientError::Configuration)?
            .extend(segments);
        Ok(url)
    }
}

impl GoogleCreateClient for ReqwestGoogleClient {
    type Error = GoogleClientError;

    async fn create_file(
        &self,
        token: &GoogleAccessToken,
        kind: FileKind,
        title: &str,
        destination: &ResolvedDestination,
    ) -> Result<ExternalResourceId, Self::Error> {
        let mime_type = match kind {
            FileKind::Sheet => "application/vnd.google-apps.spreadsheet",
            FileKind::Doc => "application/vnd.google-apps.document",
        };
        let parents = destination
            .folder_id()
            .map(|folder| vec![folder.as_str().to_owned()])
            .unwrap_or_default();
        let body = DriveCreateRequest {
            name: title,
            mime_type,
            parents,
        };
        let response = self
            .client
            .post(self.drive_files_url.clone())
            .bearer_auth(token.as_str())
            .query(&[("supportsAllDrives", "true"), ("fields", "id")])
            .json(&body)
            .send()
            .await
            .map_err(|_| GoogleClientError::Transport)?;
        if !response.status().is_success() {
            return Err(classify_status(response.status()));
        }
        let payload: ResourceIdResponse = response
            .json()
            .await
            .map_err(|_| GoogleClientError::InvalidResponse)?;
        ExternalResourceId::new(payload.id).map_err(|_| GoogleClientError::InvalidResponse)
    }
}

impl GoogleCalendarClient for ReqwestGoogleClient {
    async fn calendar_access(
        &self,
        token: &GoogleAccessToken,
        calendar: &crate::calendar::CalendarId,
    ) -> CalendarAccess {
        let Ok(url) = self.calendar_url(&["users", "me", "calendarList", calendar.as_str()]) else {
            return CalendarAccess::Denied;
        };
        match self
            .client
            .get(url)
            .bearer_auth(token.as_str())
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match response.json::<CalendarListEntry>().await {
                    Ok(entry) if matches!(entry.access_role.as_str(), "owner" | "writer") => {
                        CalendarAccess::Writable
                    }
                    Ok(_) => CalendarAccess::Denied,
                    Err(_) => CalendarAccess::RetryableFailure,
                }
            }
            Ok(response) if response.status().is_client_error() => CalendarAccess::Denied,
            Ok(_) | Err(_) => CalendarAccess::RetryableFailure,
        }
    }

    async fn create_event(
        &self,
        token: &GoogleAccessToken,
        request: &CalendarEventRequest,
    ) -> CalendarProviderOutcome {
        let calendar_id = match request.calendar() {
            CalendarSelection::Primary => "primary",
            CalendarSelection::Alternate(id) => id.as_str(),
        };
        let Ok(url) = self.calendar_url(&["calendars", calendar_id, "events"]) else {
            return CalendarProviderOutcome::Terminal;
        };
        let attendees = request
            .attendees()
            .iter()
            .map(|email| CalendarAttendee {
                email: email.as_str(),
            })
            .collect();
        let mut overrides = Vec::new();
        if let Some(minutes) = request.reminders().push_minutes() {
            overrides.push(CalendarReminder {
                method: "popup",
                minutes,
            });
        }
        if let Some(minutes) = request.reminders().email_minutes() {
            overrides.push(CalendarReminder {
                method: "email",
                minutes,
            });
        }
        let body = CalendarCreateRequest {
            summary: request.title().as_str(),
            description: request.description().map(|value| value.as_str()),
            start: CalendarDateTime {
                date_time: request.start().as_str(),
                time_zone: request.timezone().as_str(),
            },
            end: CalendarDateTime {
                date_time: request.end().as_str(),
                time_zone: request.timezone().as_str(),
            },
            attendees,
            reminders: CalendarReminderSet {
                use_default: overrides.is_empty(),
                overrides,
            },
        };
        let send_updates = if request.send_invitations() {
            "all"
        } else {
            "none"
        };
        let response = self
            .client
            .post(url)
            .bearer_auth(token.as_str())
            .query(&[("sendUpdates", send_updates)])
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(_) => return CalendarProviderOutcome::Ambiguous,
        };
        if response.status().is_client_error() {
            return CalendarProviderOutcome::Terminal;
        }
        if !response.status().is_success() {
            return CalendarProviderOutcome::RetryableFailure;
        }
        match response.json::<ResourceIdResponse>().await {
            Ok(payload) => match ExternalResourceId::new(payload.id) {
                Ok(id) => CalendarProviderOutcome::Created(id),
                Err(_) => CalendarProviderOutcome::RetryableFailure,
            },
            Err(_) => CalendarProviderOutcome::Ambiguous,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GoogleClientError {
    #[error("Google API client configuration is invalid")]
    Configuration,
    #[error("Google API transport failed")]
    Transport,
    #[error("Google API request was rejected")]
    Rejected,
    #[error("Google API is temporarily unavailable")]
    Unavailable,
    #[error("Google API returned an invalid response")]
    InvalidResponse,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DriveCreateRequest<'a> {
    name: &'a str,
    mime_type: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    parents: Vec<String>,
}

#[derive(Deserialize)]
struct ResourceIdResponse {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarListEntry {
    access_role: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarCreateRequest<'a> {
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    start: CalendarDateTime<'a>,
    end: CalendarDateTime<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attendees: Vec<CalendarAttendee<'a>>,
    reminders: CalendarReminderSet,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarDateTime<'a> {
    date_time: &'a str,
    time_zone: &'a str,
}

#[derive(Serialize)]
struct CalendarAttendee<'a> {
    email: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CalendarReminderSet {
    use_default: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    overrides: Vec<CalendarReminder>,
}

#[derive(Serialize)]
struct CalendarReminder {
    method: &'static str,
    minutes: u16,
}

fn parse_endpoint(value: &str) -> Result<Url, GoogleClientError> {
    let url = Url::parse(value).map_err(|_| GoogleClientError::Configuration)?;
    let local_http = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| host == "localhost" || host == "127.0.0.1");
    if url.scheme() != "https" && !local_http {
        return Err(GoogleClientError::Configuration);
    }
    Ok(url)
}

fn classify_status(status: StatusCode) -> GoogleClientError {
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        GoogleClientError::Unavailable
    } else {
        GoogleClientError::Rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_remote_endpoint_is_rejected() {
        assert!(matches!(
            ReqwestGoogleClient::with_endpoints(
                "http://provider.example/calendar/v3/",
                "https://www.googleapis.com/drive/v3/files",
            ),
            Err(GoogleClientError::Configuration)
        ));
    }
}
